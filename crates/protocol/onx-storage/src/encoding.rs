//! Canonical block-body encoding for persistence.
//!
//! Kept in the storage crate (not `onx-stf`) per the Phase 4 constraint:
//! storage consumes the STF's types without redefining them.
//!
//! Layout (big-endian, length-prefixed — messages are variable-length):
//! ```text
//! msg_count u32be(4) || [msg_len u32be(4) || msg_bytes]*
//! ```
//! Any deviation (truncation, over-long, or a message that fails to parse)
//! is corruption, not a parse choice.

use crate::error::StorageError;
use onx_stf::block::BlockBody;

/// Byte length of the message-count prefix.
pub const BODY_COUNT_LEN: usize = 4;

/// Byte length of each per-message length prefix.
pub const MSG_LEN_PREFIX: usize = 4;

/// Canonical encoding of a block body.
pub fn encode_body(body: &BlockBody) -> Vec<u8> {
    let mut out = Vec::with_capacity(BODY_COUNT_LEN + body.messages.len() * 240);
    out.extend_from_slice(&(body.messages.len() as u32).to_be_bytes());
    for msg in &body.messages {
        let msg_bytes = msg.to_bytes();
        out.extend_from_slice(&(msg_bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(&msg_bytes);
    }
    out
}

/// Strict decode of a block body. Rejects wrong lengths and trailing bytes —
/// a truncated or over-long body is corruption, never a partial block.
pub fn decode_body(bytes: &[u8]) -> Result<BlockBody, StorageError> {
    use onx_stf::block::ExternalMessage;
    if bytes.len() < BODY_COUNT_LEN {
        return Err(StorageError::Corrupt(format!(
            "truncated block body: {} bytes, need at least {BODY_COUNT_LEN}",
            bytes.len()
        )));
    }
    let count = u32::from_be_bytes(bytes[0..4].try_into().expect("slice len checked")) as usize;
    // Root fix (wave-2): `count` is untrusted input and must not drive the
    // upfront allocation. A forged count (u32::MAX in a 160-byte body) made
    // the old code reserve ~1.2 TB up front and abort the process instead
    // of returning an error. Bound the reservation by what the input can
    // actually hold: every message costs at least its 4-byte length prefix,
    // so no valid body claims more than `max_messages` messages. A forged
    // count is clamped here and then rejected with a clean error by the
    // loop's truncation checks below. (The wave-1 guard in
    // `decode_block_file` remains as defense-in-depth at the file layer.)
    let max_messages = (bytes.len() - BODY_COUNT_LEN) / MSG_LEN_PREFIX;
    let mut messages = Vec::with_capacity(count.min(max_messages));
    let mut off = BODY_COUNT_LEN;
    for i in 0..count {
        if bytes.len() < off + MSG_LEN_PREFIX {
            return Err(StorageError::Corrupt(format!(
                "truncated block body: message {i} length prefix missing"
            )));
        }
        let msg_len = u32::from_be_bytes(
            bytes[off..off + MSG_LEN_PREFIX]
                .try_into()
                .expect("len checked"),
        ) as usize;
        off += MSG_LEN_PREFIX;
        if bytes.len() < off + msg_len {
            return Err(StorageError::Corrupt(format!(
                "truncated block body: message {i} needs {msg_len} bytes, {} remain",
                bytes.len() - off
            )));
        }
        let msg = ExternalMessage::from_bytes(&bytes[off..off + msg_len]).map_err(|e| {
            StorageError::Corrupt(format!("stored message {i} failed to decode: {e}"))
        })?;
        messages.push(msg);
        off += msg_len;
    }
    if off != bytes.len() {
        return Err(StorageError::Corrupt(format!(
            "block body has {} trailing bytes after {count} messages",
            bytes.len() - off
        )));
    }
    Ok(BlockBody { messages })
}

#[cfg(test)]
mod tests {
    use super::*;
    use onx_data_structures::AccountId;
    use onx_stf::message::{ExternalMessage, MsgKind};

    fn msg(from: u8, to: u8) -> ExternalMessage {
        // Opaque bytes for the encode/decode round-trip (no verification
        // at the codec layer).
        ExternalMessage {
            chain_id: [0xCC; 32],
            from: AccountId::from_bytes([from; 32]),
            nonce: 0,
            kind: MsgKind::Transfer,
            to: AccountId::from_bytes([to; 32]),
            amount_nanos: 1_000,
            fee_nanos: 10,
            message: Vec::new(),
            pubkey: [0u8; 32],
            signature: [0xAB; 64],
        }
    }

    #[test]
    fn body_round_trip() {
        let body = BlockBody {
            messages: vec![msg(1, 2), msg(3, 4)],
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
            messages: vec![msg(1, 2)],
        };
        let mut enc = encode_body(&body);
        enc.pop();
        assert!(decode_body(&enc).is_err());
    }

    #[test]
    fn trailing_bytes_rejected() {
        let body = BlockBody {
            messages: vec![msg(1, 2)],
        };
        let mut enc = encode_body(&body);
        enc.push(0);
        assert!(decode_body(&enc).is_err());
    }

    #[test]
    fn forged_message_count_returns_error_without_allocation() {
        // Root-cause regression test for the review's 160-byte crash file:
        // a u32::MAX message count with no bodies behind it. This feeds
        // `decode_body` DIRECTLY, bypassing the wave-1 `decode_block_file`
        // boundary guard, so it exercises the root fix: the old code
        // attempted a ~1.2 TB reservation here and aborted the process;
        // the fixed code clamps the reservation and returns a clean error.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 156]);
        assert_eq!(bytes.len(), 160);
        assert!(
            matches!(decode_body(&bytes), Err(StorageError::Corrupt(_))),
            "forged count must be a clean decode error, not an abort"
        );

        // A merely-implausible count is rejected the same way.
        let mut bytes2 = Vec::new();
        bytes2.extend_from_slice(&1_000_000u32.to_be_bytes());
        bytes2.extend_from_slice(&[0u8; 156]);
        assert!(matches!(
            decode_body(&bytes2),
            Err(StorageError::Corrupt(_))
        ));
    }
}
