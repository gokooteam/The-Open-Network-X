//! Regression tests pinning the wave-2 incremental trie behavior.
//!
//! The reviewer's finding: full-state root recomputation made block
//! validation scale with total accounts. These tests pin the fix via
//! *operation counters* (`ShardStateTree::trie_cells_built`), not wall
//! time:
//! - inserting k accounts into an n-account tree constructs O(k log n)
//!   trie cells — not O(k·n).
//! - `state_root_hash()` constructs zero cells (pure cache read).
//! - roots remain byte-identical to the pre-wave-2 batch construction
//!   (covered by the `incremental_matches_batch_construction` unit test
//!   plus the golden-vector suites).

use onx_data_structures::AccountId;
use onx_state_model::{AccountState, ShardStateTree, StorageStat};

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
    // Spread across the key space like real (hashed) account IDs, so the
    // trie is balanced. Sequential low bytes would build a degenerate
    // deep-and-skinny trie (all keys share their high bits), which is
    // correct but not representative.
    let mut b = [0u8; 32];
    let mut x = (i as u64)
        .wrapping_mul(0x9E3779B97F4A7C15)
        .wrapping_add(0xBF58476D1CE4E5B9);
    for chunk in b.chunks_mut(8) {
        // splitmix64
        x = x.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        chunk.copy_from_slice(&z.to_be_bytes());
    }
    AccountId::from_bytes(b)
}

/// A block touching k=10 accounts on an n=10,000-account state must
/// construct O(k log n) trie cells. Bound: 64 cells per insert (the real
/// cost is ~17: ~14 branch nodes + 1 leaf + ~2 value chunks). A full
/// recomputation would construct ~200k cells *per insert*.
#[test]
fn block_touching_k_accounts_builds_o_k_log_n_cells() {
    let mut tree = ShardStateTree::new();
    for i in 0..10_000 {
        tree.insert(account_id(i), funded_account()).unwrap();
    }
    let root_before = tree.state_root_hash().unwrap();

    let built_before = tree.trie_cells_built();
    for i in 10_000..10_010 {
        tree.insert(account_id(i), funded_account()).unwrap();
    }
    let built = tree.trie_cells_built() - built_before;
    assert!(
        built < 10 * 64,
        "10 inserts into a 10k tree built {built} trie cells; expected O(k log n) (< 640)"
    );

    // The root changed (new accounts committed).
    assert_ne!(tree.state_root_hash().unwrap(), root_before);
}

/// `state_root_hash()` is a pure cache read: it constructs zero trie
/// cells no matter how large the state is.
#[test]
fn state_root_hash_builds_zero_cells() {
    let mut tree = ShardStateTree::new();
    for i in 0..10_000 {
        tree.insert(account_id(i), funded_account()).unwrap();
    }
    let built_before = tree.trie_cells_built();
    let root1 = tree.state_root_hash().unwrap();
    let root2 = tree.state_root_hash().unwrap();
    assert_eq!(root1, root2);
    assert_eq!(
        tree.trie_cells_built(),
        built_before,
        "state_root_hash() must not construct trie cells"
    );
}

/// Replacing an existing account (the common per-block write: balance /
/// nonce / lt change) also costs O(log n) cells, and the old leaf is
/// retired — the trie does not accumulate garbage.
#[test]
fn account_update_is_logarithmic_and_retires_old_leaf() {
    let mut tree = ShardStateTree::new();
    for i in 0..1_000 {
        tree.insert(account_id(i), funded_account()).unwrap();
    }

    let built_before = tree.trie_cells_built();
    // Update 10 existing accounts (new balances).
    for i in 0..10 {
        let mut st = funded_account();
        if let AccountState::Active { balance_nanos, .. } = &mut st {
            *balance_nanos = 999_000_000 - i as u128;
        }
        tree.insert(account_id(i), st).unwrap();
    }
    let built = tree.trie_cells_built() - built_before;
    assert!(
        built < 10 * 64,
        "10 account updates built {built} trie cells; expected O(k log n) (< 640)"
    );

    // The full cell set stays proportional to the account count: every
    // account contributes ~1 leaf + ~2 chunks + its share of branches.
    // 1000 accounts must not produce more than 1000 * 64 cells.
    let cells = tree.trie_cells().unwrap();
    assert!(
        cells.len() < 1000 * 64,
        "trie cell set has {} cells for 1000 accounts; expected < 64000",
        cells.len()
    );
}

/// Cloning the tree (what `apply_block` does per block) shares the trie
/// via `Rc`: the clone must not duplicate trie construction work.
#[test]
fn clone_shares_trie_without_rebuilding() {
    let mut tree = ShardStateTree::new();
    for i in 0..1_000 {
        tree.insert(account_id(i), funded_account()).unwrap();
    }
    let built_before = tree.trie_cells_built();
    let cloned = tree.clone();
    assert_eq!(cloned.trie_cells_built(), built_before);
    assert_eq!(
        cloned.state_root_hash().unwrap(),
        tree.state_root_hash().unwrap()
    );
    // Mutating the clone does not affect the original's root.
    let mut mutated = cloned;
    mutated.insert(account_id(1_000), funded_account()).unwrap();
    assert_ne!(
        mutated.state_root_hash().unwrap(),
        tree.state_root_hash().unwrap()
    );
}
