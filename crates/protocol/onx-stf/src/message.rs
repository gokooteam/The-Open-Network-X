//! Message-based transaction model (ADR-0001, ADR-0002).
//!
//! Replaces the synchronous V3 transaction model with the actor model from
//! `docs/specification/transactions.md` §2: accounts interact *exclusively*
//! via asynchronous messages. There are no synchronous transfers anymore —
//! every value movement is a message delivery.
//!
//! Two message types:
//! - [`ExternalMessage`]: "from nowhere" — an off-chain actor submits a
//!   signed message to their own account. The built-in wallet handler
//!   (STF, not VM) verifies the signature and nonce, debits the fee, and
//!   queues exactly one internal message. External messages are what block
//!   bodies commit to.
//! - [`InternalMessage`]: account to account — derived deterministically
//!   during block execution, never submitted. Delivery executes as the
//!   receiver's own transaction. A message that cannot be processed
//!   bounces (value returned to the sender, minus fees already taken).
//!
//! Replay protection (ADR-0007):
//! - External messages: the sender's nonce (exact match, bumped on success).
//! - Internal messages: the message ID (`id()`), checked against a
//!   per-block processed set at delivery. Internal messages are derived,
//!   never submitted, so cross-block replay is structurally impossible;
//!   the check is defense-in-depth within a block.

use crate::error::StfError;
use onx_data_structures::AccountId;
use onx_primitives::{domain_hash, DomainTag, PublicKey, SecretKey, Signature, Uint128, Uint64};

/// Domain tag for external message identity: `domain_hash(tag, wire_bytes)`.
pub const ONX_MSG_EXT_V1: DomainTag = DomainTag::from_ascii("ONX_MSG_EXT_V1");
/// Domain tag for the external message signature payload: the signature is
/// over `tag || body_bytes`. The body includes `chain_id`, so a signature
/// minted for one chain never verifies on another (ADR-0005).
pub const ONX_MSG_EXT_SIGN_V1: DomainTag = DomainTag::from_ascii("ONX_MSG_EXT_SIGN_V1");
/// Domain tag for internal message identity / delivery ID.
pub const ONX_MSG_INT_V1: DomainTag = DomainTag::from_ascii("ONX_MSG_INT_V1");
/// Domain tag for the block's ordered external-message-set commitment.
pub const ONX_MSGS_ROOT_V1: DomainTag = DomainTag::from_ascii("ONX_MSGS_ROOT_V1");
/// Domain tag for key-derived addresses: `address = domain_hash(tag, pubkey)`.
pub const ONX_ADDR_V1: DomainTag = DomainTag::from_ascii("ONX_ADDR_V1");

/// Derive the account address for a public key (ADR-0006).
///
/// A key-derived account's address commits to its key: the first spend
/// reveals a pubkey that must hash to the account's address. An address
/// that was not derived from any key can never be spent from — the reveal
/// can never match.
pub fn derive_address(pubkey: &[u8; 32]) -> AccountId {
    AccountId::from_bytes(domain_hash(&ONX_ADDR_V1, pubkey))
}

/// Message kind discriminant (external messages only; internal messages
/// carry a payload whose emptiness decides execution).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MsgKind {
    /// Plain value transfer. The message bytes must be empty.
    Transfer = 0,
    /// Contract call: `message` carries the inbound payload for the
    /// recipient contract's code, executed by the TVM on delivery.
    ContractCall = 1,
}

impl MsgKind {
    pub fn from_u8(value: u8) -> Result<Self, StfError> {
        match value {
            0 => Ok(Self::Transfer),
            1 => Ok(Self::ContractCall),
            other => Err(StfError::BadMsgKind(other)),
        }
    }
}

/// Maximum message payload bytes. Bounds message size: the payload is fed
/// to contract execution on delivery, so an unbounded payload is a
/// resource-exhaustion vector even before gas accounting.
pub const MAX_MESSAGE_BYTES: usize = 65_535;

/// Canonical byte length of the fixed prefix of one [`ExternalMessage`]
/// body (everything before the variable-length message payload):
/// `chain_id(32) || from(32) || nonce u64be(8) || kind(1) || to(32) ||`
/// `amount_nanos u128be(16) || fee_nanos u128be(16) || msg_len u32be(4)`
/// = 141 bytes. The full body is `141 + msg_len + 32` (trailing pubkey).
pub const EXT_BODY_PREFIX_LEN: usize = 141;

/// An external inbound message ("from nowhere"): an off-chain actor's
/// signed instruction to their own account.
///
/// The wallet handler (STF) verifies `signature` against the sender
/// account's key (or a revealed key for key-derived accounts), checks
/// `nonce`, debits `amount_nanos + fee_nanos`, and queues exactly one
/// [`InternalMessage`]. The signature covers every body field — including
/// `chain_id`, so a message signed for one chain is invalid on any other
/// (ADR-0005) — and including `pubkey`, so a key reveal cannot be swapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalMessage {
    /// Chain identity (genesis hash). Must equal the state's chain ID.
    pub chain_id: [u8; 32],
    pub from: AccountId,
    pub nonce: u64,
    pub kind: MsgKind,
    pub to: AccountId,
    pub amount_nanos: u128,
    pub fee_nanos: u128,
    /// Contract call payload. Must be empty for `Transfer`.
    pub message: Vec<u8>,
    /// Key reveal for the first spend from a key-derived account
    /// (ADR-0006). All zeros otherwise — key rotation is out of scope.
    pub pubkey: [u8; 32],
    pub signature: [u8; 64],
}

impl ExternalMessage {
    /// Canonical encoding of the signed body (big-endian, strict).
    pub fn body_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(EXT_BODY_PREFIX_LEN + self.message.len());
        out.extend_from_slice(&self.chain_id);
        out.extend_from_slice(&self.from.to_bytes());
        out.extend_from_slice(&Uint64(self.nonce).encode());
        out.push(self.kind as u8);
        out.extend_from_slice(&self.to.to_bytes());
        out.extend_from_slice(&Uint128(self.amount_nanos).encode());
        out.extend_from_slice(&Uint128(self.fee_nanos).encode());
        out.extend_from_slice(&(self.message.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.message);
        out.extend_from_slice(&self.pubkey);
        out
    }

    /// Parse exactly one canonical external message body (unsigned part).
    pub fn body_from_bytes(bytes: &[u8]) -> Result<Self, StfError> {
        // Minimum: fixed prefix + empty message + pubkey.
        const MIN_BODY_LEN: usize = EXT_BODY_PREFIX_LEN + 32;
        if bytes.len() < MIN_BODY_LEN {
            return Err(StfError::MalformedMessage {
                expected_len: MIN_BODY_LEN,
                got_len: bytes.len(),
            });
        }
        let mut chain_id = [0u8; 32];
        chain_id.copy_from_slice(&bytes[0..32]);
        let mut from = [0u8; 32];
        from.copy_from_slice(&bytes[32..64]);
        let nonce = u64::from_be_bytes(bytes[64..72].try_into().expect("slice len checked"));
        let kind = MsgKind::from_u8(bytes[72])?;
        let mut to = [0u8; 32];
        to.copy_from_slice(&bytes[73..105]);
        let mut amount_b = [0u8; 16];
        amount_b.copy_from_slice(&bytes[105..121]);
        let mut fee_b = [0u8; 16];
        fee_b.copy_from_slice(&bytes[121..137]);
        let msg_len =
            u32::from_be_bytes(bytes[137..141].try_into().expect("slice len checked")) as usize;
        if msg_len > MAX_MESSAGE_BYTES {
            return Err(StfError::MessageTooLarge { len: msg_len });
        }
        let expected_len = EXT_BODY_PREFIX_LEN + msg_len + 32;
        if bytes.len() != expected_len {
            return Err(StfError::MalformedMessage {
                expected_len,
                got_len: bytes.len(),
            });
        }
        let message = bytes[EXT_BODY_PREFIX_LEN..EXT_BODY_PREFIX_LEN + msg_len].to_vec();
        let mut pubkey = [0u8; 32];
        pubkey.copy_from_slice(&bytes[EXT_BODY_PREFIX_LEN + msg_len..]);
        if kind == MsgKind::Transfer && !message.is_empty() {
            return Err(StfError::MalformedMessage {
                expected_len,
                got_len: bytes.len(),
            });
        }
        Ok(Self {
            chain_id,
            from: AccountId::from_bytes(from),
            nonce,
            kind,
            to: AccountId::from_bytes(to),
            amount_nanos: u128::from_be_bytes(amount_b),
            fee_nanos: u128::from_be_bytes(fee_b),
            message,
            pubkey,
            signature: [0u8; 64],
        })
    }

    /// Build a signed external message. This is the wallet/producer-side
    /// constructor; the STF never signs, it only verifies.
    #[allow(clippy::too_many_arguments)]
    pub fn new_signed(
        chain_id: [u8; 32],
        kind: MsgKind,
        from: AccountId,
        nonce: u64,
        to: AccountId,
        amount_nanos: u128,
        fee_nanos: u128,
        message: Vec<u8>,
        pubkey: [u8; 32],
        secret: &SecretKey,
    ) -> Self {
        assert!(
            message.len() <= MAX_MESSAGE_BYTES,
            "message too large: {} > {MAX_MESSAGE_BYTES}",
            message.len()
        );
        let mut msg = Self {
            chain_id,
            from,
            nonce,
            kind,
            to,
            amount_nanos,
            fee_nanos,
            message,
            pubkey,
            signature: [0u8; 64],
        };
        let sig = secret.sign(&ONX_MSG_EXT_SIGN_V1, &msg.body_bytes());
        msg.signature = sig.encode();
        msg
    }

    /// Verify this message's signature against `pubkey`. Pure
    /// cryptography: no state access. The wallet handler calls this only
    /// after resolving which key authorizes the sender.
    pub fn verify_signature(&self, pubkey: &PublicKey) -> Result<(), StfError> {
        let sig =
            Signature::decode_exact(&self.signature).map_err(|_| StfError::InvalidSignature)?;
        pubkey
            .verify(&ONX_MSG_EXT_SIGN_V1, &self.body_bytes(), &sig)
            .map_err(|_| StfError::InvalidSignature)
    }

    /// Canonical wire encoding: body followed by the 64-byte signature.
    pub fn to_bytes(&self) -> Vec<u8> {
        let body = self.body_bytes();
        let mut out = Vec::with_capacity(body.len() + Signature::BYTE_LEN);
        out.extend_from_slice(&body);
        out.extend_from_slice(&self.signature);
        out
    }

    /// Parse exactly one canonical external message. Rejects wrong lengths
    /// and trailing bytes. Parsing does NOT verify the signature — that is
    /// the wallet handler's job.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StfError> {
        // Minimum: fixed prefix + empty message + pubkey + signature.
        const MIN_WIRE_LEN: usize = EXT_BODY_PREFIX_LEN + 32 + Signature::BYTE_LEN;
        if bytes.len() < MIN_WIRE_LEN {
            return Err(StfError::MalformedMessage {
                expected_len: MIN_WIRE_LEN,
                got_len: bytes.len(),
            });
        }
        let body_len = bytes.len() - Signature::BYTE_LEN;
        let mut msg = Self::body_from_bytes(&bytes[..body_len])?;
        msg.signature.copy_from_slice(&bytes[body_len..]);
        Ok(msg)
    }

    /// Domain-separated hash of the wire encoding: the message's identity.
    /// Also serves as the `origin` anchor for the internal message the
    /// wallet handler derives from this external.
    pub fn hash(&self) -> [u8; 32] {
        domain_hash(&ONX_MSG_EXT_V1, &self.to_bytes())
    }
}

/// An internal message: account to account, derived deterministically
/// during block execution, never submitted.
///
/// Created by the wallet handler (one per external message) and by bounce
/// logic. Delivery executes as the receiver's own transaction: value
/// credit, plus TVM execution when `payload` is non-empty and the receiver
/// has contract code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalMessage {
    pub src: AccountId,
    pub dest: AccountId,
    pub value_nanos: u128,
    /// Fee budget carried for gas accounting on contract calls. The fee
    /// itself was debited and split at the wallet handler; this field only
    /// bounds VM execution (`gas_limit = fee_nanos * GAS_PER_NANO`).
    pub fee_nanos: u128,
    /// Contract call payload. Empty = plain value transfer (no VM run,
    /// even if the receiver has code).
    pub payload: Vec<u8>,
    /// True for bounce messages (value returned to the original sender).
    /// A bounce is never itself bounced: if its destination cannot
    /// receive, the value is burned (defensive; unreachable in practice —
    /// senders are Active at auth time and nothing freezes accounts
    /// mid-block in this milestone).
    pub is_bounce: bool,
    /// Uniqueness anchor: the external message hash for wallet-emitted
    /// messages, the bounced message's ID for bounces. Two different
    /// externals can never derive the same internal ID.
    pub origin: [u8; 32],
}

impl InternalMessage {
    /// Canonical encoding (big-endian, strict):
    /// `src(32) || dest(32) || value u128be(16) || fee u128be(16) ||`
    /// `payload_len u32be(4) || payload || is_bounce(1) || origin(32)`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + 32 + 16 + 16 + 4 + self.payload.len() + 1 + 32);
        out.extend_from_slice(&self.src.to_bytes());
        out.extend_from_slice(&self.dest.to_bytes());
        out.extend_from_slice(&Uint128(self.value_nanos).encode());
        out.extend_from_slice(&Uint128(self.fee_nanos).encode());
        out.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.payload);
        out.push(u8::from(self.is_bounce));
        out.extend_from_slice(&self.origin);
        out
    }

    /// The delivery ID: domain-separated hash of the canonical encoding.
    /// Checked against the per-block processed set at delivery —
    /// double delivery is impossible (ADR-0007).
    pub fn id(&self) -> [u8; 32] {
        domain_hash(&ONX_MSG_INT_V1, &self.to_bytes())
    }
}

/// Commitment to the ordered external-message set of a block.
///
/// `domain_hash(ONX_MSGS_ROOT_V1, msg[0].hash() || msg[1].hash() || ...)`.
/// The empty body commits to `domain_hash(ONX_MSGS_ROOT_V1, b"")`.
pub fn msgs_root(messages: &[ExternalMessage]) -> [u8; 32] {
    let mut preimage = Vec::with_capacity(messages.len() * 32);
    for msg in messages {
        preimage.extend_from_slice(&msg.hash());
    }
    domain_hash(&ONX_MSGS_ROOT_V1, &preimage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_chain_id() -> [u8; 32] {
        [0xAA; 32]
    }

    fn test_secret() -> SecretKey {
        SecretKey::from_seed(&[0x42; 32]).unwrap()
    }

    #[test]
    fn external_message_encoding_is_canonical() {
        let msg = ExternalMessage::new_signed(
            test_chain_id(),
            MsgKind::Transfer,
            AccountId::from_bytes([1u8; 32]),
            7,
            AccountId::from_bytes([2u8; 32]),
            1_000,
            10,
            Vec::new(),
            [0u8; 32],
            &test_secret(),
        );
        let bytes = msg.to_bytes();
        // Transfer with empty message: 141-byte prefix + 32-byte pubkey + 64-byte signature.
        assert_eq!(bytes.len(), EXT_BODY_PREFIX_LEN + 32 + 64);
        assert_eq!(bytes.len(), 237);
        assert_eq!(
            &bytes[..EXT_BODY_PREFIX_LEN],
            &msg.body_bytes()[..EXT_BODY_PREFIX_LEN]
        );
        // Field offsets: chain_id 0..32, from 32..64, nonce 64..72,
        // kind 72, to 73..105, amount 105..121, fee 121..137,
        // msg_len 137..141, pubkey 141..173.
        assert_eq!(&bytes[0..32], &test_chain_id());
        assert_eq!(u64::from_be_bytes(bytes[64..72].try_into().unwrap()), 7);
        assert_eq!(bytes[72], 0x00);
        assert_eq!(
            u128::from_be_bytes(bytes[105..121].try_into().unwrap()),
            1_000
        );
        let back = ExternalMessage::from_bytes(&bytes).unwrap();
        assert_eq!(back, msg);
        assert!(ExternalMessage::from_bytes(&bytes[..bytes.len() - 1]).is_err());
        let mut long = bytes.clone();
        long.push(0);
        assert!(ExternalMessage::from_bytes(&long).is_err());
        assert_eq!(back.hash(), msg.hash());
    }

    #[test]
    fn signature_binds_chain_id() {
        let secret = test_secret();
        let pubkey = secret.public_key();
        let msg = ExternalMessage::new_signed(
            test_chain_id(),
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
        assert!(msg.verify_signature(&pubkey).is_ok());
        // Same bytes, different chain: the signature does not verify
        // because chain_id is inside the signed body.
        let mut evil = msg.clone();
        evil.chain_id = [0xBB; 32];
        assert!(evil.verify_signature(&pubkey).is_err());
    }

    #[test]
    fn transfer_with_payload_rejected_at_parse() {
        let mut msg = ExternalMessage::new_signed(
            test_chain_id(),
            MsgKind::Transfer,
            AccountId::from_bytes([1u8; 32]),
            0,
            AccountId::from_bytes([2u8; 32]),
            100,
            1,
            Vec::new(),
            [0u8; 32],
            &test_secret(),
        );
        msg.kind = MsgKind::ContractCall;
        msg.message = b"hi".to_vec();
        // Re-encode as transfer-with-payload by hand: kind byte 0, msg_len 2.
        let mut bytes = msg.to_bytes();
        bytes[72] = 0x00;
        assert!(ExternalMessage::from_bytes(&bytes).is_err());
    }

    #[test]
    fn internal_message_id_is_stable_and_unique() {
        let base = InternalMessage {
            src: AccountId::from_bytes([1u8; 32]),
            dest: AccountId::from_bytes([2u8; 32]),
            value_nanos: 100,
            fee_nanos: 1,
            payload: Vec::new(),
            is_bounce: false,
            origin: [0x11; 32],
        };
        let id1 = base.id();
        assert_eq!(id1, base.id());
        // Different origin -> different id.
        let mut other = base.clone();
        other.origin = [0x12; 32];
        assert_ne!(other.id(), id1);
        // Bounce flag flips the id.
        let mut bounced = base.clone();
        bounced.is_bounce = true;
        assert_ne!(bounced.id(), id1);
    }

    #[test]
    fn derive_address_is_deterministic() {
        let pk = [0x42u8; 32];
        assert_eq!(derive_address(&pk), derive_address(&pk));
        let mut other = pk;
        other[0] ^= 0xff;
        assert_ne!(derive_address(&other), derive_address(&pk));
        assert_ne!(derive_address(&[0u8; 32]), derive_address(&pk));
    }
}
