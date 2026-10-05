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
    /// Block body holds more transactions than fit in a `u32` tx_count.
    /// Practically unreachable (a `Vec` that long cannot exist in memory),
    /// but the checked conversion fails closed instead of truncating.
    TooManyTransactions { count: usize },
    /// State trie construction failed while computing a root hash
    /// (fail-closed: the error propagates, never a silent constant —
    /// Phase 0 bug 4). Deterministic given the same input state.
    StateTrie(StateModelError),
    /// Sender account carries no public key (all-zero pubkey). The account
    /// can receive but never spend. The all-zero encoding is the Ed25519
    /// identity point, for which a degenerate signature verifies under any
    /// message — so keylessness is checked explicitly, never left to the
    /// signature verifier's edge behavior.
    SenderHasNoKey(AccountId),
    /// Transaction nonce does not equal the sender account's current nonce.
    /// Covers both replay (nonce already used) and gaps (nonce skipped):
    /// neither is ever valid.
    NonceMismatch { expected: u64, got: u64 },
    /// Ed25519 signature invalid: wrong key, tampered body, or malformed
    /// signature bytes. Also covers the unreachable case of a stored
    /// pubkey that fails point decoding (genesis validates keys, so only
    /// a corrupt state could produce one — still fail closed).
    InvalidSignature,
    /// Account nonce increment overflowed u64 (practically unreachable —
    /// 2^64 spends — but silent wrapping is never acceptable in consensus
    /// code).
    NonceOverflow,
    /// Transaction kind byte is not a known `TxKind` discriminant.
    BadTxKind(u8),
    /// Contract-call message exceeds `MAX_MESSAGE_BYTES`.
    MessageTooLarge { len: usize },
    /// A contract call targeted an account with no contract code. Contract
    /// calls never create accounts — the recipient must already be an
    /// `Active` contract.
    ContractHasNoCode(AccountId),
    /// Contract execution raised a TVM exception (including out-of-gas).
    /// The transaction is invalid, which makes the block invalid —
    /// fail-closed, with all of the transaction's effects reverted (the VM
    /// runs before any state write, so there is nothing to roll back).
    VmExecutionFailed { kind: String },
    /// Contract execution produced outbound messages. The VM's message
    /// egress is deliberately unwired in this milestone — a contract that
    /// tries to send messages makes its transaction invalid rather than
    /// having the messages silently dropped.
    OutMessagesNotSupported { count: usize },
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
            Self::TooManyTransactions { count } => {
                write!(f, "too many transactions: {count} exceeds u32::MAX")
            }
            Self::StateTrie(e) => write!(f, "state trie construction failed: {e}"),
            Self::SenderHasNoKey(a) => {
                write!(f, "sender {a:?} has no public key and cannot spend")
            }
            Self::NonceMismatch { expected, got } => {
                write!(
                    f,
                    "nonce mismatch: account expects {expected}, tx carries {got}"
                )
            }
            Self::InvalidSignature => write!(f, "invalid transaction signature"),
            Self::NonceOverflow => write!(f, "account nonce overflow"),
            Self::BadTxKind(b) => write!(f, "unknown transaction kind byte: {b:#04x}"),
            Self::MessageTooLarge { len } => {
                write!(f, "contract message too large: {len} bytes")
            }
            Self::ContractHasNoCode(a) => {
                write!(f, "contract call to account {a:?} which has no code")
            }
            Self::VmExecutionFailed { kind } => {
                write!(f, "contract execution failed: {kind}")
            }
            Self::OutMessagesNotSupported { count } => write!(
                f,
                "contract produced {count} outbound messages; message egress is not yet supported"
            ),
        }
    }
}

impl std::error::Error for StfError {}

impl From<StateModelError> for StfError {
    fn from(e: StateModelError) -> Self {
        Self::StateTrie(e)
    }
}
