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
//! - A PANIC during the dry-run proposal (e.g. an interpreter bug on a
//!   hostile message) is contained per message (ADR-0029): the panicking
//!   message is isolated by prefix search, dropped as a LOCAL event with
//!   a loud log (message hash, sender, nonce, panic payload, stack trace
//!   via the panic hook), and block production continues. This is what
//!   breaks the systemd restart loop: the poison message is gone from the
//!   drop dir's pending set instead of killing the process on every tick.
//! - `commit_block` errors (e.g. `HeadMismatch`, `ForkDetected`) are fatal
//!   to the tick but not the loop: they indicate state moved under us,
//!   which on a single writer means a bug. Logged loudly; the next tick
//!   reloads state fresh.
//! - A PANIC during `commit_block` (real block application) is NEVER
//!   contained: it propagates and halts the node (ADR-0029). A panic
//!   there means this node's own execution is broken — mapping it to
//!   "invalid block" or bouncing the message would let a broken node keep
//!   running and silently diverge. Panics are never "invalid block" and
//!   never silently bounced, on either path.
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
use onx_stf::{propose_block, ExternalMessage, SigEntry, State, StfError};
use onx_storage::ChainStore;
use onx_telemetry::TelemetryHandle;
use std::any::Any;
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Canonical (pubkey-sorted) genesis validator public keys, for
/// `validator_index` assignment (ADR-0032).
fn canonical_validators(store: &ChainStore) -> Result<Vec<onx_primitives::PublicKey>, TickError> {
    let doc = store
        .genesis_document()
        .map_err(|e| TickError::Fatal(format!("producer: cannot load genesis: {e}")))?
        .ok_or_else(|| TickError::Fatal("producer: no genesis in store".to_string()))?;
    let mut keys: Vec<onx_primitives::PublicKey> = doc
        .validators
        .iter()
        .map(|v| {
            onx_primitives::PublicKey::decode_exact(&v.pubkey)
                .map_err(|e| TickError::Fatal(format!("producer: bad genesis key: {e}")))
        })
        .collect::<Result<_, _>>()?;
    keys.sort_by(|a, b| a.encode().cmp(&b.encode()));
    Ok(keys)
}

/// Sign a block with the producer's key (ONXBLK05).
///
/// Returns the signature section entries. If no signing key is configured,
/// returns an empty section (the block is unsigned — verification will
/// reject it; this is the pre-key transitional state).
///
/// `validators` is the canonical (pubkey-sorted) genesis validator list;
/// `validator_index` is the signer's position in it.
fn sign_block(
    block: &Block,
    chain_id: &[u8; 32],
    signing_key: Option<&onx_primitives::SecretKey>,
    validators: &[onx_primitives::PublicKey],
) -> Result<Vec<SigEntry>, String> {
    let Some(secret) = signing_key else {
        return Ok(vec![]);
    };
    let pubkey = secret.public_key();
    let index = validators
        .iter()
        .position(|v| v.encode() == pubkey.encode())
        .ok_or_else(|| "signing key pubkey not in genesis validator list".to_string())?;
    let preimage = block.header.sign_bytes(chain_id);
    let sig = secret.sign_raw(&preimage);
    Ok(vec![SigEntry {
        validator_index: index as u32,
        sig: sig.encode(),
    }])
}

pub struct ProducerConfig {
    pub fee_collector: AccountId,
    pub poll_interval: Duration,
    pub tx_pool_dir: PathBuf,
    pub blocks_dir: PathBuf,
    /// Optional telemetry handle; block height / pool size are reported
    /// when present.
    pub telemetry: Option<TelemetryHandle>,
    /// Validator signing key (32-byte seed). If present, blocks are signed
    /// (ONXBLK05); the key's pubkey must match a genesis validator
    /// (checked at startup — TRAP 4).
    pub signing_key: Option<onx_primitives::SecretKey>,
}

#[derive(Debug, Default)]
pub struct ProducerStats {
    pub blocks_produced: u64,
    pub msgs_committed: u64,
    pub txs_rejected: u64,
    pub ticks_idle: u64,
}

/// Install the producer panic hook exactly once: on any panic, log the
/// payload and a full stack trace LOUDLY to stderr, then run the default
/// hook. `force_capture` (not `capture`) so the trace is recorded even
/// when `RUST_BACKTRACE` is unset — panics are rare, the cost is fine.
fn install_panic_backtrace_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            eprintln!(
                "onxd PANIC: {info}\nstack backtrace:\n{:?}",
                std::backtrace::Backtrace::force_capture()
            );
            default(info);
        }));
    });
}

/// TEST-ONLY hook: when armed with a message hash, [`propose_block_caught`]
/// panics on any probe whose message set contains that hash — simulating
/// an interpreter/STF panic during the producer dry-run. Production builds
/// have no hook here.
#[cfg(test)]
static PANIC_ON_MSG_HASH: std::sync::Mutex<Option<[u8; 32]>> = std::sync::Mutex::new(None);

/// TEST-ONLY: arm the dry-run panic injection for one message hash.
#[cfg(test)]
pub(crate) fn test_arm_propose_panic_on(msg_hash: [u8; 32]) {
    *PANIC_ON_MSG_HASH.lock().unwrap() = Some(msg_hash);
}

/// TEST-ONLY: disarm the dry-run panic injection.
#[cfg(test)]
pub(crate) fn test_disarm_propose_panic() {
    *PANIC_ON_MSG_HASH.lock().unwrap() = None;
}

#[cfg(test)]
fn maybe_inject_propose_panic(messages: &[ExternalMessage]) {
    // Copy the target out and DROP the guard before any panic: panicking
    // while holding a std Mutex guard would poison the mutex and turn the
    // next probe's lock() into a second, unrelated panic.
    let target: Option<[u8; 32]> = *PANIC_ON_MSG_HASH.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hash) = target {
        if messages.iter().any(|m| m.hash() == hash) {
            panic!(
                "injected test panic: dry-run execution panicked on message {}",
                hex::encode(hash)
            );
        }
    }
}

/// A dry-run proposal that may have panicked: `Ok(inner)` is the normal
/// `propose_block` result; `Err(payload)` is a captured panic.
///
/// PANIC POLICY (ADR-0029): the producer dry-run runs INSIDE
/// `catch_unwind`. A panic here is a LOCAL event — the offending message
/// is isolated and dropped, the panic is logged loudly, and block
/// production continues. `propose_block` takes `&State` and clones
/// internally, so a caught panic cannot leave the caller's state half
/// mutated; `AssertUnwindSafe` documents that the closure's inputs carry
/// no unwind-sensitive interior state across the boundary.
///
/// This containment is deliberately NOT applied to real block
/// application: `commit_block` (which runs the STF's `apply_block`) is
/// never wrapped, so a panic there propagates and halts the node. A panic
/// is never an "invalid block", never a bounce, and never silent.
fn propose_block_caught(
    state: &State,
    messages: Vec<ExternalMessage>,
    lt: u64,
    fee_collector: AccountId,
) -> Result<Result<Block, StfError>, Box<dyn Any + Send>> {
    catch_unwind(AssertUnwindSafe(|| {
        // TEST-ONLY: simulates an interpreter/STF panic DURING the
        // dry-run, i.e. inside the containment boundary.
        #[cfg(test)]
        maybe_inject_propose_panic(&messages);
        // ONXBLK05: protocol_version 1; block_time is wall-clock at proposal
        // (producer policy: never stamp ahead of its own clock).
        let block_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        propose_block(state, messages, lt, fee_collector, 1, block_time)
    }))
}

/// One-line summary of a captured panic payload for loud logging.
fn panic_summary(payload: &Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "<non-string panic payload>".to_string()
    }
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
    // Panic hook first: any panic anywhere in this process gets its
    // payload and a full stack trace logged loudly to stderr before the
    // default handler runs. This is what makes contained dry-run panics
    // diagnosable after the fact, and apply_block panics diagnosable
    // before the halt.
    install_panic_backtrace_hook();
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

#[derive(Debug)]
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
    //
    // PANIC POLICY (ADR-0029): this call is deliberately NOT wrapped in
    // catch_unwind. A panic inside commit_block/apply_block — real block
    // application — means this node's own execution is broken; it
    // propagates and halts the node. Mapping it to "invalid block" or
    // bouncing the message would let a broken node keep running and
    // silently diverge from honest nodes.
    //
    // ONXBLK05: sign the block (if a signing key is configured), then
    // commit block+signatures atomically (TRAP 2).
    let validators = canonical_validators(store)?;
    let sig_entries = sign_block(
        &block,
        &state.chain_id,
        cfg.signing_key.as_ref(),
        &validators,
    )
    .map_err(|e| TickError::Fatal(format!("producer: signing failed: {e}")))?;
    store
        .commit_block(&state, &block, &sig_entries)
        .map_err(|e| TickError::Fatal(format!("producer: commit_block failed: {e}")))?;

    // 7. Emit the canonical block file (feeds `onx replay` directly),
    // atomically: a crash mid-write must never leave a torn `.blk` file.
    atomic_write_block_file(
        &cfg.blocks_dir,
        block.header.seqno,
        &encode_block_file(&block, &sig_entries),
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

/// Build a block from candidates, dropping ONLY messages the STF rejects
/// or that panic the dry-run.
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
/// Panic path (ADR-0029): if the dry-run PANICS, the panicking message is
/// isolated the same way and dropped as a LOCAL event — loud log, message
/// removed, production continues. This is what breaks the kill-restart
/// loop: without containment, the poison message sits in the drop dir and
/// kills the process on every tick.
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
    let mut candidates = candidates;
    loop {
        if candidates.is_empty() {
            return Ok(None);
        }
        match propose_block_caught(state, candidates.clone(), lt, fee_collector) {
            Ok(Ok(block)) => return Ok(Some(block)),
            Ok(Err(e)) => {
                eprintln!(
                    "producer: propose_block rejected filtered candidates ({e}); isolating offending message(s)"
                );
                match find_first_bad_prefix(state, &candidates, lt, fee_collector) {
                    Err(()) => {
                        // A bisection probe panicked: this is not a
                        // rejection — switch to panic isolation for the
                        // same candidate set rather than misreading the
                        // panic as a verdict.
                        eprintln!(
                            "producer: bisection probe panicked during rejection isolation; switching to panic isolation"
                        );
                        drop_panicking_message(
                            state,
                            &mut candidates,
                            lt,
                            fee_collector,
                            mempool,
                            stats,
                        )?;
                    }
                    Ok(None) => {
                        return Err(TickError::Retryable(
                            "propose_block failed but no bad prefix found (STF bug?); mempool left intact"
                                .to_string(),
                        ));
                    }
                    Ok(Some(k)) => {
                        let culprit = candidates.remove(k);
                        eprintln!(
                            "producer: dropping message {} from sender {}: rejected by block proposal",
                            hex::encode(culprit.hash()),
                            hex::encode(culprit.from.to_bytes())
                        );
                        mempool.reject_candidate(
                            &culprit.hash(),
                            "propose_block rejected this message",
                        );
                        stats.txs_rejected += 1;
                        // Hold the culprit's same-sender successors for a later block:
                        // their nonces are gapped until the sender resubmits the missing
                        // one. They stay in `pending/` — only this block's working set
                        // shrinks.
                        let (sender, nonce) = (culprit.from, culprit.nonce);
                        candidates.retain(|m| m.from != sender || m.nonce < nonce);
                    }
                }
            }
            Err(panic) => {
                eprintln!(
                    "producer: PANIC during dry-run block proposal: {}",
                    panic_summary(&panic)
                );
                drop_panicking_message(state, &mut candidates, lt, fee_collector, mempool, stats)?;
            }
        }
    }
}

/// Isolate the message whose dry-run execution panics and drop it as a
/// LOCAL event: the message is removed from the candidate set, moved to
/// the mempool's rejected dir with a loud log (message hash, sender,
/// nonce — the stack trace comes from the panic hook installed at
/// producer startup), and its same-sender successors are held for a later
/// block, exactly like the rejection path. A panicking message must not
/// take honest messages down with it either.
///
/// Returns `Err(TickError::Retryable)` when no panicking message can be
/// isolated (the panic is not message-caused): the tick is skipped with
/// the mempool intact — never blame a message for a panic it did not
/// cause.
fn drop_panicking_message(
    state: &State,
    candidates: &mut Vec<ExternalMessage>,
    lt: u64,
    fee_collector: AccountId,
    mempool: &mut Mempool,
    stats: &mut ProducerStats,
) -> Result<(), TickError> {
    let k = find_first_panicking_prefix(state, candidates, lt, fee_collector).ok_or_else(|| {
        TickError::Retryable(
            "dry-run panicked but no panicking message prefix found (panic is not message-caused); mempool left intact"
                .to_string(),
        )
    })?;
    let culprit = candidates.remove(k);
    eprintln!(
        "producer: PANIC CONTAINED — dropping message {} from sender {} nonce {}: \
         dry-run execution panicked. Local event only: message moved to rejected/, \
         block production continues. This is NOT an invalid block and NOT a bounce.",
        hex::encode(culprit.hash()),
        hex::encode(culprit.from.to_bytes()),
        culprit.nonce,
    );
    mempool.reject_candidate(
        &culprit.hash(),
        "dry-run execution panicked (local fault, not a bounce)",
    );
    stats.txs_rejected += 1;
    let (sender, nonce) = (culprit.from, culprit.nonce);
    candidates.retain(|m| m.from != sender || m.nonce < nonce);
    Ok(())
}

/// Binary search for the smallest `k` such that
/// `propose_block(candidates[..=k])` fails. Returns `Ok(None)` only if the full
/// set proposes cleanly (the caller already saw it fail, so this is
/// defensive).
///
/// The predicate is monotonic: adding messages cannot repair an earlier
/// wallet-handler rejection (balance only decreases through a block, and
/// every other phase-1 check is per-message). Prefixes preserve per-sender
/// nonce contiguity, so every probe is meaningful.
///
/// Probes run inside [`propose_block_caught`]: if a probe PANICS, `Err(())`
/// is returned and the caller switches to panic isolation — a panicking
/// probe is not a rejection and must never be misread as one.
fn find_first_bad_prefix(
    state: &State,
    candidates: &[ExternalMessage],
    lt: u64,
    fee_collector: AccountId,
) -> Result<Option<usize>, ()> {
    // Invariant: propose(candidates[..lo]) succeeds, propose(candidates[..hi]) fails.
    // lo = 0 holds because the empty prefix has no messages to reject;
    // hi = len holds because the caller saw the full set fail.
    let mut lo = 0usize;
    let mut hi = candidates.len();
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        match propose_block_caught(state, candidates[..mid].to_vec(), lt, fee_collector) {
            Ok(Ok(_)) => lo = mid,
            Ok(Err(_)) => hi = mid,
            Err(_) => return Err(()),
        }
    }
    // propose(..lo) ok, propose(..lo+1) fails → culprit is index lo.
    if lo < candidates.len() {
        Ok(Some(lo))
    } else {
        Ok(None)
    }
}

/// Binary search for the smallest `k` such that the dry-run proposal of
/// `candidates[..=k]` PANICS. Returns `None` when no prefix panics — in
/// particular when the EMPTY prefix already panics, which means the panic
/// is not message-caused (bad state/config) and no message may be blamed
/// for it.
///
/// Monotonicity caveat: unlike rejections, panics are not provably
/// monotonic in the prefix (a panic can depend on accumulated dry-run
/// state). The STF dry-run is deterministic, so in practice the bisection
/// still isolates the message whose addition first triggers the panic; a
/// non-deterministic panic is an STF bug and is logged loudly rather than
/// silently absorbed. The outer loop re-evaluates after every drop, so a
/// mis-isolation costs one message, not the tick.
fn find_first_panicking_prefix(
    state: &State,
    candidates: &[ExternalMessage],
    lt: u64,
    fee_collector: AccountId,
) -> Option<usize> {
    // Empty-prefix probe: panicking here means the panic is not
    // message-caused. Never blame a message for it.
    if propose_block_caught(state, Vec::new(), lt, fee_collector).is_err() {
        return None;
    }
    // Invariant: propose(..lo) does not panic, propose(..hi) panics.
    // hi = len holds because the caller observed the full set panic.
    let mut lo = 0usize;
    let mut hi = candidates.len();
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if propose_block_caught(state, candidates[..mid].to_vec(), lt, fee_collector).is_err() {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    // propose(..lo) clean, propose(..lo+1) panics → culprit is index lo.
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
                Ok(signed) => {
                    let committed = store
                        .block_hash_for_seqno(seqno)
                        .map_err(|e| format!("producer: block hash lookup failed: {e}"))?;
                    committed != Some(signed.block.header.hash())
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
        let sig_bytes = store
            .get_block_sigs(&hash)
            .map_err(|e| format!("producer: sig lookup failed: {e}"))?
            .ok_or_else(|| format!("producer: missing sigs for seqno {seqno}"))?;
        let sig_entries = onx::auth::decode_sig_section(&sig_bytes)
            .map_err(|e| format!("producer: bad stored sigs for seqno {seqno}: {e}"))?;
        let block = Block { header, body };
        atomic_write_block_file(blocks_dir, seqno, &encode_block_file(&block, &sig_entries))?;
        regenerated += 1;
        eprintln!("producer: regenerated block file for seqno {seqno}");
    }
    Ok(regenerated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use onx_stf::{ExternalMessage, MsgKind, State};
    use onx_storage::support::{test_fee_collector, test_genesis, test_secret_key};

    /// RAII guard: disarms the dry-run panic injection on drop, so a
    /// failing assertion cannot leak an armed hook into other tests.
    struct PanicArmGuard;
    impl PanicArmGuard {
        fn arm(msg_hash: [u8; 32]) -> Self {
            test_arm_propose_panic_on(msg_hash);
            Self
        }
    }
    impl Drop for PanicArmGuard {
        fn drop(&mut self) {
            test_disarm_propose_panic();
        }
    }

    fn test_account(idx: u8) -> AccountId {
        let mut b = [0u8; 32];
        b[0] = idx;
        AccountId::from_bytes(b)
    }

    fn signed_transfer(state: &State, from_idx: u8, nonce: u64) -> ExternalMessage {
        let from = test_account(from_idx);
        let secret = test_secret_key(&from);
        ExternalMessage::new_signed(
            state.chain_id,
            MsgKind::Transfer,
            from,
            nonce,
            test_account(0x99),
            1_000,
            10,
            Vec::new(),
            [0u8; 32],
            &secret,
        )
    }

    /// ADR-0029 dry-run panic containment: a message whose dry-run
    /// execution panics is a LOCAL event — it is isolated by prefix
    /// search, dropped loudly, and block production continues with the
    /// remaining messages. The node must not die, wedge, or quarantine
    /// the mempool.
    #[test]
    fn dry_run_panic_is_contained_per_message() {
        let state = State::from_genesis(&test_genesis());
        let fee_collector = test_fee_collector();
        let poison = signed_transfer(&state, 0, 0);
        let honest = signed_transfer(&state, 1, 0);

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("onxd-panic-test-{}-{nanos}", std::process::id()));
        let mut mempool = Mempool::new(&dir, 1000, state.chain_id, fee_collector).expect("mempool");
        let mut stats = ProducerStats::default();

        let _guard = PanicArmGuard::arm(poison.hash());
        let block = propose_robust(
            &state,
            vec![poison.clone(), honest.clone()],
            state.last_lt + 1,
            fee_collector,
            &mut mempool,
            &mut stats,
        )
        .expect("propose_robust must not fail on a contained panic")
        .expect("a block is still produced from the surviving message");

        // The panicking message is gone; the honest one is committed.
        assert_eq!(block.body.messages.len(), 1);
        assert_eq!(block.body.messages[0].hash(), honest.hash());
        assert_eq!(
            stats.txs_rejected, 1,
            "exactly the panicking message is dropped"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
