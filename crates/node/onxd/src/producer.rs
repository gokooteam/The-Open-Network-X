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
//! - `propose_block` failing with `BlockGasExceeded` is not a rejection:
//!   every message is valid, there are just too many for one block
//!   (ADR-0034's `MAX_GAS_PER_BLOCK`). The block ends before the message
//!   that tipped it over; that message and everything after it stay in
//!   `pending/` for a later block. Rejecting it instead would strand the
//!   sender's later nonces behind it, and anyone could get honest messages
//!   dropped by filling blocks with heavy calls.
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

use crate::blockfiles::{
    atomic_write_block_file, regenerate_missing_block_files, sweep_temp_block_files,
};
use crate::mempool::Mempool;
use onx::blockfile::{block_file_name, decode_block_file, encode_block_file, BLOCK_FILE_MAGIC};
use onx_data_structures::AccountId;
use onx_networking::block_sync::MAX_BLOCK_FILE_BYTES;
use onx_stf::block::{Block, BLOCK_HEADER_BYTE_LEN, PROTOCOL_VERSION, SIG_ENTRY_BYTE_LEN};
use onx_stf::{propose_block, ExternalMessage, SigEntry, State, StfError};
use onx_storage::encoding::{BODY_COUNT_LEN, MSG_LEN_PREFIX};
use onx_storage::ChainStore;
use onx_telemetry::TelemetryHandle;
use std::any::Any;
use std::cell::Cell;
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

thread_local! {
    /// While set, Sentry must not capture (or stall on) panics on this thread.
    ///
    /// `propose_block_caught` sets it for the dry-run containment boundary:
    /// panics there are deliberate probes for hostile-message isolation
    /// (ADR-0029), not crashes. The panic-hook wrapper installed in `main`
    /// skips sentry's hook entirely while the flag is set — a `before_send`
    /// filter is not enough, because the hook's synchronous flush still
    /// stalls the thread. One summary event is sent for the isolated
    /// message instead (`drop_panicking_message`, rate-limited).
    static SUPPRESS_SENTRY_PANIC: Cell<bool> = const { Cell::new(false) };
}

/// Whether Sentry panic reporting is currently suppressed on this thread.
/// Read by the panic-hook wrapper installed in `main.rs`.
pub fn suppress_sentry_panic() -> bool {
    SUPPRESS_SENTRY_PANIC.with(|f| f.get())
}

/// Which kind of Sentry summary is being rate-limited. Warnings (an isolated
/// hostile message) and errors (a genuine STF/state bug) get independent
/// slots: a hostile-message flood must never delay or suppress an STF-bug
/// report, and the dropped-counts must not mix the two kinds.
#[derive(Clone, Copy)]
enum SummaryKind {
    Warning,
    Error,
}

/// Rate-limit state: (current window start, events dropped in this window).
type RateLimitState = (Option<std::time::Instant>, u32);

/// Pure window logic, extracted for testing: `Some(dropped)` when the
/// caller may send now (with the count of same-kind events dropped since
/// the last send), `None` when this event must be dropped.
fn take_slot(state: &mut RateLimitState, now: std::time::Instant) -> Option<u32> {
    match state.0 {
        Some(t) if now.duration_since(t) < Duration::from_secs(60) => {
            state.1 += 1;
            None
        }
        _ => {
            let dropped = state.1;
            state.0 = Some(now);
            state.1 = 0;
            Some(dropped)
        }
    }
}

/// At most one Sentry summary event per 60s window *per kind*; returns
/// `Some(dropped)` with the number of same-kind events dropped since the
/// last sent one when the caller may send now, `None` when this event must
/// be dropped. A hostile-message flood must not burn Sentry quota or fill
/// sentry's queue (a full queue silently drops events, so a real crash
/// arriving mid-flood could be lost).
fn take_summary_slot(kind: SummaryKind) -> Option<u32> {
    use std::sync::{LazyLock, Mutex};
    use std::time::Instant;
    static STATES: LazyLock<[Mutex<RateLimitState>; 2]> =
        LazyLock::new(|| [Mutex::new((None, 0)), Mutex::new((None, 0))]);
    // Poison recovery instead of expect: the state is a plain
    // timestamp+counter pair, so a poisoned lock still holds usable data.
    // A diagnostic-only feature must never panic the producer.
    let mut state = STATES[kind as usize]
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    take_slot(&mut state, Instant::now())
}

/// Canonical (pubkey-sorted) genesis validator refs, for `validator_index`
/// assignment and self-verification (ADR-0032).
fn canonical_validators(
    store: &ChainStore,
) -> Result<Vec<onx::auth::GenesisValidatorRef>, TickError> {
    let doc = store
        .genesis_document()
        .map_err(|e| TickError::Fatal(format!("producer: cannot load genesis: {e}")))?
        .ok_or_else(|| TickError::Fatal("producer: no genesis in store".to_string()))?;
    let mut refs: Vec<onx::auth::GenesisValidatorRef> = doc
        .validators
        .iter()
        .map(|v| {
            // Validate the key with the strict predicate at load; the raw
            // bytes are what the verifier consumes.
            onx_primitives::PublicKey::decode_exact(&v.pubkey)
                .map_err(|e| TickError::Fatal(format!("producer: bad genesis key: {e}")))?;
            Ok(onx::auth::GenesisValidatorRef {
                pubkey: v.pubkey,
                stake: v.stake,
            })
        })
        .collect::<Result<_, TickError>>()?;
    refs.sort_by_key(|r| r.pubkey);
    Ok(refs)
}

/// Sign a block with the producer's key (ONXBLK05).
///
/// Returns the signature section entries. A signing key is REQUIRED:
/// an unsigned block would be rejected by every verifier (replay fails
/// closed on empty/insufficient stake), so producing one is never useful —
/// fail here with a clear error instead of emitting a dead block.
///
/// `validators` is the canonical (pubkey-sorted) genesis validator list;
/// `validator_index` is the signer's position in it.
fn sign_block(
    block: &Block,
    chain_id: &[u8; 32],
    signing_key: Option<&onx_primitives::SecretKey>,
    validators: &[onx::auth::GenesisValidatorRef],
) -> Result<Vec<SigEntry>, String> {
    let Some(secret) = signing_key else {
        return Err(
            "no signing key configured: block production requires --signing-key \
             (an unsigned block would be rejected by every verifier)"
                .to_string(),
        );
    };
    let pubkey = secret.public_key().encode();
    let index = validators
        .iter()
        .position(|v| v.pubkey == pubkey)
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
pub(crate) fn install_panic_backtrace_hook() {
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
    parent_block_time: u64,
) -> Result<Result<Block, StfError>, Box<dyn Any + Send>> {
    // Suppress Sentry panic reports for the containment boundary (see
    // SUPPRESS_SENTRY_PANIC). The guard restores the previous value (not
    // just `false`) so a future nested containment call can't clear the
    // flag early.
    struct SuppressGuard {
        prev: bool,
    }
    impl Drop for SuppressGuard {
        fn drop(&mut self) {
            SUPPRESS_SENTRY_PANIC.with(|f| f.set(self.prev));
        }
    }
    let prev = SUPPRESS_SENTRY_PANIC.with(|f| f.replace(true));
    let _guard = SuppressGuard { prev };
    catch_unwind(AssertUnwindSafe(|| {
        // TEST-ONLY: simulates an interpreter/STF panic DURING the
        // dry-run, i.e. inside the containment boundary.
        #[cfg(test)]
        maybe_inject_propose_panic(&messages);
        // ONXBLK05: protocol_version stamps the header with the constant this
        // node understands (the STF and the acceptance layer both reject
        // anything else); block_time is wall-clock at proposal (producer
        // policy: never stamp ahead of its own clock), clamped to the
        // parent's block_time so a stepped-back clock can never produce
        // a block the verifier rejects (monotonic, non-decreasing).
        let wall_clock = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let block_time = wall_clock.max(parent_block_time);
        propose_block(
            state,
            messages,
            lt,
            fee_collector,
            PROTOCOL_VERSION,
            block_time,
        )
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
    // A producer without a signing key cannot make a single valid block:
    // every verifier rejects unsigned blocks, so starting the loop would
    // just burn ticks until the first transaction arrives and then die.
    // Fail at startup with a clear error instead of a systemd restart loop.
    if cfg.signing_key.is_none() {
        return Err(
            "producer: no signing key configured (set signing_key_path in config \
             or pass --signing-key); refusing to start"
                .to_string(),
        );
    }
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
    //
    // Parent block_time for the monotonicity clamp: genesis (block 1's
    // parent) has block_time 0; otherwise read the committed parent header.
    let parent_block_time = if state.seqno == 0 {
        0
    } else {
        let parent_hash = store
            .block_hash_for_seqno(state.seqno)
            .map_err(|e| TickError::Fatal(format!("producer: parent hash lookup failed: {e}")))?
            .ok_or_else(|| {
                TickError::Fatal(format!(
                    "producer: parent block {} not committed",
                    state.seqno
                ))
            })?;
        store
            .get_block_header(&parent_hash)
            .map_err(|e| TickError::Fatal(format!("producer: parent header lookup failed: {e}")))?
            .ok_or_else(|| {
                TickError::Fatal(format!(
                    "producer: parent block {} header missing",
                    state.seqno
                ))
            })?
            .block_time
    };
    // Canonical validator set, loaded once: needed for signing (the
    // signer's index in `sign_block`) and self-verification below. Its
    // size no longer feeds the block-size check: the producer writes
    // exactly one signature, so the sig-section budget is
    // `producer_sig_section_bytes()`, not per-validator (ADR-0045).
    let validators = canonical_validators(store)?;
    let max_sig_section_bytes = producer_sig_section_bytes();
    let block = match propose_robust(
        &state,
        candidates,
        lt,
        cfg.fee_collector,
        parent_block_time,
        max_sig_section_bytes,
        mempool,
        stats,
    )? {
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
    let sig_entries = sign_block(
        &block,
        &state.chain_id,
        cfg.signing_key.as_ref(),
        &validators,
    )
    .map_err(|e| TickError::Fatal(format!("producer: signing failed: {e}")))?;
    // Self-verification (audit blocker 2): run the same acceptance check
    // every verifier runs, BEFORE committing. A block we produced must
    // never be one our own verifier rejects — fail the tick loudly
    // instead of committing a block replay would refuse.
    onx::auth::verify_block_auth(
        &state.chain_id,
        &block.header,
        parent_block_time,
        &sig_entries,
        &validators,
    )
    .map_err(|e| TickError::Fatal(format!("producer: self-verification failed: {e}")))?;
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
/// Byte budget reserved for the block file's signature section (ADR-0045).
///
/// The producer attaches exactly one signature — its own (`sign_block`
/// returns a single `SigEntry`; the validator list only locates the
/// signer's index) — so the section is always 4 (entry count) + one
/// entry, independent of the genesis validator set's size. An earlier
/// revision reserved space for *every* genesis validator (4 + n·68);
/// with ~124,000 validators that reservation alone exceeded
/// `MAX_BLOCK_FILE_BYTES`, so `largest_fitting_prefix` returned zero
/// and the producer halted on a `Fatal` tick error — even though the
/// block it would have written carried exactly one 68-byte signature
/// (devin 🟡 review on PR #47). Revisit when multi-validator quorum
/// signing lands (M6): the budget must then cover however many
/// signatures the producer attaches.
fn producer_sig_section_bytes() -> usize {
    4usize.saturating_add(SIG_ENTRY_BYTE_LEN)
}

/// Worst-case encoded `.blk` file size for these messages: magic + header +
/// the full signature section (`max_sig_section_bytes`: the producer's own
/// single signature — see [`producer_sig_section_bytes`]) + the length-prefixed
/// body. Deterministic — the same messages always give the same size, so every
/// honest producer agrees on the bound, and it must match
/// `onx::blockfile::encode_block_file`'s layout exactly.
fn worst_case_block_file_bytes(
    messages: &[ExternalMessage],
    max_sig_section_bytes: usize,
) -> usize {
    BLOCK_FILE_MAGIC
        .len()
        .saturating_add(BLOCK_HEADER_BYTE_LEN)
        .saturating_add(max_sig_section_bytes)
        .saturating_add(BODY_COUNT_LEN)
        .saturating_add(
            messages
                .iter()
                .map(|m| MSG_LEN_PREFIX.saturating_add(m.to_bytes().len()))
                .fold(0usize, |a, b| a.saturating_add(b)),
        )
}

/// Largest prefix of `candidates` whose worst-case encoded file fits
/// [`MAX_BLOCK_FILE_BYTES`]. Single linear pass — the size function is
/// monotone in the prefix length, so the first overflow point is the
/// answer. Callers truncate the tail; the held messages keep their
/// mempool order for the next block.
fn largest_fitting_prefix(candidates: &[ExternalMessage], max_sig_section_bytes: usize) -> usize {
    let mut acc = BLOCK_FILE_MAGIC
        .len()
        .saturating_add(BLOCK_HEADER_BYTE_LEN)
        .saturating_add(max_sig_section_bytes)
        .saturating_add(BODY_COUNT_LEN);
    let mut k = 0usize;
    for msg in candidates {
        acc = acc.saturating_add(MSG_LEN_PREFIX.saturating_add(msg.to_bytes().len()));
        if acc > MAX_BLOCK_FILE_BYTES {
            break;
        }
        k = k.saturating_add(1);
    }
    k
}
/// Gas-cap path (ADR-0034): if the first failing prefix fails with
/// `BlockGasExceeded`, its last message is valid but doesn't fit. The
/// candidates are cut just before it (a prefix the bisection already saw
/// propose cleanly) and the rest are held in `pending/` for the next block,
/// nothing rejected. Only a message that exceeds the cap on its own (the
/// cut would leave nothing) is rejected, since it can never fit in any
/// block and would otherwise stall every candidate behind it.
///
/// A second full failure after isolation is a genuine STF bug: loud log,
/// skip the tick, mempool intact. There is deliberately no bulk quarantine.
// 8 params: mirrors the existing #[allow] on try_execute_contract/deliver;
// bundling would obscure the call sites.
#[allow(clippy::too_many_arguments)]
fn propose_robust(
    state: &State,
    candidates: Vec<ExternalMessage>,
    lt: u64,
    fee_collector: AccountId,
    parent_block_time: u64,
    max_sig_section_bytes: usize,
    mempool: &mut Mempool,
    stats: &mut ProducerStats,
) -> Result<Option<Block>, TickError> {
    let mut candidates = candidates;
    // Block-size bound (ADR-0045): never propose a block the sync layer
    // cannot serve. A committed block whose encoded file exceeds
    // MAX_BLOCK_FILE_BYTES strands every follower at that height — the
    // server refuses it and no peer can deliver it. Trim the tail (held
    // for the next block, never rejected) until the worst-case encoded
    // file — the producer's own single signature — fits. This is a pure
    // size check, no STF involved; it mirrors the BlockGasExceeded arm in
    // the loop below.
    let fitting = largest_fitting_prefix(&candidates, max_sig_section_bytes);
    if fitting < candidates.len() {
        eprintln!(
            "producer: block file size bound reached ({} > {}); holding {} message(s) for a later block",
            worst_case_block_file_bytes(&candidates, max_sig_section_bytes),
            MAX_BLOCK_FILE_BYTES,
            candidates.len() - fitting,
        );
        candidates.truncate(fitting);
    }
    if candidates.is_empty() {
        // Unreachable in practice: a single max-size message encodes to
        // ~66 KiB against an 8 MiB budget. If it ever happens the size
        // model is wrong — fail loudly, never spin on an empty set.
        return Err(TickError::Fatal(
            "producer: block-size budget exceeded by a single message (size-model bug)".to_string(),
        ));
    }
    loop {
        if candidates.is_empty() {
            return Ok(None);
        }
        match propose_block_caught(
            state,
            candidates.clone(),
            lt,
            fee_collector,
            parent_block_time,
        ) {
            Ok(Ok(block)) => return Ok(Some(block)),
            Ok(Err(e)) => {
                eprintln!(
                    "producer: propose_block rejected filtered candidates ({e}); isolating offending message(s)"
                );
                match find_first_bad_prefix(state, &candidates, lt, fee_collector, e) {
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
                    Ok(Some((k, StfError::BlockGasExceeded { used, cap }))) if k > 0 => {
                        // Valid messages, just too many for one block: end the
                        // block before the one that tipped it over and hold the
                        // rest for the next block. Nothing is rejected.
                        eprintln!(
                            "producer: block gas cap reached ({used} > {cap}) at message {}; \
                             holding {} message(s) for a later block",
                            hex::encode(candidates[k].hash()),
                            candidates.len() - k
                        );
                        candidates.truncate(k);
                    }
                    Ok(Some((k, err))) => {
                        let culprit = candidates.remove(k);
                        eprintln!(
                            "producer: dropping message {} from sender {}: rejected by block proposal",
                            hex::encode(culprit.hash()),
                            hex::encode(culprit.from.to_bytes())
                        );
                        let reason = if matches!(err, StfError::BlockGasExceeded { .. }) {
                            "message alone exceeds the block gas cap"
                        } else {
                            "propose_block rejected this message"
                        };
                        mempool.reject_candidate(&culprit.hash(), reason);
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
    let k = match find_first_panicking_prefix(state, candidates, lt, fee_collector) {
        Ok(Some(k)) => k,
        Ok(None) => {
            // Defensive: the caller saw the full set panic, so this should
            // not happen. Retry without a Sentry report — there is no
            // culprit and no payload to describe.
            return Err(TickError::Retryable(
                "dry-run panicked but no panicking message prefix found (panic is not message-caused); mempool left intact"
                    .to_string(),
            ));
        }
        Err(payload) => {
            // The dry-run panicked with NO messages: the panic is NOT
            // message-caused, so this is a genuine STF/state bug, not a
            // hostile message. It happens under the suppress flag and
            // retries every tick — without this report it would be
            // completely invisible in Sentry. Include the caught panic
            // message (the full backtrace is in the daemon's stderr log
            // from the producer's panic hook). Rate-limited on the Error
            // slot, independent of the hostile-message Warning slot.
            if let Some(dropped) = take_summary_slot(SummaryKind::Error) {
                let dropped_note = if dropped > 0 {
                    format!(" ({dropped} similar reports dropped by rate limit)")
                } else {
                    String::new()
                };
                sentry::capture_message(
                    &format!(
                        "producer: dry-run panicked with no panicking message prefix \
                         (not message-caused; mempool left intact); panic: {}{dropped_note}",
                        panic_summary(&payload),
                    ),
                    sentry::Level::Error,
                );
            }
            return Err(TickError::Retryable(
                "dry-run panicked but no panicking message prefix found (panic is not message-caused); mempool left intact"
                    .to_string(),
            ));
        }
    };
    let culprit = candidates.remove(k);
    eprintln!(
        "producer: PANIC CONTAINED — dropping message {} from sender {} nonce {}: \
         dry-run execution panicked. Local event only: message moved to rejected/, \
         block production continues. This is NOT an invalid block and NOT a bounce.",
        hex::encode(culprit.hash()),
        hex::encode(culprit.from.to_bytes()),
        culprit.nonce,
    );
    // One Sentry event for the isolated message, rate-limited on the Warning
    // slot (see take_summary_slot). The individual probe panics were
    // suppressed (see SUPPRESS_SENTRY_PANIC); without this, a hostile
    // message would be invisible in Sentry. No-op when Sentry is not
    // initialized.
    if let Some(dropped) = take_summary_slot(SummaryKind::Warning) {
        let dropped_note = if dropped > 0 {
            format!(" ({dropped} similar reports dropped by rate limit)")
        } else {
            String::new()
        };
        sentry::capture_message(
            &format!(
                "producer: contained dry-run panic; dropped message {} from {} nonce {} \
                 (local fault, not a bounce){dropped_note}",
                hex::encode(culprit.hash()),
                hex::encode(culprit.from.to_bytes()),
                culprit.nonce,
            ),
            sentry::Level::Warning,
        );
    }
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
/// `propose_block(candidates[..=k])` fails, returned with the error that
/// prefix fails with (`full_err` is the full set's, which the caller already
/// saw). Returns `Ok(None)` only if the full set proposes cleanly (the
/// caller already saw it fail, so this is defensive).
///
/// The predicate is monotonic: adding messages cannot repair an earlier
/// wallet-handler rejection (balance only decreases through a block, and
/// every other phase-1 check is per-message), and block gas only grows with
/// more deliveries. Prefixes preserve per-sender nonce contiguity, so every
/// probe is meaningful.
///
/// Probes run inside [`propose_block_caught`]: if a probe PANICS, `Err(())`
/// is returned and the caller switches to panic isolation — a panicking
/// probe is not a rejection and must never be misread as one.
fn find_first_bad_prefix(
    state: &State,
    candidates: &[ExternalMessage],
    lt: u64,
    fee_collector: AccountId,
    full_err: StfError,
) -> Result<Option<(usize, StfError)>, ()> {
    // Invariant: propose(candidates[..lo]) succeeds, propose(candidates[..hi])
    // fails with hi_err.
    // lo = 0 holds because the empty prefix has no messages to reject;
    // hi = len holds because the caller saw the full set fail with full_err.
    let mut lo = 0usize;
    let mut hi = candidates.len();
    let mut hi_err = full_err;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        match propose_block_caught(state, candidates[..mid].to_vec(), lt, fee_collector, 0) {
            Ok(Ok(_)) => lo = mid,
            Ok(Err(e)) => {
                hi = mid;
                hi_err = e;
            }
            Err(_) => return Err(()),
        }
    }
    // propose(..lo) ok, propose(..lo+1) fails → culprit is index lo.
    if lo < candidates.len() {
        Ok(Some((lo, hi_err)))
    } else {
        Ok(None)
    }
}

/// Binary search for the smallest `k` such that the dry-run proposal of
/// `candidates[..=k]` PANICS. Returns `Ok(Some(k))` for the culprit index,
/// `Ok(None)` when no prefix panics. Returns `Err(payload)` when the EMPTY
/// prefix already panics, which means the panic is not message-caused
/// (bad state/config) — the payload propagates so the caller reports the
/// genuine bug instead of blaming a message for it.
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
) -> Result<Option<usize>, Box<dyn Any + Send>> {
    // Empty-prefix probe: panicking here means the panic is not
    // message-caused. Propagate the payload so the caller can report the
    // genuine bug; never blame a message for it.
    let _ = propose_block_caught(state, Vec::new(), lt, fee_collector, 0)?;
    // Invariant: propose(..lo) does not panic, propose(..hi) panics.
    // hi = len holds because the caller observed the full set panic.
    let mut lo = 0usize;
    let mut hi = candidates.len();
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if propose_block_caught(state, candidates[..mid].to_vec(), lt, fee_collector, 0).is_err() {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    // propose(..lo) clean, propose(..lo+1) panics → culprit is index lo.
    Ok(if lo < candidates.len() {
        Some(lo)
    } else {
        None
    })
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

    /// Build a signed test transfer. `seq` is a per-account sequence offset:
    /// the message nonce is the account's next expected on-chain nonce
    /// (read from `state`) plus `seq`, so the first message from an account
    /// takes `seq = 0`. The nonce is state-derived, never a literal — the
    /// hard-coded-nonce check must not fire on test fixtures.
    fn signed_transfer(state: &State, from_idx: u8, seq: u64) -> ExternalMessage {
        let from = test_account(from_idx);
        let secret = test_secret_key(&from);
        let base = state.tree.get(&from).map(|a| a.nonce()).unwrap_or(0);
        let nonce = base.saturating_add(seq);
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
            0,
            // The producer's sig-section budget (one entry: its own
            // signature); two small transfers are far under the file cap.
            producer_sig_section_bytes(),
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

    /// ADR-0045: the producer's size model must mirror
    /// `encode_block_file`'s layout byte-for-byte — the trim decision is
    /// only sound if the model and the encoder agree.
    #[test]
    fn size_model_matches_encode_block_file_exactly() {
        let state = State::from_genesis(&test_genesis());
        let fee_collector = test_fee_collector();
        // (from_idx, seq) pairs — seq is the per-account offset, so account 0
        // takes nonces base+0 and base+1, account 1 takes base+0. See the
        // signed_transfer doc comment: nonces are state-derived, not literals.
        let msgs: Vec<_> = [(0u8, 0u64), (1, 0), (0, 1)]
            .into_iter()
            .map(|(from_idx, seq)| signed_transfer(&state, from_idx, seq))
            .collect();
        let block = propose_block(
            &state,
            msgs,
            state.last_lt + 1,
            fee_collector,
            PROTOCOL_VERSION,
            0,
        )
        .expect("propose_block succeeds on valid transfers");
        assert_eq!(block.body.messages.len(), 3);
        // One validator in the test genesis: worst-case == actual section.
        let sig_entries = vec![SigEntry {
            validator_index: 0,
            sig: [0x77; 64],
        }];
        let encoded = encode_block_file(&block, &sig_entries);
        let sig_section_bytes = 4 + sig_entries.len() * SIG_ENTRY_BYTE_LEN;
        assert_eq!(
            encoded.len(),
            worst_case_block_file_bytes(&block.body.messages, sig_section_bytes),
            "size model diverged from encode_block_file",
        );
    }

    /// Devin 🟡 review on PR #47: the old sig-section budget reserved
    /// 4 + n·68 bytes for *every* genesis validator. With ~124,000
    /// validators that reservation alone exceeded `MAX_BLOCK_FILE_BYTES`,
    /// so `largest_fitting_prefix` returned zero and the producer halted
    /// with a `Fatal` tick error — even though the block it would have
    /// written carried exactly one 68-byte signature. The budget is the
    /// producer's own single signature and must not grow with the
    /// validator set.
    #[test]
    fn sig_section_budget_ignores_validator_set_size() {
        assert_eq!(producer_sig_section_bytes(), 4 + SIG_ENTRY_BYTE_LEN);
        // At 130,000 validators the old formula (4 + n·68 ≈ 8.43 MiB)
        // exceeded the whole sync budget; a small message must still fit
        // under the production budget.
        let state = State::from_genesis(&test_genesis());
        // seq 0: the account's next expected on-chain nonce, state-derived.
        let msg = signed_transfer(&state, 0, 0);
        assert!(largest_fitting_prefix(&[msg], producer_sig_section_bytes()) > 0);
    }

    /// ADR-0045: a candidate set bigger than the sync servable bound is
    /// trimmed (tail held for the next block), never committed as a block
    /// no follower can fetch — and never rejected as invalid.
    ///
    /// The messages are ContractCalls with near-max payloads to an account
    /// without code: they bounce on delivery (a normal outcome — the
    /// message is still included in the block) and burn ~0 gas, so the
    /// size trim fires instead of the gas cap, with only ~160 signatures
    /// to compute instead of ~27,000 small transfers.
    #[test]
    fn oversize_candidate_set_is_trimmed_not_rejected() {
        let state = State::from_genesis(&test_genesis());
        let fee_collector = test_fee_collector();
        // The producer's sig-section budget (one entry: its own signature).
        let max_sig_section_bytes = producer_sig_section_bytes();
        let payload = vec![0x5au8; 60_000];
        let mut candidates = Vec::new();
        // Per-account sequence offsets. The message nonce is each account's
        // next expected on-chain nonce (read from state) plus its offset —
        // state-derived, never a literal (see signed_transfer).
        let mut seqs = [0u64; 8];
        for i in 0..160 {
            let from_idx = (i % 8) as u8;
            let from = test_account(from_idx);
            let secret = test_secret_key(&from);
            let base = state.tree.get(&from).map(|a| a.nonce()).unwrap_or(0);
            let nonce = base.saturating_add(seqs[from_idx as usize]);
            candidates.push(ExternalMessage::new_signed(
                state.chain_id,
                MsgKind::ContractCall,
                from,
                nonce,
                test_account(0x99),
                1_000,
                10,
                payload.clone(),
                [0u8; 32],
                &secret,
            ));
            seqs[from_idx as usize] += 1;
        }
        assert!(
            worst_case_block_file_bytes(&candidates, max_sig_section_bytes) > MAX_BLOCK_FILE_BYTES,
            "test setup must actually exceed the cap"
        );
        let expected_kept = largest_fitting_prefix(&candidates, max_sig_section_bytes);
        assert!(expected_kept < candidates.len());
        assert!(expected_kept > 0);

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("onxd-sizetest-{nanos}-{}", std::process::id()));
        let mut mempool = Mempool::new(&dir, 1000, state.chain_id, fee_collector).expect("mempool");
        let mut stats = ProducerStats::default();
        let block = propose_robust(
            &state,
            candidates,
            state.last_lt + 1,
            fee_collector,
            0,
            max_sig_section_bytes,
            &mut mempool,
            &mut stats,
        )
        .expect("propose_robust succeeds on an oversize set")
        .expect("a block is still produced");
        assert_eq!(
            block.body.messages.len(),
            expected_kept,
            "the size trim keeps exactly the fitting prefix"
        );
        assert!(
            worst_case_block_file_bytes(&block.body.messages, max_sig_section_bytes)
                <= MAX_BLOCK_FILE_BYTES,
            "committed block is servable"
        );
        // Nothing was rejected: the held tail stays in the mempool for
        // the next block.
        assert_eq!(stats.txs_rejected, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ADR-0045 review follow-up (CodeRabbit Major / Greptile P2 on PR
    /// #47): the size trim is covered end to end, not just at
    /// `propose_robust`. An oversize candidate set goes through `run_tick`:
    /// the trimmed block commits, its `.blk` file decodes, verifies, and
    /// re-applies to the committed state root (replay), and the held tail
    /// commits on the next tick. Nothing is rejected — the tail is held,
    /// not dropped.
    ///
    /// The calls target an account with no code: they bounce on delivery
    /// (a normal outcome — the message is still included in the block) and
    /// burn ~0 gas, so the size trim fires instead of the gas cap.
    #[test]
    fn oversize_set_replays_and_held_tail_commits_next_tick() {
        use onx_data_structures::{ShardIdent, WorkchainIdent};
        use onx_primitives::SecretKey;
        use onx_state_model::{AccountState, GenesisDocument, GenesisValidator, StorageStat};
        use onx_stf::apply_block;
        use std::collections::BTreeMap;

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "onxd-trimreplay-test-{}-{nanos}",
            std::process::id()
        ));
        let tx_pool_dir = root.join("pool");
        std::fs::create_dir_all(&tx_pool_dir).unwrap();

        let fee_collector = test_fee_collector();
        let contract = AccountId::from_bytes([0xC0; 32]);
        let senders: Vec<AccountId> = (0..8).map(test_account).collect();
        let validator_key = SecretKey::from_seed(&[0x11; 32]).unwrap();

        let stat = StorageStat {
            cell_count: 0,
            byte_count: 0,
            bit_count: 0,
        };
        let mut accounts = BTreeMap::new();
        for id in &senders {
            accounts.insert(
                *id,
                AccountState::Active {
                    balance_nanos: 1_000_000_000,
                    last_trans_lt: 0,
                    code: None,
                    data: None,
                    storage_stat: stat,
                    pubkey: test_secret_key(id).public_key().encode(),
                    nonce: 0,
                },
            );
        }
        accounts.insert(
            contract,
            AccountState::Active {
                balance_nanos: 0,
                last_trans_lt: 0,
                code: None,
                data: None,
                storage_stat: stat,
                pubkey: [0u8; 32],
                nonce: 0,
            },
        );
        let doc = GenesisDocument::new(
            WorkchainIdent::BASIC,
            ShardIdent::root(WorkchainIdent::BASIC),
            vec![GenesisValidator {
                pubkey: validator_key.public_key().encode(),
                stake: 1_000,
            }],
            accounts,
        )
        .unwrap();
        let store = ChainStore::open(root.join("db")).unwrap();
        store.init_genesis(&doc).unwrap();
        let genesis_state = store.load_state().unwrap().unwrap();

        // 160 near-max-payload calls across 8 senders: the worst-case file
        // is ~9.6 MiB against the 8 MiB servable cap, so the trim must fire.
        let payload = vec![0x5au8; 60_000];
        // Per-account sequence offsets. The message nonce is each account's
        // next expected on-chain nonce (read from state) plus its offset —
        // state-derived, never a literal (CodeQL hard-coded-value rule).
        let mut seqs = [0u64; 8];
        let mut sent = Vec::new();
        for i in 0..160 {
            let from_idx = i % 8;
            let from = senders[from_idx];
            let secret = test_secret_key(&from);
            let base = genesis_state
                .tree
                .get(&from)
                .map(|a| a.nonce())
                .unwrap_or(0);
            let msg = ExternalMessage::new_signed(
                genesis_state.chain_id,
                MsgKind::ContractCall,
                from,
                base.saturating_add(seqs[from_idx]),
                contract,
                1_000,
                10,
                payload.clone(),
                [0u8; 32],
                &secret,
            );
            let name = format!("{}.msg", hex::encode(msg.hash()));
            std::fs::write(tx_pool_dir.join(name), msg.to_bytes()).unwrap();
            sent.push(msg.hash());
            seqs[from_idx] += 1;
        }

        let cfg = ProducerConfig {
            fee_collector,
            poll_interval: Duration::from_millis(0),
            tx_pool_dir: tx_pool_dir.clone(),
            blocks_dir: root.join("blocks"),
            telemetry: None,
            signing_key: Some(validator_key),
        };
        std::fs::create_dir_all(&cfg.blocks_dir).unwrap();
        let mut mempool =
            Mempool::new(&tx_pool_dir, 1000, genesis_state.chain_id, fee_collector).unwrap();
        let mut stats = ProducerStats::default();

        // Tick 1: the trim fires — a block commits with the fitting prefix
        // and the tail stays held in the mempool.
        assert!(run_tick(&store, &mut mempool, &cfg, &mut stats).unwrap());
        let hash1 = store.block_hash_for_seqno(1).unwrap().unwrap();
        let header1 = store.get_block_header(&hash1).unwrap().unwrap();
        let body1 = store.get_block_body(&hash1).unwrap().unwrap();
        assert!(
            !body1.messages.is_empty() && body1.messages.len() < 160,
            "tick 1 must commit a trimmed, non-empty block"
        );
        assert!(
            !mempool.is_empty(),
            "the held tail stays in the mempool for tick 2"
        );

        // Replay: the emitted .blk file decodes, its signature section
        // verifies under the same acceptance check every verifier runs,
        // and re-applying it from genesis reproduces the committed root.
        let file_bytes = std::fs::read(cfg.blocks_dir.join(block_file_name(1))).unwrap();
        let signed = decode_block_file(&file_bytes).expect("block file decodes");
        assert_eq!(signed.block.header.hash(), hash1);
        assert_eq!(signed.block.body.messages.len(), body1.messages.len());
        let validators = canonical_validators(&store).expect("validators");
        onx::auth::verify_block_auth(
            &genesis_state.chain_id,
            &signed.block.header,
            0, // genesis is block 1's parent: block_time 0
            &signed.sig_entries,
            &validators,
        )
        .expect("replayed block's signatures verify");
        let (replayed, _) = apply_block(&genesis_state, &signed.block).expect("replay applies");
        assert_eq!(
            replayed.state_root().unwrap(),
            header1.state_root,
            "replay of the trimmed block reproduces the committed root"
        );

        // Tick 2: the held tail commits. Everything lands, nothing rejected.
        assert!(run_tick(&store, &mut mempool, &cfg, &mut stats).unwrap());
        assert_eq!(stats.blocks_produced, 2);
        assert_eq!(stats.txs_rejected, 0);
        assert!(mempool.is_empty());
        let mut committed = Vec::new();
        for seqno in 1..=2 {
            let h = store.block_hash_for_seqno(seqno).unwrap().unwrap();
            let b = store.get_block_body(&h).unwrap().unwrap();
            committed.extend(b.messages.iter().map(|m| m.hash()));
        }
        committed.sort();
        sent.sort();
        assert_eq!(
            committed, sent,
            "all 160 messages committed across the two ticks"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Contract code that burns ~9.8M gas and then halts successfully: a
    /// loop of eight `DUP HASHCELL DROP`s on the data cell, run 6000 times.
    ///
    /// ```text
    ///       PUSHINT 6000            ; data, n
    /// loop: SWAP                    ; n, data
    ///       (DUP HASHCELL DROP) x8  ; n, data
    ///       SWAP                    ; data, n
    ///       PUSHINT 1
    ///       SUB 128                 ; data, n-1
    ///       DUP ISZERO              ; data, n-1, n-1==0
    ///       UNTIL loop              ; re-enter while n-1 != 0
    ///       DROP                    ; data
    /// ```
    fn gas_burner_code() -> onx_state_model::Cell {
        fn pushint(v: u16) -> Vec<u8> {
            let mut out = vec![0x08, 0x00];
            let mut word = [0u8; 32];
            word[30..].copy_from_slice(&v.to_be_bytes());
            out.extend_from_slice(&word);
            out
        }
        let mut body = vec![0x03]; // SWAP
        for _ in 0..8 {
            body.extend_from_slice(&[0x02, 0x61, 0x01]); // DUP HASHCELL DROP
        }
        body.push(0x03); // SWAP
        body.extend(pushint(1));
        body.extend_from_slice(&[0x11, 0x00, 0x80, 0x00]); // SUB width=128
        body.extend_from_slice(&[0x02, 0x16]); // DUP ISZERO
        let back = -(body.len() as i16 + 2);
        body.extend_from_slice(&[0x7B, back as i8 as u8]); // UNTIL loop
        let mut code = pushint(6000);
        code.extend(body);
        code.push(0x01); // DROP
        onx_state_model::Cell::new(code, vec![]).unwrap()
    }

    /// The block gas cap (ADR-0034) must split an over-cap batch across
    /// blocks, never drop a valid message. Twelve ~9.8M-gas contract calls
    /// (six senders, two nonces each) need ~118M gas against a 100M cap:
    /// the first block takes ten, the next takes the other two, and
    /// nothing goes to `rejected/`. Before the fix the producer bisected to
    /// the eleventh call, rejected it, and stranded its sender's next nonce.
    #[test]
    fn block_gas_cap_splits_batch_across_blocks_without_rejecting() {
        use onx_data_structures::{ShardIdent, WorkchainIdent};
        use onx_primitives::SecretKey;
        use onx_state_model::{AccountState, Cell, GenesisDocument, GenesisValidator, StorageStat};
        use std::collections::BTreeMap;

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("onxd-gascap-test-{}-{nanos}", std::process::id()));
        let tx_pool_dir = root.join("pool");
        std::fs::create_dir_all(&tx_pool_dir).unwrap();

        let fee_collector = test_fee_collector();
        let contract = AccountId::from_bytes([0xC0; 32]);
        let senders: Vec<AccountId> = (0..6).map(test_account).collect();
        let validator_key = SecretKey::from_seed(&[0x11; 32]).unwrap();

        let stat = StorageStat {
            cell_count: 0,
            byte_count: 0,
            bit_count: 0,
        };
        let mut accounts = BTreeMap::new();
        for id in &senders {
            accounts.insert(
                *id,
                AccountState::Active {
                    balance_nanos: 1_000_000_000,
                    last_trans_lt: 0,
                    code: None,
                    data: None,
                    storage_stat: stat,
                    pubkey: test_secret_key(id).public_key().encode(),
                    nonce: 0,
                },
            );
        }
        accounts.insert(
            contract,
            AccountState::Active {
                balance_nanos: 0,
                last_trans_lt: 0,
                code: Some(gas_burner_code()),
                data: Some(Cell::new(vec![], vec![]).unwrap()),
                storage_stat: stat,
                pubkey: [0u8; 32],
                nonce: 0,
            },
        );
        let doc = GenesisDocument::new(
            WorkchainIdent::BASIC,
            ShardIdent::root(WorkchainIdent::BASIC),
            vec![GenesisValidator {
                pubkey: validator_key.public_key().encode(),
                stake: 1_000,
            }],
            accounts,
        )
        .unwrap();
        let store = ChainStore::open(root.join("db")).unwrap();
        store.init_genesis(&doc).unwrap();
        let state = store.load_state().unwrap().unwrap();

        // Each call buys the full per-message gas (10_000 nanos * 1_000).
        let mut sent = Vec::new();
        for id in &senders {
            for nonce in 0..2 {
                let msg = ExternalMessage::new_signed(
                    state.chain_id,
                    MsgKind::ContractCall,
                    *id,
                    nonce,
                    contract,
                    1,
                    10_000,
                    vec![0x01],
                    [0u8; 32],
                    &test_secret_key(id),
                );
                let name = format!("{}.msg", hex::encode(msg.hash()));
                std::fs::write(tx_pool_dir.join(name), msg.to_bytes()).unwrap();
                sent.push(msg.hash());
            }
        }

        let cfg = ProducerConfig {
            fee_collector,
            poll_interval: Duration::from_millis(0),
            tx_pool_dir: tx_pool_dir.clone(),
            blocks_dir: root.join("blocks"),
            telemetry: None,
            signing_key: Some(validator_key),
        };
        std::fs::create_dir_all(&cfg.blocks_dir).unwrap();
        let mut mempool = Mempool::new(&tx_pool_dir, 1000, state.chain_id, fee_collector).unwrap();
        let mut stats = ProducerStats::default();

        assert!(run_tick(&store, &mut mempool, &cfg, &mut stats).unwrap());
        assert_eq!(stats.msgs_committed, 10, "ten calls fit under the cap");
        assert_eq!(mempool.len(), 2, "the other two are held, not dropped");

        assert!(run_tick(&store, &mut mempool, &cfg, &mut stats).unwrap());
        assert_eq!(stats.blocks_produced, 2);
        assert_eq!(stats.msgs_committed, 12, "every call is committed");
        assert_eq!(stats.txs_rejected, 0);
        assert!(mempool.is_empty());
        assert_eq!(
            tx_pool_dir.join("rejected").read_dir().unwrap().count(),
            0,
            "nothing went to rejected/"
        );

        // Both blocks together carry exactly the twelve calls, and every
        // sender's nonce advanced to 2.
        let mut committed = Vec::new();
        for seqno in 1..=2 {
            let hash = store.block_hash_for_seqno(seqno).unwrap().unwrap();
            let body = store.get_block_body(&hash).unwrap().unwrap();
            committed.extend(body.messages.iter().map(|m| m.hash()));
        }
        committed.sort();
        sent.sort();
        assert_eq!(committed, sent);
        let head = store.load_state().unwrap().unwrap();
        for id in &senders {
            match head.tree.get(id) {
                Some(AccountState::Active { nonce, .. }) => assert_eq!(*nonce, 2),
                other => panic!("sender {id:?} not active: {other:?}"),
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod rate_limit_tests {
    use super::{take_slot, RateLimitState};
    use std::time::{Duration, Instant};

    fn fresh() -> RateLimitState {
        (None, 0)
    }

    #[test]
    fn first_event_in_window_sends_with_zero_dropped() {
        let mut s = fresh();
        let now = Instant::now();
        assert_eq!(take_slot(&mut s, now), Some(0));
    }

    #[test]
    fn events_within_window_are_dropped() {
        let mut s = fresh();
        let t0 = Instant::now();
        assert_eq!(take_slot(&mut s, t0), Some(0));
        // Second and third events inside the 60s window must drop.
        assert_eq!(take_slot(&mut s, t0 + Duration::from_secs(1)), None);
        assert_eq!(take_slot(&mut s, t0 + Duration::from_secs(59)), None);
    }

    #[test]
    fn dropped_count_reported_on_next_send() {
        let mut s = fresh();
        let t0 = Instant::now();
        assert_eq!(take_slot(&mut s, t0), Some(0));
        assert_eq!(take_slot(&mut s, t0 + Duration::from_secs(1)), None);
        assert_eq!(take_slot(&mut s, t0 + Duration::from_secs(2)), None);
        assert_eq!(take_slot(&mut s, t0 + Duration::from_secs(3)), None);
        // Window expired: next send reports the 3 dropped events.
        assert_eq!(take_slot(&mut s, t0 + Duration::from_secs(61)), Some(3));
    }

    #[test]
    fn window_resets_dropped_count_after_send() {
        let mut s = fresh();
        let t0 = Instant::now();
        assert_eq!(take_slot(&mut s, t0), Some(0));
        assert_eq!(take_slot(&mut s, t0 + Duration::from_secs(1)), None);
        assert_eq!(take_slot(&mut s, t0 + Duration::from_secs(61)), Some(1));
        // Fresh window: no drops accumulated.
        assert_eq!(take_slot(&mut s, t0 + Duration::from_secs(122)), Some(0));
    }

    #[test]
    fn warning_and_error_states_are_independent() {
        // Two separate states must not cross-contaminate: burning the
        // Warning slot leaves the Error slot able to send.
        let mut warning: RateLimitState = fresh();
        let mut error: RateLimitState = fresh();
        let t0 = Instant::now();
        assert_eq!(take_slot(&mut warning, t0), Some(0));
        assert_eq!(take_slot(&mut warning, t0 + Duration::from_secs(1)), None);
        // Error slot untouched: sends immediately with zero dropped.
        assert_eq!(take_slot(&mut error, t0 + Duration::from_secs(1)), Some(0));
    }
}
