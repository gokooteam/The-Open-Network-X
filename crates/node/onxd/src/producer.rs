//! Single-node block-production loop.
//!
//! This is the daemon's beating heart: drain the mempool, propose a block
//! through the honest producer path, commit it atomically, advance the head,
//! repeat. When the mempool is empty the loop sleeps — no busy-loop, no
//! empty blocks.
//!
//! ## Cadence: demand-based
//!
//! A block is produced when, and only when, the mempool holds at least one
//! valid candidate. Time-based production (a block every N seconds) would
//! work — `lt` is logical, so empty blocks would still be deterministic —
//! but empty blocks bloat the chain for no reason on a single node with no
//! consensus timing requirements. The poll interval (`block_poll_interval_ms`,
//! default 200ms) is a *liveness* parameter only: it bounds how long a
//! submitted transaction waits, and it never appears in block content.
//!
//! ## Block content determinism
//!
//! Given the same mempool state, the loop always builds the same block:
//! candidates come from `Mempool::select_candidates` (deterministic
//! `(sender, nonce)` order), `lt` is `head.last_lt + 1` (logical, never
//! wall-clock), and the fee collector is fixed config. Timing affects
//! *which* messages made it into the mempool before a tick, never the
//! block built from a given mempool state.
//!
//! ## Identities
//!
//! The node operator configures `fee_collector` (an account id, hex) —
//! explicit, no magic accounts. The daemon never holds sender keys: it only
//! verifies signatures against on-chain pubkeys. (Block-level authorship
//! signatures are a consensus-phase concern; this milestone has one honest
//! producer and no validator set.)
//!
//! ## Shutdown
//!
//! The loop checks an `AtomicBool` every tick. On shutdown it finishes the
//! current tick — including any in-flight `commit_block`, which is atomic
//! (fully committed or not at all) — then returns. A proposed-but-never-
//! committed block simply never existed: its transactions remain in
//! `pending/` and are re-proposed on the next startup. There is no such
//! thing as a half-written block.
//!
//! ## Failure handling
//!
//! The loop never panics on bad input and never wedges:
//! - `propose_block` failing on a filtered candidate set means the filter
//!   and the STF disagree (a bug — the shared [`WalletMirror`](crate::mempool::WalletMirror)
//!   validation should have caught it). The producer isolates the offending
//!   message(s) by prefix search against the real STF and drops ONLY those,
//!   then retries; a second failure is logged loudly and the tick is
//!   skipped with the mempool intact. There is no bulk quarantine: one bad
//!   message must never take honest messages down with it.
//! - `commit_block` errors (e.g. `HeadMismatch`, `ForkDetected`) are fatal
//!   to the tick but not the loop: they indicate state moved under us,
//!   which on a single writer means a bug. Logged loudly; the next tick
//!   reloads state fresh.
//!
//! ## Block files
//!
//! Block files are written atomically (temp file + rename) AFTER the
//! database commit. A crash in between used to leave a gap in `blocks/`;
//! now the database is the source of truth and any missing (or torn)
//! block file is regenerated from it at startup.

use crate::mempool::Mempool;
use onx::blockfile::{block_file_name, decode_block_file, encode_block_file};
use onx_data_structures::AccountId;
use onx_stf::block::Block;
use onx_stf::{propose_block, ExternalMessage, State};
use onx_storage::ChainStore;
use onx_telemetry::TelemetryHandle;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct ProducerConfig {
    pub fee_collector: AccountId,
    pub poll_interval: Duration,
    pub tx_pool_dir: PathBuf,
    pub blocks_dir: PathBuf,
    /// Optional telemetry handle; block height / pool size are reported
    /// when present.
    pub telemetry: Option<TelemetryHandle>,
}

#[derive(Debug, Default)]
pub struct ProducerStats {
    pub blocks_produced: u64,
    pub msgs_committed: u64,
    pub txs_rejected: u64,
    pub ticks_idle: u64,
}

/// Run the block-production loop until `shutdown` is set.
///
/// Blocking: callers should run this on a dedicated thread (e.g.
/// `tokio::task::spawn_blocking`). Returns statistics on clean shutdown,
/// or a fatal error string (corrupt store, missing genesis, lt overflow).
pub fn run_producer_loop(
    store: ChainStore,
    mut mempool: Mempool,
    cfg: ProducerConfig,
    shutdown: Arc<AtomicBool>,
) -> Result<ProducerStats, String> {
    fs::create_dir_all(&cfg.blocks_dir)
        .map_err(|e| format!("producer: cannot create blocks dir: {e}"))?;
    // Crash recovery for the block-file gap: a kill between the atomic DB
    // commit and the block-file write leaves blocks committed but files
    // missing. The database holds every committed header and body, so
    // missing (or torn) files are regenerated from it here.
    let swept = sweep_temp_block_files(&cfg.blocks_dir)?;
    let regenerated = regenerate_missing_block_files(&store, &cfg.blocks_dir)?;
    if swept > 0 || regenerated > 0 {
        eprintln!(
            "producer: startup recovery: swept {swept} temp files, regenerated {regenerated} block files"
        );
    }
    // Crash-safe mempool: rehydrate pending/ from the previous run, if any.
    let rehydrated = mempool.rehydrate()?;
    if rehydrated > 0 {
        eprintln!("producer: rehydrated {rehydrated} pending transactions from previous run");
    }

    let mut stats = ProducerStats::default();

    while !shutdown.load(Ordering::Relaxed) {
        match run_tick(&store, &mut mempool, &cfg, &mut stats) {
            Ok(produced) => {
                if produced {
                    // Block committed.
                } else {
                    stats.ticks_idle += 1;
                }
            }
            Err(TickError::Fatal(e)) => return Err(e),
            Err(TickError::Retryable(e)) => {
                // Loud log, skip the tick, mempool intact. There is no bulk
                // quarantine: only messages the STF actually rejects are
                // ever dropped (see propose_robust).
                eprintln!("producer: tick failed, retrying next tick: {e}");
            }
        }
        if let Some(t) = &cfg.telemetry {
            t.set_tx_pool_size(mempool.len() as i64);
        }
        std::thread::sleep(cfg.poll_interval);
    }
    Ok(stats)
}

enum TickError {
    /// Stop the loop: corrupt store, missing genesis, lt overflow.
    Fatal(String),
    /// Skip this tick and retry: proposal/validation disagreement.
    Retryable(String),
}

/// One production tick. Returns Ok(true) if a block was committed.
fn run_tick(
    store: &ChainStore,
    mempool: &mut Mempool,
    cfg: &ProducerConfig,
    stats: &mut ProducerStats,
) -> Result<bool, TickError> {
    // 1. Intake: sweep the drop directory.
    let intake = mempool
        .scan_drop_dir(&cfg.tx_pool_dir, store)
        .map_err(TickError::Fatal)?;
    stats.txs_rejected += intake.rejected;

    // 2. Fresh head state (fail-closed: gap-2 root verification inside).
    let state: State = store
        .load_state()
        .map_err(|e| TickError::Fatal(format!("producer: load_state failed: {e}")))?
        .ok_or_else(|| {
            TickError::Fatal("producer: no state (genesis not initialized)".to_string())
        })?;

    // 3. Deterministic candidate selection against this exact state.
    let candidates = mempool
        .select_candidates(&state)
        .map_err(TickError::Fatal)?;
    if candidates.is_empty() {
        return Ok(false);
    }

    // 4. Logical time: strictly increasing, never wall-clock.
    let lt = state
        .last_lt
        .checked_add(1)
        .ok_or_else(|| TickError::Fatal("producer: logical time overflow".to_string()))?;

    // 5. Honest producer path: dry-run through the same apply as validation.
    // propose_robust drops ONLY messages the STF actually rejects (never
    // the whole set) if the filtered candidates unexpectedly fail.
    let block = match propose_robust(&state, candidates, lt, cfg.fee_collector, mempool, stats)? {
        Some(block) => block,
        None => return Ok(false), // everything was dropped; idle tick
    };

    // 6. Atomic commit: STF re-validation + state/body/index/head in one txn.
    store
        .commit_block(&state, &block)
        .map_err(|e| TickError::Fatal(format!("producer: commit_block failed: {e}")))?;

    // 7. Emit the canonical block file (feeds `onx replay` directly),
    // atomically: a crash mid-write must never leave a torn `.blk` file.
    atomic_write_block_file(
        &cfg.blocks_dir,
        block.header.seqno,
        &encode_block_file(&block),
    )
    .map_err(TickError::Fatal)?;

    // 8. Remove committed transactions from the mempool.
    mempool.remove_committed(&block.body.messages);

    stats.blocks_produced += 1;
    stats.msgs_committed += block.body.messages.len() as u64;
    if let Some(t) = &cfg.telemetry {
        t.set_block_height(block.header.seqno as i64);
    }
    eprintln!(
        "producer: committed block seqno={} msgs={} root={}",
        block.header.seqno,
        block.body.messages.len(),
        hex::encode(block.header.state_root)
    );
    Ok(true)
}

/// Build a block from candidates, dropping ONLY messages the STF rejects.
///
/// Fast path: `propose_block` on the whole set. The mempool filter already
/// mirrors the wallet handler via the shared [`WalletMirror`](crate::mempool::WalletMirror)
/// validation, so this succeeds in honest operation.
///
/// Slow path (defense in depth): if the filtered set unexpectedly fails,
/// the offending message is isolated by prefix search against the real STF
/// — `propose_block` fails on the first invalid message in order — and
/// rejected alone. Same-sender successors of a culprit are *held* for a
/// later block (their nonces are gapped until the sender resubmits the
/// missing nonce), never rejected: they did nothing wrong.
///
/// A second full failure after isolation is a genuine STF bug: loud log,
/// skip the tick, mempool intact. There is deliberately no bulk quarantine.
fn propose_robust(
    state: &State,
    candidates: Vec<ExternalMessage>,
    lt: u64,
    fee_collector: AccountId,
    mempool: &mut Mempool,
    stats: &mut ProducerStats,
) -> Result<Option<Block>, TickError> {
    match propose_block(state, candidates.clone(), lt, fee_collector) {
        Ok(block) => return Ok(Some(block)),
        Err(e) => eprintln!(
            "producer: propose_block rejected filtered candidates ({e}); isolating offending message(s)"
        ),
    }
    let mut candidates = candidates;
    loop {
        if candidates.is_empty() {
            return Ok(None);
        }
        let k = find_first_bad_prefix(state, &candidates, lt, fee_collector).ok_or_else(|| {
            TickError::Retryable(
                "propose_block failed but no bad prefix found (STF bug?); mempool left intact"
                    .to_string(),
            )
        })?;
        let culprit = candidates.remove(k);
        eprintln!(
            "producer: dropping message {} from sender {}: rejected by block proposal",
            hex::encode(culprit.hash()),
            hex::encode(culprit.from.to_bytes())
        );
        mempool.reject_candidate(&culprit.hash(), "propose_block rejected this message");
        stats.txs_rejected += 1;
        // Hold the culprit's same-sender successors for a later block:
        // their nonces are gapped until the sender resubmits the missing
        // one. They stay in `pending/` — only this block's working set
        // shrinks.
        let (sender, nonce) = (culprit.from, culprit.nonce);
        candidates.retain(|m| m.from != sender || m.nonce < nonce);
        match propose_block(state, candidates.clone(), lt, fee_collector) {
            Ok(block) => return Ok(Some(block)),
            Err(e) => eprintln!(
                "producer: still failing after dropping culprit ({e}); continuing isolation"
            ),
        }
    }
}

/// Binary search for the smallest `k` such that
/// `propose_block(candidates[..=k])` fails. Returns `None` only if the full
/// set proposes cleanly (the caller already saw it fail, so this is
/// defensive).
///
/// The predicate is monotonic: adding messages cannot repair an earlier
/// wallet-handler rejection (balance only decreases through a block, and
/// every other phase-1 check is per-message). Prefixes preserve per-sender
/// nonce contiguity, so every probe is meaningful.
fn find_first_bad_prefix(
    state: &State,
    candidates: &[ExternalMessage],
    lt: u64,
    fee_collector: AccountId,
) -> Option<usize> {
    // Invariant: propose(candidates[..lo]) succeeds, propose(candidates[..hi]) fails.
    // lo = 0 holds because the empty prefix has no messages to reject;
    // hi = len holds because the caller saw the full set fail.
    let mut lo = 0usize;
    let mut hi = candidates.len();
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if propose_block(state, candidates[..mid].to_vec(), lt, fee_collector).is_ok() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    // propose(..lo) ok, propose(..lo+1) fails → culprit is index lo.
    if lo < candidates.len() {
        Some(lo)
    } else {
        None
    }
}

/// Temp path for an atomic block-file write. Hidden name, per-process
/// suffix: a crash leaves a stray `.tmp-block-*` file (swept at startup),
/// never a torn `.blk` file.
fn temp_block_path(blocks_dir: &Path, seqno: u32) -> PathBuf {
    blocks_dir.join(format!(
        ".tmp-block-{:08}-{}.blk",
        seqno,
        std::process::id()
    ))
}

/// Atomically write a block file: write + fsync a temp file in the same
/// directory, then rename over the target. Rename is atomic on a single
/// filesystem, so readers never see a partial file.
fn atomic_write_block_file(blocks_dir: &Path, seqno: u32, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let tmp = temp_block_path(blocks_dir, seqno);
    {
        let mut f = fs::File::create(&tmp)
            .map_err(|e| format!("producer: cannot create temp block file: {e}"))?;
        f.write_all(bytes)
            .map_err(|e| format!("producer: cannot write temp block file: {e}"))?;
        f.sync_all()
            .map_err(|e| format!("producer: cannot fsync temp block file: {e}"))?;
    }
    fs::rename(&tmp, blocks_dir.join(block_file_name(seqno)))
        .map_err(|e| format!("producer: cannot publish block file: {e}"))?;
    Ok(())
}

/// Remove temp files left by crashed block-file writes.
fn sweep_temp_block_files(blocks_dir: &Path) -> Result<usize, String> {
    let mut swept = 0;
    let entries =
        fs::read_dir(blocks_dir).map_err(|e| format!("producer: cannot read blocks dir: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("producer: dir entry failed: {e}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".tmp-block-") {
            fs::remove_file(entry.path())
                .map_err(|e| format!("producer: cannot sweep temp file: {e}"))?;
            swept += 1;
        }
    }
    Ok(swept)
}

/// Regenerate block files missing from `blocks_dir`, from the database.
///
/// A crash between the atomic DB commit and the block-file write leaves a
/// block committed but its `.blk` file absent — a gap `onx replay` would
/// choke on. The database holds every committed header and body, so files
/// are rebuilt here at startup. Existing files are verified against the
/// committed header hash and rewritten on mismatch (covers torn files
/// written before atomic writes existed).
fn regenerate_missing_block_files(store: &ChainStore, blocks_dir: &Path) -> Result<usize, String> {
    let head_seqno = match store
        .head()
        .map_err(|e| format!("producer: head lookup failed: {e}"))?
    {
        Some((seqno, _)) => seqno,
        None => return Ok(0), // genesis only: no blocks to regenerate
    };
    let mut regenerated = 0;
    for seqno in 1..=head_seqno {
        let path = blocks_dir.join(block_file_name(seqno));
        let needs_write = match fs::read(&path) {
            Ok(bytes) => match decode_block_file(&bytes) {
                Ok(block) => {
                    let committed = store
                        .block_hash_for_seqno(seqno)
                        .map_err(|e| format!("producer: block hash lookup failed: {e}"))?;
                    committed != Some(block.header.hash())
                }
                Err(_) => true, // undecodable: rewrite
            },
            Err(_) => true, // missing: rewrite
        };
        if !needs_write {
            continue;
        }
        let hash = store
            .block_hash_for_seqno(seqno)
            .map_err(|e| format!("producer: block hash lookup failed: {e}"))?
            .ok_or_else(|| format!("producer: no committed block at seqno {seqno}"))?;
        let header = store
            .get_block_header(&hash)
            .map_err(|e| format!("producer: header lookup failed: {e}"))?
            .ok_or_else(|| format!("producer: missing header for seqno {seqno}"))?;
        let body = store
            .get_block_body(&hash)
            .map_err(|e| format!("producer: body lookup failed: {e}"))?
            .ok_or_else(|| format!("producer: missing body for seqno {seqno}"))?;
        let block = Block { header, body };
        atomic_write_block_file(blocks_dir, seqno, &encode_block_file(&block))?;
        regenerated += 1;
        eprintln!("producer: regenerated block file for seqno {seqno}");
    }
    Ok(regenerated)
}
