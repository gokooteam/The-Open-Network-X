//! Crash probe for Phase 4 storage testing.
//!
//! Commits `num_blocks` blocks to the database at `db_path`, printing a
//! flushed progress line after every commit. The crash test SIGKILLs this
//! process at a random point, reopens the database, verifies full-or-nothing
//! semantics, and resumes — the resumed chain must match an uninterrupted
//! run byte-for-byte.
//!
//! Usage: `onx-crash-probe <db_path> <num_blocks> [seed]`

use onx_stf::propose_block;
use onx_storage::support::{
    test_accounts, test_block_lt, test_block_txs, test_fee_collector, test_genesis,
};
use onx_storage::ChainStore;
use std::io::Write;

fn hex(h: &[u8; 32]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: onx-crash-probe <db_path> <num_blocks> [seed]");
        std::process::exit(2);
    }
    let db_path = &args[1];
    let num_blocks: u32 = args[2].parse().expect("num_blocks must be a u32");
    let seed: u64 = args
        .get(3)
        .map(|s| s.parse().expect("seed must be a u64"))
        .unwrap_or(0xC0FFEE);

    let store = ChainStore::open(db_path).expect("open store");
    let doc = test_genesis();
    store.init_genesis(&doc).expect("init genesis");

    let accounts = test_accounts();
    let collector = test_fee_collector();

    // Resume-aware: start from whatever the head says (a previous probe run
    // may have been killed mid-chain; blocks are deterministic in seqno).
    let mut state = store
        .load_state()
        .expect("load state")
        .expect("genesis state");
    while state.seqno < num_blocks {
        let next_seqno = state.seqno + 1;
        let msgs = test_block_txs(seed, next_seqno, &accounts, state.chain_id);
        let block = propose_block(&state, msgs, test_block_lt(next_seqno), collector)
            .expect("propose must succeed");
        store
            .commit_block(&state, &block)
            .expect("commit must succeed");
        // Reload from disk: exercises the read path under kill pressure and
        // keeps the probe honest (it never trusts its own memory).
        state = store.load_state().expect("reload state").expect("state");
        let root = state.state_root().expect("state root");
        println!("committed seqno={} root={}", state.seqno, hex(&root));
        std::io::stdout().flush().expect("flush stdout");
    }
    println!("done seqno={}", state.seqno);
}
