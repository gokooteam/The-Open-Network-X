//! On-disk block file format for `onx replay`.
//!
//! Layout (big-endian, fixed-size, strict):
//! ```text
//! magic "ONXBLK01"(8) || header(148) || body(u32be count || 96-byte txs)
//! ```
//!
//! The magic prefix makes "not a block file" a distinct, immediate error
//! rather than a confusing parse failure. Everything after the magic reuses
//! the canonical encodings from `onx-stf` (`BlockHeader::from_bytes`) and
//! `onx-storage` (`decode_body`); both are strict and fail closed on
//! truncation, wrong lengths, or trailing bytes.

use onx_stf::block::{Block, BlockBody, BlockHeader, BLOCK_HEADER_BYTE_LEN};
use onx_storage::{decode_body, encode_body};

/// Magic prefix identifying a block file. Versioned so a future format
/// change is detectable instead of silently misparsed.
pub const BLOCK_FILE_MAGIC: &[u8; 8] = b"ONXBLK01";

/// Canonical block file name for a sequence number: zero-padded so
/// lexicographic filename order matches chain order.
pub fn block_file_name(seqno: u32) -> String {
    format!("block-{seqno:08}.blk")
}

/// Errors decoding a block file. All are fail-closed: a bad file aborts
/// the replay, it never produces a partial chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockFileError {
    /// Missing or wrong magic prefix — not a block file.
    BadMagic,
    /// File shorter than magic + header.
    TruncatedHeader { got_len: usize },
    /// The header itself is malformed (strict 148-byte parse failed).
    BadHeader(String),
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
            Self::BadBody(e) => write!(f, "malformed block body: {e}"),
        }
    }
}

impl std::error::Error for BlockFileError {}

/// Encode a block to its canonical file bytes.
pub fn encode_block_file(block: &Block) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        BLOCK_FILE_MAGIC.len() + BLOCK_HEADER_BYTE_LEN + 4 + block.body.transactions.len() * 96,
    );
    out.extend_from_slice(BLOCK_FILE_MAGIC);
    out.extend_from_slice(&block.header.to_bytes());
    out.extend_from_slice(&encode_body(&block.body));
    out
}

/// Strictly decode a block file. Any deviation is an error, never a guess.
pub fn decode_block_file(bytes: &[u8]) -> Result<Block, BlockFileError> {
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
    let body: BlockBody = decode_body(&rest[BLOCK_HEADER_BYTE_LEN..])
        .map_err(|e| BlockFileError::BadBody(e.to_string()))?;
    if body.transactions.len() as u32 != header.tx_count {
        return Err(BlockFileError::BadBody(format!(
            "header tx_count {} != body tx count {}",
            header.tx_count,
            body.transactions.len()
        )));
    }
    Ok(Block { header, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use onx_data_structures::AccountId;
    use onx_stf::block::Transaction;

    fn sample_block() -> Block {
        let tx = Transaction {
            from: AccountId::from_bytes([1u8; 32]),
            to: AccountId::from_bytes([2u8; 32]),
            amount_nanos: 1_000,
            fee_nanos: 10,
        };
        Block::assemble(
            1,
            [9u8; 32],
            1,
            -1,
            AccountId::from_bytes([3u8; 32]),
            vec![tx],
            [7u8; 32],
        )
    }

    #[test]
    fn block_file_round_trip() {
        let block = sample_block();
        let bytes = encode_block_file(&block);
        assert_eq!(decode_block_file(&bytes).unwrap(), block);
    }

    #[test]
    fn bad_magic_rejected() {
        let mut bytes = encode_block_file(&sample_block());
        bytes[0] ^= 0xff;
        assert_eq!(decode_block_file(&bytes), Err(BlockFileError::BadMagic));
    }

    #[test]
    fn truncated_rejected() {
        let bytes = encode_block_file(&sample_block());
        let cut = &bytes[..bytes.len() / 2];
        assert!(decode_block_file(cut).is_err());
    }

    #[test]
    fn trailing_byte_rejected() {
        let mut bytes = encode_block_file(&sample_block());
        bytes.push(0);
        assert!(matches!(
            decode_block_file(&bytes),
            Err(BlockFileError::BadBody(_))
        ));
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
