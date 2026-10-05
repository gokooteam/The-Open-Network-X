//! Canonical block-body encoding for persistence.
//!
//! Kept in the storage crate (not `onx-stf`) per the Phase 4 constraint:
//! storage consumes the STF's types without redefining them.
//!
//! Layout (big-endian, length-prefixed — transactions are variable-length
//! since V3 introduced message-carrying transactions):
//! ```text
//! tx_count u32be(4) || [tx_len u32be(4) || tx_bytes]*
//! ```
//! Any deviation (truncation, over-long, or a tx that fails to parse) is
//! corruption, not a parse choice.

use crate::error::StorageError;
use onx_stf::block::BlockBody;

/// Byte length of the transaction-count prefix.
pub const BODY_COUNT_LEN: usize = 4;

/// Byte length of each per-transaction length prefix.
pub const TX_LEN_PREFIX: usize = 4;

/// Canonical encoding of a block body.
pub fn encode_body(body: &BlockBody) -> Vec<u8> {
    let mut out = Vec::with_capacity(BODY_COUNT_LEN + body.transactions.len() * 200);
    out.extend_from_slice(&(body.transactions.len() as u32).to_be_bytes());
    for tx in &body.transactions {
        let tx_bytes = tx.to_bytes();
        out.extend_from_slice(&(tx_bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(&tx_bytes);
    }
    out
}

/// Strict decode of a block body. Rejects wrong lengths and trailing bytes —
/// a truncated or over-long body is corruption, never a partial block.
pub fn decode_body(bytes: &[u8]) -> Result<BlockBody, StorageError> {
    use onx_stf::block::Transaction;
    if bytes.len() < BODY_COUNT_LEN {
        return Err(StorageError::Corrupt(format!(
            "truncated block body: {} bytes, need at least {BODY_COUNT_LEN}",
            bytes.len()
        )));
    }
    let count = u32::from_be_bytes(bytes[0..4].try_into().expect("slice len checked")) as usize;
    let mut transactions = Vec::with_capacity(count);
    let mut off = BODY_COUNT_LEN;
    for i in 0..count {
        if bytes.len() < off + TX_LEN_PREFIX {
            return Err(StorageError::Corrupt(format!(
                "truncated block body: tx {i} length prefix missing"
            )));
        }
        let tx_len = u32::from_be_bytes(
            bytes[off..off + TX_LEN_PREFIX]
                .try_into()
                .expect("len checked"),
        ) as usize;
        off += TX_LEN_PREFIX;
        if bytes.len() < off + tx_len {
            return Err(StorageError::Corrupt(format!(
                "truncated block body: tx {i} needs {tx_len} bytes, {} remain",
                bytes.len() - off
            )));
        }
        let tx = Transaction::from_bytes(&bytes[off..off + tx_len]).map_err(|e| {
            StorageError::Corrupt(format!("stored transaction {i} failed to decode: {e}"))
        })?;
        transactions.push(tx);
        off += tx_len;
    }
    if off != bytes.len() {
        return Err(StorageError::Corrupt(format!(
            "block body has {} trailing bytes after {count} txs",
            bytes.len() - off
        )));
    }
    Ok(BlockBody { transactions })
}

#[cfg(test)]
mod tests {
    use super::*;
    use onx_data_structures::AccountId;
    use onx_stf::block::{Transaction, TxKind};

    fn tx(from: u8, to: u8) -> Transaction {
        // Opaque bytes for the encode/decode round-trip (no verification
        // at the codec layer).
        Transaction {
            kind: TxKind::Transfer,
            from: AccountId::from_bytes([from; 32]),
            to: AccountId::from_bytes([to; 32]),
            amount_nanos: 1_000,
            fee_nanos: 10,
            nonce: 0,
            message: Vec::new(),
            signature: [0xAB; 64],
        }
    }

    #[test]
    fn body_round_trip() {
        let body = BlockBody {
            transactions: vec![tx(1, 2), tx(3, 4)],
        };
        assert_eq!(decode_body(&encode_body(&body)).unwrap(), body);
    }

    #[test]
    fn empty_body_round_trip() {
        let body = BlockBody::default();
        let enc = encode_body(&body);
        assert_eq!(enc.len(), BODY_COUNT_LEN);
        assert_eq!(decode_body(&enc).unwrap(), body);
    }

    #[test]
    fn truncated_body_rejected() {
        let body = BlockBody {
            transactions: vec![tx(1, 2)],
        };
        let mut enc = encode_body(&body);
        enc.pop();
        assert!(decode_body(&enc).is_err());
    }

    #[test]
    fn trailing_bytes_rejected() {
        let body = BlockBody {
            transactions: vec![tx(1, 2)],
        };
        let mut enc = encode_body(&body);
        enc.push(0);
        assert!(decode_body(&enc).is_err());
    }
}
