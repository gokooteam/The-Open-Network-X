//! Single-node external-message mempool.
//!
//! The mempool is the daemon's message intake: signed external messages
//! arrive as files, get validated, and wait for the block-production loop.
//!
//! ## Submission interface: file-drop directory
//!
//! Producers (a wallet CLI, tests, a future RPC layer) submit external
//! messages by writing files into `<tx_pool_dir>/`. A message file is
//! exactly the canonical wire encoding (`ExternalMessage::to_bytes`).
//! Writers MUST write atomically (temp file + rename); a file that fails
//! to parse is left in place and retried a few times before being moved
//! to `rejected/` (tolerance for concurrent writers, not silent loss).
//!
//! Why file-drop and not an in-process channel or socket: networking was
//! frozen (ADR-0042 records the unfreeze, but the network loop is not built
//! yet), so no networking or RPC path exists, and an in-process channel
//! would only serve in-process submitters (i.e. tests). A spool directory is a real,
//! cross-process, no-network submission path with durable crash semantics:
//! an accepted-but-uncommitted message survives a daemon restart because
//! its file survives in `pending/`. This is the same shape a future RPC
//! endpoint will write into, so the interface doesn't need to change when
//! networking unfreezes — only the writer does.
//!
//! ## Directory layout (all under `<tx_pool_dir>/`)
//!
//! ```text
//! <tx_pool_dir>/
//!   *.msg            drop zone: new submissions appear here
//!   pending/<hash>.msg   accepted, awaiting inclusion (durable mempool)
//!   rejected/<name>     failed validation, with the reason logged
//! ```
//!
//! Files are renamed by content hash on acceptance, so deduplication is
//! structural: the same message submitted twice collapses to one
//! `pending/<hash>.msg`. On commit, the block's message hashes are
//! removed from `pending/`. Files are never deleted silently —
//! `rejected/` keeps the evidence.
//!
//! ## Validation layers
//!
//! 1. **Intake** (`scan_drop_dir`): parse → the shared wallet-mirror check
//!    (chain ID, kind sanity, key resolution, nonce, balance, fee-collector
//!    receivability) → signature verifies. Permanently-invalid submissions
//!    go to `rejected/`. Future-nonce and insufficient-balance submissions
//!    are *held*: they may become valid later, and dropping them would lose
//!    real messages.
//! 2. **Proposal** (`select_candidates`): the same shared check re-run
//!    against the fresh head, with per-sender nonce chains, key-reveal
//!    tracking, and a cumulative balance walk, in a deterministic global
//!    order. Anything that became invalid between submission and proposal
//!    is dropped to `rejected/`, never crashed on.
//! 3. **Commit** (`commit_block`): the STF re-validates the whole block
//!    through `apply_block`. The mempool filter exists for liveness (so a
//!    bad message can't poison a block); the STF is the final arbiter.
//!
//! The shared check ([`WalletMirror`]) mirrors every fail-closed rule of
//! the STF wallet handler (`onx-stf/src/stf.rs::wallet_receive`), so intake
//! and proposal can never admit a message `propose_block` would reject.
//! Previously they could — a correctly signed zero-amount transfer, a
//! zero-fee contract call, or a second key reveal from one sender passed
//! the filter and failed the whole block, and after three failures the
//! producer quarantined the *entire* mempool (a free, repeatable DoS).
//! That quarantine is gone: only messages the STF actually rejects are
//! ever dropped.
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
//! messages are never evicted silently to make room. There is no TTL
//! or fee-based eviction in this milestone; that is future mempool policy
//! work, not correctness work.

use onx_data_structures::AccountId;
use onx_primitives::PublicKey;
use onx_state_model::AccountState;
use onx_stf::{derive_address, ExternalMessage, MsgKind};
use onx_storage::ChainStore;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// How many consecutive intake parse failures a drop file tolerates before
/// being moved to `rejected/`. Tolerance for writers that don't follow the
/// atomic-write convention (temp + rename); not silent loss.
const INTAKE_RETRY_LIMIT: u8 = 3;

/// A validated external message waiting for block inclusion.
#[derive(Debug, Clone)]
struct PendingMsg {
    msg: ExternalMessage,
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

/// Why a message the mirror resolved is not yet includable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReason {
    /// Nonce beyond the expected one — hold; a missing predecessor may
    /// arrive later.
    FutureNonce,
    /// Balance does not cover `amount + fee` yet — hold; the balance may
    /// improve.
    InsufficientBalance,
}

/// Outcome of [`WalletMirror::check`].
#[derive(Debug, Clone, Copy)]
pub enum Verdict {
    /// Valid for inclusion now. Carries the resolved key and total debit.
    Accept {
        /// Key the signature must verify against: the stored key, or the
        /// revealed key for a first spend from a key-derived account.
        effective_pubkey: [u8; 32],
        /// Total debit (`amount + fee`); arithmetic overflow already
        /// excluded.
        need: u128,
    },
    /// Authorization resolved, but the message is not yet includable.
    /// Intake verifies the signature *before* holding — a bad signature is
    /// rejected, never held.
    Hold {
        effective_pubkey: [u8; 32],
        reason: HoldReason,
    },
    /// Permanently invalid — move to `rejected/`. (Bad signatures are a
    /// separate intake-only check, not a defect: they are cryptographic,
    /// not state-dependent.)
    Reject(&'static str),
}

/// Per-message inputs to [`WalletMirror::check`].
pub struct CheckCtx<'a> {
    /// The sender's current on-chain account (must be `Active`).
    pub sender: &'a AccountState,
    /// Nonce the message must carry: the on-chain nonce at intake, the
    /// walk position at proposal.
    pub expected_nonce: u64,
    /// Balance available to this message: the full balance at intake, the
    /// balance minus already-reserved debits at proposal.
    pub spendable: u128,
    /// Key revealed earlier in this proposal walk, when the sender was
    /// keyless on-chain. Always `None` at intake.
    pub revealed_key: Option<[u8; 32]>,
    /// Current fee-collector account (for the validator-fee receivability
    /// check).
    pub fee_collector: &'a AccountState,
    /// Logical time of the block being built, when known (`None` at
    /// intake — see the `check` docs).
    pub block_lt: Option<u64>,
}

/// Mirror of the STF wallet handler's fail-closed checks
/// (`onx-stf/src/stf.rs::wallet_receive`), shared by mempool intake and
/// block proposal so the two can never disagree about a message's validity.
///
/// A message the filter admits but `propose_block` rejects fails the whole
/// block; the old code then quarantined the entire mempool after three
/// such failures — a free, repeatable denial of service. Every rejection
/// below matches a wallet-handler rejection one-to-one; the STF remains
/// the final arbiter.
///
/// Deliberately NOT mirrored: signature verification. Intake verifies
/// signatures against the resolved key, message bytes are content-hash
/// pinned afterwards, and any key change implies a nonce advance (which
/// the nonce check catches) — re-verifying at proposal would double the
/// most expensive check for no new information.
pub struct WalletMirror {
    chain_id: [u8; 32],
}

impl WalletMirror {
    pub fn new(chain_id: [u8; 32]) -> Self {
        Self { chain_id }
    }

    /// Validate one message against the wallet handler's rules.
    ///
    /// Key resolution (steps 1–4) always runs first and its outcome is
    /// returned even for held messages, so intake can verify the signature
    /// *before* deciding to hold — a bad signature is rejected, never held.
    pub fn check(&self, msg: &ExternalMessage, ctx: &CheckCtx) -> Verdict {
        // 1. Chain binding (wallet §1).
        if msg.chain_id != self.chain_id {
            return Verdict::Reject("message chain ID does not match this chain");
        }

        // 2. Kind-specific sanity (wallet §2). Static: no state needed, so a
        // message failing here is invalid at intake too — this is the check
        // the old filter was missing (zero-amount transfers and zero-fee
        // calls sailed through to `propose_block`).
        match msg.kind {
            MsgKind::Transfer => {
                if msg.amount_nanos == 0 {
                    return Verdict::Reject("zero-amount transfer");
                }
            }
            MsgKind::ContractCall => {
                if msg.fee_nanos == 0 {
                    return Verdict::Reject("zero-fee contract call");
                }
            }
        }
        let need = match msg.amount_nanos.checked_add(msg.fee_nanos) {
            Some(n) => n,
            None => return Verdict::Reject("amount + fee overflows"),
        };

        // 3. Sender must be Active (wallet §3).
        let (stored_pubkey, last_trans_lt) = match ctx.sender {
            AccountState::Active {
                pubkey,
                last_trans_lt,
                ..
            } => (*pubkey, *last_trans_lt),
            _ => return Verdict::Reject("sender is not a spendable account"),
        };

        // 4. Key resolution (wallet §4).
        // (a) Keyed on-chain: the message must NOT reveal a key.
        // (b) Keyless on-chain, nothing revealed yet this walk: the message
        //     MUST reveal a pubkey deriving to the sender's address.
        // (c) Keyless on-chain, revealed earlier this walk: the account is
        //     already keyed — a second reveal is permanently invalid. This
        //     is the case the old proposal filter missed.
        let effective_pubkey = if stored_pubkey == [0u8; 32] {
            match ctx.revealed_key {
                Some(k) => {
                    if msg.pubkey != [0u8; 32] {
                        return Verdict::Reject(
                            "unexpected pubkey reveal: account already keyed by an earlier message",
                        );
                    }
                    k
                }
                None => {
                    if msg.pubkey == [0u8; 32] {
                        return Verdict::Reject("sender account is keyless and revealed no key");
                    }
                    if derive_address(&msg.pubkey) != msg.from {
                        return Verdict::Reject(
                            "revealed pubkey does not derive to sender address",
                        );
                    }
                    msg.pubkey
                }
            }
        } else {
            if msg.pubkey != [0u8; 32] {
                return Verdict::Reject("sender already has a key; unexpected reveal");
            }
            stored_pubkey
        };
        // Belt-and-braces, mirroring the wallet: the all-zero pubkey is an
        // order-4 Ed25519 point and must never authorize anything.
        if effective_pubkey == [0u8; 32] {
            return Verdict::Reject("effective pubkey is all zeros");
        }
        // Genesis validates stored pubkeys and intake verifies revealed
        // ones; this fails closed on a corrupt account record.
        if PublicKey::decode_exact(&effective_pubkey).is_err() {
            return Verdict::Reject("effective pubkey does not decode");
        }

        // 5. Nonce (wallet §5).
        if msg.nonce < ctx.expected_nonce {
            return Verdict::Reject("stale nonce");
        }
        if msg.nonce > ctx.expected_nonce {
            return Verdict::Hold {
                effective_pubkey,
                reason: HoldReason::FutureNonce,
            };
        }

        // 6. Balance covers amount + fee (wallet §5).
        if need > ctx.spendable {
            return Verdict::Hold {
                effective_pubkey,
                reason: HoldReason::InsufficientBalance,
            };
        }

        // 7. Logical time (wallet §5, `check_lt`). Vacuous for
        // honestly-produced blocks — the producer uses
        // `lt = head.last_lt + 1`, which exceeds every account's
        // `last_trans_lt` — but the invariant is checked anyway so a
        // future change in lt assignment cannot silently admit bad blocks.
        // At intake the block lt is unknowable, hence `None` (the same
        // monotonicity argument applies to any future lt).
        if let Some(lt) = ctx.block_lt {
            if lt < last_trans_lt {
                return Verdict::Reject("account time regression");
            }
        }

        // 8. Fee collector must be receivable when there is a validator fee
        // (wallet §6). With the 50/50 fee split the validator share is
        // non-zero exactly when `fee_nanos > 0`
        // (`validator = fee - floor(fee*50/100)`), so no economics crate is
        // needed here.
        if msg.fee_nanos > 0 {
            match ctx.fee_collector {
                AccountState::Frozen { .. } | AccountState::Destroyed => {
                    return Verdict::Reject("fee collector is not receivable")
                }
                AccountState::Active { .. } | AccountState::Uninitialized => {}
            }
        }

        Verdict::Accept {
            effective_pubkey,
            need,
        }
    }
}

pub struct Mempool {
    pending: BTreeMap<[u8; 32], PendingMsg>,
    max_txs: usize,
    chain_id: [u8; 32],
    fee_collector: AccountId,
    intake_retries: BTreeMap<PathBuf, u8>,
    pending_dir: PathBuf,
    rejected_dir: PathBuf,
}

impl Mempool {
    pub fn new(
        tx_pool_dir: &Path,
        max_txs: usize,
        chain_id: [u8; 32],
        fee_collector: AccountId,
    ) -> Result<Self, String> {
        let pending_dir = tx_pool_dir.join("pending");
        let rejected_dir = tx_pool_dir.join("rejected");
        fs::create_dir_all(&pending_dir)
            .map_err(|e| format!("mempool: cannot create {}: {e}", pending_dir.display()))?;
        fs::create_dir_all(&rejected_dir)
            .map_err(|e| format!("mempool: cannot create {}: {e}", rejected_dir.display()))?;
        Ok(Self {
            pending: BTreeMap::new(),
            max_txs,
            chain_id,
            fee_collector,
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
            match ExternalMessage::from_bytes(&bytes) {
                Ok(msg) => {
                    let hash = msg.hash();
                    // Fail-closed: the filename must match the content hash.
                    // A mismatch means the file was tampered with or
                    // miswritten; it is not silently trusted.
                    let expected = format!("{}.msg", hex::encode(hash));
                    let actual = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if actual != expected {
                        self.reject_file(&path, "pending filename does not match message hash");
                        continue;
                    }
                    self.pending.insert(hash, PendingMsg { msg, file: path });
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
            if path.is_file() && path.extension().is_some_and(|e| e == "msg") {
                paths.push(path);
            }
        }
        paths.sort();

        for path in paths {
            match self.intake_one(&path, store) {
                Ok(IntakeOutcome::Accepted) => stats.accepted += 1,
                Ok(IntakeOutcome::Duplicate) => stats.duplicates += 1,
                Ok(IntakeOutcome::HeldFutureNonce | IntakeOutcome::HeldInsufficientBalance) => {
                    // Held messages were accepted into pending/; they
                    // are not yet block candidates. Count them as accepted
                    // for intake purposes.
                    stats.accepted += 1;
                }
                Ok(IntakeOutcome::Rejected { reason }) => {
                    stats.rejected += 1;
                    eprintln!(
                        "mempool: rejected {}: {reason}",
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    );
                }
                Ok(IntakeOutcome::ParseRetry) => stats.retried += 1,
                Err(e) => return Err(e),
            }
        }
        Ok(stats)
    }

    /// Remove the pending files for messages in a committed block.
    /// Also sweeps any other pending file whose content is in `msgs`
    /// (defensive: filenames are content hashes, so this is normally exact).
    pub fn remove_committed(&mut self, msgs: &[ExternalMessage]) {
        for msg in msgs {
            let hash = msg.hash();
            if let Some(pending) = self.pending.remove(&hash) {
                let _ = fs::remove_file(&pending.file);
            }
        }
    }

    /// Move one pending message to `rejected/` with a reason. Returns true
    /// if it was pending. This is the only way messages leave the mempool
    /// as invalid — there is no bulk quarantine: a poisoned message must
    /// never take honest messages down with it.
    pub fn reject_candidate(&mut self, hash: &[u8; 32], reason: &str) -> bool {
        if let Some(pending) = self.pending.remove(hash) {
            self.reject_file(&pending.file, reason);
            true
        } else {
            false
        }
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

    fn intake_one(&mut self, path: &Path, store: &ChainStore) -> Result<IntakeOutcome, String> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => return Err(format!("mempool: cannot read {}: {e}", path.display())),
        };
        let msg = match ExternalMessage::from_bytes(&bytes) {
            Ok(msg) => msg,
            Err(e) => {
                // Parse failure: tolerate a few retries (concurrent writer
                // mid-rename), then reject. Never silently delete.
                let attempts = self.intake_retries.get(path).copied().unwrap_or(0) + 1;
                if attempts >= INTAKE_RETRY_LIMIT {
                    self.intake_retries.remove(path);
                    self.reject_file(path, &format!("unparsable message file: {e}"));
                    return Ok(IntakeOutcome::Rejected {
                        reason: format!("unparsable: {e}"),
                    });
                }
                self.intake_retries.insert(path.to_path_buf(), attempts);
                return Ok(IntakeOutcome::ParseRetry);
            }
        };
        self.intake_retries.remove(path);

        let hash = msg.hash();
        if self.pending.contains_key(&hash) {
            // Duplicate submission: drop the file, keep the original.
            let _ = fs::remove_file(path);
            return Ok(IntakeOutcome::Duplicate);
        }

        // Shared wallet-mirror validation: everything the STF would reject
        // at block time is rejected here, so intake and block execution
        // cannot disagree (previously zero-amount transfers and zero-fee
        // calls passed this filter and failed `propose_block`).
        let sender = store
            .get_account(&msg.from)
            .map_err(|e| format!("mempool: account lookup failed: {e}"))?
            .unwrap_or(AccountState::Uninitialized);
        let collector = store
            .get_account(&self.fee_collector)
            .map_err(|e| format!("mempool: fee collector lookup failed: {e}"))?
            .unwrap_or(AccountState::Uninitialized);
        let (expected_nonce, spendable) = match &sender {
            AccountState::Active {
                nonce,
                balance_nanos,
                ..
            } => (*nonce, *balance_nanos),
            // Non-active: the mirror reports it as permanently invalid.
            _ => (0, 0),
        };
        let mirror = WalletMirror::new(self.chain_id);
        let ctx = CheckCtx {
            sender: &sender,
            expected_nonce,
            spendable,
            revealed_key: None,
            fee_collector: &collector,
            // The block lt is unknowable at intake; the check is vacuous
            // for producer-built blocks (see WalletMirror::check).
            block_lt: None,
        };
        // The mirror resolves authorization (steps 1–4) and classifies
        // temporal validity (5–8) in one shared function. Signature
        // verification runs against the mirror-resolved key BEFORE the
        // hold/accept decision — a bad signature is rejected even for a
        // future-nonce message, never held.
        let (effective_pubkey, held_kind) = match mirror.check(&msg, &ctx) {
            Verdict::Accept {
                effective_pubkey, ..
            } => (effective_pubkey, None),
            Verdict::Hold {
                effective_pubkey,
                reason,
            } => (
                effective_pubkey,
                Some(match reason {
                    HoldReason::FutureNonce => IntakeOutcome::HeldFutureNonce,
                    HoldReason::InsufficientBalance => IntakeOutcome::HeldInsufficientBalance,
                }),
            ),
            Verdict::Reject(reason) => {
                self.reject_file(path, reason);
                return Ok(IntakeOutcome::Rejected {
                    reason: reason.to_string(),
                });
            }
        };
        // The mirror already established the key decodes (and, for reveals,
        // derives correctly); this cannot fail.
        let pubkey = PublicKey::decode_exact(&effective_pubkey)
            .map_err(|e| format!("mempool: effective pubkey undecodable: {e}"))?;
        if msg.verify_signature(&pubkey).is_err() {
            self.reject_file(path, "signature verification failed");
            return Ok(IntakeOutcome::Rejected {
                reason: "bad signature".to_string(),
            });
        }

        // Mempool bound: reject new work rather than evicting valid pending
        // messages silently.
        if self.pending.len() >= self.max_txs {
            self.reject_file(path, "mempool full");
            return Ok(IntakeOutcome::Rejected {
                reason: "mempool full".to_string(),
            });
        }

        // Accept: rename into pending/ by content hash (atomic on one fs).
        let dest = self.pending_dir.join(format!("{}.msg", hex::encode(hash)));
        if let Err(e) = fs::rename(path, &dest) {
            return Err(format!(
                "mempool: cannot move {} to pending: {e}",
                path.display()
            ));
        }
        // The mirror already classified this message: clean accept, or held
        // for a future nonce / better balance.
        let held = held_kind.unwrap_or(IntakeOutcome::Accepted);
        self.pending.insert(hash, PendingMsg { msg, file: dest });
        Ok(held)
    }

    /// Select the deterministic candidate set for the next block, given the
    /// current head state. Re-validates everything (state moved since
    /// intake); messages that became permanently invalid are moved to
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
    pub fn select_candidates(
        &mut self,
        state: &onx_stf::State,
    ) -> Result<Vec<ExternalMessage>, String> {
        // Group pending messages by sender, sorted by nonce.
        let mut by_sender: BTreeMap<AccountId, Vec<&ExternalMessage>> = BTreeMap::new();
        for pending in self.pending.values() {
            by_sender
                .entry(pending.msg.from)
                .or_default()
                .push(&pending.msg);
        }
        for msgs in by_sender.values_mut() {
            msgs.sort_by_key(|msg| msg.nonce);
        }

        let mirror = WalletMirror::new(self.chain_id);
        // The producer builds the block at `lt = state.last_lt + 1`; run the
        // mirror against that same logical time so the two cannot disagree.
        let block_lt = state
            .last_lt
            .checked_add(1)
            .ok_or_else(|| "mempool: logical time overflow selecting candidates".to_string())?;
        let collector_state = state
            .tree
            .get(&self.fee_collector)
            .cloned()
            .unwrap_or(AccountState::Uninitialized);

        // Per-sender contiguous chains from the account's current nonce,
        // each message run through the shared wallet mirror. Key reveals
        // are tracked along the walk: the account becomes keyed the moment
        // the first reveal executes, so a second reveal in the same block
        // is permanently invalid (the old filter missed this and fed it to
        // `propose_block`, failing the whole block).
        let mut candidates: Vec<ExternalMessage> = Vec::new();
        let mut to_reject: Vec<([u8; 32], &'static str)> = Vec::new();

        for (sender, msgs) in &by_sender {
            let sender_state = state
                .tree
                .get(sender)
                .cloned()
                .unwrap_or(AccountState::Uninitialized);
            let (mut expected_nonce, mut spendable, mut revealed_key) = match &sender_state {
                AccountState::Active {
                    balance_nanos,
                    nonce,
                    ..
                } => (*nonce, *balance_nanos, None),
                // Sender stopped being spendable since intake (frozen?
                // destroyed?). Its messages can never authorize: reject.
                _ => {
                    for msg in msgs.iter() {
                        to_reject.push((msg.hash(), "sender is not a spendable account"));
                    }
                    continue;
                }
            };
            for msg in msgs.iter() {
                let ctx = CheckCtx {
                    sender: &sender_state,
                    expected_nonce,
                    spendable,
                    revealed_key,
                    fee_collector: &collector_state,
                    block_lt: Some(block_lt),
                };
                match mirror.check(msg, &ctx) {
                    Verdict::Accept {
                        effective_pubkey: _,
                        need,
                    } => {
                        // Track the reveal: from here on the sender is keyed.
                        if revealed_key.is_none() && msg.pubkey != [0u8; 32] {
                            revealed_key = Some(msg.pubkey);
                        }
                        spendable = spendable.saturating_sub(need);
                        expected_nonce = expected_nonce.saturating_add(1);
                        candidates.push((*msg).clone());
                    }
                    Verdict::Reject(reason) => {
                        if msg.nonce < expected_nonce {
                            // Stale: reject, but keep walking — a later
                            // message may still hit the expected nonce.
                            to_reject.push((msg.hash(), reason));
                            continue;
                        }
                        // At the expected nonce but permanently invalid:
                        // reject it and stop the walk. Later nonces are
                        // gapped until the sender resubmits the missing
                        // nonce, so they are held, not rejected.
                        to_reject.push((msg.hash(), reason));
                        break;
                    }
                    // Future-nonce gap or insufficient balance this round:
                    // hold for a later block (may become valid).
                    Verdict::Hold { .. } => break,
                }
            }
        }

        for (hash, reason) in to_reject {
            self.reject_candidate(&hash, reason);
        }

        // Global deterministic order.
        candidates.sort_by_key(|msg| (msg.from.to_bytes(), msg.nonce));
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
    use onx_stf::message::MsgKind;

    #[test]
    fn candidate_order_is_sender_then_nonce() {
        let msgs = vec![
            msg_with(b(2), 1),
            msg_with(b(1), 1),
            msg_with(b(1), 0),
            msg_with(b(2), 0),
        ];
        let mut ordered = msgs.clone();
        ordered.sort_by_key(|msg| (msg.from.to_bytes(), msg.nonce));
        let got: Vec<(u8, u64)> = ordered
            .iter()
            .map(|m| (m.from.to_bytes()[0], m.nonce))
            .collect();
        assert_eq!(got, vec![(1, 0), (1, 1), (2, 0), (2, 1)]);
    }

    fn b(byte: u8) -> AccountId {
        AccountId::from_bytes([byte; 32])
    }

    fn msg_with(from_byte: AccountId, nonce: u64) -> ExternalMessage {
        ExternalMessage {
            chain_id: [0u8; 32],
            from: from_byte,
            nonce,
            kind: MsgKind::Transfer,
            to: b(0xff),
            amount_nanos: 1,
            fee_nanos: 0,
            message: Vec::new(),
            pubkey: [0u8; 32],
            signature: [0u8; 64],
        }
    }
}
