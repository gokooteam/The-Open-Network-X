//! Canonical block-body encoding for persistence.
//!
//! Kept in the storage crate (not `onx-stf`) per the Phase 4 constraint:
//! storage consumes the STF's types without redefining them.
//!
//! Layout (big-endian, fixed-size):
//! ```text
//! tx_count u32be(4) || tx[0](96) || tx[1](96) || ...
//! ```
//! Each transaction is exactly [`TRANSACTION_BYTE_LEN`] bytes (see
//! `onx_stf::block`), so the total length is `4 + 96 * tx_count` and any
//! deviation is corruption, not a parse choice.

use crate::error::StorageError;
use onx_stf::block::{BlockBody, Transaction, TRANSACTION_BYTE_LEN};
use onx_stf::StfError;

/// Byte length of the transaction-count prefix.
pub const BODY_COUNT_LEN: usize = 4;

/// Canonical encoding of a block body.
pub fn encode_body(body: &BlockBody) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(BODY_COUNT_LEN + body.transactions.len() * TRANSACTION_BYTE_LEN);
    out.extend_from_slice(&(body.transactions.len() as u32).to_be_bytes());
    for tx in &body.transactions {
        out.extend_from_slice(&tx.to_bytes());
    }
    out
}

/// Strict decode of a block body. Rejects wrong lengths and trailing bytes —
/// a truncated or over-long body is corruption, never a partial block.
pub fn decode_body(bytes: &[u8]) -> Result<BlockBody, StorageError> {
    if bytes.len() < BODY_COUNT_LEN {
        return Err(StorageError::Corrupt(format!(
            "truncated block body: {} bytes, need at least {BODY_COUNT_LEN}",
            bytes.len()
        )));
    }
    let count = u32::from_be_bytes(bytes[0..4].try_into().expect("slice len checked")) as usize;
    let expected = BODY_COUNT_LEN + count * TRANSACTION_BYTE_LEN;
    if bytes.len() != expected {
        return Err(StorageError::Corrupt(format!(
            "block body length mismatch: header says {count} txs ({expected} bytes), got {} bytes",
            bytes.len()
        )));
    }
    let mut transactions = Vec::with_capacity(count);
    for i in 0..count {
        let off = BODY_COUNT_LEN + i * TRANSACTION_BYTE_LEN;
        let tx = Transaction::from_bytes(&bytes[off..off + TRANSACTION_BYTE_LEN]).map_err(|e| {
            let StfError::MalformedTransaction { got_len, .. } = e else {
                return StorageError::Corrupt(format!(
                    "stored transaction {i} failed to decode: {e}"
                ));
            };
            StorageError::Corrupt(format!(
                "stored transaction {i} has wrong length: {got_len}"
            ))
        })?;
        transactions.push(tx);
    }
    Ok(BlockBody { transactions })
}

#[cfg(test)]
mod tests {
    use super::*;
    use onx_data_structures::AccountId;

    fn tx(from: u8, to: u8) -> Transaction {
        Transaction {
            from: AccountId::from_bytes([from; 32]),
            to: AccountId::from_bytes([to; 32]),
            amount_nanos: 1_000,
            fee_nanos: 10,
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
