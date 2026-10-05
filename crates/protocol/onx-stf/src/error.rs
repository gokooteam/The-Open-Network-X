use onx_data_structures::AccountId;
use onx_state_model::StateModelError;
use std::fmt;

/// Errors from block validation and state transition application.
///
/// Every variant is deterministic: the same `(State, Block)` input always
/// yields the same error. No variant carries I/O, time, or randomness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StfError {
    /// Block sequence number is not exactly one more than the last applied.
    BadSeqno { expected: u32, got: u32 },
    /// `header.prev_hash` does not match the last applied block hash.
    PrevHashMismatch,
    /// Block workchain does not match the state's workchain.
    WorkchainMismatch { state: i32, block: i32 },
    /// Block logical time is not strictly greater than the last applied lt.
    LogicalTimeRegression { last_lt: u64, block_lt: u64 },
    /// Recomputed transaction-set hash does not match the header commitment.
    TxsRootMismatch {
        expected: [u8; 32],
        actual: [u8; 32],
    },
    /// Recomputed post-state root does not match the header's claimed root.
    StateRootMismatch {
        expected: [u8; 32],
        actual: [u8; 32],
    },
    /// Transaction amount is zero. Zero-value transfers are rejected as noise.
    ZeroAmount,
    /// Transaction fee plus amount overflowed u128 when summed.
    FeeArithmeticOverflow,
    /// Sender account does not exist or is not in a spendable state.
    /// Only `Active` accounts can send.
    SenderNotSpendable(AccountId),
    /// Sender balance cannot cover `amount + fee`.
    InsufficientFunds {
        account: AccountId,
        have_nanos: u128,
        need_nanos: u128,
    },
    /// Receiver is `Frozen` or `Destroyed`. (`Uninitialized` receivers are
    /// created as fresh `Active` accounts.)
    ReceiverNotReceivable(AccountId),
    /// Balance arithmetic overflowed (practically unreachable given the
    /// supply cap, but checked anyway — silent wrapping is never acceptable
    /// in consensus code).
    BalanceOverflow,
    /// A transaction tried to move an account's logical time backwards
    /// within a block, which cannot happen for honestly constructed blocks.
    /// (Equality with the block lt is allowed: intra-block ordering is by
    /// transaction index.)
    AccountTimeRegression { account: AccountId },
    /// Transaction bytes are not the canonical length.
    MalformedTransaction { expected_len: usize, got_len: usize },
    /// Block header bytes are not the canonical length.
    MalformedHeader { expected_len: usize, got_len: usize },
    /// Header `tx_count` does not match the number of body transactions.
    TxCountMismatch { header: u32, body: usize },
    /// State trie construction failed while computing a root hash
    /// (fail-closed: the error propagates, never a silent constant —
    /// Phase 0 bug 4). Deterministic given the same input state.
    StateTrie(StateModelError),
}

impl fmt::Display for StfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadSeqno { expected, got } => {
                write!(f, "bad seqno: expected {expected}, got {got}")
            }
            Self::PrevHashMismatch => write!(f, "prev_hash does not match last applied block hash"),
            Self::WorkchainMismatch { state, block } => {
                write!(f, "workchain mismatch: state {state}, block {block}")
            }
            Self::LogicalTimeRegression { last_lt, block_lt } => write!(
                f,
                "logical time regression: block lt {block_lt} <= last lt {last_lt}"
            ),
            Self::TxsRootMismatch { .. } => {
                write!(f, "txs_root mismatch: header does not commit to this body")
            }
            Self::StateRootMismatch { .. } => {
                write!(
                    f,
                    "state root mismatch: header claims a different post-state"
                )
            }
            Self::ZeroAmount => write!(f, "zero-amount transfer rejected"),
            Self::FeeArithmeticOverflow => write!(f, "amount + fee overflowed u128"),
            Self::SenderNotSpendable(a) => {
                write!(f, "sender {a:?} is not an Active account")
            }
            Self::InsufficientFunds {
                have_nanos,
                need_nanos,
                ..
            } => write!(
                f,
                "insufficient funds: have {have_nanos} nanos, need {need_nanos}"
            ),
            Self::ReceiverNotReceivable(a) => {
                write!(f, "receiver {a:?} is Frozen or Destroyed")
            }
            Self::BalanceOverflow => write!(f, "balance arithmetic overflow"),
            Self::AccountTimeRegression { account } => {
                write!(f, "account {account:?} logical time regression")
            }
            Self::MalformedTransaction {
                expected_len,
                got_len,
            } => write!(
                f,
                "malformed transaction: expected {expected_len} bytes, got {got_len}"
            ),
            Self::MalformedHeader {
                expected_len,
                got_len,
            } => write!(
                f,
                "malformed block header: expected {expected_len} bytes, got {got_len}"
            ),
            Self::TxCountMismatch { header, body } => write!(
                f,
                "tx_count mismatch: header says {header}, body has {body} transactions"
            ),
            Self::StateTrie(e) => write!(f, "state trie construction failed: {e}"),
        }
    }
}

impl std::error::Error for StfError {}

impl From<StateModelError> for StfError {
    fn from(e: StateModelError) -> Self {
        Self::StateTrie(e)
    }
}
