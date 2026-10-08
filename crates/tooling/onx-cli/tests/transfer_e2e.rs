//! End-to-end: messages built by the real `onx-cli` binary are accepted by
//! the real STF against the checked-in devnet genesis (`config/genesis.toml`).
//!
//! 1. The devnet faucet (public test seed `[0x11; 32]`, explicit genesis
//!    address) pays a freshly created wallet's key-derived address.
//! 2. That wallet spends back with `--reveal-key` (first spend, ADR-0006).
//! 3. Negative cases: a missing reveal and a foreign chain ID are rejected
//!    by `propose_block`, so the CLI is not trivially producing bytes the
//!    node would take regardless.

use onx_data_structures::AccountId;
use onx_stf::{apply_block, propose_block, ExternalMessage, State, StfError, PROTOCOL_VERSION};
use std::path::{Path, PathBuf};
use std::process::Command;

const FAUCET: &str = "d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c9778737";

fn genesis_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../config/genesis.toml")
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("onx-cli-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `onx-cli`, require success, return `key=value` stdout lines.
fn run(args: &[&str]) -> Vec<(String, String)> {
    let out = Command::new(env!("CARGO_BIN_EXE_onx-cli"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "onx-cli {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn field(lines: &[(String, String)], key: &str) -> String {
    lines
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("no {key}= in output {lines:?}"))
        .1
        .clone()
}

fn account(hex_str: &str) -> AccountId {
    AccountId::from_bytes(hex::decode(hex_str).unwrap().try_into().unwrap())
}

fn balance(state: &State, id: &AccountId) -> u128 {
    state.tree.get(id).map(|s| s.balance_nanos()).unwrap_or(0)
}

/// Read the single message file the CLI wrote and check its name.
fn read_pool_msg(lines: &[(String, String)]) -> ExternalMessage {
    let path = PathBuf::from(field(lines, "file"));
    let msg = ExternalMessage::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(field(lines, "hash"), hex::encode(msg.hash()));
    assert_eq!(
        path.file_name().unwrap().to_string_lossy(),
        format!("{}.msg", hex::encode(msg.hash()))
    );
    msg
}

fn genesis_state() -> State {
    let config = onx_genesis::parse_config(genesis_path()).unwrap();
    State::from_genesis(&onx_genesis::build_genesis_document(&config).unwrap())
}

fn block(state: &State, msgs: Vec<ExternalMessage>) -> Result<State, StfError> {
    let lt = state.last_lt + 1;
    let b = propose_block(state, msgs, lt, account(FAUCET), PROTOCOL_VERSION, 0)?;
    Ok(apply_block(state, &b)?.0)
}

#[test]
fn faucet_to_new_wallet_and_back() {
    let dir = scratch("roundtrip");
    let pool = dir.join("pool");
    std::fs::create_dir_all(&pool).unwrap();
    let seed = dir.join("faucet.seed");
    std::fs::write(&seed, "11".repeat(32)).unwrap();
    let genesis = genesis_path();
    let genesis = genesis.to_str().unwrap();

    let wallet_dir = dir.join("wallet");
    let created = run(&["wallet", "create", "--out", wallet_dir.to_str().unwrap()]);
    let wallet_file = wallet_dir.join("wallet.json");
    let wallet_addr = field(&created, "address");
    let shown = run(&[
        "wallet",
        "address",
        "--wallet",
        wallet_file.to_str().unwrap(),
    ]);
    assert_eq!(field(&shown, "address"), wallet_addr);

    let state0 = genesis_state();
    let faucet = account(FAUCET);
    let recipient = account(&wallet_addr);
    let faucet_before = balance(&state0, &faucet);

    // 1. Faucet -> wallet.
    let out = run(&[
        "transfer",
        "--genesis",
        genesis,
        "--seed-file",
        seed.to_str().unwrap(),
        "--from",
        FAUCET,
        "--to",
        &wallet_addr,
        "--amount",
        "500",
        "--fee",
        "10",
        "--nonce",
        "0",
        "--out",
        pool.to_str().unwrap(),
    ]);
    assert_eq!(field(&out, "chain_id"), hex::encode(state0.chain_id));
    let msg1 = read_pool_msg(&out);
    let state1 = block(&state0, vec![msg1]).expect("faucet transfer must apply");
    assert_eq!(balance(&state1, &recipient), 500);
    // The faucet is also the fee collector: it pays 500 + 10 and gets the
    // validator half of the fee back.
    let (_, validator_fee) = onx_economics::split_transaction_fee(10);
    assert_eq!(
        balance(&state1, &faucet),
        faucet_before - 510 + validator_fee
    );

    // 2. Wallet -> faucet, first spend: must reveal the key.
    let chain_hex = hex::encode(state0.chain_id);
    let no_reveal = run(&[
        "transfer",
        "--chain-id",
        &chain_hex,
        "--wallet",
        wallet_file.to_str().unwrap(),
        "--to",
        FAUCET,
        "--amount",
        "100",
        "--fee",
        "2",
        "--nonce",
        "0",
    ]);
    let bad = ExternalMessage::from_bytes(&hex::decode(field(&no_reveal, "msg")).unwrap()).unwrap();
    assert!(
        matches!(
            block(&state1, vec![bad]),
            Err(StfError::SenderHasNoKey(id)) if id == recipient
        ),
        "keyless spend without reveal must fail with SenderHasNoKey"
    );

    let out = run(&[
        "transfer",
        "--chain-id",
        &chain_hex,
        "--wallet",
        wallet_file.to_str().unwrap(),
        "--to",
        FAUCET,
        "--amount",
        "100",
        "--fee",
        "2",
        "--nonce",
        "0",
        "--reveal-key",
        "--out",
        pool.to_str().unwrap(),
    ]);
    assert_eq!(field(&out, "from"), wallet_addr);
    let msg2 = read_pool_msg(&out);
    let state2 = block(&state1, vec![msg2]).expect("reveal spend must apply");
    assert_eq!(balance(&state2, &recipient), 500 - 102);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn foreign_chain_id_is_rejected_by_stf() {
    let dir = scratch("foreign");
    let seed = dir.join("faucet.seed");
    std::fs::write(&seed, "11".repeat(32)).unwrap();
    let out = run(&[
        "transfer",
        "--chain-id",
        &"00".repeat(32),
        "--seed-file",
        seed.to_str().unwrap(),
        "--from",
        FAUCET,
        "--to",
        FAUCET,
        "--amount",
        "1",
        "--nonce",
        "0",
    ]);
    let msg = ExternalMessage::from_bytes(&hex::decode(field(&out, "msg")).unwrap()).unwrap();
    assert!(matches!(
        block(&genesis_state(), vec![msg]),
        Err(StfError::WrongChainId { .. })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cli_refuses_stateless_invalid_transfers() {
    let dir = scratch("invalid");
    let seed = dir.join("faucet.seed");
    std::fs::write(&seed, "11".repeat(32)).unwrap();
    // Each case must fail inside onx-cli's own validation, so every case
    // checks the specific error text, not just a non-zero exit.
    let fails_with = |to: &str, amount: &str, extra: &[&str], expected: &str| {
        let mut args = vec![
            "transfer".to_string(),
            "--chain-id".into(),
            "00".repeat(32),
            "--seed-file".into(),
            seed.to_str().unwrap().into(),
            "--to".into(),
            to.into(),
            "--amount".into(),
            amount.into(),
            "--nonce".into(),
            "0".into(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        let out = Command::new(env!("CARGO_BIN_EXE_onx-cli"))
            .args(&args)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?} unexpectedly succeeded");
        assert!(stderr.contains(expected), "{args:?}: stderr {stderr:?}");
    };
    fails_with(FAUCET, "0", &[], "amount must be non-zero");
    // The faucet's explicit genesis address is not derived from its key.
    fails_with(
        FAUCET,
        "1",
        &["--from", FAUCET, "--reveal-key"],
        "not the key-derived address",
    );
    fails_with("abcd", "1", &[], "--to: expected 32 bytes, got 2");
    fails_with(FAUCET, "1", &["--from", "zz"], "--from: invalid hex");
    let _ = std::fs::remove_dir_all(&dir);
}
