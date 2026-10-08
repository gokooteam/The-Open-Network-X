//! Authenticated block headers (ADR-0032, ONXBLK05 step 3).
//!
//! This is the block-acceptance layer: signature decoding and verification
//! shared by `onx replay` and the node. It lives OUTSIDE the STF — the STF
//! stays a pure state-transition function (header in, state out);
//! authentication is a separate concern.
//!
//! Layout (big-endian, strict):
//! ```text
//! sig_section = count(u32be) || [validator_index(u32be) || sig(64)]*
//! ```
//!
//! Verification (ADR-0032 §5):
//! 1. Parse header (strict 160-byte decode) and signature section (strict).
//! 2. Recompute `block_hash`; the signature binds `chain_id || block_hash`.
//! 3. For each entry: look up the validator pubkey by index, check the
//!    strict Ed25519 predicate (canonical, on-curve, large-order), verify
//!    the signature over the 96-byte preimage.
//! 4. Sum the stake of valid signers; require strictly more than 2/3 of
//!    genesis total stake.

use onx_primitives::{PublicKey, Signature};
use onx_stf::block::{BlockHeader, SigEntry, PROTOCOL_VERSION, SIG_ENTRY_BYTE_LEN};

/// Errors in signature-section decoding or block authentication.
/// All are fail-closed: a bad section or bad signature rejects the block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// Signature section truncated (no count, or fewer bytes than claimed).
    TruncatedSection { claimed: u32, got_len: usize },
    /// Trailing bytes after the last entry.
    TrailingSectionBytes { extra: usize },
    /// Indices not strictly ascending (unsorted or duplicate).
    UnorderedIndices,
    /// Validator index out of range of the genesis list.
    IndexOutOfRange { index: u32, validators: usize },
    /// Validator public key fails the strict predicate.
    BadValidatorKey { index: u32, reason: String },
    /// Signature does not verify.
    BadSignature { index: u32 },
    /// Valid signers hold <= 2/3 of total genesis stake.
    InsufficientStake { signed: u128, total: u128 },
    /// Stake arithmetic overflowed u128 (practically unreachable — it
    /// would take 2^64 max-stake validators — but silent wrapping or
    /// saturation is never acceptable in consensus code).
    StakeOverflow,
    /// Empty validator set.
    EmptyValidatorSet,
    /// Block declares a protocol version this node does not understand.
    UnsupportedProtocolVersion { got: u32 },
    /// `block_time` went backwards relative to the parent (ADR-0032:
    /// monotonic, non-decreasing across sequence numbers).
    BlockTimeRegression { parent: u64, block: u64 },
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TruncatedSection { claimed, got_len } => write!(
                f,
                "sig section claims {claimed} entries but holds {got_len} bytes"
            ),
            Self::TrailingSectionBytes { extra } => {
                write!(f, "sig section has {extra} trailing bytes")
            }
            Self::UnorderedIndices => {
                write!(f, "sig section indices not strictly ascending")
            }
            Self::IndexOutOfRange { index, validators } => write!(
                f,
                "validator index {index} out of range ({validators} validators)"
            ),
            Self::BadValidatorKey { index, reason } => {
                write!(f, "validator {index} key rejected: {reason}")
            }
            Self::BadSignature { index } => {
                write!(f, "bad signature from validator {index}")
            }
            Self::InsufficientStake { signed, total } => {
                write!(f, "insufficient stake: {signed}/{total} signed (need >2/3)")
            }
            Self::StakeOverflow => write!(f, "stake arithmetic overflowed u128"),
            Self::EmptyValidatorSet => write!(f, "empty validator set"),
            Self::UnsupportedProtocolVersion { got } => {
                write!(f, "unsupported protocol version {got}")
            }
            Self::BlockTimeRegression { parent, block } => write!(
                f,
                "block_time went backwards: parent {parent}, block {block}"
            ),
        }
    }
}

impl std::error::Error for AuthError {}

/// Encode a signature section (re-exported from onx-stf).
pub use onx_stf::block::encode_sig_section;

/// Strictly decode a signature section. Any deviation is an error.
pub fn decode_sig_section(bytes: &[u8]) -> Result<Vec<SigEntry>, AuthError> {
    if bytes.len() < 4 {
        return Err(AuthError::TruncatedSection {
            claimed: 0,
            got_len: bytes.len(),
        });
    }
    let count = u32::from_be_bytes(bytes[0..4].try_into().expect("length checked"));
    // Bound the claim before allocating: each entry costs 68 bytes.
    let need = 4usize.saturating_add((count as usize).saturating_mul(SIG_ENTRY_BYTE_LEN));
    if need > bytes.len() {
        return Err(AuthError::TruncatedSection {
            claimed: count,
            got_len: bytes.len(),
        });
    }
    let mut entries = Vec::with_capacity(count as usize);
    let mut off = 4;
    for _ in 0..count {
        let index = u32::from_be_bytes(bytes[off..off + 4].try_into().expect("bounds checked"));
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&bytes[off + 4..off + SIG_ENTRY_BYTE_LEN]);
        entries.push(SigEntry {
            validator_index: index,
            sig,
        });
        off += SIG_ENTRY_BYTE_LEN;
    }
    if off != bytes.len() {
        return Err(AuthError::TrailingSectionBytes {
            extra: bytes.len() - off,
        });
    }
    // Canonical order: strictly ascending, no duplicates.
    if !entries
        .windows(2)
        .all(|w| w[0].validator_index < w[1].validator_index)
    {
        return Err(AuthError::UnorderedIndices);
    }
    Ok(entries)
}

/// A genesis validator: public key bytes and stake, in canonical
/// (pubkey-sorted) order. `validator_index` addresses this list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenesisValidatorRef {
    pub pubkey: [u8; 32],
    pub stake: u64,
}

/// Verify an authenticated block header (ADR-0032 §5).
///
/// - `chain_id`: 32-byte genesis hash the signatures bind.
/// - `header`: the parsed 160-byte header.
/// - `parent_block_time`: the parent block's `block_time` (0 for block 1,
///   whose parent is genesis). `header.block_time` must be >= it.
/// - `entries`: the decoded signature section.
/// - `validators`: genesis validators in canonical order.
///
/// Returns the total stake of valid signers on success. Fails closed on
/// any malformed input, bad key, bad signature, unordered indices,
/// version mismatch, time regression, or insufficient stake.
///
/// Canonicality (strictly ascending indices) is checked here independently
/// of `decode_sig_section`, so direct callers cannot bypass it.
pub fn verify_block_auth(
    chain_id: &[u8; 32],
    header: &BlockHeader,
    parent_block_time: u64,
    entries: &[SigEntry],
    validators: &[GenesisValidatorRef],
) -> Result<u128, AuthError> {
    if validators.is_empty() {
        return Err(AuthError::EmptyValidatorSet);
    }
    // Version-gated validity (ADR-0032): defense in depth alongside the
    // STF's own check in `apply_block`.
    if header.protocol_version != PROTOCOL_VERSION {
        return Err(AuthError::UnsupportedProtocolVersion {
            got: header.protocol_version,
        });
    }
    // Monotonic block_time (ADR-0032): non-decreasing across seqnos. The
    // STF stays pure — time is a header input checked here, in the
    // acceptance layer, not execution state.
    if header.block_time < parent_block_time {
        return Err(AuthError::BlockTimeRegression {
            parent: parent_block_time,
            block: header.block_time,
        });
    }
    // Canonical entry order: strictly ascending validator indices.
    for pair in entries.windows(2) {
        if pair[1].validator_index <= pair[0].validator_index {
            return Err(AuthError::UnorderedIndices);
        }
    }
    let preimage = header.sign_bytes(chain_id);
    // Stake sums in u128 with CHECKED arithmetic. Summing in u64 (even
    // saturating) then widening is wrong: with three validators at
    // u64::MAX, one signer (1/3 of real stake) saturates both sums to
    // u64::MAX and passes. u128 cannot saturate in practice (it would
    // take 2^64 max-stake validators), and checked ops fail closed if
    // it ever does.
    let mut signed_stake: u128 = 0;
    for e in entries {
        let v = validators
            .get(e.validator_index as usize)
            .ok_or(AuthError::IndexOutOfRange {
                index: e.validator_index,
                validators: validators.len(),
            })?;
        // Strict predicate: canonical encoding, on-curve, large-order.
        // A small-order or non-canonical validator key is a forgery vector.
        let pubkey =
            PublicKey::decode_exact(&v.pubkey).map_err(|err| AuthError::BadValidatorKey {
                index: e.validator_index,
                reason: err.to_string(),
            })?;
        let sig = Signature::decode_exact(&e.sig).map_err(|_| AuthError::BadSignature {
            index: e.validator_index,
        })?;
        pubkey
            .verify_raw(&preimage, &sig)
            .map_err(|_| AuthError::BadSignature {
                index: e.validator_index,
            })?;
        signed_stake = signed_stake
            .checked_add(v.stake as u128)
            .ok_or(AuthError::StakeOverflow)?;
    }
    let mut total: u128 = 0;
    for v in validators {
        total = total
            .checked_add(v.stake as u128)
            .ok_or(AuthError::StakeOverflow)?;
    }
    // Strictly more than 2/3: signed*3 > total*2 (no float, no rounding),
    // all in checked u128.
    let lhs = signed_stake
        .checked_mul(3)
        .ok_or(AuthError::StakeOverflow)?;
    let rhs = total.checked_mul(2).ok_or(AuthError::StakeOverflow)?;
    if lhs <= rhs {
        return Err(AuthError::InsufficientStake {
            signed: signed_stake,
            total,
        });
    }
    Ok(signed_stake)
}
