use crate::block::{txs_root, Block, Transaction};
use crate::error::StfError;
use crate::state::State;
use onx_data_structures::AccountId;
use onx_state_model::{AccountState, ShardStateTree, StorageStat};

/// What happened to one transaction during block application.
///
/// Recorded for every transaction in a successfully applied block, in
/// block order. Receipts are deterministic given `(State, Block)` and are
/// part of what Phase 5's equivalence check compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedTx {
    pub tx_hash: [u8; 32],
    pub sender: AccountId,
    pub receiver: AccountId,
    pub amount_nanos: u128,
    pub fee_burned_nanos: u128,
    pub fee_validator_nanos: u128,
    pub sender_balance_after: u128,
    pub receiver_balance_after: u128,
}

/// The ordered per-transaction outcomes of [`apply_block`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Receipts(pub Vec<AppliedTx>);

/// Build the next valid block for a state: the block-producer counterpart
/// to [`apply_block`].
///
/// Validates the chain preconditions (sequence, hash chain, workchain,
/// strictly increasing `lt`), dry-runs the transactions against a scratch
/// copy of the state, and assembles a block whose header commits to the
/// transaction set and carries the correct post-state root.
///
/// Producers MUST use this (or an equivalent correct construction) — a
/// block assembled by hand with a wrong `state_root` is simply rejected
/// by `apply_block`.
pub fn propose_block(
    state: &State,
    transactions: Vec<Transaction>,
    lt: u64,
    fee_collector: AccountId,
) -> Result<Block, StfError> {
    use crate::block::Block;

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
    apply_txs(&mut scratch, &transactions, lt, &fee_collector)?;
    let state_root = scratch.state_root_hash()?;

    Ok(Block::assemble(
        seqno,
        state.last_hash,
        lt,
        state.workchain,
        fee_collector,
        transactions,
        state_root,
    ))
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
/// 5. `header.tx_count` matches the body length, and the recomputed
///    `txs_root` matches the header commitment.
/// 6. Transactions apply strictly in body order; the first invalid
///    transaction aborts the whole block (an invalid transaction makes
///    an invalid block — there is no "skip and continue").
/// 7. The recomputed post-state root must equal `header.state_root`.
///    A block cannot lie about its result.
///
/// On success the returned state's `last_hash` is `header.hash()`,
/// chaining the next block to this one.
pub fn apply_block(state: &State, block: &Block) -> Result<(State, Receipts), StfError> {
    let h = &block.header;

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
    if h.tx_count as usize != block.body.transactions.len() {
        return Err(StfError::TxCountMismatch {
            header: h.tx_count,
            body: block.body.transactions.len(),
        });
    }
    let actual_txs_root = txs_root(&block.body.transactions);
    if actual_txs_root != h.txs_root {
        return Err(StfError::TxsRootMismatch {
            expected: h.txs_root,
            actual: actual_txs_root,
        });
    }

    let mut tree = state.tree.clone();
    let applied = apply_txs(&mut tree, &block.body.transactions, h.lt, &h.fee_collector)?;

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
        seqno: h.seqno,
        last_lt: h.lt,
        last_hash: h.hash(),
    };
    Ok((new_state, Receipts(applied)))
}

/// Apply transactions in order to a tree, returning per-transaction
/// receipts. Shared by [`apply_block`] (validator path) and
/// [`propose_block`] (producer dry-run) so both execute byte-identical logic.
fn apply_txs(
    tree: &mut ShardStateTree,
    transactions: &[Transaction],
    lt: u64,
    fee_collector: &AccountId,
) -> Result<Vec<AppliedTx>, StfError> {
    let mut applied = Vec::with_capacity(transactions.len());
    for tx in transactions {
        applied.push(apply_tx(tree, tx, lt, fee_collector)?);
    }
    Ok(applied)
}

/// Apply one transaction to the tree.
///
/// Logical-time rule: the touching transaction's block `lt` must satisfy
/// `lt >= account.last_trans_lt`. Equality is allowed *within* a block
/// because intra-block ordering is total (transaction index). Cross-block
/// strictness comes from `apply_block`'s rule 4 (`block.lt > last_lt`), so
/// the invariant "every account's lt <= last applied block lt" holds
/// inductively from genesis (genesis accounts start at lt 0).
///
/// This deliberately deviates from `AccountState::validate_transition`'s
/// strict `new_lt > cur_lt`: that rule would make a second touch of the
/// same account within one block impossible. The STF owns this check
/// instead, and documents it here.
///
/// Fail-closed ordering: everything that can fail is validated *before*
/// any account is written, so a rejected transaction leaves the tree
/// untouched.
fn apply_tx(
    tree: &mut ShardStateTree,
    tx: &Transaction,
    lt: u64,
    fee_collector: &AccountId,
) -> Result<AppliedTx, StfError> {
    if tx.amount_nanos == 0 {
        return Err(StfError::ZeroAmount);
    }
    let total_debit = tx
        .amount_nanos
        .checked_add(tx.fee_nanos)
        .ok_or(StfError::FeeArithmeticOverflow)?;

    // --- Validate sender (read-only) ---
    let sender_state = tree
        .get(&tx.from)
        .cloned()
        .unwrap_or(AccountState::Uninitialized);
    let (sender_balance, sender_code_hash, sender_data_hash, sender_storage_stat) =
        match &sender_state {
            AccountState::Active {
                balance_nanos,
                code_hash,
                data_hash,
                storage_stat,
                ..
            } => (*balance_nanos, *code_hash, *data_hash, *storage_stat),
            _ => return Err(StfError::SenderNotSpendable(tx.from)),
        };
    if sender_balance < total_debit {
        return Err(StfError::InsufficientFunds {
            account: tx.from,
            have_nanos: sender_balance,
            need_nanos: total_debit,
        });
    }
    check_lt(&sender_state, lt, tx.from)?;

    // --- Validate receiver (read-only) ---
    let receiver_state = tree
        .get(&tx.to)
        .cloned()
        .unwrap_or(AccountState::Uninitialized);
    match &receiver_state {
        AccountState::Frozen { .. } | AccountState::Destroyed => {
            return Err(StfError::ReceiverNotReceivable(tx.to))
        }
        AccountState::Active { .. } | AccountState::Uninitialized => {}
    }
    check_lt(&receiver_state, lt, tx.to)?;

    // --- Validate fee collector touch (read-only) ---
    // The collector is only touched when there is a validator fee to credit;
    // zero-fee transactions must not create dust accounts. The write phase
    // re-reads the collector post-debit (see below); this pre-check only
    // establishes fail-closed validity before any write happens.
    let (burned, validator_fee) = onx_economics::split_transaction_fee(tx.fee_nanos);
    if validator_fee > 0 {
        let s = tree
            .get(fee_collector)
            .cloned()
            .unwrap_or(AccountState::Uninitialized);
        match &s {
            AccountState::Frozen { .. } | AccountState::Destroyed => {
                return Err(StfError::ReceiverNotReceivable(*fee_collector))
            }
            AccountState::Active { .. } | AccountState::Uninitialized => {}
        }
        check_lt(&s, lt, *fee_collector)?;
    }

    // --- All checks passed: write in a fixed order ---
    // Order: sender debit, receiver credit, collector credit. The receiver
    // and collector are RE-READ after the sender debit: when from == to
    // (or the collector is the sender/receiver), the debit must be visible
    // to the later writes. Reusing the pre-debit copies would clobber the
    // debit — sequential writes are only correct if each write sees the
    // previous ones.
    let sender_after = sender_balance
        .checked_sub(total_debit)
        .ok_or(StfError::BalanceOverflow)?;
    tree.insert(
        tx.from,
        AccountState::Active {
            balance_nanos: sender_after,
            last_trans_lt: lt,
            code_hash: sender_code_hash,
            data_hash: sender_data_hash,
            storage_stat: sender_storage_stat,
        },
    );

    let receiver_current = tree
        .get(&tx.to)
        .cloned()
        .unwrap_or(AccountState::Uninitialized);
    // Type cannot have changed on debit (only balance/lt move), but the lt
    // check is re-run against the fresh read for the from == to case.
    check_lt(&receiver_current, lt, tx.to)?;
    let receiver_after = credit_account(tree, tx.to, &receiver_current, tx.amount_nanos, lt)?;

    if validator_fee > 0 {
        let collector_current = tree
            .get(fee_collector)
            .cloned()
            .unwrap_or(AccountState::Uninitialized);
        check_lt(&collector_current, lt, *fee_collector)?;
        credit_account(tree, *fee_collector, &collector_current, validator_fee, lt)?;
    }

    Ok(AppliedTx {
        tx_hash: tx.hash(),
        sender: tx.from,
        receiver: tx.to,
        amount_nanos: tx.amount_nanos,
        fee_burned_nanos: burned,
        fee_validator_nanos: validator_fee,
        sender_balance_after: sender_after,
        receiver_balance_after: receiver_after,
    })
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

/// Credit `amount` to an account known to be `Active` or `Uninitialized`,
/// setting its logical time to `lt`. Returns the new balance.
fn credit_account(
    tree: &mut ShardStateTree,
    id: AccountId,
    current: &AccountState,
    amount: u128,
    lt: u64,
) -> Result<u128, StfError> {
    let (new_balance, code_hash, data_hash, storage_stat) = match current {
        AccountState::Active {
            balance_nanos,
            code_hash,
            data_hash,
            storage_stat,
            ..
        } => (
            balance_nanos
                .checked_add(amount)
                .ok_or(StfError::BalanceOverflow)?,
            *code_hash,
            *data_hash,
            *storage_stat,
        ),
        AccountState::Uninitialized => (
            amount,
            [0u8; 32],
            [0u8; 32],
            StorageStat {
                cell_count: 0,
                byte_count: 0,
            },
        ),
        // Frozen/Destroyed are rejected by the caller before we get here.
        _ => {
            return Err(StfError::ReceiverNotReceivable(id));
        }
    };
    tree.insert(
        id,
        AccountState::Active {
            balance_nanos: new_balance,
            last_trans_lt: lt,
            code_hash,
            data_hash,
            storage_stat,
        },
    );
    Ok(new_balance)
}
