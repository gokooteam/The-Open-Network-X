//! TVM replay acceptance: contract calls exercised by the real `onx replay`
//! binary across separate OS processes.
//!
//! A counter contract is installed via the genesis TOML (`code_hex` /
//! `data_hex`); three blocks each carrying one signed contract call are
//! produced with the honest `propose_block` path; then two fresh `onx replay`
//! subprocesses must print byte-identical `final_state_root`s, and the
//! replayed state must show the counter at 3 — proving the VM executed
//! deterministically inside replay, not just in-process.

use onx::blockfile::{block_file_name, encode_block_file};
use onx_data_structures::AccountId;
use onx_primitives::SecretKey;
use onx_state_model::AccountState;
use onx_stf::{propose_block, Block, State, Transaction};
use std::path::{Path, PathBuf};
use std::process::Command;

// Counter contract code/data as hex of canonical cell bytes (verified
// against `Cell::to_bytes`; see the tvm_integration suite for the
// bytecode's derivation and meaning).
const COUNTER_CODE_HEX: &str = "00344546004003010800000000000000000000000000000000000000000000000000000000000000000110008000400342004000414d";
const COUNTER_DATA_HEX: &str = "00080000000000000000";

fn addr_hex(byte: u8) -> String {
    format!("{byte:02x}").repeat(32)
}

fn test_secret_key(byte: u8) -> SecretKey {
    SecretKey::from_seed(&[byte; 32]).expect("fixed test seed decodes")
}

fn write_contract_genesis_toml(dir: &Path) -> PathBuf {
    let sender_pubkey = hex::encode(test_secret_key(0xa1).public_key().encode());
    let toml = format!(
        "# TVM replay test genesis — fixed, deterministic\n\
         [[balances]]\naddress = \"{sender}\"\namount = 1000000000000\npublic_key = \"{sender_pubkey}\"\n\n\
         [[balances]]\naddress = \"{contract}\"\namount = 1000000\ncode_hex = \"{COUNTER_CODE_HEX}\"\ndata_hex = \"{COUNTER_DATA_HEX}\"\n\n\
         [[balances]]\naddress = \"{collector}\"\namount = 1000000\n\n\
         [[validators]]\npublic_key = \"{val}\"\nstake = 1000\n\n\
         [[workchains]]\nid = -1\nname = \"masterchain\"\nenabled = true\n",
        sender = addr_hex(0xa1),
        contract = addr_hex(0xc0),
        collector = addr_hex(0xcc),
        val = addr_hex(0x11),
    );
    let path = dir.join("genesis.toml");
    std::fs::write(&path, toml).unwrap();
    path
}

fn replay_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_onx"))
}

fn run_replay(genesis: &Path, blocks: &Path, data_dir: &Path) -> std::process::Output {
    Command::new(replay_bin())
        .args([
            "replay",
            "--genesis",
            genesis.to_str().unwrap(),
            "--blocks",
            blocks.to_str().unwrap(),
            "--data-dir",
            data_dir.to_str().unwrap(),
        ])
        .output()
        .expect("failed to spawn onx replay")
}

fn final_root(stdout: &str) -> &str {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("final_state_root="))
        .expect("replay printed no final_state_root")
}

fn tmpdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("onx-tvm-replay-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn read_counter(cell: &onx_state_model::Cell) -> u64 {
    let bytes = cell.data_bytes();
    u64::from_be_bytes(bytes[..8].try_into().unwrap())
}

#[test]
fn tvm_replay_two_processes_byte_identical_with_counter() {
    let dir = tmpdir("two-process");
    let genesis = write_contract_genesis_toml(&dir);
    let blocks_dir = dir.join("blocks");
    std::fs::create_dir_all(&blocks_dir).unwrap();

    // Build the chain in-process via the honest producer path.
    let config = onx_genesis::parse_config(&genesis).unwrap();
    let doc = onx_genesis::build_genesis_document(&config).unwrap();
    let mut state = State::from_genesis(&doc);
    let sender = AccountId::from_bytes([0xa1; 32]);
    let contract = AccountId::from_bytes([0xc0; 32]);
    let collector = AccountId::from_bytes([0xcc; 32]);
    let secret = test_secret_key(0xa1);

    // Sanity: the TOML-installed contract carries the expected code/data.
    match state.tree.get(&contract).expect("contract in genesis") {
        AccountState::Active {
            code: Some(code),
            data: Some(data),
            ..
        } => {
            assert_eq!(read_counter(data), 0);
            let _ = code;
        }
        other => panic!("contract account malformed: {other:?}"),
    }

    let mut blocks: Vec<Block> = Vec::new();
    for i in 0..3u64 {
        let tx = Transaction::new_signed_call(
            sender,
            contract,
            1_000,
            100_000,
            i,
            b"increment".to_vec(),
            &secret,
        );
        let block = propose_block(&state, vec![tx], i + 1, collector).unwrap();
        let (next, _) = onx_stf::apply_block(&state, &block).unwrap();
        state = next;
        blocks.push(block);
    }
    // In-process: the counter advanced to 3.
    match state.tree.get(&contract).unwrap() {
        AccountState::Active {
            data: Some(data), ..
        } => assert_eq!(read_counter(data), 3),
        other => panic!("contract account malformed: {other:?}"),
    }
    let expected_root = state.tree.state_root_hash().unwrap();

    for block in &blocks {
        let path = blocks_dir.join(block_file_name(block.header.seqno));
        std::fs::write(path, encode_block_file(block)).unwrap();
    }

    // Two fresh `onx replay` subprocesses over the same inputs.
    let out1 = run_replay(&genesis, &blocks_dir, &dir.join("data1"));
    assert!(
        out1.status.success(),
        "replay 1 failed: {}",
        String::from_utf8_lossy(&out1.stderr)
    );
    let out2 = run_replay(&genesis, &blocks_dir, &dir.join("data2"));
    assert!(
        out2.status.success(),
        "replay 2 failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let stdout1 = String::from_utf8(out1.stdout).unwrap();
    let stdout2 = String::from_utf8(out2.stdout).unwrap();
    let root1 = final_root(&stdout1);
    let root2 = final_root(&stdout2);
    assert_eq!(root1, root2, "two replay processes disagreed");
    assert_eq!(
        root1,
        hex::encode(expected_root),
        "replay root differs from in-memory apply"
    );

    // The replayed state (loaded from the subprocess's data dir) shows
    // the counter at 3: the VM really executed inside replay.
    let store = onx_storage::ChainStore::open(dir.join("data1").join("chain.redb")).unwrap();
    let replayed = store.load_state().unwrap().expect("replay wrote state");
    match replayed.tree.get(&contract).unwrap() {
        AccountState::Active {
            data: Some(data), ..
        } => assert_eq!(read_counter(data), 3),
        other => panic!("replayed contract account malformed: {other:?}"),
    }
}

#[test]
fn tvm_replay_rejects_block_with_failing_contract_call() {
    // A block containing a contract call whose VM throws is rejected by
    // replay, and the head does not advance (fail-closed, same as any
    // invalid transaction).
    let dir = tmpdir("reject");
    let genesis = write_contract_genesis_toml(&dir);
    let blocks_dir = dir.join("blocks");
    std::fs::create_dir_all(&blocks_dir).unwrap();

    let config = onx_genesis::parse_config(&genesis).unwrap();
    let doc = onx_genesis::build_genesis_document(&config).unwrap();
    let state = State::from_genesis(&doc);
    let sender = AccountId::from_bytes([0xa1; 32]);
    let contract = AccountId::from_bytes([0xc0; 32]);
    let collector = AccountId::from_bytes([0xcc; 32]);
    let secret = test_secret_key(0xa1);

    // One good block first (so the head exists), then a block whose
    // contract call targets a codeless account.
    let good = Transaction::new_signed_call(
        sender,
        contract,
        1_000,
        100_000,
        0,
        b"increment".to_vec(),
        &secret,
    );
    let block1 = propose_block(&state, vec![good], 1, collector).unwrap();
    let _ = onx_stf::apply_block(&state, &block1).unwrap();
    std::fs::write(
        blocks_dir.join(block_file_name(1)),
        encode_block_file(&block1),
    )
    .unwrap();

    let codeless = AccountId::from_bytes([0xd0; 32]);
    // Hand-assemble the bad block with Block::assemble (which computes the
    // txs_root honestly): apply_block validates transactions before the
    // state root, so the contract-call failure is what rejects it. (The
    // state root here is a dummy — unreachable, since tx validation fails
    // first.)
    let bad_tx = Transaction::new_signed_call(
        sender,
        codeless,
        1_000,
        100_000,
        1,
        b"increment".to_vec(),
        &secret,
    );
    let bad_block = onx_stf::block::Block::assemble(
        2,
        block1.header.hash(),
        2,
        -1,
        collector,
        vec![bad_tx],
        [0u8; 32], // state root is irrelevant: tx validation fails first
    )
    .unwrap();
    std::fs::write(
        blocks_dir.join(block_file_name(2)),
        encode_block_file(&bad_block),
    )
    .unwrap();

    let out = run_replay(&genesis, &blocks_dir, &dir.join("data"));
    assert!(
        !out.status.success(),
        "replay should have rejected the failing contract call"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rejected"),
        "unexpected replay error: {stderr}"
    );
}
