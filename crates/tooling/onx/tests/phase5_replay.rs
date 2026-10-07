//! Phase 5 acceptance suite: the deterministic-replay milestone.
//!
//! Every test drives the real `onx replay` binary as a subprocess — no
//! mocks. Criteria (from the plan's Phase 5 section, strictest reading):
//!   a. Golden vectors — genesis root and roots-after-N as hardcoded hex.
//!   b. Two-process determinism — two fresh runs, byte-identical output.
//!   c. Crash recovery — kill -9 mid-replay, resume, root matches.
//!   d. Equivalence — pure in-memory apply vs persisted replay agree.
//!   e. Corrupted block files rejected (tampered, truncated, bad prev_hash).
//!   f. Invalid-message block rejected, head not advanced.
//!   g. Idempotent re-run over committed blocks is a no-op.

use onx::blockfile::{block_file_name, decode_block_file, encode_block_file};
use onx_data_structures::AccountId;
use onx_primitives::SecretKey;
use onx_state_model::AccountState;
use onx_stf::block::SigEntry;
use onx_stf::{propose_block, Block, ExternalMessage, MsgKind, State};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// ---------------------------------------------------------------------------
// Deterministic fixtures
// ---------------------------------------------------------------------------

fn addr_hex(byte: u8) -> String {
    format!("{byte:02x}").repeat(32)
}

/// Deterministic signing key for a test account byte.
///
/// Test-only: the seed is public, so these keys are not secret. The
/// corresponding pubkey is written into the genesis TOML, so messages
/// from these accounts actually authorize.
fn test_secret_key(byte: u8) -> SecretKey {
    SecretKey::from_seed(&[byte; 32]).expect("fixed test seed decodes")
}

fn write_genesis_toml(dir: &Path) -> PathBuf {
    let mut toml = String::from("# test genesis — fixed, deterministic\n");
    for i in 0..4u8 {
        let byte = 0xaa + i;
        let pubkey_hex = hex::encode(test_secret_key(byte).public_key().encode());
        toml.push_str(&format!(
            "[[balances]]\naddress = \"{}\"\namount = 1000000000000\npublic_key = \"{}\"\n\n",
            addr_hex(byte),
            pubkey_hex
        ));
    }
    toml.push_str(&format!(
        "[[validators]]\npublic_key = \"{}\"\nstake = 1000\n\n",
        // Real deterministic key: explicit hex validator keys must clear the
        // strict predicate (canonical, on-curve, large-order).
        hex::encode(test_secret_key(0x11).public_key().encode())
    ));
    toml.push_str("[[workchains]]\nid = -1\nname = \"masterchain\"\nenabled = true\n");
    let path = dir.join("genesis.toml");
    std::fs::write(&path, toml).unwrap();
    path
}

fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

fn account_id(byte: u8) -> AccountId {
    AccountId::from_bytes([byte; 32])
}

/// Build `n_blocks` of `msgs_per` deterministic external messages via the
/// honest producer path. Returns the blocks and the pure in-memory final
/// state.
fn build_chain(
    genesis_toml: &Path,
    n_blocks: u32,
    msgs_per: usize,
    seed: u64,
) -> (Vec<Block>, State, [u8; 32]) {
    let config = onx_genesis::parse_config(genesis_toml).unwrap();
    let doc = onx_genesis::build_genesis_document(&config).unwrap();
    // Chain identity is the genesis hash; every message must carry it
    // (ADR-0005).
    let chain_id = doc.genesis_hash();
    let mut state = State::from_genesis(&doc);
    let collector = account_id(0xaa);
    let mut rng = seed;
    let mut blocks = Vec::new();
    // Per-account nonces: every signed message consumes the sender's
    // current nonce, so the fixture tracks them alongside the chain.
    let mut nonces: BTreeMap<AccountId, u64> = BTreeMap::new();
    for _ in 0..n_blocks {
        let mut msgs = Vec::new();
        for _ in 0..msgs_per {
            let from = account_id(0xaa + (xorshift64(&mut rng) % 4) as u8);
            let mut to = account_id(0xaa + (xorshift64(&mut rng) % 4) as u8);
            if to == from {
                to = account_id(0xdd);
            }
            let nonce = nonces.get(&from).copied().unwrap_or(0);
            nonces.insert(from, nonce + 1);
            // The account byte doubles as the key seed (see test_secret_key).
            let secret = test_secret_key(from.to_bytes()[0]);
            msgs.push(ExternalMessage::new_signed(
                chain_id,
                MsgKind::Transfer,
                from,
                nonce,
                to,
                (1_000 + xorshift64(&mut rng) % 50_000) as u128,
                (10 + xorshift64(&mut rng) % 100) as u128,
                Vec::new(),
                [0u8; 32],
                &secret,
            ));
        }
        let lt = state.last_lt + 1;
        let block = propose_block(&state, msgs, lt, collector, 1, 0).unwrap();
        let (new_state, _) = onx_stf::apply_block(&state, &block).unwrap();
        state = new_state;
        blocks.push(block);
    }
    (blocks, state, chain_id)
}

fn sign_block(block: &Block, chain_id: &[u8; 32]) -> Vec<SigEntry> {
    use onx_primitives::SecretKey;
    let secret = SecretKey::from_seed(&[0x11u8; 32]).unwrap();
    let preimage = block.header.sign_bytes(chain_id);
    vec![SigEntry {
        validator_index: 0,
        sig: secret.sign_raw(&preimage).encode(),
    }]
}

fn write_block_files(dir: &Path, blocks: &[Block], chain_id: &[u8; 32]) {
    std::fs::create_dir_all(dir).unwrap();
    for block in blocks {
        let path = dir.join(block_file_name(block.header.seqno));
        std::fs::write(path, encode_block_file(block, &sign_block(block, chain_id))).unwrap();
    }
}

fn replay_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_onx"))
}

fn run_replay(genesis: &Path, blocks: &Path, data_dir: &Path) -> Output {
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

fn stdout_of(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("replay stdout not UTF-8")
}

/// Extract `final_state_root=…` from replay stdout.
fn final_root(stdout: &str) -> &str {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("final_state_root="))
        .expect("replay printed no final_state_root")
}

fn tmpdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("onx-phase5-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// Golden vectors.
//
// Generated ONCE from the implementation (2026-10-05) and frozen: these
// values must never be regenerated to match the code. If they ever stop
// matching, the code changed — investigate, do not update the vectors.
// Amethyst approved the Phase 5 acceptance tests (including golden vectors)
// with her "Go" on 2026-10-05.
//
// REFROZEN 2026-10-05 for the tx-auth upgrade (ONX_TX_V2): genesis accounts
// now carry Ed25519 pubkeys (141-byte account encoding instead of 101) and
// all transactions are signed with per-account nonces, so every root below
// legitimately changed. The refreeze was regenerated from the V2
// implementation via `replay_prints_vectors_for_freezing` and the chain
// verified internally consistent (replay succeeds, roots deterministic
// across runs). This is the new frozen baseline: the same rule applies —
// investigate, do not update.
//
// NOTE 2026-10-05, message-model milestone (ADR-0001/ADR-0002):
// synchronous transactions were replaced by external messages (new wire
// encoding, new domain tags, header commitment `msgs_root`, block files
// now `ONXBLK04`). The transfer fixture below was regenerated from the
// message-model implementation via `replay_prints_vectors_for_freezing`
// and came back BYTE-IDENTICAL to the V2 vectors — the wallet handler
// reproduces the V2 state transitions exactly for plain transfers, so
// these vectors stand unchanged. Parity, not coincidence: any divergence
// would have been a state-machine regression. Same rule stands:
// investigate, do not update.
// ---------------------------------------------------------------------------

/// Genesis root of the fixed test genesis config above.
const GOLDEN_GENESIS_ROOT: &str =
    "079404cb2379be4802d27f77101e92c232cb94af2f93b3c8274995e85b99c04d";
/// State root after block 3 of the fixed 5-block × 4-msg chain (seed 0xC10C).
const GOLDEN_ROOT_AFTER_3: &str =
    "3fbb03b29519164464d470e1facbb62f96d0f1ea4fe2e4dce3df87661650af36";
/// Final state root after block 5 of the fixed chain.
const GOLDEN_ROOT_AFTER_5: &str =
    "8b2f6aa6de16db8b22ac3f8fdde779ae0e292b6912a21dbf1915f98747aa6098";

fn root_at_seqno(stdout: &str, seqno: u32) -> &str {
    let prefix = format!("seqno={seqno} ");
    stdout
        .lines()
        .find(|l| l.starts_with(&prefix))
        .and_then(|l| l.split_whitespace().find_map(|p| p.strip_prefix("root=")))
        .unwrap_or_else(|| panic!("no root line for seqno {seqno}"))
}

#[test]
fn replay_prints_vectors_for_freezing() {
    let dir = tmpdir("vectors");
    let genesis = write_genesis_toml(&dir);
    let (blocks, _, chain_id) = build_chain(&genesis, 5, 4, 0xC10C);
    let blocks_dir = dir.join("blocks");
    write_block_files(&blocks_dir, &blocks, &chain_id);
    let out = run_replay(&genesis, &blocks_dir, &dir.join("data"));
    assert!(
        out.status.success(),
        "replay failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = stdout_of(&out);
    println!(
        "GOLDEN_GENESIS_ROOT={}",
        stdout
            .lines()
            .find_map(|l| l.strip_prefix("genesis_root="))
            .unwrap()
    );
    println!("GOLDEN_ROOT_AFTER_3={}", root_at_seqno(&stdout, 3));
    println!("GOLDEN_ROOT_AFTER_5={}", final_root(&stdout));
    println!("full stdout:\n{stdout}");
}

// ---------------------------------------------------------------------------
// (a) Golden vectors
// ---------------------------------------------------------------------------

#[test]
fn replay_matches_golden_vectors() {
    if GOLDEN_GENESIS_ROOT == "REPLACE_ME_GENESIS" {
        eprintln!("golden vectors not frozen yet — run replay_prints_vectors_for_freezing first");
        return;
    }
    let dir = tmpdir("golden");
    let genesis = write_genesis_toml(&dir);
    let (blocks, _, chain_id) = build_chain(&genesis, 5, 4, 0xC10C);
    let blocks_dir = dir.join("blocks");
    write_block_files(&blocks_dir, &blocks, &chain_id);
    let out = run_replay(&genesis, &blocks_dir, &dir.join("data"));
    assert!(
        out.status.success(),
        "replay failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = stdout_of(&out);
    let genesis_root = stdout
        .lines()
        .find_map(|l| l.strip_prefix("genesis_root="))
        .unwrap();
    assert_eq!(
        genesis_root, GOLDEN_GENESIS_ROOT,
        "genesis root changed — investigate, do not update the vector"
    );
    assert_eq!(
        root_at_seqno(&stdout, 3),
        GOLDEN_ROOT_AFTER_3,
        "root after block 3 changed — investigate"
    );
    assert_eq!(
        final_root(&stdout),
        GOLDEN_ROOT_AFTER_5,
        "final root changed — investigate"
    );
}

// ---------------------------------------------------------------------------
// (b) Two-process determinism
// ---------------------------------------------------------------------------

#[test]
fn replay_two_processes_byte_identical() {
    let dir = tmpdir("twoproc");
    let genesis = write_genesis_toml(&dir);
    let (blocks, _, chain_id) = build_chain(&genesis, 10, 8, 0x5EED);
    let blocks_dir = dir.join("blocks");
    write_block_files(&blocks_dir, &blocks, &chain_id);

    let out1 = run_replay(&genesis, &blocks_dir, &dir.join("data1"));
    let out2 = run_replay(&genesis, &blocks_dir, &dir.join("data2"));
    assert!(
        out1.status.success(),
        "run 1 failed: {}",
        String::from_utf8_lossy(&out1.stderr)
    );
    assert!(
        out2.status.success(),
        "run 2 failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    assert_eq!(out1.stdout, out2.stdout, "two fresh replays diverged");
    assert!(out1.stderr.is_empty() && out2.stderr.is_empty());
}

// ---------------------------------------------------------------------------
// (d) Equivalence: pure in-memory vs persisted replay
// ---------------------------------------------------------------------------

#[test]
fn replay_equivalence_in_memory_vs_persisted() {
    let dir = tmpdir("equiv");
    let genesis = write_genesis_toml(&dir);
    let (blocks, mem_state, chain_id) = build_chain(&genesis, 12, 6, 0xE901);
    let mem_root = hex::encode(mem_state.state_root().unwrap());
    let blocks_dir = dir.join("blocks");
    write_block_files(&blocks_dir, &blocks, &chain_id);

    let out = run_replay(&genesis, &blocks_dir, &dir.join("data"));
    assert!(
        out.status.success(),
        "replay failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        final_root(&stdout_of(&out)),
        mem_root,
        "persisted replay diverged from pure in-memory apply"
    );
}

// ---------------------------------------------------------------------------
// (e) Corrupted block files are rejected
// ---------------------------------------------------------------------------

fn run_expect_failure(dir: &Path, genesis: &Path, blocks_dir: &Path, case: &str) -> Output {
    let out = run_replay(genesis, blocks_dir, &dir.join(format!("data-{case}")));
    assert!(
        !out.status.success(),
        "{case}: replay should have failed but succeeded:\n{}",
        stdout_of(&out)
    );
    out
}

#[test]
fn replay_rejects_tampered_block_file() {
    let dir = tmpdir("tamper");
    let genesis = write_genesis_toml(&dir);
    let (blocks, _, chain_id) = build_chain(&genesis, 4, 4, 0x7A1);
    let blocks_dir = dir.join("blocks");
    write_block_files(&blocks_dir, &blocks, &chain_id);
    // Flip a byte in the middle of block 3's body (a message field).
    let p3 = blocks_dir.join(block_file_name(3));
    let mut bytes = std::fs::read(&p3).unwrap();
    bytes[200] ^= 0xff;
    std::fs::write(&p3, bytes).unwrap();
    let out = run_expect_failure(&dir, &genesis, &blocks_dir, "tamper");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("onx replay failed"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn replay_rejects_truncated_block_file() {
    let dir = tmpdir("trunc");
    let genesis = write_genesis_toml(&dir);
    let (blocks, _, chain_id) = build_chain(&genesis, 4, 4, 0x7A2);
    let blocks_dir = dir.join("blocks");
    write_block_files(&blocks_dir, &blocks, &chain_id);
    let p2 = blocks_dir.join(block_file_name(2));
    let bytes = std::fs::read(&p2).unwrap();
    std::fs::write(&p2, &bytes[..bytes.len() / 2]).unwrap();
    run_expect_failure(&dir, &genesis, &blocks_dir, "trunc");
}

#[test]
fn replay_rejects_wrong_prev_hash() {
    let dir = tmpdir("prevhash");
    let genesis = write_genesis_toml(&dir);
    let (blocks, _, chain_id) = build_chain(&genesis, 3, 4, 0x7A3);
    // Hand-assemble a block with a lying prev_hash (Block::assemble does not
    // run the STF — the replay must catch it).
    let bad = &blocks[1];
    let mut evil = bad.clone();
    evil.header.seqno = 3; // the file name says block 3; the header must agree
    evil.header.prev_hash = [0xab; 32];
    // Sign the tampered header: auth passes, the STF must catch the lie.
    let blocks_dir = dir.join("blocks");
    write_block_files(&blocks_dir, &blocks[..2], &chain_id);
    std::fs::write(
        blocks_dir.join(block_file_name(3)),
        encode_block_file(&evil, &sign_block(&evil, &chain_id)),
    )
    .unwrap();
    let out = run_expect_failure(&dir, &genesis, &blocks_dir, "prevhash");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("PrevHashMismatch") || stderr.contains("rejected"),
        "unexpected stderr: {stderr}"
    );
    // Round-trip sanity: the file on disk really is what we wrote.
    let reread =
        decode_block_file(&std::fs::read(blocks_dir.join(block_file_name(3))).unwrap()).unwrap();
    assert_eq!(reread.block.header.prev_hash, [0xab; 32]);
}

// ---------------------------------------------------------------------------
// (f) Invalid-message block rejected; head not advanced
// ---------------------------------------------------------------------------

#[test]
fn replay_rejects_invalid_message_block() {
    let dir = tmpdir("badmsg");
    let genesis = write_genesis_toml(&dir);
    let (blocks, state, chain_id) = build_chain(&genesis, 3, 4, 0xBAD);
    // Hand-assemble block 4 with a properly signed message spending far
    // more than any balance. msgs_root is correct (assemble computes it);
    // the STF must reject the message itself — the wallet error fires
    // before the state-root check.
    let sender = account_id(0xaa);
    let nonce = match state.tree.get(&sender) {
        Some(AccountState::Active { nonce, .. }) => *nonce,
        _ => panic!("fixture sender must be active"),
    };
    let evil_msgs = vec![ExternalMessage::new_signed(
        chain_id,
        MsgKind::Transfer,
        sender,
        nonce,
        account_id(0xbb),
        u128::MAX,
        10,
        Vec::new(),
        [0u8; 32],
        &test_secret_key(0xaa),
    )];
    let prev = &blocks[2];
    let evil = Block::assemble(
        4,
        prev.header.hash(),
        prev.header.lt + 1,
        -1,
        account_id(0xaa),
        evil_msgs,
        [0xff; 32], // garbage claimed root: must not mask the message error
        1,
        0,
    )
    .expect("evil block has one message");
    let blocks_dir = dir.join("blocks");
    write_block_files(&blocks_dir, &blocks, &chain_id);
    std::fs::write(
        blocks_dir.join(block_file_name(4)),
        encode_block_file(&evil, &sign_block(&evil, &chain_id)),
    )
    .unwrap();

    let out = run_expect_failure(&dir, &genesis, &blocks_dir, "badmsg");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("insufficient funds") || stderr.contains("rejected"),
        "unexpected stderr: {stderr}"
    );

    // Head must not have advanced: drop the evil file, replay the valid
    // prefix to completion, and the chain must still reach its golden root.
    std::fs::remove_file(blocks_dir.join(block_file_name(4))).unwrap();
    let out2 = run_replay(&genesis, &blocks_dir, &dir.join("data-badmsg"));
    assert!(
        out2.status.success(),
        "valid-prefix replay failed after rejection: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let (valid_blocks, valid_state, _chain_id) = build_chain(&genesis, 3, 4, 0xBAD);
    assert_eq!(valid_blocks.len(), 3);
    assert_eq!(
        final_root(&stdout_of(&out2)),
        hex::encode(valid_state.state_root().unwrap())
    );
}

// ---------------------------------------------------------------------------
// (g) Idempotent re-run
// ---------------------------------------------------------------------------

#[test]
fn replay_rerun_is_idempotent_noop() {
    let dir = tmpdir("idem");
    let genesis = write_genesis_toml(&dir);
    let (blocks, _, chain_id) = build_chain(&genesis, 6, 5, 0x1DEA);
    let blocks_dir = dir.join("blocks");
    write_block_files(&blocks_dir, &blocks, &chain_id);
    let data = dir.join("data");

    let out1 = run_replay(&genesis, &blocks_dir, &data);
    assert!(
        out1.status.success(),
        "first replay failed: {}",
        String::from_utf8_lossy(&out1.stderr)
    );
    let out2 = run_replay(&genesis, &blocks_dir, &data);
    assert!(
        out2.status.success(),
        "second replay failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let s2 = stdout_of(&out2);
    assert!(
        s2.lines().filter(|l| l.contains("status=skipped")).count() == 6,
        "expected 6 skips on re-run, got:\n{s2}"
    );
    assert_eq!(final_root(&stdout_of(&out1)), final_root(&s2));
}

// ---------------------------------------------------------------------------
// (c) Crash recovery through the real CLI
// ---------------------------------------------------------------------------

#[test]
fn replay_crash_kill9_resume_matches() {
    let dir = tmpdir("crash");
    let genesis = write_genesis_toml(&dir);
    // Enough blocks that replay is still running after ~200ms.
    let (blocks, mem_state, chain_id) = build_chain(&genesis, 400, 8, 0xC8A5);
    let expected_root = hex::encode(mem_state.state_root().unwrap());
    let blocks_dir = dir.join("blocks");
    write_block_files(&blocks_dir, &blocks, &chain_id);

    for iter in 0..3 {
        let data = dir.join(format!("data-crash-{iter}"));
        let mut child = Command::new(replay_bin())
            .args([
                "replay",
                "--genesis",
                genesis.to_str().unwrap(),
                "--blocks",
                blocks_dir.to_str().unwrap(),
                "--data-dir",
                data.to_str().unwrap(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn replay");
        std::thread::sleep(std::time::Duration::from_millis(200));
        // Honest kill: the test only counts if the child was still alive.
        assert!(
            child.try_wait().expect("try_wait").is_none(),
            "replay finished before the kill — make the chain longer"
        );
        let pid = child.id();
        unsafe { libc_kill9(pid) };
        let _ = child.wait();

        // Resume: re-run to completion on the same data dir.
        let out = run_replay(&genesis, &blocks_dir, &data);
        assert!(
            out.status.success(),
            "resume after kill -9 failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            final_root(&stdout_of(&out)),
            expected_root,
            "resumed replay diverged from uninterrupted run (iter {iter})"
        );
    }
}

#[cfg(unix)]
unsafe fn libc_kill9(pid: u32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    let rc = unsafe { kill(pid as i32, 9) };
    assert_eq!(rc, 0, "kill -9 failed");
}

#[cfg(not(unix))]
unsafe fn libc_kill9(_pid: u32) {
    panic!("kill -9 test requires unix");
}
