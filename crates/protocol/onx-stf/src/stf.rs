//! Message-based state transition function (ADR-0001).
//!
//! The synchronous transfer model is gone. Accounts interact *exclusively*
//! via asynchronous messages (`docs/specification/transactions.md` §2):
//!
//! - **Phase 1 — wallet (auth).** Each external message is authenticated
//!   in block order by the built-in wallet handler (STF, not VM):
//!   chain-ID binding, sender key/nonce/signature, balance. On success the
//!   sender is debited (`amount + fee`, fee split 50/50 burn/validator),
//!   the nonce bumps, and exactly one internal message is queued.
//! - **Phase 2 — delivery.** The queue drains FIFO. Each delivery executes
//!   as the receiver's own transaction: value credit, plus TVM execution
//!   when the payload is non-empty and the receiver has contract code.
//!   A message that cannot be processed **bounces**: the value (fees
//!   already taken) returns to the sender as a new internal message,
//!   appended to the same queue under the same ordering and replay rules.
//!
//! Delivery order is FIFO per (sender, receiver) pair: externals are
//! processed in order, each emits at most one internal in order, and
//! bounces are appended in delivery order — so messages enter the queue
//! in generation order and leave in FIFO order.
//!
//! Replay protection (ADR-0007): external messages are covered by the
//! sender nonce; internal messages by their ID against a per-block
//! processed set. Internal messages are derived, never submitted, so
//! cross-block replay is structurally impossible.

use crate::block::{msgs_root, AssembleParams, Block, PROTOCOL_VERSION};
use crate::error::StfError;
use crate::message::{derive_address, ExternalMessage, InternalMessage, MsgKind};
use crate::state::State;
use onx_data_structures::{AccountId, FullAddress, Message, MessageType, WorkchainIdent};
use onx_execution::{ExceptionKind, ExecutionContext, ExecutionResult, Interpreter, StackValue};
use onx_primitives::{domain_hash, DomainTag, PublicKey};
use onx_state_model::{
    AccountState, BagOfCells, Cell, ContractCellDags, ShardStateTree, StorageStat,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Gas purchased per nano-Onyxii of declared message fee, for contract calls.
/// The fee still splits 50/50 burn/validator via the normal fee model —
/// gas only bounds execution; there is no gas refund and no fee market yet
/// (both deferred). A contract call must carry a non-zero fee (rejected at
/// the wallet handler otherwise); a zero gas limit inside the VM bounces
/// the delivery.
pub const GAS_PER_NANO: u64 = 1_000;

/// Maximum gas any single message execution may consume (ADR-0034).
/// The VM gas limit is `min(fee_nanos * GAS_PER_NANO, MAX_GAS_PER_MESSAGE)`.
/// Consensus rule: all nodes derive the identical limit from the same fee.
pub const MAX_GAS_PER_MESSAGE: u64 = 10_000_000;

/// Maximum total gas across all message deliveries in one block (ADR-0034).
/// `apply_messages` fails closed with `StfError::BlockGasExceeded` when the
/// running total would exceed this. Consensus rule: a block is valid iff the
/// sum of per-delivery `gas_used` is `<= MAX_GAS_PER_BLOCK`.
pub const MAX_GAS_PER_BLOCK: u64 = 100_000_000;

/// Compute the VM gas limit for a message's contract execution (ADR-0034).
/// The fee buys gas at `GAS_PER_NANO`, saturating at `u64::MAX`, then the
/// per-message cap `MAX_GAS_PER_MESSAGE` clamps it. Pure function of the
/// fee — all nodes derive the identical limit.
fn message_gas_limit(fee_nanos: u128) -> u64 {
    (fee_nanos
        .saturating_mul(GAS_PER_NANO as u128)
        .min(u64::MAX as u128) as u64)
        .min(MAX_GAS_PER_MESSAGE)
}

/// Accumulate one delivery's gas into the block total (ADR-0034).
/// Returns the new total, or `BlockGasExceeded` if it would pass
/// `MAX_GAS_PER_BLOCK`. Pure function — the consensus rule in one place.
fn accumulate_block_gas(current_total: u64, additional: u64) -> Result<u64, StfError> {
    // `saturating_add`: the true total is bounded by
    // max_deliveries * MAX_GAS_PER_MESSAGE << u64::MAX, so saturation only
    // triggers on a corrupted receipt — and a saturated total is still
    // correctly > cap. The reported `used` stays exact in all reachable cases.
    let new_total = current_total.saturating_add(additional);
    if new_total > MAX_GAS_PER_BLOCK {
        return Err(StfError::BlockGasExceeded {
            used: new_total,
            cap: MAX_GAS_PER_BLOCK,
        });
    }
    Ok(new_total)
}

/// Domain tag for the flat hash of a contract call's inbound payload
/// bytes, carried as the VM message's `body_cell_hash`. The interpreter
/// does not yet read the message — this commits to the delivered bytes so
/// the integration is byte-exact when it does. Multi-cell body chains are
/// future work.
pub const ONX_MSG_BODY_V1: DomainTag = DomainTag::from_ascii("ONX_MSG_BODY_V1");

/// Defensive bound: at most 4 deliveries per external message in a block.
/// Each external yields exactly one internal, which yields at most one
/// bounce; the factor 4 is headroom. Unreachable while contracts cannot
/// emit messages, but the bound fails closed instead of looping forever
/// if that ever changes.
const MAX_DELIVERIES_PER_EXTERNAL: usize = 4;

/// One internal-message delivery: what happened when a message reached
/// its destination (or bounced).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryReceipt {
    /// The internal message's delivery ID (`InternalMessage::id()`).
    pub msg_id: [u8; 32],
    pub src: AccountId,
    pub dest: AccountId,
    pub value_nanos: u128,
    /// True when the message could not be processed and was bounced
    /// (value returned to `src` minus fees, as a new internal message).
    pub bounced: bool,
    /// True when the delivery failed *fatally* (ADR-0037): the message's
    /// gas budget was exhausted (`ExceptionKind::OutOfGas`), so the value
    /// is NOT returned — it is credited to the destination like a plain
    /// transfer, with no contract data update and no bounce queued.
    /// Invariant: at most one of `bounced` / `fatal` is true; both false
    /// means the delivery was processed.
    pub fatal: bool,
    /// TVM gas consumed; 0 for plain value deliveries. Bounce and fatal
    /// receipts report the gas the VM burned before failing (ADR-0037) —
    /// the ADR-0034 block cap must see executed work even when the
    /// delivery's state effects reverted.
    pub gas_used: u64,
}

/// What happened to one external message during block application.
///
/// Recorded for every external message in a successfully applied block, in
/// block order. Receipts are deterministic given `(State, Block)` and are
/// part of what the replay equivalence check compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMessage {
    pub msg_hash: [u8; 32],
    pub sender: AccountId,
    pub nonce: u64,
    pub fee_burned_nanos: u128,
    pub fee_validator_nanos: u128,
    /// One receipt per delivery caused by this message, in delivery order:
    /// first the wallet-emitted internal, then any bounce it triggered.
    pub deliveries: Vec<DeliveryReceipt>,
}

/// The ordered per-message outcomes of [`apply_block`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Receipts(pub Vec<AppliedMessage>);

/// Build the next valid block for a state: the block-producer counterpart
/// to [`apply_block`].
///
/// Validates the chain preconditions (sequence, hash chain, workchain,
/// strictly increasing `lt`), dry-runs the external messages against a
/// scratch copy of the state (wallet phase + delivery phase), and assembles
/// a block whose header commits to the external message set and carries
/// the correct post-state root.
///
/// Producers MUST use this (or an equivalent correct construction) — a
/// block assembled by hand with a wrong `state_root` is simply rejected
/// by `apply_block`.
pub fn propose_block(
    state: &State,
    messages: Vec<ExternalMessage>,
    lt: u64,
    fee_collector: AccountId,
    protocol_version: u32,
    block_time: u64,
) -> Result<Block, StfError> {
    let seqno = state.seqno.checked_add(1).ok_or(StfError::BadSeqno {
        expected: 0,
        got: u32::MAX,
    })?;
    if lt <= state.last_lt {
        return Err(StfError::LogicalTimeRegression {
            last_lt: state.last_lt,
            block_lt: lt,
        });
    }
    let mut scratch = state.tree.clone();
    apply_messages(
        &mut scratch,
        &messages,
        lt,
        state.workchain,
        &state.chain_id,
        &fee_collector,
    )?;
    let state_root = scratch.state_root_hash()?;

    Block::assemble(AssembleParams {
        seqno,
        prev_hash: state.last_hash,
        lt,
        workchain: state.workchain,
        fee_collector,
        messages,
        state_root,
        protocol_version,
        block_time,
    })
}

/// Apply a block to a state: the pure state transition function.
///
/// ```text
/// apply_block(&State, &Block) -> Result<(State, Receipts), StfError>
/// ```
///
/// Validation order (each step fail-closed):
/// 1. `header.seqno` is exactly `state.seqno + 1`.
/// 2. `header.prev_hash` equals the last applied block hash.
/// 3. `header.workchain` matches the state's workchain.
/// 4. `header.lt` is strictly greater than the last applied lt.
/// 5. `header.msg_count` matches the body length, and the recomputed
///    `msgs_root` matches the header commitment.
/// 6. Phase 1: external messages authenticate strictly in body order via
///    the wallet handler; the first invalid message aborts the whole
///    block (an invalid message makes an invalid block — there is no
///    "skip and continue").
/// 7. Phase 2: internal messages deliver FIFO; a re-delivered internal
///    aborts the block (`DoubleDelivery`).
/// 8. The recomputed post-state root must equal `header.state_root`.
///    A block cannot lie about its result.
///
/// On success the returned state's `last_hash` is `header.hash()`,
/// chaining the next block to this one. The `chain_id` carries forward
/// unchanged — it is the genesis hash, fixed for the chain's lifetime.
pub fn apply_block(state: &State, block: &Block) -> Result<(State, Receipts), StfError> {
    let h = &block.header;

    // Version-gated validity (ADR-0032): reject anything we don't understand
    // before any other check — the version field IS the upgrade mechanism.
    if h.protocol_version != PROTOCOL_VERSION {
        return Err(StfError::UnsupportedProtocolVersion {
            got: h.protocol_version,
        });
    }
    let expected_seqno = state.seqno.checked_add(1).ok_or(StfError::BadSeqno {
        expected: 0,
        got: h.seqno,
    })?;
    if h.seqno != expected_seqno {
        return Err(StfError::BadSeqno {
            expected: expected_seqno,
            got: h.seqno,
        });
    }
    if h.prev_hash != state.last_hash {
        return Err(StfError::PrevHashMismatch);
    }
    if h.workchain != state.workchain {
        return Err(StfError::WorkchainMismatch {
            state: state.workchain,
            block: h.workchain,
        });
    }
    if h.lt <= state.last_lt {
        return Err(StfError::LogicalTimeRegression {
            last_lt: state.last_lt,
            block_lt: h.lt,
        });
    }
    if h.msg_count as usize != block.body.messages.len() {
        return Err(StfError::MsgCountMismatch {
            header: h.msg_count,
            body: block.body.messages.len(),
        });
    }
    let actual_msgs_root = msgs_root(&block.body.messages);
    if actual_msgs_root != h.msgs_root {
        return Err(StfError::MsgsRootMismatch {
            expected: h.msgs_root,
            actual: actual_msgs_root,
        });
    }

    let mut tree = state.tree.clone();
    let applied = apply_messages(
        &mut tree,
        &block.body.messages,
        h.lt,
        state.workchain,
        &state.chain_id,
        &h.fee_collector,
    )?;

    let new_root = tree.state_root_hash()?;
    if new_root != h.state_root {
        return Err(StfError::StateRootMismatch {
            expected: h.state_root,
            actual: new_root,
        });
    }

    let new_state = State {
        tree,
        workchain: state.workchain,
        chain_id: state.chain_id,
        seqno: h.seqno,
        last_lt: h.lt,
        last_hash: h.hash(),
    };
    Ok((new_state, Receipts(applied)))
}

/// Apply external messages in two phases, returning per-message receipts.
/// Shared by [`apply_block`] (validator path) and [`propose_block`]
/// (producer dry-run) so both execute byte-identical logic.
fn apply_messages(
    tree: &mut ShardStateTree,
    externals: &[ExternalMessage],
    lt: u64,
    workchain: i32,
    chain_id: &[u8; 32],
    fee_collector: &AccountId,
) -> Result<Vec<AppliedMessage>, StfError> {
    // Phase 1 — wallet: authenticate each external in order, queue one
    // internal message per external. The queue carries the index of the
    // originating external so delivery receipts route to the right entry.
    let mut applied: Vec<AppliedMessage> = Vec::with_capacity(externals.len());
    let mut queue: VecDeque<(InternalMessage, usize)> = VecDeque::new();
    for ext in externals {
        let (internal, receipt) = wallet_receive(tree, ext, lt, chain_id, fee_collector)?;
        let idx = applied.len();
        applied.push(receipt);
        queue.push_back((internal, idx));
    }

    // Phase 2 — delivery: drain the queue FIFO. Bounces are appended at the
    // back in delivery order, so per-(sender, receiver)-pair FIFO holds:
    // messages enter the queue in generation order and leave in FIFO order.
    let mut processed: BTreeSet<[u8; 32]> = BTreeSet::new();
    let max_deliveries = externals.len().saturating_mul(MAX_DELIVERIES_PER_EXTERNAL);
    let mut done = 0usize;
    // ADR-0034: per-block gas cap. Running total of per-delivery `gas_used`;
    // the block is invalid (fail-closed) if the total would exceed
    // MAX_GAS_PER_BLOCK. Checked incrementally so we fail fast, but the rule
    // is on the total: sum(gas_used) <= MAX_GAS_PER_BLOCK.
    let mut block_gas_used: u64 = 0;
    while let Some((msg, idx)) = queue.pop_front() {
        // Bounded by the `TooManyDeliveries` check below: `done` never gets
        // near `usize::MAX`; saturation unreachable.
        done = done.saturating_add(1);
        if done > max_deliveries {
            return Err(StfError::TooManyDeliveries {
                max: max_deliveries,
            });
        }
        let receipt = deliver(
            tree,
            msg,
            lt,
            workchain,
            chain_id,
            &mut queue,
            idx,
            &mut processed,
        )?;
        block_gas_used = accumulate_block_gas(block_gas_used, receipt.gas_used)?;
        applied[idx].deliveries.push(receipt);
    }
    Ok(applied)
}

/// The built-in wallet handler: authenticate one external message.
///
/// This is the auth layer; the VM is the execution layer and is never
/// involved here. On success the sender is debited `amount + fee` (fee
/// split 50/50 burn/validator), the nonce bumps, a revealed key is stored
/// for key-derived accounts, and exactly one internal message is returned
/// for the delivery phase.
///
/// Authorization (checked first, fail-closed): the message's `chain_id`
/// must match the state's; the sender must be `Active`; a keyless sender
/// must reveal a pubkey that derives to its address (ADR-0006); a keyed
/// sender must not reveal one; the nonce must match exactly; the Ed25519
/// signature (over chain-bound body bytes) must verify; the balance must
/// cover `amount + fee`.
///
/// Fail-closed ordering: everything that can fail is validated *before*
/// any account is written, so a rejected message leaves the tree untouched.
fn wallet_receive(
    tree: &mut ShardStateTree,
    ext: &ExternalMessage,
    lt: u64,
    chain_id: &[u8; 32],
    fee_collector: &AccountId,
) -> Result<(InternalMessage, AppliedMessage), StfError> {
    // --- 1. Chain binding ---
    // The signature already covers `chain_id`, so this is belt-and-braces —
    // but it fails fast with a clear error instead of a bare bad signature.
    if ext.chain_id != *chain_id {
        return Err(StfError::WrongChainId {
            expected: *chain_id,
            got: ext.chain_id,
        });
    }

    // --- 2. Kind-specific sanity ---
    match ext.kind {
        MsgKind::Transfer => {
            if ext.amount_nanos == 0 {
                return Err(StfError::ZeroAmount);
            }
            // Non-empty transfer payloads are rejected at parse time.
        }
        MsgKind::ContractCall => {
            // A contract call must buy gas. Zero-fee calls are a
            // sender-side fault: rejected here (fail-closed), not bounced.
            if ext.fee_nanos == 0 {
                return Err(StfError::ZeroFeeContractCall);
            }
        }
    }
    let total_debit = ext
        .amount_nanos
        .checked_add(ext.fee_nanos)
        .ok_or(StfError::FeeArithmeticOverflow)?;

    // --- 3. Sender must be Active (read-only) ---
    let sender_state = tree
        .get(&ext.from)
        .cloned()
        .unwrap_or(AccountState::Uninitialized);
    let (
        sender_balance,
        sender_code,
        sender_data,
        sender_storage_stat,
        stored_pubkey,
        sender_nonce,
    ) = match &sender_state {
        AccountState::Active {
            balance_nanos,
            code,
            data,
            storage_stat,
            pubkey,
            nonce,
            ..
        } => (
            *balance_nanos,
            code.clone(),
            data.clone(),
            *storage_stat,
            *pubkey,
            *nonce,
        ),
        _ => return Err(StfError::SenderNotSpendable(ext.from)),
    };

    // --- 4. Key resolution ---
    // (a) Keyed account: the message must NOT reveal a key (no rotation
    //     this milestone); the signature is verified against the stored key.
    // (b) Keyless account: the message MUST reveal a pubkey that derives to
    //     the account's address (ADR-0006). The reveal is stored, so the
    //     account is keyed from now on. An address that was not derived
    //     from any key can never satisfy this and stays unspendable.
    // The all-zero pubkey is rejected explicitly in both cases: it is an
    // order-4 Ed25519 point, for which a degenerate signature verifies
    // under any message — keylessness is never left to the verifier.
    let effective_pubkey = if stored_pubkey == [0u8; 32] {
        if ext.pubkey == [0u8; 32] {
            return Err(StfError::SenderHasNoKey(ext.from));
        }
        if derive_address(&ext.pubkey) != ext.from {
            return Err(StfError::AddressKeyMismatch { account: ext.from });
        }
        ext.pubkey
    } else {
        if ext.pubkey != [0u8; 32] {
            return Err(StfError::UnexpectedPubkeyReveal(ext.from));
        }
        stored_pubkey
    };

    // --- 5. Nonce, signature, balance, lt (read-only) ---
    if ext.nonce != sender_nonce {
        return Err(StfError::NonceMismatch {
            expected: sender_nonce,
            got: ext.nonce,
        });
    }
    // Genesis validates stored pubkeys; a revealed pubkey that fails point
    // decoding fails closed here (it cannot have signed correctly anyway).
    let pubkey =
        PublicKey::decode_exact(&effective_pubkey).map_err(|_| StfError::InvalidSignature)?;
    ext.verify_signature(&pubkey)?;

    if sender_balance < total_debit {
        return Err(StfError::InsufficientFunds {
            account: ext.from,
            have_nanos: sender_balance,
            need_nanos: total_debit,
        });
    }
    check_lt(&sender_state, lt, ext.from)?;

    // --- 6. Fee collector touch (read-only) ---
    // The collector is only touched when there is a validator fee to
    // credit; zero-fee messages must not create dust accounts. The write
    // phase re-reads the collector post-debit (see below); this pre-check
    // only establishes fail-closed validity before any write happens.
    let (burned, validator_fee) = onx_economics::split_transaction_fee(ext.fee_nanos);
    if validator_fee > 0 {
        let s = tree
            .get(fee_collector)
            .cloned()
            .unwrap_or(AccountState::Uninitialized);
        match &s {
            AccountState::Frozen { .. } | AccountState::Destroyed => {
                return Err(StfError::FeeCollectorNotReceivable(*fee_collector))
            }
            AccountState::Active { .. } | AccountState::Uninitialized => {}
        }
        check_lt(&s, lt, *fee_collector)?;
    }

    // --- 7. Write: debit, nonce++, store revealed key, lt ---
    let sender_after = sender_balance
        .checked_sub(total_debit)
        .ok_or(StfError::BalanceOverflow)?;
    let nonce_after = sender_nonce.checked_add(1).ok_or(StfError::NonceOverflow)?;
    tree.insert(
        ext.from,
        AccountState::Active {
            balance_nanos: sender_after,
            last_trans_lt: lt,
            code: sender_code,
            data: sender_data,
            storage_stat: sender_storage_stat,
            pubkey: effective_pubkey,
            nonce: nonce_after,
        },
    )?;

    // --- 8. Collector credit (re-read post-debit for from == collector) ---
    if validator_fee > 0 {
        let collector_current = tree
            .get(fee_collector)
            .cloned()
            .unwrap_or(AccountState::Uninitialized);
        check_lt(&collector_current, lt, *fee_collector)?;
        credit_account(tree, *fee_collector, &collector_current, validator_fee, lt)?;
    }

    // --- 9. Emit the internal message for the delivery phase ---
    let internal = InternalMessage {
        src: ext.from,
        dest: ext.to,
        value_nanos: ext.amount_nanos,
        fee_nanos: ext.fee_nanos,
        payload: ext.message.clone(),
        is_bounce: false,
        origin: ext.hash(),
    };
    let receipt = AppliedMessage {
        msg_hash: ext.hash(),
        sender: ext.from,
        nonce: ext.nonce,
        fee_burned_nanos: burned,
        fee_validator_nanos: validator_fee,
        deliveries: Vec::new(),
    };
    Ok((internal, receipt))
}

/// Deliver one internal message: execute it as the receiver's own
/// transaction.
///
/// Fail-closed replay check first: an internal message ID already in the
/// per-block `processed` set aborts the block (`DoubleDelivery`).
///
/// Then the destination decides:
/// - `Active` + empty payload → plain value credit.
/// - `Active` + payload + code → TVM execution (gas from the message fee);
///   success credits value and updates contract data, a non-fatal failure
///   bounces, gas exhaustion is fatal (ADR-0037: value credited to the
///   destination, no bounce).
/// - `Active` + payload + no code → bounce.
/// - `Uninitialized` + empty payload → create a keyless `Active` account.
/// - `Uninitialized` + payload → bounce (calls never create accounts).
/// - `Frozen`/`Destroyed` → bounce.
///
/// A bounce queues a new internal message returning the value (fees already
/// taken) to the original sender. A bounce is never itself bounced: if its
/// destination cannot receive, the block fails closed (`BounceUndeliverable`).
/// That case is unreachable in honest operation — the bounce target was an
/// `Active` sender at wallet time and nothing freezes accounts mid-block —
/// so reaching it means state corruption or a dispatch bug, and halting is
/// safer than silently burning funds.
///
/// The delivery's own effects are revert-by-construction: the VM runs pure
/// before any write, and a bounced delivery writes nothing at all.
#[allow(clippy::too_many_arguments)]
fn deliver(
    tree: &mut ShardStateTree,
    msg: InternalMessage,
    lt: u64,
    workchain: i32,
    chain_id: &[u8; 32],
    queue: &mut VecDeque<(InternalMessage, usize)>,
    ext_idx: usize,
    processed: &mut BTreeSet<[u8; 32]>,
) -> Result<DeliveryReceipt, StfError> {
    let id = msg.id();
    if !processed.insert(id) {
        return Err(StfError::DoubleDelivery { msg_id: id });
    }
    let mut receipt = DeliveryReceipt {
        msg_id: id,
        src: msg.src,
        dest: msg.dest,
        value_nanos: msg.value_nanos,
        bounced: false,
        fatal: false,
        gas_used: 0,
    };

    let dest_state = tree
        .get(&msg.dest)
        .cloned()
        .unwrap_or(AccountState::Uninitialized);

    // Decide process vs bounce vs fatal. The VM runs pure here (no tree
    // mutation); its output is applied only on the process/fatal paths below.
    let mut gas_used = 0u64;
    let mut new_data: Option<Cell> = None;
    let mut must_bounce = false;
    let mut is_fatal = false;
    match &dest_state {
        AccountState::Frozen { .. } | AccountState::Destroyed => must_bounce = true,
        AccountState::Uninitialized if !msg.payload.is_empty() => must_bounce = true,
        AccountState::Active { code, .. } if !msg.payload.is_empty() && code.is_none() => {
            must_bounce = true;
        }
        AccountState::Active {
            code: Some(code),
            data,
            ..
        } if !msg.payload.is_empty() => {
            match try_execute_contract(
                code,
                data.as_ref(),
                &msg,
                lt,
                workchain,
                msg.dest,
                tree.contract_cells_mut(),
                chain_id,
            ) {
                ExecOutcome::Success(out) => {
                    gas_used = out.gas_used;
                    new_data = Some(out.new_data);
                }
                ExecOutcome::Bounce { gas } => {
                    gas_used = gas;
                    must_bounce = true;
                }
                ExecOutcome::Fatal { gas } => {
                    gas_used = gas;
                    is_fatal = true;
                }
            }
        }
        _ => {}
    }

    if is_fatal {
        // ADR-0037: the message exhausted its gas budget. The value is NOT
        // returned — it is credited to the destination like a plain
        // transfer (no data update, no bounce queued). A bounce message
        // carries an empty payload and can never reach this arm, so there
        // is no bounce-of-bounce case here.
        debug_assert!(!must_bounce, "fatal and bounce are mutually exclusive");
        check_lt(&dest_state, lt, msg.dest)?;
        credit_account(tree, msg.dest, &dest_state, msg.value_nanos, lt)?;
        receipt.fatal = true;
        receipt.gas_used = gas_used;
        return Ok(receipt);
    }

    if must_bounce {
        // A bounce is never itself bounced — fail closed instead.
        if msg.is_bounce {
            return Err(StfError::BounceUndeliverable { msg_id: id });
        }
        let bounced = InternalMessage {
            src: msg.dest,
            dest: msg.src,
            value_nanos: msg.value_nanos,
            fee_nanos: 0,
            payload: Vec::new(),
            is_bounce: true,
            origin: id,
        };
        queue.push_back((bounced, ext_idx));
        receipt.bounced = true;
        receipt.gas_used = gas_used;
        return Ok(receipt);
    }

    // Receivable: apply the writes. `check_lt` first (fail-closed before
    // mutation), then credit, then the contract data update on the
    // post-credit account so the balance movement is preserved.
    check_lt(&dest_state, lt, msg.dest)?;
    credit_account(tree, msg.dest, &dest_state, msg.value_nanos, lt)?;
    if let Some(data) = &new_data {
        update_contract_data(tree, msg.dest, data, lt)?;
    }
    receipt.gas_used = gas_used;
    Ok(receipt)
}

/// Enforce `lt >= account.last_trans_lt` (`Uninitialized` counts as 0).
fn check_lt(state: &AccountState, lt: u64, account: AccountId) -> Result<(), StfError> {
    let cur = match state {
        AccountState::Active { last_trans_lt, .. } | AccountState::Frozen { last_trans_lt, .. } => {
            *last_trans_lt
        }
        AccountState::Uninitialized | AccountState::Destroyed => 0,
    };
    if lt < cur {
        return Err(StfError::AccountTimeRegression { account });
    }
    Ok(())
}

/// Credit `amount` to an account known to be receivable (`Active` or
/// `Uninitialized`), setting its logical time to `lt`. Returns the new
/// balance.
///
/// Accounts created by receiving are born *keyless* (`pubkey` all zeros,
/// `nonce` 0). They can spend later only via key reveal, and only if their
/// address is key-derived (ADR-0006): the first spend reveals a pubkey
/// that must hash to the address. An address that was not derived from any
/// key can never be spent from.
fn credit_account(
    tree: &mut ShardStateTree,
    id: AccountId,
    current: &AccountState,
    amount: u128,
    lt: u64,
) -> Result<u128, StfError> {
    let (new_balance, code, data, storage_stat, pubkey, nonce) = match current {
        AccountState::Active {
            balance_nanos,
            code,
            data,
            storage_stat,
            pubkey,
            nonce,
            ..
        } => (
            balance_nanos
                .checked_add(amount)
                .ok_or(StfError::BalanceOverflow)?,
            code.clone(),
            data.clone(),
            *storage_stat,
            *pubkey,
            *nonce,
        ),
        AccountState::Uninitialized => (
            amount,
            None,
            None,
            StorageStat {
                cell_count: 0,
                byte_count: 0,
                bit_count: 0,
            },
            [0u8; 32],
            0,
        ),
        // Frozen/Destroyed are bounced by the caller before we get here.
        _ => {
            return Err(StfError::ReceiverNotReceivable(id));
        }
    };
    tree.insert(
        id,
        AccountState::Active {
            balance_nanos: new_balance,
            last_trans_lt: lt,
            code,
            data,
            storage_stat,
            pubkey,
            nonce,
        },
    )?;
    Ok(new_balance)
}

/// Output of a successful contract execution: the contract's new
/// persistent data cell and the gas consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ContractExecOutput {
    new_data: Cell,
    gas_used: u64,
}

/// What a contract execution attempt means for the delivery (ADR-0037).
///
/// - `Success`: apply the value credit and the data update.
/// - `Bounce`: queue a bounce message returning the value to the sender.
/// - `Fatal`: credit the value to the destination (no data update, no
///   bounce) — the message exhausted its gas budget.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExecOutcome {
    Success(ContractExecOutput),
    Bounce { gas: u64 },
    Fatal { gas: u64 },
}

/// The ADR-0037 fatal-vs-bounce taxonomy, as a pure predicate over the
/// closed `ExceptionKind` set. Only `OutOfGas` is fatal: the network spent
/// the full paid budget, so returning the value would price griefing at
/// the fee alone. Every other kind — whether raised by the VM or
/// deliberately by the contract via `THROW` (0x77 maps onto the original
/// four kinds; `CallStackOverflow` is VM-raised only, ADR-0039) — bounces:
/// an early, cheap failure or a deliberate rejection, and the sender is
/// refunded.
///
/// `reference/vectors/fatal_bounce.json` pins this table; the agreement
/// test asserts it covers the closed set exhaustively.
///
/// The match is explicit (no wildcard): adding a new `ExceptionKind`
/// fails compilation here until its outcome is decided.
pub fn is_fatal_exception(kind: &ExceptionKind) -> bool {
    match kind {
        ExceptionKind::OutOfGas => true,
        ExceptionKind::IntegerOverflow
        | ExceptionKind::AbsentNode
        | ExceptionKind::MalformedCell
        | ExceptionKind::TypeMismatch
        | ExceptionKind::CallStackOverflow => false,
    }
}

/// Execute a contract call against the recipient's code and data.
///
/// Pure: reads only the already-fetched code/data and the tree's persisted
/// contract cell DAGs, never touches the tree's accounts. Returns the
/// ADR-0037 delivery outcome: `Success` on clean execution with no
/// out-messages; `Bounce` on a non-fatal TVM exception or an out-message
/// egress attempt (deliberately unwired this milestone — the message would
/// otherwise be silently dropped, so the value bounces instead); `Fatal`
/// when the execution exhausted its gas budget (`OutOfGas`).
///
/// Calling convention (documented, deterministic):
/// - The contract's persistent data cell is pushed on the operand stack at
///   entry. (The interpreter has no c4-push opcode yet; the STF seeds the
///   stack instead.)
/// - The interpreter's cell store is seeded with the account's persisted
///   code/data DAGs, so `CTOS`/`LDREF` resolve to the actual stored
///   children (wave-2 VM contract). After execution the drained store is
///   rebuilt into fresh DAGs and written back to `contract_cells`.
/// - `interp.message_body` is set from the internal message's payload, so
///   the `MSGBODY` (0x82) opcode exposes the real inbound body.
/// - `SETDATA` (0x4D) installs the new persistent data cell; on halt, the
///   interpreter's data is the contract's new state.
/// - `ExecutionContext.gen_utime` is derived from the block lt — the VM
///   never sees wall-clock time.
/// - Gas limit is `min(fee_nanos * GAS_PER_NANO, MAX_GAS_PER_MESSAGE)`
///   (ADR-0034; saturating at `u64::MAX` before the cap).
#[allow(clippy::too_many_arguments)]
fn try_execute_contract(
    code: &Cell,
    data: Option<&Cell>,
    msg: &InternalMessage,
    lt: u64,
    workchain: i32,
    account_id: AccountId,
    contract_cells: &mut BTreeMap<AccountId, ContractCellDags>,
    chain_id: &[u8; 32],
) -> ExecOutcome {
    let data_cell = data
        .cloned()
        .unwrap_or_else(|| Cell::new(vec![], vec![]).expect("empty cell is valid"));

    // ADR-0034: per-message gas cap. The fee still buys gas at
    // GAS_PER_NANO, but no single message may exceed MAX_GAS_PER_MESSAGE.
    // (Fee mechanics unchanged: the full fee_nanos is debited regardless.)
    let gas_limit = message_gas_limit(msg.fee_nanos);
    // gen_utime is NOT wall-clock: it is the block's logical time,
    // saturated into u32. Feeding real time here would break determinism.
    let context = ExecutionContext {
        gen_utime: u32::try_from(lt).unwrap_or(u32::MAX),
        start_lt: lt,
        end_lt: lt,
        gas_limit,
        // ADR-0038: the VM's chain identity comes from state, never from
        // the message or local config — CHKSIGNU's chain-bound tag is only
        // as trustworthy as this value.
        chain_id: *chain_id,
    };
    let message = inbound_message(msg, lt, workchain);

    let mut interp = Interpreter::new(code.clone(), data_cell.clone(), message, context);
    // Seed the interpreter's cell store with the account's persisted
    // code/data DAGs. The constructor only seeds the two roots; without
    // the rest of the DAGs, `LDREF` on their children would fail closed
    // with `AbsentNode` even though the content exists.
    if let Some(dags) = contract_cells.get(&account_id) {
        for (hash, cell) in dags.code.cells().iter().chain(dags.data.cells().iter()) {
            interp.cell_store.insert(*hash, cell.clone());
        }
    }
    // The `Message` only commits to `body_cell_hash`; the host holds the
    // actual payload and sets it here so `MSGBODY` exposes real data.
    interp.message_body = msg.payload.clone();
    interp.stack.push(StackValue::Cell(data_cell));
    match interp.run() {
        ExecutionResult::Success {
            new_data,
            out_messages,
            gas_used,
        } => {
            if out_messages.is_empty() {
                // Persist the drained cell store: rebuild the account's
                // code and data DAGs (full DAG *content*, not just root
                // hashes) as Bags-of-Cells, collected by reachability from
                // the roots through the drained store.
                let drained = std::mem::take(&mut interp.cell_store);
                // A missing DAG root is an internal invariant violation
                // (the root was just seeded or materialized). Fail safe:
                // bounce the delivery rather than applying a half-built
                // state — same as the old `None` path.
                let (code_boc, data_boc) = match (
                    dag_boc(&drained, code.hash()),
                    dag_boc(&drained, new_data.hash()),
                ) {
                    (Some(c), Some(d)) => (c, d),
                    _ => return ExecOutcome::Bounce { gas: gas_used },
                };
                contract_cells.insert(
                    account_id,
                    ContractCellDags {
                        code: code_boc,
                        data: data_boc,
                    },
                );
                ExecOutcome::Success(ContractExecOutput { new_data, gas_used })
            } else {
                ExecOutcome::Bounce { gas: gas_used }
            }
        }
        ExecutionResult::Exception { kind, gas_used } => {
            if is_fatal_exception(&kind) {
                ExecOutcome::Fatal { gas: gas_used }
            } else {
                ExecOutcome::Bounce { gas: gas_used }
            }
        }
    }
}

/// Collect the DAG reachable from `root` through `store` into a
/// [`BagOfCells`]. The root itself must be present (it was just seeded or
/// materialized — a missing root is an internal invariant violation and
/// fails closed). Child references whose content is absent are skipped:
/// the interpreter already failed closed on any it actually needed, and
/// `BagOfCells` tolerates dangling references (the wave-1 Merkle-proof
/// carve-out).
fn dag_boc(store: &BTreeMap<[u8; 32], Cell>, root: [u8; 32]) -> Option<BagOfCells> {
    if !store.contains_key(&root) {
        return None;
    }
    let mut cells = BTreeMap::new();
    let mut stack = vec![root];
    while let Some(hash) = stack.pop() {
        if cells.contains_key(&hash) {
            continue;
        }
        if let Some(cell) = store.get(&hash) {
            for child in cell.cell_refs() {
                stack.push(*child);
            }
            cells.insert(hash, cell.clone());
        }
    }
    BagOfCells::new(root, cells).ok()
}

/// Build the inbound `Message` delivered to the contract. The interpreter
/// does not yet read it, but it is part of the deterministic execution
/// input and is committed to via `body_cell_hash`.
fn inbound_message(msg: &InternalMessage, lt: u64, workchain: i32) -> Message {
    use onx_primitives::{Int32, Uint128, Uint256, Uint64};
    Message {
        msg_type: MessageType::Internal,
        src_address: FullAddress::new(WorkchainIdent(Int32(workchain)), msg.src),
        dest_address: FullAddress::new(WorkchainIdent(Int32(workchain)), msg.dest),
        amount_nanos: Uint128(msg.value_nanos),
        extra_currencies: Vec::new(),
        created_lt: Uint64(lt),
        body_cell_hash: Uint256(domain_hash(&ONX_MSG_BODY_V1, &msg.payload)),
    }
}

/// Write the contract's new persistent data cell after successful
/// execution, preserving balance, code, and all other account fields.
/// Recomputes `storage_stat` from the embedded cells.
fn update_contract_data(
    tree: &mut ShardStateTree,
    id: AccountId,
    new_data: &Cell,
    lt: u64,
) -> Result<(), StfError> {
    let current = tree
        .get(&id)
        .cloned()
        .unwrap_or(AccountState::Uninitialized);
    match current {
        AccountState::Active {
            balance_nanos,
            code,
            pubkey,
            nonce,
            ..
        } => {
            let new_stat = storage_stat_for(code.as_ref(), Some(new_data));
            tree.insert(
                id,
                AccountState::Active {
                    balance_nanos,
                    last_trans_lt: lt,
                    code,
                    data: Some(new_data.clone()),
                    storage_stat: new_stat,
                    pubkey,
                    nonce,
                },
            )?;
            Ok(())
        }
        // The caller only invokes this on the process path, where the
        // destination was just credited — so it is always Active here.
        // Fail closed anyway.
        _ => Err(StfError::ReceiverNotReceivable(id)),
    }
}

/// Storage accounting for embedded contract cells: counts the cells, their
/// canonical byte sizes, and their precise bit lengths (ADR-0037; bit
/// granularity matters after ADR-0036). Deterministic; recomputed whenever
/// code or data changes. This is the single writer of `StorageStat` on the
/// state-transition path — `AccountState::from_bytes` derives the same
/// `bit_count` from the decoded cells, so the two always agree.
fn storage_stat_for(code: Option<&Cell>, data: Option<&Cell>) -> StorageStat {
    let mut cell_count = 0u32;
    let mut byte_count = 0u64;
    let mut bit_count = 0u64;
    for cell in [code, data].into_iter().flatten() {
        // At most two cells, each a few hundred bytes: saturation unreachable.
        cell_count = cell_count.saturating_add(1);
        byte_count = byte_count.saturating_add(cell.to_bytes().len() as u64);
        bit_count = bit_count.saturating_add(cell.bit_len() as u64);
    }
    StorageStat {
        cell_count,
        byte_count,
        bit_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onx_primitives::SecretKey;

    fn test_secret() -> SecretKey {
        SecretKey::from_seed(&[0x42; 32]).unwrap()
    }

    fn funded_state() -> (State, SecretKey, AccountId, AccountId) {
        use onx_data_structures::{ShardIdent, WorkchainIdent};
        use onx_state_model::{GenesisDocument, GenesisValidator};
        use std::collections::BTreeMap;

        let secret = test_secret();
        let sender = AccountId::from_bytes([0x11; 32]);
        let receiver = AccountId::from_bytes([0x22; 32]);
        let mut accounts = BTreeMap::new();
        for (id, balance) in [(sender, 10_000_000u128), (receiver, 0u128)] {
            let pubkey = if id == sender {
                secret.public_key().encode()
            } else {
                [0u8; 32]
            };
            accounts.insert(
                id,
                AccountState::Active {
                    balance_nanos: balance,
                    last_trans_lt: 0,
                    code: None,
                    data: None,
                    storage_stat: StorageStat {
                        cell_count: 0,
                        byte_count: 0,
                        bit_count: 0,
                    },
                    pubkey,
                    nonce: 0,
                },
            );
        }
        let doc = GenesisDocument::new(
            WorkchainIdent::new(0),
            ShardIdent::root(WorkchainIdent::new(0)),
            vec![GenesisValidator {
                pubkey: [7u8; 32],
                stake: 1_000_000,
            }],
            accounts,
        )
        .unwrap();
        let state = State::from_genesis(&doc);
        (state, secret, sender, receiver)
    }

    #[test]
    fn redelivered_internal_message_is_rejected() {
        let (state, secret, sender, receiver) = funded_state();
        let mut tree = state.tree.clone();
        let collector = AccountId::from_bytes([0xCC; 32]);

        let ext = ExternalMessage::new_signed(
            state.chain_id,
            MsgKind::Transfer,
            sender,
            0,
            receiver,
            1_000,
            10,
            Vec::new(),
            [0u8; 32],
            &secret,
        );
        let (internal, _) =
            wallet_receive(&mut tree, &ext, 1, &state.chain_id, &collector).unwrap();

        let mut queue = VecDeque::new();
        let mut processed = BTreeSet::new();
        // First delivery succeeds.
        let receipt = deliver(
            &mut tree,
            internal.clone(),
            1,
            0,
            &state.chain_id,
            &mut queue,
            0,
            &mut processed,
        )
        .unwrap();
        assert!(!receipt.bounced);
        assert_eq!(
            tree.get(&receiver).unwrap().balance_nanos(),
            1_000,
            "value credited once"
        );
        // Redelivery of the same internal message is rejected — no double
        // delivery, no double spend.
        let err = deliver(
            &mut tree,
            internal,
            1,
            0,
            &state.chain_id,
            &mut queue,
            0,
            &mut processed,
        )
        .unwrap_err();
        assert!(
            matches!(err, StfError::DoubleDelivery { .. }),
            "expected DoubleDelivery, got {err:?}"
        );
        assert_eq!(
            tree.get(&receiver).unwrap().balance_nanos(),
            1_000,
            "no double credit"
        );
    }

    #[test]
    fn bounce_is_queued_and_deliverable() {
        let (state, secret, sender, _) = funded_state();
        let mut tree = state.tree.clone();
        let collector = AccountId::from_bytes([0xCC; 32]);
        // Freeze the receiver by replacing its state.
        let frozen = AccountId::from_bytes([0x33; 32]);
        tree.insert(
            frozen,
            AccountState::Frozen {
                balance_nanos: 5_000,
                last_trans_lt: 0,
                storage_hash: [0u8; 32],
            },
        )
        .unwrap();

        let ext = ExternalMessage::new_signed(
            state.chain_id,
            MsgKind::Transfer,
            sender,
            0,
            frozen,
            1_000,
            100,
            Vec::new(),
            [0u8; 32],
            &secret,
        );
        let (internal, _) =
            wallet_receive(&mut tree, &ext, 1, &state.chain_id, &collector).unwrap();

        let mut queue = VecDeque::new();
        let mut processed = BTreeSet::new();
        // Delivery to the frozen account bounces.
        let receipt = deliver(
            &mut tree,
            internal,
            1,
            0,
            &state.chain_id,
            &mut queue,
            0,
            &mut processed,
        )
        .unwrap();
        assert!(receipt.bounced);
        assert_eq!(queue.len(), 1, "bounce queued");
        // The bounce delivers value back to the sender.
        let (bounced, _) = queue.pop_front().unwrap();
        assert!(bounced.is_bounce);
        assert_eq!(bounced.dest, sender);
        assert_eq!(bounced.value_nanos, 1_000);
        let receipt2 = deliver(
            &mut tree,
            bounced,
            1,
            0,
            &state.chain_id,
            &mut queue,
            0,
            &mut processed,
        )
        .unwrap();
        assert!(!receipt2.bounced);
        // Sender: 10_000_000 - 1_000 (value) - 100 (fee) + 1_000 (bounce) = 9_999_900.
        // Fee split: 100 -> 50 burned, 50 to collector.
        assert_eq!(tree.get(&sender).unwrap().balance_nanos(), 9_999_900);
    }

    #[test]
    fn apply_block_rejects_wrong_protocol_version() {
        let (state, _secret, _sender, _receiver) = funded_state();
        let collector = AccountId::from_bytes([0xCC; 32]);
        let block = propose_block(&state, vec![], 1, collector, PROTOCOL_VERSION, 0).unwrap();
        // Sanity: the honest block applies.
        assert!(apply_block(&state, &block).is_ok());
        // Tamper the version: the STF must fail closed.
        let mut bad = block.clone();
        bad.header.protocol_version = PROTOCOL_VERSION + 1;
        let err = apply_block(&state, &bad).unwrap_err();
        assert!(
            matches!(
                err,
                StfError::UnsupportedProtocolVersion { got } if got == PROTOCOL_VERSION + 1
            ),
            "expected UnsupportedProtocolVersion, got: {err:?}"
        );
    }

    // ADR-0034 gas caps.

    #[test]
    fn gas_cap_constants_are_as_specified() {
        assert_eq!(MAX_GAS_PER_MESSAGE, 10_000_000);
        assert_eq!(MAX_GAS_PER_BLOCK, 100_000_000);
        // Block cap is an exact multiple of the message cap (10 max-gas
        // messages fit in a block).
        assert_eq!(MAX_GAS_PER_BLOCK, MAX_GAS_PER_MESSAGE * 10);
    }

    #[test]
    fn message_gas_limit_clamps_at_per_message_cap() {
        // Below the cap: fee * GAS_PER_NANO passes through.
        assert_eq!(message_gas_limit(0), 0);
        assert_eq!(message_gas_limit(1), GAS_PER_NANO);
        assert_eq!(message_gas_limit(5_000), 5_000 * GAS_PER_NANO);
        // At the cap boundary: 10_000 nanos * 1_000 = 10_000_000 = cap.
        assert_eq!(message_gas_limit(10_000), MAX_GAS_PER_MESSAGE);
        // Above the cap: clamped.
        assert_eq!(message_gas_limit(10_001), MAX_GAS_PER_MESSAGE);
        assert_eq!(message_gas_limit(100_000), MAX_GAS_PER_MESSAGE);
        assert_eq!(message_gas_limit(1_000_000_000), MAX_GAS_PER_MESSAGE);
        // Saturation: u128::MAX fee saturates the multiply, then clamps.
        assert_eq!(message_gas_limit(u128::MAX), MAX_GAS_PER_MESSAGE);
    }

    #[test]
    fn accumulate_block_gas_accepts_up_to_cap() {
        // Empty block: zero gas is fine.
        assert_eq!(accumulate_block_gas(0, 0).unwrap(), 0);
        // Normal accumulation.
        assert_eq!(accumulate_block_gas(0, 1_000).unwrap(), 1_000);
        assert_eq!(accumulate_block_gas(1_000, 2_000).unwrap(), 3_000);
        // Exactly at the cap: valid.
        assert_eq!(
            accumulate_block_gas(MAX_GAS_PER_BLOCK - 1, 1).unwrap(),
            MAX_GAS_PER_BLOCK
        );
        assert_eq!(
            accumulate_block_gas(MAX_GAS_PER_BLOCK, 0).unwrap(),
            MAX_GAS_PER_BLOCK
        );
    }

    #[test]
    fn accumulate_block_gas_rejects_over_cap() {
        // One unit over the cap: invalid.
        let err = accumulate_block_gas(MAX_GAS_PER_BLOCK, 1).unwrap_err();
        assert!(
            matches!(
                err,
                StfError::BlockGasExceeded { used, cap }
                if used == MAX_GAS_PER_BLOCK + 1 && cap == MAX_GAS_PER_BLOCK
            ),
            "expected BlockGasExceeded, got: {err:?}"
        );
        // Large overshoot.
        let err = accumulate_block_gas(0, MAX_GAS_PER_BLOCK + 1).unwrap_err();
        assert!(
            matches!(err, StfError::BlockGasExceeded { .. }),
            "expected BlockGasExceeded, got: {err:?}"
        );
        // Saturation path: u64::MAX total is still > cap, still rejected.
        let err = accumulate_block_gas(u64::MAX, u64::MAX).unwrap_err();
        assert!(
            matches!(err, StfError::BlockGasExceeded { .. }),
            "expected BlockGasExceeded on saturation, got: {err:?}"
        );
    }

    #[test]
    fn block_gas_error_displays() {
        let err = StfError::BlockGasExceeded {
            used: 101,
            cap: 100,
        };
        let s = format!("{err}");
        assert!(s.contains("101"), "display should name used: {s}");
        assert!(s.contains("100"), "display should name cap: {s}");
    }

    #[test]
    fn storage_stat_for_agrees_with_python() {
        // ADR-0037: storage_stat_for must agree with the independent Python
        // reference (reference/gen_storage_vectors.py,
        // reference/vectors/storage_stat.json), generated from the spec
        // text. Fixtures are rebuilt via Cell::new_with_bit_len from the
        // recorded (data_hex, bit_len, refs_hex) inputs.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../reference/vectors/storage_stat.json"
        );
        let text = std::fs::read_to_string(path).expect("storage_stat.json must exist");
        let v: serde_json::Value =
            serde_json::from_str(&text).expect("storage_stat.json must parse");
        assert_eq!(v["adr"].as_str(), Some("ADR-0037"));
        for case in v["cases"].as_array().expect("cases array") {
            let name = case["name"].as_str().expect("name");
            let mut cells: Vec<Cell> = Vec::new();
            for fc in case["cells"].as_array().expect("cells array") {
                let data =
                    hex::decode(fc["data_hex"].as_str().expect("data_hex")).expect("valid hex");
                let bit_len = fc["bit_len"].as_u64().expect("bit_len") as usize;
                let refs: Vec<[u8; 32]> = fc["refs_hex"]
                    .as_array()
                    .expect("refs_hex")
                    .iter()
                    .map(|r| {
                        let b = hex::decode(r.as_str().expect("hex")).expect("valid hex");
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(&b);
                        arr
                    })
                    .collect();
                cells.push(
                    Cell::new_with_bit_len(data, bit_len, refs)
                        .unwrap_or_else(|e| panic!("fixture {name} rebuild failed: {e:?}")),
                );
            }
            let (code, data) = match cells.as_slice() {
                [] => (None, None),
                [c] => (Some(c), None),
                [c, d] => (Some(c), Some(d)),
                _ => panic!("fixture {name} has more than 2 cells"),
            };
            let stat = storage_stat_for(code, data);
            let exp = &case["stat"];
            assert_eq!(
                stat.cell_count,
                exp["cell_count"].as_u64().expect("cell_count") as u32,
                "{name}: cell_count"
            );
            assert_eq!(
                stat.byte_count,
                exp["byte_count"].as_u64().expect("byte_count"),
                "{name}: byte_count"
            );
            assert_eq!(
                stat.bit_count,
                exp["bit_count"].as_u64().expect("bit_count"),
                "{name}: bit_count"
            );
        }
    }
}
