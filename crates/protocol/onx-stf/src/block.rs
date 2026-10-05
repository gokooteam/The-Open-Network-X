use crate::error::StfError;
use onx_data_structures::AccountId;
use onx_primitives::{
    domain_hash, DomainTag, Int32, PublicKey, SecretKey, Signature, Uint128, Uint32, Uint64,
};

/// Domain tag for a single canonical V3 transaction encoding (tx identity).
pub const ONX_TX_V3: DomainTag = DomainTag::from_ascii("ONX_TX_V3");
/// Domain tag for the transaction signature payload. The signed message is
/// `tag || body_bytes`; a signature produced for any other domain will not
/// verify here, even over an identical body.
pub const ONX_TX_V3_SIGN: DomainTag = DomainTag::from_ascii("ONX_TX_V3_SIGN");
/// Domain tag for the ordered transaction-set commitment in a block header.
/// The construction is unchanged (concatenation of tx hashes), but the tag
/// is bumped because tx identity itself moved to `ONX_TX_V3` — a V2-era
/// root can never collide with a V3 root.
///
/// V1 (`ONX_TX_V1` / `ONX_TXS_ROOT_V1`) and V2 (`ONX_TX_V2` /
/// `ONX_TX_V2_SIGN` / `ONX_TXS_ROOT_V2`) are dropped entirely: neither ever
/// shipped anywhere (pre-release milestone, all uncommitted), so there is
/// no chain to migrate and no reason to keep superseded transaction formats
/// alive. One canonical transaction format, not an archaeological layer cake.
pub const ONX_TXS_ROOT_V3: DomainTag = DomainTag::from_ascii("ONX_TXS_ROOT_V3");
/// Domain tag for a canonical block header encoding (unchanged by the
/// tx-auth upgrade: the header commits to tx hashes, not tx bodies).
pub const ONX_BLOCK_HDR_V1: DomainTag = DomainTag::from_ascii("ONX_BLOCK_HDR_V1");

/// Transaction kind discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TxKind {
    /// Plain value transfer. The message must be empty.
    Transfer = 0,
    /// Contract call: `message` carries the inbound message bytes for the
    /// recipient contract's code, executed by the TVM in `apply_tx`.
    ContractCall = 1,
}

impl TxKind {
    pub fn from_u8(value: u8) -> Result<Self, StfError> {
        match value {
            0 => Ok(Self::Transfer),
            1 => Ok(Self::ContractCall),
            other => Err(StfError::BadTxKind(other)),
        }
    }
}

/// Maximum inbound message bytes on a contract call. Bounds transaction
/// size: the message is fed to contract execution, so an unbounded message
/// is a resource-exhaustion vector even before gas accounting.
pub const MAX_MESSAGE_BYTES: usize = 65_535;

/// Canonical byte length of the fixed prefix of one [`Transaction`] *body*
/// (everything before the variable-length message):
/// `kind(1) || from(32) || to(32) || amount_nanos u128be(16) ||`
/// `fee_nanos u128be(16) || nonce u64be(8) || msg_len u32be(4)` = 109 bytes.
pub const TRANSACTION_BODY_PREFIX_LEN: usize = 109;

/// A signed Onyx transaction: value transfer, optionally carrying a
/// contract message.
///
/// Authorization model (unchanged from V2):
/// - `from` must be an `Active` account carrying a non-zero Ed25519 pubkey.
/// - `nonce` must equal the account's current nonce; it increments on every
///   successful spend, so a signed transaction can never be replayed and
///   nonces cannot skip.
/// - `signature` is Ed25519 over `ONX_TX_V3_SIGN || body_bytes`, i.e. it
///   covers every field except itself — including the kind byte and the
///   full message. Verification failure makes the transaction invalid,
///   which makes the block invalid (fail-closed).
///
/// Kind semantics:
/// - `Transfer` with an empty message is the plain value transfer.
///   (A `Transfer` carrying a non-empty message is malformed — rejected at
///   parse, never silently downgraded.)
/// - `ContractCall` routes `message` into the recipient contract's TVM code
///   in `apply_tx`. The recipient must be an `Active` account carrying code;
///   VM failure makes the transaction invalid, which makes the block
///   invalid (fail-closed, reverting all of the transaction's effects).
///
/// `fee_nanos` may be zero for transfers. For contract calls the fee buys
/// execution gas (`gas_limit = fee_nanos * GAS_PER_NANO` in the STF), so a
/// zero-fee contract call gets zero gas and fails closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    pub kind: TxKind,
    pub from: AccountId,
    pub to: AccountId,
    pub amount_nanos: u128,
    pub fee_nanos: u128,
    pub nonce: u64,
    pub message: Vec<u8>,
    pub signature: [u8; 64],
}

impl Transaction {
    /// Canonical encoding of the signed body: the exact bytes covered by
    /// the signature (big-endian, strict).
    pub fn body_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(TRANSACTION_BODY_PREFIX_LEN + self.message.len());
        out.push(self.kind as u8);
        out.extend_from_slice(&self.from.to_bytes());
        out.extend_from_slice(&self.to.to_bytes());
        out.extend_from_slice(&Uint128(self.amount_nanos).encode());
        out.extend_from_slice(&Uint128(self.fee_nanos).encode());
        out.extend_from_slice(&Uint64(self.nonce).encode());
        out.extend_from_slice(&(self.message.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.message);
        out
    }

    /// Parse exactly one canonical transaction body (unsigned part).
    /// Rejects bad kind bytes, over-long messages, `Transfer` with a
    /// non-empty message, and trailing bytes.
    pub fn body_from_bytes(bytes: &[u8]) -> Result<Self, StfError> {
        if bytes.len() < TRANSACTION_BODY_PREFIX_LEN {
            return Err(StfError::MalformedTransaction {
                expected_len: TRANSACTION_BODY_PREFIX_LEN,
                got_len: bytes.len(),
            });
        }
        let kind = TxKind::from_u8(bytes[0])?;
        let mut from = [0u8; 32];
        from.copy_from_slice(&bytes[1..33]);
        let mut to = [0u8; 32];
        to.copy_from_slice(&bytes[33..65]);
        let mut amount_b = [0u8; 16];
        amount_b.copy_from_slice(&bytes[65..81]);
        let mut fee_b = [0u8; 16];
        fee_b.copy_from_slice(&bytes[81..97]);
        let mut nonce_b = [0u8; 8];
        nonce_b.copy_from_slice(&bytes[97..105]);
        let msg_len =
            u32::from_be_bytes(bytes[105..109].try_into().expect("slice len checked")) as usize;
        if msg_len > MAX_MESSAGE_BYTES {
            return Err(StfError::MessageTooLarge { len: msg_len });
        }
        if bytes.len() != TRANSACTION_BODY_PREFIX_LEN + msg_len {
            return Err(StfError::MalformedTransaction {
                expected_len: TRANSACTION_BODY_PREFIX_LEN + msg_len,
                got_len: bytes.len(),
            });
        }
        let message = bytes[TRANSACTION_BODY_PREFIX_LEN..].to_vec();
        if kind == TxKind::Transfer && !message.is_empty() {
            return Err(StfError::MalformedTransaction {
                expected_len: TRANSACTION_BODY_PREFIX_LEN,
                got_len: bytes.len(),
            });
        }
        Ok(Self {
            kind,
            from: AccountId::from_bytes(from),
            to: AccountId::from_bytes(to),
            amount_nanos: u128::from_be_bytes(amount_b),
            fee_nanos: u128::from_be_bytes(fee_b),
            nonce: u64::from_be_bytes(nonce_b),
            message,
            signature: [0u8; 64],
        })
    }

    /// Build a signed transfer: assemble the body, sign
    /// `ONX_TX_V3_SIGN || body_bytes` with `secret`, attach the signature.
    ///
    /// This is the wallet/producer-side constructor. The STF never signs;
    /// it only verifies.
    pub fn new_signed(
        from: AccountId,
        to: AccountId,
        amount_nanos: u128,
        fee_nanos: u128,
        nonce: u64,
        secret: &SecretKey,
    ) -> Self {
        Self::new_signed_kind(
            TxKind::Transfer,
            from,
            to,
            amount_nanos,
            fee_nanos,
            nonce,
            Vec::new(),
            secret,
        )
    }

    /// Build a signed contract call carrying `message` bytes for the
    /// recipient contract. Panics on over-long messages — this is the
    /// producer side, and a message that cannot be encoded must never be
    /// signed (fail fast here, fail closed in the STF).
    pub fn new_signed_call(
        from: AccountId,
        to: AccountId,
        amount_nanos: u128,
        fee_nanos: u128,
        nonce: u64,
        message: Vec<u8>,
        secret: &SecretKey,
    ) -> Self {
        assert!(
            message.len() <= MAX_MESSAGE_BYTES,
            "contract message too large: {} > {MAX_MESSAGE_BYTES}",
            message.len()
        );
        Self::new_signed_kind(
            TxKind::ContractCall,
            from,
            to,
            amount_nanos,
            fee_nanos,
            nonce,
            message,
            secret,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_signed_kind(
        kind: TxKind,
        from: AccountId,
        to: AccountId,
        amount_nanos: u128,
        fee_nanos: u128,
        nonce: u64,
        message: Vec<u8>,
        secret: &SecretKey,
    ) -> Self {
        let mut tx = Self {
            kind,
            from,
            to,
            amount_nanos,
            fee_nanos,
            nonce,
            message,
            signature: [0u8; 64],
        };
        let sig = secret.sign(&ONX_TX_V3_SIGN, &tx.body_bytes());
        tx.signature = sig.encode();
        tx
    }

    /// Verify this transaction's signature against `pubkey`.
    ///
    /// Pure cryptography: no state access. The STF calls this only after
    /// establishing that the sender account exists, is `Active`, and
    /// carries a non-zero pubkey — the key-absence check lives there,
    /// not here.
    pub fn verify_signature(&self, pubkey: &PublicKey) -> Result<(), StfError> {
        let sig =
            Signature::decode_exact(&self.signature).map_err(|_| StfError::InvalidSignature)?;
        pubkey
            .verify(&ONX_TX_V3_SIGN, &self.body_bytes(), &sig)
            .map_err(|_| StfError::InvalidSignature)
    }

    /// Canonical encoding: body followed by the 64-byte signature.
    /// Variable length: `body_bytes().len() + 64`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let body = self.body_bytes();
        let mut out = Vec::with_capacity(body.len() + Signature::BYTE_LEN);
        out.extend_from_slice(&body);
        out.extend_from_slice(&self.signature);
        out
    }

    /// Parse exactly one canonical transaction. Rejects wrong lengths and
    /// trailing bytes. Note: parsing does NOT verify the signature —
    /// that is the STF's job (`apply_tx`), with the sender's on-chain
    /// pubkey as the trust anchor.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StfError> {
        if bytes.len() < TRANSACTION_BODY_PREFIX_LEN + Signature::BYTE_LEN {
            return Err(StfError::MalformedTransaction {
                expected_len: TRANSACTION_BODY_PREFIX_LEN + Signature::BYTE_LEN,
                got_len: bytes.len(),
            });
        }
        let body_len = bytes.len() - Signature::BYTE_LEN;
        let mut tx = Self::body_from_bytes(&bytes[..body_len])?;
        tx.signature.copy_from_slice(&bytes[body_len..]);
        Ok(tx)
    }

    /// Domain-separated hash of the canonical encoding: the transaction's identity.
    pub fn hash(&self) -> [u8; 32] {
        domain_hash(&ONX_TX_V3, &self.to_bytes())
    }
}

/// Commitment to the ordered transaction set of a block.
///
/// `domain_hash(ONX_TXS_ROOT_V3, tx[0].hash() || tx[1].hash() || ...)`.
/// The empty body commits to `domain_hash(ONX_TXS_ROOT_V3, b"")` — still a
/// well-defined, deterministic value.
///
/// Upgrade path (documented, not implemented): this may later become a
/// Merkle root over the transaction hashes for light-client proofs. That
/// change MUST use a new domain tag (`ONX_TXS_ROOT_V4`) so old and new
/// commitments can never collide.
pub fn txs_root(transactions: &[Transaction]) -> [u8; 32] {
    let mut preimage = Vec::with_capacity(transactions.len() * 32);
    for tx in transactions {
        preimage.extend_from_slice(&tx.hash());
    }
    domain_hash(&ONX_TXS_ROOT_V3, &preimage)
}

/// A block body: the ordered list of transactions.
///
/// Order is consensus-critical: transactions apply strictly in vector
/// order, and `txs_root` commits to that order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BlockBody {
    pub transactions: Vec<Transaction>,
}

impl BlockBody {
    pub fn txs_root(&self) -> [u8; 32] {
        txs_root(&self.transactions)
    }
}

/// A block header.
///
/// Canonical encoding (148 bytes, big-endian):
/// `seqno u32be(4) || prev_hash(32) || txs_root(32) || state_root(32) ||
///  lt u64be(8) || workchain i32be(4) || fee_collector(32) || tx_count u32be(4)`
/// = 4+32+32+32+8+4+32+4 = 148 bytes.
///
/// Fields:
/// - `state_root`: the *claimed* post-state root. The STF recomputes it and
///   rejects the block on mismatch — a block cannot lie about its result.
/// - `lt`: block logical time, strictly increasing across the chain.
/// - `fee_collector`: account credited with the validator half of fees.
///   Explicit in the header so the STF stays pure (no validator-set lookup).
/// - `tx_count`: redundant with the body but committed in the header so a
///   truncated body cannot pass the `txs_root` check by accident... (it
///   can't anyway — `txs_root` covers the full ordered set; `tx_count` is
///   carried for tooling convenience and checked for consistency).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHeader {
    pub seqno: u32,
    pub prev_hash: [u8; 32],
    pub txs_root: [u8; 32],
    pub state_root: [u8; 32],
    pub lt: u64,
    pub workchain: i32,
    pub fee_collector: AccountId,
    pub tx_count: u32,
}

/// Canonical byte length of one [`BlockHeader`].
pub const BLOCK_HEADER_BYTE_LEN: usize = 148;

impl BlockHeader {
    /// Canonical 148-byte encoding.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(BLOCK_HEADER_BYTE_LEN);
        out.extend_from_slice(&Uint32(self.seqno).encode());
        out.extend_from_slice(&self.prev_hash);
        out.extend_from_slice(&self.txs_root);
        out.extend_from_slice(&self.state_root);
        out.extend_from_slice(&Uint64(self.lt).encode());
        out.extend_from_slice(&Int32(self.workchain).encode());
        out.extend_from_slice(&self.fee_collector.to_bytes());
        out.extend_from_slice(&Uint32(self.tx_count).encode());
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
        let u32_at = |o: usize| u32::from_be_bytes(bytes[o..o + 4].try_into().unwrap());
        let u64_at = |o: usize| u64::from_be_bytes(bytes[o..o + 8].try_into().unwrap());
        let i32_at = |o: usize| i32::from_be_bytes(bytes[o..o + 4].try_into().unwrap());
        let h32_at = |o: usize| {
            let mut h = [0u8; 32];
            h.copy_from_slice(&bytes[o..o + 32]);
            h
        };
        Ok(Self {
            seqno: u32_at(0),
            prev_hash: h32_at(4),
            txs_root: h32_at(36),
            state_root: h32_at(68),
            lt: u64_at(100),
            workchain: i32_at(108),
            fee_collector: AccountId::from_bytes(h32_at(112)),
            tx_count: u32_at(144),
        })
    }

    /// Domain-separated hash of the canonical encoding: the block's identity.
    /// Chained via `prev_hash` — this is what makes the chain a chain.
    pub fn hash(&self) -> [u8; 32] {
        domain_hash(&ONX_BLOCK_HDR_V1, &self.to_bytes())
    }
}

/// Checked `usize -> u32` conversion for the header `tx_count`.
///
/// A `Vec<Transaction>` longer than `u32::MAX` cannot exist in memory, so
/// this is defense in depth: if it ever fired, silently truncating with
/// `as u32` would commit a header whose `tx_count` disagrees with the body
/// it commits to. Fail closed instead.
fn checked_tx_count(n: usize) -> Result<u32, StfError> {
    u32::try_from(n).map_err(|_| StfError::TooManyTransactions { count: n })
}

/// A full block: header (commitments) plus body (the committed transactions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub header: BlockHeader,
    pub body: BlockBody,
}

impl Block {
    /// Assemble a block, computing `txs_root` and `tx_count` from the body.
    /// The caller must fill `state_root` — see [`crate::stf::apply_block`],
    /// which verifies it.
    ///
    /// Fails closed with [`StfError::TooManyTransactions`] if the body
    /// holds more transactions than fit in a `u32` (unreachable in
    /// practice; the check exists so the invariant is explicit rather
    /// than a silent truncation).
    pub fn assemble(
        seqno: u32,
        prev_hash: [u8; 32],
        lt: u64,
        workchain: i32,
        fee_collector: AccountId,
        transactions: Vec<Transaction>,
        state_root: [u8; 32],
    ) -> Result<Self, StfError> {
        let tx_count = checked_tx_count(transactions.len())?;
        let body = BlockBody { transactions };
        let header = BlockHeader {
            seqno,
            prev_hash,
            txs_root: body.txs_root(),
            state_root,
            lt,
            workchain,
            fee_collector,
            tx_count,
        };
        Ok(Self { header, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tx_count_checked_conversion_fails_closed() {
        assert_eq!(checked_tx_count(0), Ok(0));
        assert_eq!(checked_tx_count(1), Ok(1));
        assert_eq!(checked_tx_count(u32::MAX as usize), Ok(u32::MAX));
        // A Vec this long cannot be built in memory, so the conversion
        // itself is the unit under test: it must error, never wrap.
        let too_big = usize::try_from(u64::from(u32::MAX) + 1).unwrap();
        assert_eq!(
            checked_tx_count(too_big),
            Err(StfError::TooManyTransactions { count: too_big })
        );
    }

    #[test]
    fn v3_transaction_encoding_is_canonical() {
        let secret = SecretKey::from_seed(&[0x42; 32]).unwrap();
        let tx = Transaction::new_signed(
            AccountId::from_bytes([1u8; 32]),
            AccountId::from_bytes([2u8; 32]),
            1_000,
            10,
            7,
            &secret,
        );
        let bytes = tx.to_bytes();
        // V3 transfer with empty message: 109-byte body + 64-byte signature.
        assert_eq!(bytes.len(), TRANSACTION_BODY_PREFIX_LEN + 64);
        assert_eq!(bytes.len(), 173);
        // Body is a strict prefix of the encoding.
        assert_eq!(&bytes[..TRANSACTION_BODY_PREFIX_LEN], tx.body_bytes());
        let back = Transaction::from_bytes(&bytes).unwrap();
        assert_eq!(back, tx);
        // Kind byte first, then nonce at offset 97..105, msg_len at 105..109.
        assert_eq!(bytes[0], 0x00); // Transfer
        assert_eq!(u64::from_be_bytes(bytes[97..105].try_into().unwrap()), 7);
        assert_eq!(u32::from_be_bytes(bytes[105..109].try_into().unwrap()), 0);
        // Wrong lengths rejected.
        assert!(Transaction::from_bytes(&bytes[..172]).is_err());
        let mut long = bytes.clone();
        long.push(0);
        assert!(Transaction::from_bytes(&long).is_err());
        // V2-length input (168 bytes) is not a valid V3 transaction
        // (the msg_len field would overrun).
        assert!(Transaction::from_bytes(&bytes[..168]).is_err());
        // Bad kind byte rejected.
        let mut bad_kind = bytes.clone();
        bad_kind[0] = 0x02;
        assert!(Transaction::from_bytes(&bad_kind).is_err());
    }

    #[test]
    fn contract_call_encoding_carries_message() {
        let secret = SecretKey::from_seed(&[0x42; 32]).unwrap();
        let msg = b"increment".to_vec();
        let tx = Transaction::new_signed_call(
            AccountId::from_bytes([1u8; 32]),
            AccountId::from_bytes([2u8; 32]),
            1_000,
            10,
            7,
            msg.clone(),
            &secret,
        );
        assert_eq!(tx.kind, crate::block::TxKind::ContractCall);
        let bytes = tx.to_bytes();
        assert_eq!(bytes.len(), TRANSACTION_BODY_PREFIX_LEN + msg.len() + 64);
        let back = Transaction::from_bytes(&bytes).unwrap();
        assert_eq!(back, tx);
        assert_eq!(back.message, msg);
        // Transfer with a non-empty message is malformed, not downgraded.
        let mut evil = tx.clone();
        evil.kind = crate::block::TxKind::Transfer;
        assert!(Transaction::from_bytes(&evil.to_bytes()).is_err());
    }

    #[test]
    fn signature_verifies_and_tampering_fails() {
        let secret = SecretKey::from_seed(&[0x42; 32]).unwrap();
        let pubkey = secret.public_key();
        let tx = Transaction::new_signed(
            AccountId::from_bytes([1u8; 32]),
            AccountId::from_bytes([2u8; 32]),
            1_000,
            10,
            0,
            &secret,
        );
        assert!(tx.verify_signature(&pubkey).is_ok());
        // Wrong key.
        let other = SecretKey::from_seed(&[0x43; 32]).unwrap().public_key();
        assert!(tx.verify_signature(&other).is_err());
        // Tampered amount: signature is over the original body.
        let mut evil = tx.clone();
        evil.amount_nanos = 1_001;
        assert!(evil.verify_signature(&pubkey).is_err());
        // Tampered nonce.
        let mut evil = tx.clone();
        evil.nonce = 1;
        assert!(evil.verify_signature(&pubkey).is_err());
        // Tampered signature bytes.
        let mut evil = tx.clone();
        evil.signature[0] ^= 0xff;
        assert!(evil.verify_signature(&pubkey).is_err());
        // Tampered kind: signature covers the kind byte.
        let mut evil = tx.clone();
        evil.kind = crate::block::TxKind::ContractCall;
        assert!(evil.verify_signature(&pubkey).is_err());
    }
}
