//! Deterministic test-chain scaffolding, shared by the crash probe binary
//! (`onx-crash-probe`) and the Phase 4 integration tests.
//!
//! This is NOT consensus code: it exists so the probe and the tests generate
//! byte-identical chains from the same `(seed, seqno)`. Transaction
//! generation for block `n` depends only on `(seed, n)` — never on process
//! state — so a test can regenerate exactly the block a killed probe was
//! committing and resume from the head pointer.

use onx_data_structures::{AccountId, ShardIdent, WorkchainIdent};
use onx_state_model::{AccountState, GenesisDocument, GenesisValidator, StorageStat};
use onx_stf::Transaction;
use std::collections::BTreeMap;

/// Number of funded accounts in the test genesis.
pub const TEST_ACCOUNT_COUNT: usize = 8;
/// Genesis balance per account (nanos) — vastly larger than any test spend.
pub const TEST_GENESIS_BALANCE: u128 = 1_000_000_000;
/// Transactions per test block.
pub const TEST_TXS_PER_BLOCK: usize = 8;

/// Deterministic fee-collector account for test blocks.
pub fn test_fee_collector() -> AccountId {
    AccountId::from_bytes([0xCC; 32])
}

/// Deterministic test genesis: 8 funded accounts on the basic workchain.
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
        accounts.insert(
            AccountId::from_bytes(id),
            AccountState::Active {
                balance_nanos: TEST_GENESIS_BALANCE,
                last_trans_lt: 0,
                code_hash: [0; 32],
                data_hash: [0; 32],
                storage_stat: StorageStat {
                    cell_count: 0,
                    byte_count: 0,
                },
            },
        );
    }
    GenesisDocument::new(workchain, shard, validators, accounts).expect("test genesis is valid")
}

/// Account ids of the test genesis, in ascending order.
pub fn test_accounts() -> Vec<AccountId> {
    test_genesis().accounts.keys().cloned().collect()
}

/// Deterministic transactions for block `seqno`: a pure function of
/// `(seed, seqno)`. Amounts are tiny relative to genesis balances and fees
/// are small, so every generated block is always valid against any chain
/// state that shares this genesis — no state inspection needed.
pub fn test_block_txs(seed: u64, seqno: u32, accounts: &[AccountId]) -> Vec<Transaction> {
    let mut rng = XorShift64(seed ^ (seqno as u64).wrapping_mul(0x9E3779B97F4A7C15));
    let n = accounts.len();
    (0..TEST_TXS_PER_BLOCK)
        .map(|_| {
            let from = accounts[rng.below(n)];
            let mut to = accounts[rng.below(n)];
            if to == from {
                to = accounts[(rng.below(n) + 1) % n];
            }
            Transaction {
                from,
                to,
                amount_nanos: 1 + (rng.next_u64() % 100) as u128,
                // Sometimes nonzero: exercises the fee-collector path.
                fee_nanos: (rng.next_u64() % 5) as u128,
            }
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
