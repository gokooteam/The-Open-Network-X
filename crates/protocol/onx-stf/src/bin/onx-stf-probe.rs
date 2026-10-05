//! Phase 3 cross-process determinism probe.
//!
//! Builds a fixed genesis, then applies a seeded pseudo-random sequence of
//! blocks (Onyx transfers with fees) through the honest producer path
//! (`propose_block`) and validator path (`apply_block`). Prints the final
//! state root, last block hash, and transaction count.
//!
//! Everything is deterministic: fixed seed, fixed genesis, no I/O besides
//! stdout, no time, no randomness source. Two OS processes running this
//! binary must print byte-identical output — that is exactly the replay
//! scenario (two nodes, two runs, one chain). The integration test
//! `stf_cross_process_determinism` in `tests/phase3_stf.rs` enforces it.

use onx_data_structures::{AccountId, ShardIdent, WorkchainIdent};
use onx_state_model::{
    derive_account_id, AccountState, GenesisDocument, GenesisValidator, StorageStat,
};
use onx_stf::{apply_block, propose_block, State, Transaction};
use std::collections::BTreeMap;

/// Deterministic xorshift64* — dependency-free and obviously deterministic.
/// (No `rand` crate: consensus-adjacent test tooling must not depend on an
/// external RNG whose algorithm could change under us.)
struct XorShift64(u64);

impl XorShift64 {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn active_account(balance_nanos: u128) -> AccountState {
    AccountState::Active {
        balance_nanos,
        last_trans_lt: 0,
        code_hash: [0u8; 32],
        data_hash: [0u8; 32],
        storage_stat: StorageStat {
            cell_count: 0,
            byte_count: 0,
        },
    }
}

fn main() {
    // --- Fixed genesis: 8 funded accounts on workchain 0, root shard ---
    let workchain = WorkchainIdent::new(0);
    let shard = ShardIdent::root(workchain);
    let validators = vec![GenesisValidator {
        pubkey: [7u8; 32],
        stake: 1_000_000,
    }];
    let mut accounts = BTreeMap::new();
    let names = [
        "probe-alice",
        "probe-bob",
        "probe-carol",
        "probe-dave",
        "probe-erin",
        "probe-frank",
        "probe-grace",
        "probe-heidi",
    ];
    let mut ids: Vec<AccountId> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let id = derive_account_id(name);
        // Distinct balances so the PRNG has something to chew on.
        accounts.insert(
            id,
            active_account(1_000_000_000 + (i as u128) * 111_111_111),
        );
        ids.push(id);
    }
    let fee_collector = derive_account_id("probe-collector");
    let doc = GenesisDocument::new(workchain, shard, validators, accounts)
        .expect("fixed genesis must build");
    let mut state = State::from_genesis(&doc);

    // --- Seeded block sequence ---
    // Local balance mirror so generated transactions are always valid: the
    // probe reads sender balances from here (not from the committed state),
    // so multiple same-block spends from one account see each other's debits.
    // The STF remains the source of truth — this mirror only keeps the
    // *generator* honest.
    let mut mirror: BTreeMap<AccountId, u128> = BTreeMap::new();
    for id in &ids {
        mirror.insert(
            *id,
            state.tree.get(id).map(|s| s.balance_nanos()).unwrap_or(0),
        );
    }
    mirror.insert(fee_collector, 0);
    let mut rng = XorShift64(0x1234_5678_9ABC_DEF0);
    let mut total_txs: u64 = 0;
    const BLOCKS: u64 = 25;
    for b in 1..=BLOCKS {
        let n_txs = 1 + (rng.next() % 8) as usize;
        let mut txs = Vec::with_capacity(n_txs);
        for _ in 0..n_txs {
            let from = ids[(rng.next() as usize) % ids.len()];
            let to = ids[(rng.next() as usize) % ids.len()];
            let balance = mirror.get(&from).copied().unwrap_or(0);
            // fee in [0, 999], amount in [1, balance - fee]; skip broke senders.
            let fee = (rng.next() % 1000) as u128;
            let spendable = balance.saturating_sub(fee);
            if spendable == 0 {
                continue;
            }
            let amount = 1 + (rng.next() as u128 % spendable);
            // Mirror the STF's settlement so later txs in this block see it.
            let (burned, validator_fee) = onx_economics::split_transaction_fee(fee);
            let _ = burned; // burned supply simply vanishes from the mirror
            mirror.insert(from, balance - amount - fee);
            *mirror.entry(to).or_insert(0) += amount;
            *mirror.entry(fee_collector).or_insert(0) += validator_fee;
            txs.push(Transaction {
                from,
                to,
                amount_nanos: amount,
                fee_nanos: fee,
            });
        }
        // Producer path builds the block; validator path checks it.
        let block = propose_block(&state, txs, b, fee_collector).expect("propose must succeed");
        let (next, _receipts) = apply_block(&state, &block).expect("apply must succeed");
        total_txs += block.body.transactions.len() as u64;
        state = next;
    }

    println!(
        "final_state_root={}",
        hex(&state.state_root().expect("root must compute"))
    );
    println!("final_block_hash={}", hex(&state.last_hash));
    println!("final_seqno={}", state.seqno);
    println!("total_txs={total_txs}");
}
