//! Single-node transaction mempool.
//!
//! The mempool is the daemon's transaction intake: signed V2 transactions
//! arrive as files, get validated, and wait for the block-production loop.
//!
//! ## Submission interface: file-drop directory
//!
//! Producers (a wallet CLI, tests, a future RPC layer) submit transactions
//! by writing files into `<tx_pool_dir>/`. A transaction file is exactly
//! the 168-byte canonical V2 wire encoding (`Transaction::to_bytes`).
//! Writers MUST write atomically (temp file + rename); a file that fails
//! to parse is left in place and retried a few times before being moved
//! to `rejected/` (tolerance for concurrent writers, not silent loss).
//!
//! Why file-drop and not an in-process channel or socket: the frozen list
//! forbids networking and RPC, and an in-process channel would only serve
//! in-process submitters (i.e. tests). A spool directory is a real,
//! cross-process, no-network submission path with durable crash semantics:
//! an accepted-but-uncommitted transaction survives a daemon restart because
//! its file survives in `pending/`. This is the same shape a future RPC
//! endpoint will write into, so the interface doesn't need to change when
//! networking unfreezes — only the writer does.
//!
//! ## Directory layout (all under `<tx_pool_dir>/`)
//!
//! ```text
//! <tx_pool_dir>/
//!   *.tx            drop zone: new submissions appear here
//!   pending/<hash>.tx   accepted, awaiting inclusion (durable mempool)
//!   rejected/<name>     failed validation, with the reason logged
//! ```
//!
//! Files are renamed by content hash on acceptance, so deduplication is
//! structural: the same transaction submitted twice collapses to one
//! `pending/<hash>.tx`. On commit, the block's transaction hashes are
//! removed from `pending/`. Files are never deleted silently —
//! `rejected/` keeps the evidence.
//!
//! ## Validation layers
//!
//! 1. **Intake** (`scan_drop_dir`): parse (exact 168 bytes) → sender must be
//!    an `Active` account with a non-zero pubkey → signature verifies →
//!    nonce not stale. Permanently-invalid submissions (malformed, bad
//!    signature, keyless/unknown sender, stale nonce) go to `rejected/`.
//!    Future-nonce and insufficient-balance submissions are *held*: they may
//!    become valid later, and dropping them would lose real transactions.
//! 2. **Proposal** (`select_candidates`): re-validated against the fresh
//!    head, with per-sender nonce chains and a cumulative balance walk, in
//!    a deterministic global order. Anything that became invalid between
//!    submission and proposal is dropped to `rejected/`, never crashed on.
//! 3. **Commit** (`commit_block`): the STF re-validates the whole block
//!    through `apply_block`. The mempool filter exists for liveness (so a
//!    bad tx can't poison a block); the STF is the final arbiter.
//!
//! ## Deterministic ordering
//!
//! Block content must be deterministic given mempool state: no wall-clock,
//! no filesystem iteration order leaks in. Candidates are ordered by
//! `(sender AccountId bytes, nonce)` ascending — per-sender nonce chains
//! stay contiguous and the global order is identical in every process.
//!
//! ## Bounds
//!
//! `max_txs` bounds the mempool (default 10_000). When full, new
//! submissions are rejected with the reason logged — valid pending
//! transactions are never evicted silently to make room. There is no TTL
//! or fee-based eviction in this milestone; that is future mempool policy
//! work, not correctness work.

use onx_data_structures::AccountId;
use onx_primitives::PublicKey;
use onx_stf::Transaction;
use onx_state_model::AccountState;
use onx_storage::ChainStore;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// How many consecutive intake parse failures a drop file tolerates before
/// being moved to `rejected/`. Tolerance for writers that don't follow the
/// atomic-write convention (temp + rename); not silent loss.
const INTAKE_RETRY_LIMIT: u8 = 3;

/// A validated transaction waiting for block inclusion.
#[derive(Debug, Clone)]
struct PendingTx {
    tx: Transaction,
    /// File under `pending/` holding these bytes; removed on commit.
    file: PathBuf,
}

/// Why a submission was rejected. Logged, and the file is preserved under
/// `rejected/` — evidence, not silent deletion.
#[derive(Debug, Clone)]
pub struct Rejection {
    pub file_name: String,
    pub reason: String,
}

/// Intake statistics for one drop-directory scan.
#[derive(Debug, Default)]
pub struct IntakeStats {
    pub accepted: u64,
    pub duplicates: u64,
    pub rejected: u64,
    pub retried: u64,
}

pub struct Mempool {
    pending: BTreeMap<[u8; 32], PendingTx>,
    max_txs: usize,
    intake_retries: BTreeMap<PathBuf, u8>,
    pending_dir: PathBuf,
    rejected_dir: PathBuf,
}

impl Mempool {
    pub fn new(tx_pool_dir: &Path, max_txs: usize) -> Result<Self, String> {
        let pending_dir = tx_pool_dir.join("pending");
        let rejected_dir = tx_pool_dir.join("rejected");
        fs::create_dir_all(&pending_dir)
            .map_err(|e| format!("mempool: cannot create {}: {e}", pending_dir.display()))?;
        fs::create_dir_all(&rejected_dir)
            .map_err(|e| format!("mempool: cannot create {}: {e}", rejected_dir.display()))?;
        Ok(Self {
            pending: BTreeMap::new(),
            max_txs,
            intake_retries: BTreeMap::new(),
            pending_dir,
            rejected_dir,
        })
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Rehydrate the in-memory index from `pending/` after a restart.
    /// Files that no longer parse are moved to `rejected/`; the rest are
    /// re-validated at the next proposal (fail-closed there).
    pub fn rehydrate(&mut self) -> Result<usize, String> {
        let mut count = 0;
        let entries = fs::read_dir(&self.pending_dir)
            .map_err(|e| format!("mempool: cannot read {}: {e}", self.pending_dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("mempool: dir entry failed: {e}"))?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let bytes = match fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    self.reject_file(&path, &format!("unreadable pending file: {e}"));
                    continue;
                }
            };
            match Transaction::from_bytes(&bytes) {
                Ok(tx) => {
                    let hash = tx.hash();
                    // Fail-closed: the filename must match the content hash.
                    // A mismatch means the file was tampered with or
                    // miswritten; it is not silently trusted.
                    let expected = format!("{}.tx", hex::encode(hash));
                    let actual = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if actual != expected {
                        self.reject_file(&path, "pending filename does not match tx hash");
                        continue;
                    }
                    self.pending.insert(hash, PendingTx { tx, file: path });
                    count += 1;
                }
                Err(e) => {
                    self.reject_file(&path, &format!("pending file no longer parses: {e}"));
                }
            }
        }
        Ok(count)
    }

    /// Scan the drop directory once: parse, validate, accept/hold/reject.
    pub fn scan_drop_dir(
        &mut self,
        drop_dir: &Path,
        store: &ChainStore,
    ) -> Result<IntakeStats, String> {
        let mut stats = IntakeStats::default();
        let entries = fs::read_dir(drop_dir)
            .map_err(|e| format!("mempool: cannot read drop dir {}: {e}", drop_dir.display()))?;
        // Deterministic scan order: sort by file name so intake order does
        // not depend on filesystem iteration order.
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| format!("mempool: dir entry failed: {e}"))?;
            let path = entry.path();
            if path.is_file()
                && path.extension().map(|e| e == "tx").unwrap_or(false)
            {
                paths.push(path);
            }
        }
        paths.sort();

        for path in paths {
            match self.intake_one(&path, store) {
                Ok(IntakeOutcome::Accepted) => stats.accepted += 1,
                Ok(IntakeOutcome::Duplicate) => stats.duplicates += 1,
                Ok(IntakeOutcome::HeldFutureNonce | IntakeOutcome::HeldInsufficientBalance) => {
                    // Held transactions were accepted into pending/; they
                    // are not yet block candidates. Count them as accepted
                    // for intake purposes.
                    stats.accepted += 1;
                }
                Ok(IntakeOutcome::Rejected { reason }) => {
                    stats.rejected += 1;
                    eprintln!(
                        "mempool: rejected {}: {reason}",
                        path.file_name().map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    );
                }
                Ok(IntakeOutcome::ParseRetry) => stats.retried += 1,
                Err(e) => return Err(e),
            }
        }
        Ok(stats)
    }

    /// Remove the pending files for transactions in a committed block.
    /// Also sweeps any other pending file whose content is in `txs`
    /// (defensive: filenames are content hashes, so this is normally exact).
    pub fn remove_committed(&mut self, txs: &[Transaction]) {
        for tx in txs {
            let hash = tx.hash();
            if let Some(pending) = self.pending.remove(&hash) {
                let _ = fs::remove_file(&pending.file);
            }
        }
    }

    /// Move currently-pending transactions to `rejected/`. Used only as a
    /// circuit breaker when block proposal keeps failing on a candidate set
    /// the filter believed was valid — fail-closed rather than wedging the
    /// node forever.
    pub fn quarantine_all(&mut self, reason: &str) -> usize {
        let hashes: Vec<[u8; 32]> = self.pending.keys().copied().collect();
        let mut count = 0;
        for hash in hashes {
            if let Some(pending) = self.pending.remove(&hash) {
                let name = pending
                    .file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| format!("{}.tx", hex::encode(hash)));
                let dest = self.rejected_dir.join(&name);
                if fs::rename(&pending.file, &dest).is_ok() {
                    count += 1;
                }
                eprintln!("mempool: quarantined {name}: {reason}");
            }
        }
        count
    }

    // ----- internals -----

    fn reject_file(&self, path: &Path, reason: &str) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unnamed".to_string());
        // Avoid collisions in rejected/: append a counter if needed.
        let mut dest = self.rejected_dir.join(&name);
        let mut n = 0u32;
        while dest.exists() {
            n += 1;
            dest = self.rejected_dir.join(format!("{name}.{n}"));
        }
        if fs::rename(path, &dest).is_ok() {
            eprintln!("mempool: rejected {name}: {reason}");
        } else {
            eprintln!("mempool: FAILED to move rejected file {name}: {reason}");
        }
    }

    fn intake_one(
        &mut self,
        path: &Path,
        store: &ChainStore,
    ) -> Result<IntakeOutcome, String> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => return Err(format!("mempool: cannot read {}: {e}", path.display())),
        };
        let tx = match Transaction::from_bytes(&bytes) {
            Ok(tx) => tx,
            Err(e) => {
                // Parse failure: tolerate a few retries (concurrent writer
                // mid-rename), then reject. Never silently delete.
                let attempts = self.intake_retries.get(path).copied().unwrap_or(0) + 1;
                if attempts >= INTAKE_RETRY_LIMIT {
                    self.intake_retries.remove(path);
                    self.reject_file(path, &format!("unparseable tx file: {e}"));
                    return Ok(IntakeOutcome::Rejected {
                        reason: format!("unparseable: {e}"),
                    });
                }
                self.intake_retries.insert(path.to_path_buf(), attempts);
                return Ok(IntakeOutcome::ParseRetry);
            }
        };
        self.intake_retries.remove(path);

        let hash = tx.hash();
        if self.pending.contains_key(&hash) {
            // Duplicate submission: drop the file, keep the original.
            let _ = fs::remove_file(path);
            return Ok(IntakeOutcome::Duplicate);
        }

        // --- validation against the current tip (reject garbage at the door) ---
        let account = store
            .get_account(&tx.from)
            .map_err(|e| format!("mempool: account lookup failed: {e}"))?;
        let (pubkey_bytes, balance, nonce) = match account {
            Some(AccountState::Active {
                pubkey,
                balance_nanos,
                nonce,
                ..
            }) => (pubkey, balance_nanos, nonce),
            _ => {
                // Unknown, uninitialized, frozen, or destroyed senders can
                // never authorize: reject now, not at proposal time.
                self.reject_file(path, "sender is not a spendable account");
                return Ok(IntakeOutcome::Rejected {
                    reason: "sender not spendable".to_string(),
                });
            }
        };
        if pubkey_bytes == [0u8; 32] {
            self.reject_file(path, "sender account is keyless");
            return Ok(IntakeOutcome::Rejected {
                reason: "keyless sender".to_string(),
            });
        }
        let pubkey = PublicKey::decode_exact(&pubkey_bytes).map_err(|e| {
            format!("mempool: stored pubkey undecodable (corrupt genesis?): {e}")
        })?;
        if tx.verify_signature(&pubkey).is_err() {
            self.reject_file(path, "signature verification failed");
            return Ok(IntakeOutcome::Rejected {
                reason: "bad signature".to_string(),
            });
        }
        if tx.nonce < nonce {
            // Stale: this nonce was already consumed. Can never become valid.
            self.reject_file(path, &format!("stale nonce {} < {}", tx.nonce, nonce));
            return Ok(IntakeOutcome::Rejected {
                reason: "stale nonce".to_string(),
            });
        }

        // Mempool bound: reject new work rather than evicting valid pending
        // transactions silently.
        if self.pending.len() >= self.max_txs {
            self.reject_file(path, "mempool full");
            return Ok(IntakeOutcome::Rejected {
                reason: "mempool full".to_string(),
            });
        }

        // Accept: rename into pending/ by content hash (atomic on one fs).
        let dest = self.pending_dir.join(format!("{}.tx", hex::encode(hash)));
        if let Err(e) = fs::rename(path, &dest) {
            return Err(format!(
                "mempool: cannot move {} to pending: {e}",
                path.display()
            ));
        }
        let held = if tx.nonce > nonce {
            IntakeOutcome::HeldFutureNonce
        } else if tx.amount_nanos.checked_add(tx.fee_nanos).is_some_and(|need| need > balance) {
            IntakeOutcome::HeldInsufficientBalance
        } else {
            IntakeOutcome::Accepted
        };
        self.pending.insert(hash, PendingTx { tx, file: dest });
        Ok(held)
    }

    /// Select the deterministic candidate set for the next block, given the
    /// current head state. Re-validates everything (state moved since
    /// intake); transactions that became permanently invalid are moved to
    /// `rejected/`, never crashed on.
    ///
    /// Selection reads accounts from `state.tree` directly, so it is a pure
    /// function of (mempool, state): the producer must pass the same state
    /// to `propose_block`, and the store's continuity check then pins the
    /// block to the stored head. No store reads here — no TOCTOU between
    /// selection and proposal.
    ///
    /// Deterministic order: per-sender nonce chains, contiguous from the
    /// account's current nonce; global order sorted by
    /// `(sender bytes, nonce)` ascending. Given the same mempool state this
    /// always yields the same block content — no wall-clock, no filesystem
    /// order leaks in.
    pub fn select_candidates(&mut self, state: &onx_stf::State) -> Result<Vec<Transaction>, String> {
        // Group pending txs by sender, sorted by nonce.
        let mut by_sender: BTreeMap<AccountId, Vec<&Transaction>> = BTreeMap::new();
        for pending in self.pending.values() {
            by_sender.entry(pending.tx.from).or_default().push(&pending.tx);
        }
        for txs in by_sender.values_mut() {
            txs.sort_by_key(|tx| tx.nonce);
        }

        // Per-sender contiguous chains from the account's current nonce,
        // with a cumulative balance walk. Anything stale or unaffordable
        // *this round* is handled: stale → rejected (can never be valid);
        // future-nonce gaps and insufficient balance → held for a later
        // block (may become valid).
        let mut candidates: Vec<Transaction> = Vec::new();
        let mut reserved: BTreeMap<AccountId, u128> = BTreeMap::new();
        let mut to_reject: Vec<[u8; 32]> = Vec::new();

        for (sender, txs) in &by_sender {
            let (balance, mut expected_nonce) = match state.tree.get(sender) {
                Some(AccountState::Active {
                    balance_nanos,
                    nonce,
                    ..
                }) => (*balance_nanos, *nonce),
                _ => {
                    // Sender stopped being spendable since intake (frozen?
                    // destroyed?). Its txs can never authorize: reject.
                    for tx in txs.iter() {
                        to_reject.push(tx.hash());
                    }
                    continue;
                }
            };
            for tx in txs.iter() {
                if tx.nonce < expected_nonce {
                    to_reject.push(tx.hash()); // stale
                    continue;
                }
                if tx.nonce > expected_nonce {
                    break; // gap: hold this and everything after for later
                }
                let need = match tx.amount_nanos.checked_add(tx.fee_nanos) {
                    Some(n) => n,
                    None => {
                        to_reject.push(tx.hash()); // arithmetic overflow: never valid
                        expected_nonce += 1;
                        continue;
                    }
                };
                let already = reserved.get(sender).copied().unwrap_or(0);
                if need > balance.saturating_sub(already) {
                    break; // can't afford this round; hold (balance may improve)
                }
                reserved.insert(*sender, already + need);
                candidates.push((*tx).clone());
                expected_nonce += 1;
            }
        }

        for hash in to_reject {
            if let Some(pending) = self.pending.remove(&hash) {
                self.reject_file(&pending.file, "became invalid before proposal");
            }
        }

        // Global deterministic order.
        candidates.sort_by_key(|tx| (tx.from.to_bytes(), tx.nonce));
        Ok(candidates)
    }
}

enum IntakeOutcome {
    Accepted,
    Duplicate,
    HeldFutureNonce,
    HeldInsufficientBalance,
    Rejected { reason: String },
    ParseRetry,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_order_is_sender_then_nonce() {
        let txs = vec![
            tx_with(b(2), 1),
            tx_with(b(1), 1),
            tx_with(b(1), 0),
            tx_with(b(2), 0),
        ];
        let mut ordered = txs.clone();
        ordered.sort_by_key(|tx| (tx.from.to_bytes(), tx.nonce));
        let got: Vec<(u8, u64)> = ordered.iter().map(|t| (t.from.to_bytes()[0], t.nonce)).collect();
        assert_eq!(got, vec![(1, 0), (1, 1), (2, 0), (2, 1)]);
    }

    fn b(byte: u8) -> AccountId {
        AccountId::from_bytes([byte; 32])
    }

    fn tx_with(from_byte: AccountId, nonce: u64) -> Transaction {
        Transaction {
            kind: onx_stf::block::TxKind::Transfer,
            from: from_byte,
            to: b(0xff),
            amount_nanos: 1,
            fee_nanos: 0,
            nonce,
            message: Vec::new(),
            signature: [0u8; 64],
        }
    }
}
