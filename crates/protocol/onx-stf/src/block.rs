//! Block structure for the message-based chain (ADR-0001, ADR-0002).
//!
//! A block body is the ordered list of **external messages**. Internal
//! messages are derived during execution and never appear in a block —
//! cross-block internal replay is structurally impossible.
//!
//! The header commits to the external set via `msgs_root`
//! (`ONX_MSGS_ROOT_V1`). The 160-byte header layout (ONXBLK05, ADR-0032)
//! appends `protocol_version` (u32be) and `block_time` (u64be) to the
//! previous 148-byte layout, so the layout carries no legacy ambiguity.

use crate::error::StfError;
pub use crate::message::{msgs_root, ExternalMessage};
use onx_data_structures::AccountId;
use onx_primitives::{domain_hash, DomainTag, Int32, Uint32, Uint64};

/// Domain tag for block header identity.
pub const ONX_BLOCK_HDR_V1: DomainTag = DomainTag::from_ascii("ONX_BLOCK_HDR_V1");

/// A block body: the ordered list of external messages.
///
/// Order is consensus-critical: external messages authenticate strictly in
/// vector order, and `msgs_root` commits to that order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlockBody {
    pub messages: Vec<ExternalMessage>,
}

impl BlockBody {
    pub fn msgs_root(&self) -> [u8; 32] {
        msgs_root(&self.messages)
    }
}

/// A block header.
///
/// Protocol version this node understands (ADR-0032). Validity is
/// version-gated: `apply_block` rejects any block whose `protocol_version`
/// differs — the version field IS the upgrade mechanism.
/// Genesis declares v1; bump this when the header format changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// Canonical encoding (160 bytes, big-endian, ADR-0032):
/// `seqno u32be(4) || prev_hash(32) || msgs_root(32) || state_root(32) ||
///  lt u64be(8) || workchain i32be(4) || fee_collector(32) || msg_count u32be(4) ||
///  protocol_version u32be(4) || block_time u64be(8)`
/// = 4+32+32+32+8+4+32+4+4+8 = 160 bytes.
///
/// Fields:
/// - `state_root`: the *claimed* post-state root. The STF recomputes it and
///   rejects the block on mismatch — a block cannot lie about its result.
/// - `lt`: block logical time, strictly increasing across the chain.
/// - `fee_collector`: account credited with the validator half of fees.
///   Explicit in the header so the STF stays pure (no validator-set lookup).
/// - `msg_count`: redundant with the body but committed in the header so a
///   truncated body cannot pass the `msgs_root` check by accident... (it
///   can't anyway — `msgs_root` covers the full ordered set; `msg_count` is
///   carried for tooling convenience and checked for consistency).
/// - `protocol_version`: the protocol version the block was produced under.
///   Genesis declares v1; validity is version-gated (ADR-0032).
/// - `block_time`: unix seconds, wall-clock at production. Monotonic and
///   replay-checked; the STF treats it as header input, not execution state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHeader {
    pub seqno: u32,
    pub prev_hash: [u8; 32],
    pub msgs_root: [u8; 32],
    pub state_root: [u8; 32],
    pub lt: u64,
    pub workchain: i32,
    pub fee_collector: AccountId,
    pub msg_count: u32,
    pub protocol_version: u32,
    pub block_time: u64,
}

/// Canonical byte length of one [`BlockHeader`].
pub const BLOCK_HEADER_BYTE_LEN: usize = 160;

/// Domain tag for the block signature preimage (ADR-0032).
pub const ONX_BLOCK_SIG_V1: DomainTag = DomainTag::from_ascii("ONX_BLOCK_SIG_V1");

impl BlockHeader {
    /// Canonical 160-byte encoding.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(BLOCK_HEADER_BYTE_LEN);
        out.extend_from_slice(&Uint32(self.seqno).encode());
        out.extend_from_slice(&self.prev_hash);
        out.extend_from_slice(&self.msgs_root);
        out.extend_from_slice(&self.state_root);
        out.extend_from_slice(&Uint64(self.lt).encode());
        out.extend_from_slice(&Int32(self.workchain).encode());
        out.extend_from_slice(&self.fee_collector.to_bytes());
        out.extend_from_slice(&Uint32(self.msg_count).encode());
        out.extend_from_slice(&Uint32(self.protocol_version).encode());
        out.extend_from_slice(&Uint64(self.block_time).encode());
        out
    }

    /// Parse exactly one canonical header. Rejects wrong lengths and trailing bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StfError> {
        if bytes.len() != BLOCK_HEADER_BYTE_LEN {
            return Err(StfError::MalformedHeader {
                expected_len: BLOCK_HEADER_BYTE_LEN,
                got_len: bytes.len(),
            });
        }
        // All call sites pass small constant offsets into the 160-byte header
        // (length checked above); saturation is unreachable. Explicit per the
        // crate's `arithmetic_side_effects` policy.
        let u32_at =
            |o: usize| u32::from_be_bytes(bytes[o..o.saturating_add(4)].try_into().unwrap());
        let u64_at =
            |o: usize| u64::from_be_bytes(bytes[o..o.saturating_add(8)].try_into().unwrap());
        let i32_at =
            |o: usize| i32::from_be_bytes(bytes[o..o.saturating_add(4)].try_into().unwrap());
        let h32_at = |o: usize| {
            let mut h = [0u8; 32];
            h.copy_from_slice(&bytes[o..o.saturating_add(32)]);
            h
        };
        Ok(Self {
            seqno: u32_at(0),
            prev_hash: h32_at(4),
            msgs_root: h32_at(36),
            state_root: h32_at(68),
            lt: u64_at(100),
            workchain: i32_at(108),
            fee_collector: AccountId::from_bytes(h32_at(112)),
            msg_count: u32_at(144),
            protocol_version: u32_at(148),
            block_time: u64_at(152),
        })
    }

    /// Domain-separated hash of the canonical encoding: the block's identity.
    /// Chained via `prev_hash` — this is what makes the chain a chain.
    pub fn hash(&self) -> [u8; 32] {
        domain_hash(&ONX_BLOCK_HDR_V1, &self.to_bytes())
    }

    /// The 96-byte signing preimage (ADR-0032):
    /// `pad32("ONX_BLOCK_SIG_V1") || chain_id || block_hash`.
    /// Signatures bind the chain and the exact block.
    pub fn sign_bytes(&self, chain_id: &[u8; 32]) -> [u8; 96] {
        let mut out = [0u8; 96];
        out[..32].copy_from_slice(ONX_BLOCK_SIG_V1.as_bytes());
        out[32..64].copy_from_slice(chain_id);
        out[64..96].copy_from_slice(&self.hash());
        out
    }
}

/// Checked `usize -> u32` conversion for the header `msg_count`.
///
/// A `Vec<ExternalMessage>` longer than `u32::MAX` cannot exist in memory,
/// so this is defense in depth: if it ever fired, silently truncating with
/// `as u32` would commit a header whose `msg_count` disagrees with the body
/// it commits to. Fail closed instead.
fn checked_msg_count(n: usize) -> Result<u32, StfError> {
    u32::try_from(n).map_err(|_| StfError::TooManyMessages { count: n })
}

/// A full block: header (commitments) plus body (the committed messages).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub header: BlockHeader,
    pub body: BlockBody,
}

/// One producer signature: which genesis validator signed, and how.
/// (ADR-0032; the signature section sits outside the hashed header.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SigEntry {
    /// Index into the canonical (pubkey-sorted) genesis validator list.
    pub validator_index: u32,
    /// Ed25519 signature over the 96-byte preimage.
    pub sig: [u8; 64],
}

/// Byte length of one encoded signature entry.
pub const SIG_ENTRY_BYTE_LEN: usize = 4 + 64;

/// Encode a signature section:
/// `count(u32be) || [validator_index(u32be) || sig(64)]*`.
pub fn encode_sig_section(entries: &[SigEntry]) -> Vec<u8> {
    // Capacity is a hint only: saturating arithmetic keeps the deny-level
    // arithmetic_side_effects lint happy without changing semantics.
    let cap = 4usize.saturating_add(entries.len().saturating_mul(SIG_ENTRY_BYTE_LEN));
    let mut out = Vec::with_capacity(cap);
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for e in entries {
        out.extend_from_slice(&e.validator_index.to_be_bytes());
        out.extend_from_slice(&e.sig);
    }
    out
}

impl Block {
    /// Assemble a block, computing `msgs_root` and `msg_count` from the body.
    /// The caller must fill `state_root` — see [`crate::stf::apply_block`],
    /// which verifies it.
    ///
    /// Fails closed with [`StfError::TooManyMessages`] if the body
    /// holds more messages than fit in a `u32` (unreachable in
    /// practice; the check exists so the invariant is explicit rather
    /// than a silent truncation).
    pub fn assemble(p: AssembleParams) -> Result<Self, StfError> {
        let msg_count = checked_msg_count(p.messages.len())?;
        let body = BlockBody {
            messages: p.messages,
        };
        let header = BlockHeader {
            seqno: p.seqno,
            prev_hash: p.prev_hash,
            msgs_root: body.msgs_root(),
            state_root: p.state_root,
            lt: p.lt,
            workchain: p.workchain,
            fee_collector: p.fee_collector,
            msg_count,
            protocol_version: p.protocol_version,
            block_time: p.block_time,
        };
        Ok(Self { header, body })
    }
}

/// Parameters for [`Block::assemble`]: every header field except the two
/// computed from the body (`msgs_root`, `msg_count`). A struct (rather than
/// nine positional arguments) keeps the call sites readable now that the
/// ADR-0032 fields (`protocol_version`, `block_time`) joined the header.
#[derive(Debug, Clone)]
pub struct AssembleParams {
    pub seqno: u32,
    pub prev_hash: [u8; 32],
    pub lt: u64,
    pub workchain: i32,
    pub fee_collector: AccountId,
    pub messages: Vec<ExternalMessage>,
    pub state_root: [u8; 32],
    pub protocol_version: u32,
    pub block_time: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{MsgKind, EXT_BODY_PREFIX_LEN};
    use onx_primitives::SecretKey;

    #[test]
    fn msg_count_checked_conversion_fails_closed() {
        assert_eq!(checked_msg_count(0), Ok(0));
        assert_eq!(checked_msg_count(1), Ok(1));
        assert_eq!(checked_msg_count(u32::MAX as usize), Ok(u32::MAX));
        // A Vec this long cannot be built in memory, so the conversion
        // itself is the unit under test: it must error, never wrap.
        let too_big = usize::try_from(u64::from(u32::MAX) + 1).unwrap();
        assert_eq!(
            checked_msg_count(too_big),
            Err(StfError::TooManyMessages { count: too_big })
        );
    }

    #[test]
    fn header_layout_is_160_bytes() {
        let header = BlockHeader {
            seqno: 1,
            prev_hash: [0x11; 32],
            msgs_root: [0x22; 32],
            state_root: [0x33; 32],
            lt: 42,
            workchain: -1,
            fee_collector: AccountId::from_bytes([0x44; 32]),
            msg_count: 3,
            protocol_version: 1,
            block_time: 1_790_000_000,
        };
        let bytes = header.to_bytes();
        assert_eq!(bytes.len(), BLOCK_HEADER_BYTE_LEN);
        // Field offsets: seqno 0..4, prev_hash 4..36, msgs_root 36..68,
        // state_root 68..100, lt 100..108, workchain 108..112,
        // fee_collector 112..144, msg_count 144..148,
        // protocol_version 148..152, block_time 152..160.
        assert_eq!(u32::from_be_bytes(bytes[0..4].try_into().unwrap()), 1);
        assert_eq!(&bytes[36..68], &[0x22; 32]);
        assert_eq!(u64::from_be_bytes(bytes[100..108].try_into().unwrap()), 42);
        assert_eq!(i32::from_be_bytes(bytes[108..112].try_into().unwrap()), -1);
        assert_eq!(u32::from_be_bytes(bytes[144..148].try_into().unwrap()), 3);
        assert_eq!(u32::from_be_bytes(bytes[148..152].try_into().unwrap()), 1);
        assert_eq!(
            u64::from_be_bytes(bytes[152..160].try_into().unwrap()),
            1_790_000_000
        );
        let back = BlockHeader::from_bytes(&bytes).unwrap();
        assert_eq!(back, header);
        assert!(BlockHeader::from_bytes(&bytes[..147]).is_err());
        let mut long = bytes.clone();
        long.push(0);
        assert!(BlockHeader::from_bytes(&long).is_err());
    }

    #[test]
    fn block_assemble_commits_to_messages() {
        let secret = SecretKey::from_seed(&[0x42; 32]).unwrap();
        let msg = ExternalMessage::new_signed(
            [0xAA; 32],
            MsgKind::Transfer,
            AccountId::from_bytes([1u8; 32]),
            0,
            AccountId::from_bytes([2u8; 32]),
            100,
            1,
            Vec::new(),
            [0u8; 32],
            &secret,
        );
        let block = Block::assemble(AssembleParams {
            seqno: 1,
            prev_hash: [0x11; 32],
            lt: 7,
            workchain: 0,
            fee_collector: AccountId::from_bytes([0x44; 32]),
            messages: vec![msg.clone()],
            state_root: [0x33; 32],
            protocol_version: 1,
            block_time: 0,
        })
        .unwrap();
        assert_eq!(block.header.msg_count, 1);
        assert_eq!(
            block.header.msgs_root,
            msgs_root(std::slice::from_ref(&msg))
        );
        assert_eq!(block.body.messages.len(), 1);
        // Prefix length sanity: the fixed prefix is 141 bytes; the transfer
        // body adds the 32-byte pubkey.
        assert_eq!(msg.body_bytes().len(), EXT_BODY_PREFIX_LEN + 32);
    }
}
