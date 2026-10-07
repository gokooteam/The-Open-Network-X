//! On-disk block file format for `onx replay`.
//!
//! Layout (big-endian, strict):
//! ```text
//! magic "ONXBLK05"(8) || header(160) || sig_section || body(u32be count || [u32be msg_len || msg_bytes]*)
//! sig_section = count(u32be) || [validator_index(u32be) || sig(64)]*
//! ```
//!
//! Magic `ONXBLK05`: authenticated headers (ADR-0032) — the header grew to
//! 160 bytes (`protocol_version` + `block_time`) and a signature section
//! sits outside the hashed header bytes. `ONXBLK04` files are rejected at
//! the magic check, never silently misparsed. Neither V1–V4 ever shipped
//! anywhere (pre-release milestones), so there is no migration path to
//! maintain.
//!
//! The magic prefix makes "not a block file" a distinct, immediate error
//! rather than a confusing parse failure. Everything after the magic reuses
//! the canonical encodings from `onx-stf` (`BlockHeader::from_bytes`) and
//! `onx-storage` (`decode_body`); both are strict and fail closed on
//! truncation, wrong lengths, or trailing bytes. Signature *verification*
//! is not done here — see [`crate::auth`]; the file layer only decodes.

use crate::auth::{decode_sig_section, AuthError};
use onx_stf::block::SIG_ENTRY_BYTE_LEN;
use onx_stf::block::{
    encode_sig_section, Block, BlockBody, BlockHeader, SigEntry, BLOCK_HEADER_BYTE_LEN,
};
use onx_stf::message::EXT_BODY_PREFIX_LEN;
use onx_storage::{decode_body, encode_body};

/// Magic prefix identifying a block file. Versioned so a future format
/// change is detectable instead of silently misparsed.
pub const BLOCK_FILE_MAGIC: &[u8; 8] = b"ONXBLK05";

/// Minimum wire length of one external message: the fixed body prefix (141)
/// plus the 32-byte pubkey plus the 64-byte signature, with an empty
/// payload. Any body claiming more messages than fit at this density is
/// corrupt — this bounds the decoder's upfront reservation by the actual
/// file size.
const MIN_MESSAGE_WIRE_LEN: usize = EXT_BODY_PREFIX_LEN + 32 + 64;

/// Canonical block file name for a sequence number: zero-padded so
/// lexicographic filename order matches chain order.
pub fn block_file_name(seqno: u32) -> String {
    format!("block-{seqno:08}.blk")
}

/// A block with its decoded (not yet verified) signature section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedBlock {
    pub block: Block,
    pub sig_entries: Vec<SigEntry>,
}

/// Errors decoding a block file. All are fail-closed: a bad file aborts
/// the replay, it never produces a partial chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockFileError {
    /// Missing or wrong magic prefix — not a block file.
    BadMagic,
    /// File shorter than magic + header.
    TruncatedHeader { got_len: usize },
    /// The header itself is malformed (strict 160-byte parse failed).
    BadHeader(String),
    /// The signature section is malformed.
    BadSigSection(String),
    /// The body is malformed (strict decode failed).
    BadBody(String),
}

impl std::fmt::Display for BlockFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMagic => write!(f, "not an ONX block file: bad magic prefix"),
            Self::TruncatedHeader { got_len } => write!(
                f,
                "truncated block file: {got_len} bytes, need at least {}",
                BLOCK_FILE_MAGIC.len() + BLOCK_HEADER_BYTE_LEN
            ),
            Self::BadHeader(e) => write!(f, "malformed block header: {e}"),
            Self::BadSigSection(e) => write!(f, "malformed signature section: {e}"),
            Self::BadBody(e) => write!(f, "malformed block body: {e}"),
        }
    }
}

impl std::error::Error for BlockFileError {}

impl From<AuthError> for BlockFileError {
    fn from(e: AuthError) -> Self {
        BlockFileError::BadSigSection(e.to_string())
    }
}

/// Encode a block to its canonical file bytes, with its signature section.
pub fn encode_block_file(block: &Block, sig_entries: &[SigEntry]) -> Vec<u8> {
    let sig_bytes = encode_sig_section(sig_entries);
    let mut out = Vec::with_capacity(
        BLOCK_FILE_MAGIC.len()
            + BLOCK_HEADER_BYTE_LEN
            + sig_bytes.len()
            + 4
            + block.body.messages.len() * 240,
    );
    out.extend_from_slice(BLOCK_FILE_MAGIC);
    out.extend_from_slice(&block.header.to_bytes());
    out.extend_from_slice(&sig_bytes);
    out.extend_from_slice(&encode_body(&block.body));
    out
}

/// Strictly decode a block file. Any deviation is an error, never a guess.
/// Signatures are decoded but NOT verified here — verification is the
/// acceptance layer's job ([`crate::auth::verify_block_auth`]).
pub fn decode_block_file(bytes: &[u8]) -> Result<SignedBlock, BlockFileError> {
    let magic_len = BLOCK_FILE_MAGIC.len();
    if bytes.len() < magic_len || &bytes[..magic_len] != BLOCK_FILE_MAGIC {
        return Err(BlockFileError::BadMagic);
    }
    let rest = &bytes[magic_len..];
    if rest.len() < BLOCK_HEADER_BYTE_LEN {
        return Err(BlockFileError::TruncatedHeader {
            got_len: bytes.len(),
        });
    }
    let header = BlockHeader::from_bytes(&rest[..BLOCK_HEADER_BYTE_LEN])
        .map_err(|e| BlockFileError::BadHeader(e.to_string()))?;
    let after_header = &rest[BLOCK_HEADER_BYTE_LEN..];
    // The signature section is length-prefixed; decode it strictly, then
    // the body follows. A hostile count is bounded by the actual bytes.
    if after_header.len() < 4 {
        return Err(BlockFileError::BadSigSection(
            "truncated signature section: no count".to_string(),
        ));
    }
    let sig_count = u32::from_be_bytes(after_header[..4].try_into().expect("length checked"));
    let sig_len = 4usize.saturating_add((sig_count as usize).saturating_mul(SIG_ENTRY_BYTE_LEN));
    if sig_len > after_header.len() {
        return Err(BlockFileError::BadSigSection(format!(
            "sig section claims {sig_count} entries but only {} bytes remain",
            after_header.len()
        )));
    }
    let sig_entries = decode_sig_section(&after_header[..sig_len]).map_err(BlockFileError::from)?;
    let body_bytes = &after_header[sig_len..];
    // Allocation guard: `decode_body` reserves `count` message slots up
    // front, straight from the body's first 4 bytes, BEFORE checking the
    // file is that long. Validate the claim against the actual length
    // first: every message costs at least its 4-byte length prefix plus
    // the minimum wire encoding, so a hostile count is rejected here with
    // a clean error and the reservation stays bounded by the file size.
    if body_bytes.len() >= 4 {
        let claimed =
            u32::from_be_bytes(body_bytes[..4].try_into().expect("length checked above")) as u64;
        let min_per_message = 4u64 + MIN_MESSAGE_WIRE_LEN as u64;
        // No overflow: claimed <= u32::MAX, so the product fits in u64.
        if claimed * min_per_message > body_bytes.len() as u64 - 4 {
            return Err(BlockFileError::BadBody(format!(
                "block body claims {claimed} messages but the file holds only {} body bytes",
                body_bytes.len()
            )));
        }
    }
    let body: BlockBody =
        decode_body(body_bytes).map_err(|e| BlockFileError::BadBody(e.to_string()))?;
    if body.messages.len() as u32 != header.msg_count {
        return Err(BlockFileError::BadBody(format!(
            "header msg_count {} != body message count {}",
            header.msg_count,
            body.messages.len()
        )));
    }
    Ok(SignedBlock {
        block: Block { header, body },
        sig_entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use onx_data_structures::AccountId;
    use onx_stf::message::{ExternalMessage, MsgKind};

    fn sample_block() -> Block {
        // Pure encode/decode round-trip: the signature is opaque bytes here
        // (no verification at the file layer — that's the acceptance layer).
        let msg = ExternalMessage {
            chain_id: [0xCC; 32],
            from: AccountId::from_bytes([1u8; 32]),
            nonce: 0,
            kind: MsgKind::Transfer,
            to: AccountId::from_bytes([2u8; 32]),
            amount_nanos: 1_000,
            fee_nanos: 10,
            message: Vec::new(),
            pubkey: [0u8; 32],
            signature: [0xAB; 64],
        };
        Block::assemble(
            1,
            [9u8; 32],
            1,
            -1,
            AccountId::from_bytes([3u8; 32]),
            vec![msg],
            [7u8; 32],
            1,
            1_790_000_000,
        )
        .expect("sample block assembles")
    }

    fn sample_sigs() -> Vec<SigEntry> {
        vec![SigEntry {
            validator_index: 0,
            sig: [0x5A; 64],
        }]
    }

    #[test]
    fn block_file_round_trip() {
        let block = sample_block();
        let sigs = sample_sigs();
        let bytes = encode_block_file(&block, &sigs);
        let decoded = decode_block_file(&bytes).unwrap();
        assert_eq!(decoded.block, block);
        assert_eq!(decoded.sig_entries, sigs);
    }

    #[test]
    fn empty_sig_section_round_trip() {
        let block = sample_block();
        let bytes = encode_block_file(&block, &[]);
        let decoded = decode_block_file(&bytes).unwrap();
        assert_eq!(decoded.block, block);
        assert!(decoded.sig_entries.is_empty());
    }

    #[test]
    fn bad_magic_rejected() {
        let mut bytes = encode_block_file(&sample_block(), &sample_sigs());
        bytes[0] ^= 0xff;
        assert_eq!(decode_block_file(&bytes), Err(BlockFileError::BadMagic));
    }

    #[test]
    fn truncated_rejected() {
        let bytes = encode_block_file(&sample_block(), &sample_sigs());
        let cut = &bytes[..bytes.len() / 2];
        assert!(decode_block_file(cut).is_err());
    }

    #[test]
    fn trailing_byte_rejected() {
        let mut bytes = encode_block_file(&sample_block(), &sample_sigs());
        bytes.push(0);
        assert!(matches!(
            decode_block_file(&bytes),
            Err(BlockFileError::BadBody(_))
        ));
    }

    #[test]
    fn hostile_sig_count_rejected_without_allocation() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(BLOCK_FILE_MAGIC);
        bytes.extend_from_slice(&[0u8; BLOCK_HEADER_BYTE_LEN]);
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            decode_block_file(&bytes),
            Err(BlockFileError::BadSigSection(_))
        ));
    }

    #[test]
    fn hostile_message_count_rejected_without_allocation() {
        // Magic + a zeroed (parseable) header + empty sig section +
        // a u32::MAX message count with no bodies behind it.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(BLOCK_FILE_MAGIC);
        bytes.extend_from_slice(&[0u8; BLOCK_HEADER_BYTE_LEN]);
        bytes.extend_from_slice(&0u32.to_be_bytes()); // empty sig section
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            decode_block_file(&bytes),
            Err(BlockFileError::BadBody(_))
        ));

        // A merely-implausible count is rejected the same way.
        let mut bytes2 = Vec::new();
        bytes2.extend_from_slice(BLOCK_FILE_MAGIC);
        bytes2.extend_from_slice(&[0u8; BLOCK_HEADER_BYTE_LEN]);
        bytes2.extend_from_slice(&0u32.to_be_bytes());
        bytes2.extend_from_slice(&1_000_000u32.to_be_bytes());
        assert!(matches!(
            decode_block_file(&bytes2),
            Err(BlockFileError::BadBody(_))
        ));

        // An empty-but-well-formed body still decodes.
        let mut bytes3 = Vec::new();
        bytes3.extend_from_slice(BLOCK_FILE_MAGIC);
        bytes3.extend_from_slice(&[0u8; BLOCK_HEADER_BYTE_LEN]);
        bytes3.extend_from_slice(&0u32.to_be_bytes());
        bytes3.extend_from_slice(&0u32.to_be_bytes());
        let decoded = decode_block_file(&bytes3).unwrap();
        assert_eq!(decoded.block.header.msg_count, 0);
        assert!(decoded.block.body.messages.is_empty());
    }

    #[test]
    fn file_names_sort_in_chain_order() {
        let mut names: Vec<String> = (1..=12).map(block_file_name).collect();
        names.sort();
        let seqnos: Vec<u32> = names
            .iter()
            .map(|n| n["block-".len()..n.len() - ".blk".len()].parse().unwrap())
            .collect();
        assert_eq!(seqnos, (1..=12).collect::<Vec<_>>());
    }
}
