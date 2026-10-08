//! Per-block account-map copy at 100k accounts (MILESTONES.md, M4).
//!
//! `propose_block` and `apply_block` both take `&State` and work on a copy
//! of the account tree, so a panicking or failing block can never corrupt
//! the caller's state (the producer's dry-run panic containment depends on
//! this). The question M4 asks is what that copy costs at 100k accounts.
//!
//! Two kinds of test live here:
//!
//! - Always-on regression checks (`clone_*`): cheap, deterministic, and
//!   they fail if cloning the tree stops sharing structure.
//! - `#[ignore]`d timing probes. Timing is not consensus behaviour and is
//!   machine-dependent, so it never gates CI. Run them with
//!
//!   ```text
//!   cargo test --release -p onx-stf --test account_map_copy -- --ignored --nocapture
//!   ```

use onx_data_structures::{AccountId, ShardIdent, WorkchainIdent};
use onx_primitives::{domain_hash, DomainTag, SecretKey};
use onx_state_model::{
    derive_account_id, AccountState, GenesisDocument, GenesisValidator, ShardStateTree, StorageStat,
};
use onx_stf::{apply_block, propose_block, ExternalMessage, MsgKind, State};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const BENCH_KEY_V1: DomainTag = DomainTag::from_ascii("ONX_BENCH_KEY_V1");

fn secret(i: u64) -> SecretKey {
    let seed = domain_hash(&BENCH_KEY_V1, &i.to_be_bytes());
    SecretKey::from_seed(&seed).expect("domain hash output is a valid seed")
}

fn active(balance_nanos: u128, pubkey: [u8; 32]) -> AccountState {
    AccountState::Active {
        balance_nanos,
        last_trans_lt: 0,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 0,
            byte_count: 0,
        },
        pubkey,
        nonce: 0,
    }
}

/// Genesis with `n` funded accounts. Only the first `signers` accounts get
/// real keys (deriving 100k Ed25519 keys would dominate the setup time and
/// measure nothing we care about); the rest carry a dummy pubkey.
fn genesis_state(n: u64, signers: u64) -> (State, Vec<(AccountId, SecretKey)>) {
    let workchain = WorkchainIdent::new(0);
    let shard = ShardIdent::root(workchain);
    let validators = vec![GenesisValidator {
        pubkey: [7u8; 32],
        stake: 1_000_000,
    }];
    let mut accounts = BTreeMap::new();
    let mut keyed = Vec::new();
    for i in 0..n {
        let id = derive_account_id(&format!("bench-{i}"));
        let pubkey = if i < signers {
            let sk = secret(i);
            let pk = sk.public_key().encode();
            keyed.push((id, sk));
            pk
        } else {
            [9u8; 32]
        };
        accounts.insert(id, active(1_000_000_000_000, pubkey));
    }
    let doc = GenesisDocument::new(workchain, shard, validators, accounts).expect("bench genesis");
    (State::from_genesis(&doc), keyed)
}

fn transfers(state: &State, keyed: &[(AccountId, SecretKey)], nonce: u64) -> Vec<ExternalMessage> {
    keyed
        .iter()
        .enumerate()
        .map(|(i, (from, sk))| {
            let to = keyed[(i + 1) % keyed.len()].0;
            ExternalMessage::new_signed(
                state.chain_id,
                MsgKind::Transfer,
                *from,
                nonce,
                to,
                1_000,
                10,
                Vec::new(),
                [0u8; 32],
                sk,
            )
        })
        .collect()
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

/// A clone must be independent: mutating it leaves the original's accounts
/// and root untouched. (This is what makes `&State` in the STF safe.)
#[test]
fn clone_is_independent_of_original() {
    let (state, _) = genesis_state(64, 0);
    let before_root = state.tree.state_root_hash().unwrap();
    let before_accounts: Vec<(AccountId, AccountState)> = state
        .tree
        .accounts()
        .map(|(id, st)| (*id, st.clone()))
        .collect();

    let mut copy: ShardStateTree = state.tree.clone();
    let id = derive_account_id("bench-0");
    copy.insert(id, active(1, [1u8; 32])).unwrap();
    copy.insert(derive_account_id("new-account"), active(2, [2u8; 32]))
        .unwrap();

    assert_eq!(state.tree.state_root_hash().unwrap(), before_root);
    let after: Vec<(AccountId, AccountState)> = state
        .tree
        .accounts()
        .map(|(id, st)| (*id, st.clone()))
        .collect();
    assert_eq!(after, before_accounts);
    assert_ne!(copy.state_root_hash().unwrap(), before_root);
    assert_eq!(copy.get(&id).unwrap().balance_nanos(), 1);
    assert_eq!(
        state.tree.get(&id).unwrap().balance_nanos(),
        1_000_000_000_000
    );
}

/// A failed block leaves the caller's state exactly as it was, at a size
/// where a shallow-copy bug would show up as shared mutation.
#[test]
fn failed_block_leaves_state_untouched() {
    let (state, keyed) = genesis_state(1_000, 4);
    let snapshot = state.clone();
    let mut block = propose_block(
        &state,
        transfers(&state, &keyed, 0),
        1,
        derive_account_id("bench-collector"),
        1,
        0,
    )
    .unwrap();
    block.header.state_root = [0u8; 32];
    assert!(apply_block(&state, &block).is_err());
    assert_eq!(state, snapshot);
    assert_eq!(
        state.tree.state_root_hash().unwrap(),
        snapshot.tree.state_root_hash().unwrap()
    );
}

#[test]
#[ignore = "timing probe; run with --release -- --ignored --nocapture"]
fn measure_copy_and_block_cycle_at_100k_accounts() {
    const N: u64 = 100_000;
    const ROUNDS: usize = 15;
    let t = Instant::now();
    let (mut state, keyed) = genesis_state(N, 8);
    eprintln!("setup: {N} accounts in {:?}", t.elapsed());

    let clone_times: Vec<Duration> = (0..ROUNDS)
        .map(|_| {
            let t = Instant::now();
            let c = std::hint::black_box(state.tree.clone());
            let d = t.elapsed();
            drop(c);
            d
        })
        .collect();

    let fee_collector = derive_account_id("bench-collector");
    let mut propose_times = Vec::new();
    let mut apply_times = Vec::new();
    for r in 0..ROUNDS {
        let msgs = transfers(&state, &keyed, r as u64);
        let lt = state.last_lt + 1;
        let t = Instant::now();
        let block = propose_block(&state, msgs, lt, fee_collector, 1, 0).unwrap();
        propose_times.push(t.elapsed());
        let t = Instant::now();
        let (next, _) = apply_block(&state, &block).unwrap();
        apply_times.push(t.elapsed());
        state = next;
    }

    eprintln!(
        "accounts={N} median over {ROUNDS} rounds: tree.clone()={:?} propose_block(8 msgs)={:?} apply_block(8 msgs)={:?}",
        median(clone_times),
        median(propose_times),
        median(apply_times),
    );
}
