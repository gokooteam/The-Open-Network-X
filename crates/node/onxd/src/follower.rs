//! Follower sync loop (M5): fetch → verify → apply → persist.
//!
//! The follower keeps its own copy of the chain without producing blocks.
//! It polls its configured producer peer for the next block file, runs the
//! exact same acceptance checks as `onx replay` (strict decode,
//! [`verify_block_auth`](onx::auth::verify_block_auth), then the STF's
//! `apply_block` inside `commit_block`), and persists atomically.
//!
//! ## Trust boundary (check order is load-bearing)
//!
//! 1. Framing: [`SyncClient::fetch_block`](onx_networking::SyncClient::fetch_block)
//!    validates the response envelope (magic, version, seqno match, size cap).
//! 2. Strict decode: [`decode_block_file`](onx::blockfile::decode_block_file)
//!    rejects any malformed byte, never guesses.
//! 3. Seqno binding: the served block's header seqno must equal the
//!    requested seqno — a peer serving another block's file for this seqno
//!    is Byzantine, not helpful.
//! 4. Authentication: `verify_block_auth` checks the ONXBLK05 signatures
//!    against the genesis validators (strict >2/3 stake), the protocol
//!    version, and block_time monotonicity vs the committed parent.
//! 5. Execution: `commit_block` re-runs the pure STF (`apply_block`) —
//!    seqno, prev-hash, workchain, lt, msgs_root, and the claimed state
//!    root are all re-derived, never trusted — then persists atomically.
//!
//! A block that fails any step is rejected loudly and the follower retries
//! the same seqno: the chain is gapless, so a bad block is never skipped.
//!
//! ## Kill -9 resume
//!
//! `commit_block` is atomic and the block-file write is atomic (temp +
//! rename). A kill at any point leaves either (a) nothing committed — the
//! next loop refetches the same seqno, or (b) the block committed with its
//! file missing — repaired at startup by
//! [`regenerate_missing_block_files`](crate::blockfiles::regenerate_missing_block_files).
//! The loop always resumes from `load_state()`'s head, never from memory.

use crate::blockfiles::{
    atomic_write_block_file, regenerate_missing_block_files, sweep_temp_block_files,
};
use crate::producer::install_panic_backtrace_hook;
use onx::auth::{verify_block_auth, GenesisValidatorRef};
use onx::blockfile::decode_block_file;
use onx_networking::{KeyDescription, SyncClient, SyncConfig, SyncError, SyncPeer, SyncTransport};
use onx_primitives::PublicKey;
use onx_storage::ChainStore;
use onx_telemetry::TelemetryHandle;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

/// Follower loop tuning.
#[derive(Debug, Clone)]
pub struct FollowerConfig {
    /// Directory for fetched block files (`block-{seqno:08}.blk`), same
    /// layout as the producer's so `onx replay` works unchanged.
    pub blocks_dir: PathBuf,
    /// How long to wait before retrying when the peer has nothing new (or
    /// served a bad block). Liveness only, never in block content.
    pub poll_interval: Duration,
    /// Sync-protocol timeouts (fetch, ack).
    pub sync: SyncConfig,
    /// Optional telemetry handle; block height is reported when present.
    pub telemetry: Option<TelemetryHandle>,
}

/// Follower loop statistics.
#[derive(Debug, Default)]
pub struct FollowerStats {
    /// Blocks fetched, verified, and committed this run.
    pub blocks_synced: u64,
    /// Fetches that found nothing new (peer slow, unreachable, or has no
    /// new block yet).
    pub waits: u64,
    /// Fetched blocks rejected by the acceptance checks (a Byzantine peer
    /// wasting our time — retried, never skipped).
    pub rejected: u64,
}

/// What one follower step did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowStep {
    /// A new block was fetched, verified, and committed.
    Synced { seqno: u32 },
    /// The peer had nothing new (fetch timeout).
    Waiting,
}

/// A follower-step failure. Retryable means "log loudly, retry the same
/// seqno next tick"; Fatal means "halt the node".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowError {
    /// The peer served a bad block (or the fetch failed transiently).
    /// Never fatal: a Byzantine peer must not kill the follower.
    Retryable(String),
    /// Our own state is inconsistent (corrupt store, seqno overflow,
    /// our own disk broken). Halts, loudly.
    Fatal(String),
}

impl std::fmt::Display for FollowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable(e) => write!(f, "{e}"),
            Self::Fatal(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FollowError {}

/// Acceptance context: everything the follower needs that never changes
/// within a run. Built once at startup from the store's genesis.
pub struct FollowerContext {
    /// Genesis hash; block signatures bind to it.
    pub chain_id: [u8; 32],
    /// Genesis validators in canonical (pubkey-sorted) order — the same
    /// order every verifier uses (ADR-0032).
    pub validators: Vec<GenesisValidatorRef>,
}

impl FollowerContext {
    /// Load the chain id and canonical validator list from the store's
    /// genesis document.
    pub fn load(store: &ChainStore) -> Result<Self, String> {
        let chain_id = store
            .chain_id()
            .map_err(|e| format!("follower: cannot read chain id: {e}"))?
            .ok_or_else(|| "follower: no genesis in store (chain id unavailable)".to_string())?;
        let doc = store
            .genesis_document()
            .map_err(|e| format!("follower: cannot load genesis: {e}"))?
            .ok_or_else(|| "follower: no genesis in store".to_string())?;
        let mut validators: Vec<GenesisValidatorRef> = doc
            .validators
            .iter()
            .map(|v| {
                // Validate the key with the strict predicate at load; the
                // raw bytes are what the verifier consumes.
                PublicKey::decode_exact(&v.pubkey)
                    .map_err(|e| format!("follower: bad genesis key: {e}"))?;
                Ok(GenesisValidatorRef {
                    pubkey: v.pubkey,
                    stake: v.stake,
                })
            })
            .collect::<Result<_, String>>()?;
        validators.sort_by_key(|v| v.pubkey);
        Ok(Self {
            chain_id,
            validators,
        })
    }
}

/// Parse a static peer descriptor (ADR-0043, as amended for the ADNL
/// channel): `<ed25519-pubkey-hex>@<host:port>`.
///
/// The abstract address is DERIVED from the key
/// (`KeyDescription::compute_abstract_address`) and pinned — a peer
/// presenting a different key computes a different address and its
/// datagrams are ignored. Pinning the key is strictly stronger than
/// pinning the address alone, and the channel needs the key anyway for
/// the X25519 handshake.
pub fn parse_sync_peer(s: &str) -> Result<SyncPeer, String> {
    let (key_hex, endpoint) = s
        .split_once('@')
        .ok_or_else(|| format!("bad peer descriptor (want <pubkey-hex>@<host:port>): {s}"))?;
    let key_bytes = hex::decode(key_hex)
        .map_err(|_| format!("bad peer descriptor: pubkey is not hex: {key_hex}"))?;
    if key_bytes.len() != 32 {
        return Err(format!(
            "bad peer descriptor: pubkey must be 32 bytes, got {}",
            key_bytes.len()
        ));
    }
    let mut key_arr = [0u8; 32];
    key_arr.copy_from_slice(&key_bytes);
    // Strict predicate (canonical, on-curve, large-order): a malformed
    // peer key is a config error, caught here rather than mid-handshake.
    let public_key =
        PublicKey::decode_exact(&key_arr).map_err(|e| format!("bad peer descriptor: {e}"))?;
    let endpoint: SocketAddr = endpoint
        .parse()
        .map_err(|e| format!("bad peer descriptor: bad endpoint: {e}"))?;
    let address = KeyDescription::new_ed25519(public_key).compute_abstract_address();
    Ok(SyncPeer {
        public_key,
        endpoint,
        address,
    })
}

/// Parent block_time for the monotonicity check: genesis (block 1's parent)
/// has block_time 0; otherwise read the committed parent header — the same
/// rule the producer and `onx replay` use.
fn parent_block_time(store: &ChainStore, seqno: u32) -> Result<u64, FollowError> {
    if seqno <= 1 {
        return Ok(0);
    }
    let parent_seqno = seqno - 1;
    let parent_hash = store
        .block_hash_for_seqno(parent_seqno)
        .map_err(|e| FollowError::Fatal(format!("follower: parent hash lookup failed: {e}")))?
        .ok_or_else(|| {
            FollowError::Fatal(format!(
                "follower: parent block {parent_seqno} not committed"
            ))
        })?;
    store
        .get_block_header(&parent_hash)
        .map_err(|e| FollowError::Fatal(format!("follower: parent header lookup failed: {e}")))?
        .ok_or_else(|| {
            FollowError::Fatal(format!(
                "follower: parent block {parent_seqno} header missing"
            ))
        })
        .map(|h| h.block_time)
}

/// Fetch and apply the next block after the store's head: the single
/// follower step, factored out of the loop so tests can drive it
/// deterministically.
///
/// - `Ok(FollowStep::Waiting)`: the peer had nothing new (fetch timeout).
/// - `Ok(Synced)`: the head advanced.
/// - `Err(Retryable)`: the peer served something bad — logged loudly by
///   the caller, retried, never skipped.
/// - `Err(Fatal)`: our own state is inconsistent — halt.
///
/// There is deliberately no "already committed, skip" branch: the loop is
/// head-driven and `commit_block` advances the head atomically, so
/// `block_hash_for_seqno(head + 1)` can never hit. (File-driven `onx
/// replay` needs that branch; the follower doesn't.) The kill window
/// between commit and file write is closed by the loop's startup
/// regeneration, same as the producer.
///
/// PANIC POLICY (ADR-0029): `commit_block` (real block application) is
/// never wrapped — a panic there propagates and halts the node, same as
/// the producer path. A panic is never "invalid block".
pub async fn follow_next_block<T: SyncTransport>(
    store: &ChainStore,
    client: &SyncClient<T>,
    cfg: &FollowerConfig,
    ctx: &FollowerContext,
) -> Result<FollowStep, FollowError> {
    // 1. Head seqno. Kill -9 resume starts here: the commit is atomic, so
    //    the head is always a fully-committed state. Only the seqno (a
    //    `u32`) is held across the fetch await below — the full `State`
    //    carries an `Rc` and is not `Send`, so it is loaded after.
    let head_seqno = store
        .head()
        .map_err(|e| FollowError::Fatal(format!("follower: head lookup failed: {e}")))?
        .map(|(seqno, _)| seqno)
        .unwrap_or(0);
    let next = head_seqno
        .checked_add(1)
        .ok_or_else(|| FollowError::Fatal("follower: sequence number overflow".to_string()))?;

    // 2. Fetch. A timeout or transport failure means the peer has nothing
    //    for us right now (or is unreachable) — not an error, not a
    //    rejection: wait. Framing faults are the peer's misbehavior:
    //    reject loudly and retry the same seqno.
    let bytes = match client.fetch_block(next).await {
        Ok(b) => b,
        Err(SyncError::Timeout { .. } | SyncError::Transport(_)) => return Ok(FollowStep::Waiting),
        Err(e) => {
            return Err(FollowError::Retryable(format!(
                "follower: fetch of block {next}: peer misbehaved: {e}"
            )))
        }
    };

    // 3. Strict decode. Any deviation is the peer's fault, never a guess.
    let signed = decode_block_file(&bytes).map_err(|e| {
        FollowError::Retryable(format!("follower: block {next}: bad block file: {e}"))
    })?;
    let block = signed.block;

    // 4. Seqno binding: the served block must be the requested one.
    if block.header.seqno != next {
        return Err(FollowError::Retryable(format!(
            "follower: block {next}: peer served block with seqno {}",
            block.header.seqno
        )));
    }

    // 5. Authenticate: ONXBLK05 signatures, version, block_time
    //    monotonicity — the acceptance layer, outside the STF.
    let parent_time = parent_block_time(store, next)?;
    verify_block_auth(
        &ctx.chain_id,
        &block.header,
        parent_time,
        &signed.sig_entries,
        &ctx.validators,
    )
    .map_err(|e| FollowError::Retryable(format!("follower: block {next}: auth failed: {e}")))?;

    // Full head state, loaded AFTER the last await: `State` is not
    // `Send` (it carries an `Rc`), so it must never cross one. The
    // follower is the only writer to its store and the loop is
    // sequential, so the head cannot have moved under us.
    let state = store
        .load_state()
        .map_err(|e| FollowError::Fatal(format!("follower: load_state failed: {e}")))?
        .ok_or_else(|| {
            FollowError::Fatal("follower: no state (genesis not initialized)".to_string())
        })?;
    debug_assert_eq!(
        state.seqno, head_seqno,
        "follower: head moved under a sequential loop — single-writer invariant broken"
    );

    // 6. Apply + persist atomically. `commit_block` re-runs the pure STF
    //    (fail-closed: seqno, prev-hash, msgs_root, claimed state root…),
    //    then commits state/body/sigs/head in one transaction. Never
    //    wrapped: a panic here halts the node (ADR-0029).
    store
        .commit_block(&state, &block, &signed.sig_entries)
        .map_err(|e| {
            FollowError::Retryable(format!("follower: block {next}: STF rejected: {e}"))
        })?;

    // 7. Emit the canonical block file, atomically. The fetched bytes
    //    passed the strict decoder, so they are canonical by construction
    //    — write what the peer served, not a re-encode.
    atomic_write_block_file(&cfg.blocks_dir, next, &bytes).map_err(FollowError::Fatal)?;

    if let Some(t) = &cfg.telemetry {
        t.set_block_height(next as i64);
    }
    Ok(FollowStep::Synced { seqno: next })
}

/// Run the follower loop until `shutdown` is set.
///
/// Blocking on nothing but the peer: each step fetches the next block
/// after the head, verifies it fully, and commits it. Returns statistics
/// on clean shutdown, or a fatal error string (fork detected, corrupt
/// store, our own disk broken).
pub async fn run_follower_loop<T: SyncTransport>(
    store: ChainStore,
    client: SyncClient<T>,
    cfg: FollowerConfig,
    shutdown: Arc<AtomicBool>,
) -> Result<FollowerStats, String> {
    // Panic hook first: same loud diagnostics as the producer path.
    install_panic_backtrace_hook();
    fs::create_dir_all(&cfg.blocks_dir)
        .map_err(|e| format!("follower: cannot create blocks dir: {e}"))?;
    // Crash recovery for the block-file gap, same as the producer: a kill
    // between the atomic DB commit and the block-file write leaves blocks
    // committed but files missing; the database is the source of truth.
    let swept = sweep_temp_block_files(&cfg.blocks_dir)?;
    let regenerated = regenerate_missing_block_files(&store, &cfg.blocks_dir)?;
    if swept > 0 || regenerated > 0 {
        eprintln!("follower: startup recovery: swept {swept} temp files, regenerated {regenerated} block files");
    }
    let ctx = FollowerContext::load(&store)?;
    let head_seqno = store
        .load_state()
        .map_err(|e| format!("follower: load_state failed: {e}"))?
        .ok_or_else(|| "follower: no state (genesis not initialized)".to_string())?
        .seqno;
    eprintln!(
        "follower: syncing from head seqno {head_seqno} (chain {})",
        hex::encode(ctx.chain_id)
    );

    let mut stats = FollowerStats::default();
    while !shutdown.load(Ordering::Relaxed) {
        match follow_next_block(&store, &client, &cfg, &ctx).await {
            Ok(FollowStep::Synced { seqno }) => {
                stats.blocks_synced += 1;
                eprintln!("follower: synced block seqno={seqno}");
            }
            Ok(FollowStep::Waiting) => {
                stats.waits += 1;
                sleep(cfg.poll_interval).await;
            }
            Err(FollowError::Retryable(e)) => {
                // Loud log, same seqno next tick. A Byzantine peer can
                // waste our time but never our chain: nothing is committed
                // and nothing is skipped.
                stats.rejected += 1;
                eprintln!("follower: rejected: {e}; retrying");
                sleep(cfg.poll_interval).await;
            }
            Err(FollowError::Fatal(e)) => return Err(e),
        }
    }
    Ok(stats)
}
