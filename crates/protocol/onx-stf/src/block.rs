use crate::error::StfError;
use onx_data_structures::AccountId;
use onx_primitives::{domain_hash, DomainTag, Int32, Uint128, Uint32, Uint64};

/// Domain tag for a single canonical transaction encoding.
pub const ONX_TX_V1: DomainTag = DomainTag::from_ascii("ONX_TX_V1");
/// Domain tag for the ordered transaction-set commitment in a block header.
pub const ONX_TXS_ROOT_V1: DomainTag = DomainTag::from_ascii("ONX_TXS_ROOT_V1");
/// Domain tag for a canonical block header encoding.
pub const ONX_BLOCK_HDR_V1: DomainTag = DomainTag::from_ascii("ONX_BLOCK_HDR_V1");

/// Canonical byte length of one [`Transaction`]:
/// `from(32) || to(32) || amount_nanos u128be(16) || fee_nanos u128be(16)`.
pub const TRANSACTION_BYTE_LEN: usize = 96;

/// A plain Onyx value transfer with an explicit fee.
///
/// `fee_nanos` may be zero. There is no minimum fee yet — the fee market is
/// deferred; what matters for replay is that the declared fee is applied
/// deterministically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transaction {
    pub from: AccountId,
    pub to: AccountId,
    pub amount_nanos: u128,
    pub fee_nanos: u128,
}

impl Transaction {
    /// Canonical encoding (`TRANSACTION_BYTE_LEN` bytes, big-endian).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(TRANSACTION_BYTE_LEN);
        out.extend_from_slice(&self.from.to_bytes());
        out.extend_from_slice(&self.to.to_bytes());
        out.extend_from_slice(&Uint128(self.amount_nanos).encode());
        out.extend_from_slice(&Uint128(self.fee_nanos).encode());
        out
    }

    /// Parse exactly one canonical transaction. Rejects wrong lengths and
    /// trailing bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StfError> {
        if bytes.len() != TRANSACTION_BYTE_LEN {
            return Err(StfError::MalformedTransaction {
                expected_len: TRANSACTION_BYTE_LEN,
                got_len: bytes.len(),
            });
        }
        let mut from = [0u8; 32];
        from.copy_from_slice(&bytes[0..32]);
        let mut to = [0u8; 32];
        to.copy_from_slice(&bytes[32..64]);
        let mut amount_b = [0u8; 16];
        amount_b.copy_from_slice(&bytes[64..80]);
        let mut fee_b = [0u8; 16];
        fee_b.copy_from_slice(&bytes[80..96]);
        Ok(Self {
            from: AccountId::from_bytes(from),
            to: AccountId::from_bytes(to),
            amount_nanos: u128::from_be_bytes(amount_b),
            fee_nanos: u128::from_be_bytes(fee_b),
        })
    }

    /// Domain-separated hash of the canonical encoding: the transaction's identity.
    pub fn hash(&self) -> [u8; 32] {
        domain_hash(&ONX_TX_V1, &self.to_bytes())
    }
}

/// Commitment to the ordered transaction set of a block.
///
/// `domain_hash(ONX_TXS_ROOT_V1, tx[0].hash() || tx[1].hash() || ...)`.
/// The empty body commits to `domain_hash(ONX_TXS_ROOT_V1, b"")` — still a
/// well-defined, deterministic value.
///
/// Upgrade path (documented, not implemented): this may later become a
/// Merkle root over the transaction hashes for light-client proofs. That
/// change MUST use a new domain tag (`ONX_TXS_ROOT_V2`) so old and new
/// commitments can never collide.
pub fn txs_root(transactions: &[Transaction]) -> [u8; 32] {
    let mut preimage = Vec::with_capacity(transactions.len() * 32);
    for tx in transactions {
        preimage.extend_from_slice(&tx.hash());
    }
    domain_hash(&ONX_TXS_ROOT_V1, &preimage)
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
/// Canonical encoding (145 bytes, big-endian):
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
    pub fn assemble(
        seqno: u32,
        prev_hash: [u8; 32],
        lt: u64,
        workchain: i32,
        fee_collector: AccountId,
        transactions: Vec<Transaction>,
        state_root: [u8; 32],
    ) -> Self {
        let tx_count = transactions.len() as u32;
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
        Self { header, body }
    }
}
