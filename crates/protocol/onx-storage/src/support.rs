//! Deterministic test-chain scaffolding, shared by the crash probe binary
//! (`onx-crash-probe`) and the Phase 4 integration tests.
//!
//! This is NOT consensus code: it exists so the probe and the tests generate
//! byte-identical chains from the same `(seed, seqno)`. Message
//! generation for block `n` depends only on `(seed, n, chain_id)` — never on process
//! state — so a test can regenerate exactly the block a killed probe was
//! committing and resume from the head pointer.

use onx_data_structures::{AccountId, ShardIdent, WorkchainIdent};
use onx_primitives::{domain_hash, DomainTag, SecretKey};
use onx_state_model::{AccountState, GenesisDocument, GenesisValidator, StorageStat};
use onx_stf::{ExternalMessage, MsgKind};
use std::collections::BTreeMap;

/// Domain tag for deriving test signing keys from account ids.
///
/// TEST-ONLY: the preimage is public, so these keys are not secret. This
/// exists so `test_block_txs` stays a pure function of `(seed, seqno)` —
/// no key map needs threading through every call site. Never use this
/// pattern for real keys.
pub const ONX_TEST_KEY_V1: DomainTag = DomainTag::from_ascii("ONX_TEST_KEY_V1");

/// Number of funded accounts in the test genesis.
pub const TEST_ACCOUNT_COUNT: usize = 8;
/// Genesis balance per account (nanos) — vastly larger than any test spend.
pub const TEST_GENESIS_BALANCE: u128 = 1_000_000_000;
/// Messages per test block.
pub const TEST_TXS_PER_BLOCK: usize = 8;

/// Deterministic fee-collector account for test blocks.
pub fn test_fee_collector() -> AccountId {
    AccountId::from_bytes([0xCC; 32])
}

/// Deterministic signing key for a test account id.
///
/// TEST-ONLY (see `ONX_TEST_KEY_V1`): derived from the account id itself
/// so transaction generation needs no external key state.
pub fn test_secret_key(id: &AccountId) -> SecretKey {
    let seed = domain_hash(&ONX_TEST_KEY_V1, &id.to_bytes());
    SecretKey::from_seed(&seed).expect("domain hash output is a valid seed")
}

/// Deterministic test genesis: 8 funded accounts on the basic workchain.
///
/// Every account carries the pubkey matching [`test_secret_key`], so the
/// generated transactions actually authorize.
pub fn test_genesis() -> GenesisDocument {
    let workchain = WorkchainIdent::BASIC;
    let shard = ShardIdent::root(workchain);
    let validators = vec![GenesisValidator {
        pubkey: [0xA5; 32],
        stake: 1_000,
    }];
    let mut accounts = BTreeMap::new();
    for i in 0..TEST_ACCOUNT_COUNT as u8 {
        let mut id = [0u8; 32];
        id[0] = i;
        let id = AccountId::from_bytes(id);
        let pubkey = test_secret_key(&id).public_key().encode();
        accounts.insert(
            id,
            AccountState::Active {
                balance_nanos: TEST_GENESIS_BALANCE,
                last_trans_lt: 0,
                code: None,
                data: None,
                storage_stat: StorageStat {
                    cell_count: 0,
                    byte_count: 0,
                },
                pubkey,
                nonce: 0,
            },
        );
    }
    GenesisDocument::new(workchain, shard, validators, accounts).expect("test genesis is valid")
}

/// Account ids of the test genesis, in ascending order.
pub fn test_accounts() -> Vec<AccountId> {
    test_genesis().accounts.keys().cloned().collect()
}

/// Deterministic external messages for block `seqno`: a pure function of
/// `(seed, seqno, chain_id)`. Amounts are tiny relative to genesis balances
/// and fees are small, so every generated block is always valid against the
/// sequential prefix of this seed's chain — no state inspection needed.
///
/// Messages are signed with [`test_secret_key`], and nonces are computed by
/// replaying the sender selection of all earlier blocks of the same seed:
/// block `n`'s k-th message from account A carries nonce
/// `sends(A, blocks 1..n) + k`. This keeps the function pure — the crash
/// probe and the tests can regenerate exactly the block a killed process
/// was committing — at the cost of O(n) regeneration per block, which is
/// irrelevant at test scale.
///
/// **Single-seed chains only**: this is correct only when every block
/// `1..=seqno` was (or will be) built with the same `seed`. Tests that vary
/// the seed per block (or otherwise break the uniform history) must use
/// [`TestTxGen`] instead.
pub fn test_block_txs(
    seed: u64,
    seqno: u32,
    accounts: &[AccountId],
    chain_id: [u8; 32],
) -> Vec<ExternalMessage> {
    let mut base_nonce: BTreeMap<AccountId, u64> = BTreeMap::new();
    for b in 1..seqno {
        for (from, _, _, _) in test_block_transfers(seed, b, accounts) {
            *base_nonce.entry(from).or_insert(0) += 1;
        }
    }
    test_block_txs_with_nonces(seed, seqno, accounts, &base_nonce, chain_id)
}

/// Deterministic external messages for block `seqno` with explicitly
/// supplied base nonces: the k-th message from account A carries nonce
/// `nonces[A] + k`.
///
/// Used by adversarial tests where the state's nonces do not match the
/// seed's own history (e.g. a diverged in-memory state whose seqno was
/// tampered with): the messages must still authorize against the *actual*
/// state, or `propose_block` fails before the store's continuity check is
/// even reached.
pub fn test_block_txs_with_nonces(
    seed: u64,
    seqno: u32,
    accounts: &[AccountId],
    nonces: &BTreeMap<AccountId, u64>,
    chain_id: [u8; 32],
) -> Vec<ExternalMessage> {
    let mut intra_block: BTreeMap<AccountId, u64> = BTreeMap::new();
    test_block_transfers(seed, seqno, accounts)
        .into_iter()
        .map(|(from, to, amount_nanos, fee_nanos)| {
            let nonce = nonces.get(&from).copied().unwrap_or(0)
                + intra_block.get(&from).copied().unwrap_or(0);
            *intra_block.entry(from).or_insert(0) += 1;
            ExternalMessage::new_signed(
                chain_id,
                MsgKind::Transfer,
                from,
                nonce,
                to,
                amount_nanos,
                fee_nanos,
                Vec::new(),
                [0u8; 32],
                &test_secret_key(&from),
            )
        })
        .collect()
}

/// Stateful message generator for tests whose chains do not have a
/// uniform seed history (e.g. alternating the seed per block).
///
/// `test_block_txs` computes nonces by replaying one seed's history, which is
/// wrong when the seed varies per block. `TestTxGen` instead tracks the
/// nonces it has handed out, so generated messages always authorize
/// against a state built by applying its blocks in order from genesis.
pub struct TestTxGen {
    nonces: BTreeMap<AccountId, u64>,
    chain_id: [u8; 32],
}

impl TestTxGen {
    pub fn new(chain_id: [u8; 32]) -> Self {
        Self {
            nonces: BTreeMap::new(),
            chain_id,
        }
    }

    /// Signed external messages for block `seqno` with transfer selection
    /// drawn from `seed`. Advances the internal nonce counters by this
    /// block's sends, so the next call continues where this one left off.
    pub fn block_txs(
        &mut self,
        seed: u64,
        seqno: u32,
        accounts: &[AccountId],
    ) -> Vec<ExternalMessage> {
        let txs = test_block_txs_with_nonces(seed, seqno, accounts, &self.nonces, self.chain_id);
        for (from, _, _, _) in test_block_transfers(seed, seqno, accounts) {
            *self.nonces.entry(from).or_insert(0) += 1;
        }
        txs
    }
}

impl Default for TestTxGen {
    fn default() -> Self {
        Self::new([0u8; 32])
    }
}

/// The unsigned transfer selection for block `seqno`: a pure function of
/// `(seed, seqno)`. Factored out so nonce computation can replay history
/// without re-signing.
fn test_block_transfers(
    seed: u64,
    seqno: u32,
    accounts: &[AccountId],
) -> Vec<(AccountId, AccountId, u128, u128)> {
    let mut rng = XorShift64(seed ^ (seqno as u64).wrapping_mul(0x9E3779B97F4A7C15));
    let n = accounts.len();
    (0..TEST_TXS_PER_BLOCK)
        .map(|_| {
            let from = accounts[rng.below(n)];
            let mut to = accounts[rng.below(n)];
            if to == from {
                to = accounts[(rng.below(n) + 1) % n];
            }
            (
                from,
                to,
                1 + (rng.next_u64() % 100) as u128,
                // Sometimes nonzero: exercises the fee-collector path.
                (rng.next_u64() % 5) as u128,
            )
        })
        .collect()
}

/// Logical time for a test block: strictly increasing with seqno.
pub fn test_block_lt(seqno: u32) -> u64 {
    seqno as u64
}

/// Minimal deterministic PRNG (xorshift64*). No `rand` dependency, no OS
/// entropy: the same seed always yields the same stream, in every process.
pub struct XorShift64(u64);

impl XorShift64 {
    pub fn new(seed: u64) -> Self {
        // Zero is a degenerate state; map it to a nonzero constant.
        Self(if seed == 0 { 0x2545F4914F6CDD1D } else { seed })
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform value in `0..bound` (bound > 0).
    pub fn below(&mut self, bound: usize) -> usize {
        debug_assert!(bound > 0);
        (self.next_u64() % bound as u64) as usize
    }
}
