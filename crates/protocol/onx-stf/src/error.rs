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
    /// Recomputed external-message-set hash does not match the header commitment.
    MsgsRootMismatch {
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
    /// Receiver is `Frozen` or `Destroyed`. Unreachable in the delivery path
    /// (frozen/destroyed destinations bounce instead); kept as a defensive
    /// fail-closed marker for internal caller bugs.
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
    /// Message bytes are not the canonical encoding.
    MalformedMessage { expected_len: usize, got_len: usize },
    /// Block header bytes are not the canonical length.
    MalformedHeader { expected_len: usize, got_len: usize },
    /// Header `msg_count` does not match the number of body messages.
    MsgCountMismatch { header: u32, body: usize },
    /// Block body holds more messages than fit in a `u32` msg_count.
    /// Practically unreachable (a `Vec` that long cannot exist in memory),
    /// but the checked conversion fails closed instead of truncating.
    TooManyMessages { count: usize },
    /// State trie construction failed while computing a root hash
    /// (fail-closed: the error propagates, never a silent constant —
    /// Phase 0 bug 4). Deterministic given the same input state.
    StateTrie(StateModelError),
    /// Sender account carries no public key (all-zero pubkey) and the
    /// message revealed none. A key-derived account can spend by revealing
    /// the pubkey that hashes to its address (ADR-0006); an address that
    /// was not derived from any key stays unspendable. The all-zero
    /// encoding is the Ed25519 identity point, for which a degenerate
    /// signature verifies under any message — so keylessness is checked
    /// explicitly, never left to the signature verifier's edge behavior.
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
    /// Message kind byte is not a known `MsgKind` discriminant.
    BadMsgKind(u8),
    /// Contract-call message exceeds `MAX_MESSAGE_BYTES`.
    MessageTooLarge { len: usize },
    /// External message was signed for a different chain. The chain ID is
    /// the genesis hash, carried in `State`; a signature minted for one
    /// chain never verifies on another because the signed body includes
    /// the chain ID (ADR-0005).
    WrongChainId { expected: [u8; 32], got: [u8; 32] },
    /// An internal message was already delivered in this block. The
    /// per-block processed set makes double delivery impossible —
    /// a re-delivery attempt fails the block closed (ADR-0007).
    DoubleDelivery { msg_id: [u8; 32] },
    /// The revealed pubkey does not hash to the sender's account address.
    AddressKeyMismatch { account: AccountId },
    /// The message revealed a pubkey, but the sender account already has
    /// one on file. Key rotation is out of scope for this milestone.
    UnexpectedPubkeyReveal(AccountId),
    /// A contract call with zero fee cannot buy gas. Rejected at the wallet
    /// handler (sender-side fault, fail-closed) rather than bounced.
    ZeroFeeContractCall,
    /// Defensive bound on internal-message deliveries per block exceeded.
    TooManyDeliveries { max: usize },
    /// A bounce message could not be delivered to its destination (the
    /// original sender). Unreachable in honest operation — the bounce
    /// target was `Active` at wallet time and nothing freezes accounts
    /// mid-block — so this fails the block closed rather than silently
    /// burning the value.
    BounceUndeliverable { msg_id: [u8; 32] },
    /// The fee collector account is `Frozen` or `Destroyed` and cannot
    /// receive the validator fee share. A protocol configuration fault —
    /// the block fails closed.
    FeeCollectorNotReceivable(AccountId),
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
            Self::MsgsRootMismatch { .. } => {
                write!(f, "msgs_root mismatch: header does not commit to this body")
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
            Self::MalformedMessage {
                expected_len,
                got_len,
            } => write!(
                f,
                "malformed message: expected {expected_len} bytes, got {got_len}"
            ),
            Self::MalformedHeader {
                expected_len,
                got_len,
            } => write!(
                f,
                "malformed block header: expected {expected_len} bytes, got {got_len}"
            ),
            Self::MsgCountMismatch { header, body } => write!(
                f,
                "msg_count mismatch: header says {header}, body has {body} messages"
            ),
            Self::TooManyMessages { count } => {
                write!(f, "too many messages: {count} exceeds u32::MAX")
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
            Self::BadMsgKind(b) => write!(f, "unknown message kind byte: {b:#04x}"),
            Self::MessageTooLarge { len } => {
                write!(f, "contract message too large: {len} bytes")
            }
            Self::WrongChainId { .. } => {
                write!(f, "message chain ID does not match this chain")
            }
            Self::DoubleDelivery { msg_id } => {
                write!(f, "internal message delivered twice: {msg_id:?}")
            }
            Self::AddressKeyMismatch { account } => {
                write!(f, "revealed pubkey does not derive to account {account:?}")
            }
            Self::UnexpectedPubkeyReveal(a) => {
                write!(
                    f,
                    "account {a:?} already has a key; unexpected pubkey reveal"
                )
            }
            Self::ZeroFeeContractCall => {
                write!(f, "contract call carries zero fee: no gas possible")
            }
            Self::TooManyDeliveries { max } => {
                write!(
                    f,
                    "delivery round bound exceeded: more than {max} deliveries"
                )
            }
            Self::BounceUndeliverable { msg_id } => {
                write!(f, "bounce message {msg_id:?} could not be delivered")
            }
            Self::FeeCollectorNotReceivable(a) => {
                write!(f, "fee collector {a:?} is Frozen or Destroyed")
            }
        }
    }
}

impl std::error::Error for StfError {}

impl From<StateModelError> for StfError {
    fn from(e: StateModelError) -> Self {
        Self::StateTrie(e)
    }
}
