//! Timing benchmark for `ShardStateTree::state_root_hash()` scaling.
//!
//! Reproduces the reviewer's measurement shape: full-state root
//! recomputation cost as a function of total accounts. Run in release:
//!
//! ```sh
//! cargo test -p onx-state-model --release --test bench_state_root -- --ignored --nocapture
//! ```
//!
//! These are `#[ignore]`d so the normal suite stays fast; the *behavioral*
//! regression (incremental updates via operation counters) lives in
//! `tests/trie_incremental.rs` and runs always.

use onx_data_structures::AccountId;
use onx_state_model::{AccountState, ShardStateTree, StorageStat};
use std::time::Instant;

fn funded_account() -> AccountState {
    AccountState::Active {
        balance_nanos: 1_000_000_000,
        last_trans_lt: 0,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 0,
            byte_count: 0,
            bit_count: 0,
        },
        pubkey: [0x11; 32],
        nonce: 0,
    }
}

fn account_id(i: usize) -> AccountId {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&(i as u64).to_be_bytes());
    AccountId::from_bytes(b)
}

fn build_tree(n: usize) -> ShardStateTree {
    let mut tree = ShardStateTree::new();
    for i in 0..n {
        tree.insert(account_id(i), funded_account()).unwrap();
    }
    tree
}

#[test]
#[ignore]
fn bench_state_root_scaling() {
    for n in [1_000usize, 10_000, 100_000] {
        let tree = build_tree(n);
        // Warm up (page in, settle allocator).
        let _ = tree.state_root_hash().unwrap();
        let iters = if n >= 100_000 { 5 } else { 20 };
        let start = Instant::now();
        for _ in 0..iters {
            let _ = tree.state_root_hash().unwrap();
        }
        let per_call = start.elapsed() / iters;
        println!("state_root_hash: n={n:>6}  {per_call:>10?} per call");
    }
}

#[test]
#[ignore]
fn bench_clone_scaling() {
    for n in [1_000usize, 10_000, 100_000] {
        let tree = build_tree(n);
        let _ = tree.clone();
        let iters = if n >= 100_000 { 20 } else { 100 };
        let start = Instant::now();
        for _ in 0..iters {
            let _ = tree.clone();
        }
        let per_call = start.elapsed() / iters;
        println!("tree.clone:      n={n:>6}  {per_call:>10?} per call");
    }
}

#[test]
#[ignore]
fn bench_insert_scaling() {
    // Cost of inserting ONE account into a tree of n accounts: the
    // incremental-update hot path (O(log n) after the wave-2 change,
    // O(n) before it since every insert was followed by full rebuilds
    // only at root-hash time — inserts themselves were always O(1)).
    for n in [1_000usize, 10_000, 100_000] {
        let mut tree = build_tree(n);
        let iters = 50;
        let start = Instant::now();
        for i in 0..iters {
            tree.insert(account_id(n + i), funded_account()).unwrap();
        }
        let per_call = start.elapsed() / iters as u32;
        // Force the root to be computed so the incremental work is real.
        let _ = tree.state_root_hash().unwrap();
        println!("insert+root:     n={n:>6}  {per_call:>10?} per insert");
    }
}
