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
//!   and the STF disagree (a bug or a state race). The tick is skipped with
//!   a loud log; after `consecutive_failure_limit` (default 3) consecutive
//!   failures the candidate set is quarantined to `rejected/` — fail-closed
//!   rather than spinning forever on a poisoned mempool.
//! - `commit_block` errors (e.g. `HeadMismatch`, `ForkDetected`) are fatal
//!   to the tick but not the loop: they indicate state moved under us,
//!   which on a single writer means a bug. Logged loudly; the next tick
//!   reloads state fresh.

use crate::mempool::Mempool;
use onx::blockfile::{block_file_name, encode_block_file};
use onx_data_structures::AccountId;
use onx_stf::{propose_block, State};
use onx_storage::ChainStore;
use onx_telemetry::TelemetryHandle;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct ProducerConfig {
    pub fee_collector: AccountId,
    pub poll_interval: Duration,
    pub tx_pool_dir: PathBuf,
    pub blocks_dir: PathBuf,
    /// Consecutive propose/commit failures before quarantining the
    /// candidate set instead of retrying forever.
    pub consecutive_failure_limit: u32,
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
    // Crash-safe mempool: rehydrate pending/ from the previous run, if any.
    let rehydrated = mempool.rehydrate()?;
    if rehydrated > 0 {
        eprintln!("producer: rehydrated {rehydrated} pending transactions from previous run");
    }

    let mut stats = ProducerStats::default();
    let mut consecutive_failures: u32 = 0;

    while !shutdown.load(Ordering::Relaxed) {
        match run_tick(&store, &mut mempool, &cfg, &mut stats) {
            Ok(produced) => {
                if produced {
                    consecutive_failures = 0;
                } else {
                    stats.ticks_idle += 1;
                }
            }
            Err(TickError::Fatal(e)) => return Err(e),
            Err(TickError::Retryable(e)) => {
                consecutive_failures += 1;
                eprintln!("producer: tick failed ({consecutive_failures} consecutive): {e}");
                if consecutive_failures >= cfg.consecutive_failure_limit {
                    let n = mempool.quarantine_all(&format!(
                        "{consecutive_failures} consecutive proposal failures"
                    ));
                    stats.txs_rejected += n as u64;
                    consecutive_failures = 0;
                }
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

    // 5. Honest producer path: dry-run through the same apply_txs as validation.
    let block = propose_block(&state, candidates.clone(), lt, cfg.fee_collector)
        .map_err(|e| TickError::Retryable(format!("propose_block rejected candidates: {e}")))?;

    // 6. Atomic commit: STF re-validation + state/body/index/head in one txn.
    store
        .commit_block(&state, &block)
        .map_err(|e| TickError::Fatal(format!("producer: commit_block failed: {e}")))?;

    // 7. Emit the canonical block file (feeds `onx replay` directly).
    let block_path = cfg.blocks_dir.join(block_file_name(block.header.seqno));
    fs::write(&block_path, encode_block_file(&block))
        .map_err(|e| TickError::Fatal(format!("producer: cannot write block file: {e}")))?;

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
