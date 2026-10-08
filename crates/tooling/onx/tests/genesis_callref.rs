//! A genesis-deployed contract that `CALLREF`s, end to end (ADR-0041).
//!
//! Since ADR-0039, `run()` resolves every child of the root code cell
//! before the first instruction and fails with `AbsentNode` if one is
//! missing. Genesis used to carry root cells only and never seeded
//! `contract_cells`, so a genesis contract whose code cell has children
//! bounced on every call. Here the contract comes from a genesis TOML
//! config (`code_hex` + `child_cells_hex`) through `onx-genesis`, and the
//! call goes through the STF — no hand-built tree, no `set_contract_cells`:
//!
//! 1. In memory (`State::from_genesis`): the first call executes, the
//!    callee runs, the counter advances; the second call too.
//! 2. Through storage (`init_genesis` → reopen → `load_state` →
//!    `commit_block`): the persisted DAG passes the startup invariant and
//!    the call executes.
//! 3. The document survives its canonical encoding (version 2) unchanged.
//! 4. A config that omits the child's content is refused at genesis
//!    instead of producing a contract that can never run.

use onx_data_structures::AccountId;
use onx_genesis::{build_genesis_document, Balance, GenesisConfig, Validator, Workchain};
use onx_primitives::SecretKey;
use onx_state_model::{AccountState, Cell, GenesisDocument, GENESIS_VERSION_CONTRACT_DAGS};
use onx_stf::block::SigEntry;
use onx_stf::{apply_block, propose_block, Block, ExternalMessage, MsgKind, Receipts, State};
use onx_storage::ChainStore;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Fixtures (same caller/callee shapes as onx-stf's `callref_stf.rs`).
// ---------------------------------------------------------------------------

/// Callee: `PUSHINT 1; ADD 128; RET` — increments the top of the stack.
fn increment_callee() -> Cell {
    let mut code = vec![0x08, 0x00];
    let mut one = [0u8; 32];
    one[31] = 1;
    code.extend_from_slice(&one);
    code.extend_from_slice(&[0x10, 0x00, 0x80, 0x00, 0x72]);
    Cell::new(code, vec![]).unwrap()
}

/// Caller: `CTOS; LDU 64; SWAP; DROP; CALLREF 0; NEWC; SWAP; STBITS 64;
/// ENDC; SETDATA` — the increment happens only in the callee.
fn caller_code(callee: &Cell) -> Cell {
    let code = vec![
        0x45, 0x46, 0x00, 0x40, 0x03, 0x01, 0x71, 0x00, 0x40, 0x03, 0x42, 0x00, 0x40, 0x00, 0x41,
        0x4D,
    ];
    Cell::new(code, vec![callee.hash()]).unwrap()
}

fn counter_data(value: u64) -> Cell {
    Cell::new(value.to_be_bytes().to_vec(), vec![]).unwrap()
}

fn cell_hex(cell: &Cell) -> String {
    hex::encode(cell.to_bytes())
}

fn addr_hex(byte: u8) -> String {
    format!("{byte:02x}").repeat(32)
}

fn key(byte: u8) -> SecretKey {
    SecretKey::from_seed(&[byte; 32]).expect("fixed test seed decodes")
}

const SENDER: u8 = 0xa1;
const CONTRACT: u8 = 0xc0;
const COLLECTOR: u8 = 0xcc;
const VALIDATOR: u8 = 0x11;

fn balance(address: String, amount: u64) -> Balance {
    Balance {
        address,
        amount,
        public_key: None,
        code_hex: None,
        data_hex: None,
        child_cells_hex: Vec::new(),
    }
}

/// The genesis config: a keyed sender, the caller contract (counter 0)
/// with `child_cells` as its `child_cells_hex`, and a fee collector.
fn config(child_cells: &[Cell]) -> GenesisConfig {
    let callee = increment_callee();
    let mut sender = balance(addr_hex(SENDER), 1_000_000_000_000);
    sender.public_key = Some(hex::encode(key(SENDER).public_key().encode()));
    let mut contract = balance(addr_hex(CONTRACT), 1_000_000);
    contract.code_hex = Some(cell_hex(&caller_code(&callee)));
    contract.data_hex = Some(cell_hex(&counter_data(0)));
    contract.child_cells_hex = child_cells.iter().map(cell_hex).collect();
    GenesisConfig {
        balances: vec![sender, contract, balance(addr_hex(COLLECTOR), 1_000_000)],
        validators: vec![Validator {
            public_key: hex::encode(key(VALIDATOR).public_key().encode()),
            stake: 1_000,
        }],
        workchains: vec![Workchain {
            id: -1,
            name: "masterchain".to_string(),
            enabled: true,
        }],
    }
}

fn genesis_doc() -> GenesisDocument {
    build_genesis_document(&config(&[increment_callee()])).expect("genesis builds")
}

fn id(byte: u8) -> AccountId {
    AccountId::from_bytes([byte; 32])
}

/// A block with one signed call from the sender, at the sender's next nonce
/// as recorded in `state`.
fn call_block(state: &State, lt: u64) -> Block {
    let nonce = state.tree.get(&id(SENDER)).expect("sender exists").nonce();
    let msg = ExternalMessage::new_signed(
        state.chain_id,
        MsgKind::ContractCall,
        id(SENDER),
        nonce,
        id(CONTRACT),
        1_000,
        100_000,
        b"increment".to_vec(),
        [0u8; 32],
        &key(SENDER),
    );
    propose_block(state, vec![msg], lt, id(COLLECTOR), 1, 0).expect("block proposes")
}

fn counter_of(state: &State) -> u64 {
    match state.tree.get(&id(CONTRACT)).expect("contract exists") {
        AccountState::Active {
            data: Some(data), ..
        } => u64::from_be_bytes(data.data_bytes()[..8].try_into().unwrap()),
        other => panic!("contract account malformed: {other:?}"),
    }
}

fn assert_executed(receipts: &Receipts, what: &str) {
    let deliveries = &receipts.0[0].deliveries;
    assert_eq!(deliveries.len(), 1, "{what}: a bounce would add a refund");
    let (bounced, fatal) = (deliveries[0].bounced, deliveries[0].fatal);
    assert!(
        !bounced && !fatal,
        "{what}: a genesis contract's CALLREF must execute (bounced: {bounced}, fatal: {fatal})"
    );
    assert!(deliveries[0].gas_used > 0);
}

fn temp_db(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "onx-genesis-callref-{tag}-{}-{nanos}.redb",
        std::process::id()
    ))
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[test]
fn genesis_contract_callref_executes_through_the_stf() {
    let state = State::from_genesis(&genesis_doc());
    assert_eq!(counter_of(&state), 0);

    let block = call_block(&state, 1);
    let (state, receipts) = apply_block(&state, &block).expect("call 1 applies");
    assert_executed(&receipts, "call 1");
    assert_eq!(counter_of(&state), 1, "the callee did the increment");

    let block = call_block(&state, 2);
    let (state, receipts) = apply_block(&state, &block).expect("call 2 applies");
    assert_executed(&receipts, "call 2");
    assert_eq!(counter_of(&state), 2);
}

#[test]
fn genesis_contract_callref_executes_through_storage() {
    let path = temp_db("storage");
    let doc = genesis_doc();
    {
        let store = ChainStore::open(&path).expect("fresh store opens");
        store.init_genesis(&doc).expect("genesis initializes");
    }
    // Reopen: the ADR-0029 startup invariant checks the persisted DAGs.
    let store = ChainStore::open(&path).expect("genesis DAGs pass the startup invariant");
    let state = store.load_state().unwrap().expect("genesis state loads");
    let dags = state
        .tree
        .contract_cells(&id(CONTRACT))
        .expect("genesis persisted the contract's DAGs");
    assert!(
        dags.code.get_cell(&increment_callee().hash()).is_some(),
        "the persisted code DAG carries the callee"
    );

    let block = call_block(&state, 1);
    let (next, receipts) = apply_block(&state, &block).expect("call applies");
    assert_executed(&receipts, "stored call");
    assert_eq!(counter_of(&next), 1);
    let sig = SigEntry {
        validator_index: 0,
        sig: key(VALIDATOR)
            .sign_raw(&block.header.sign_bytes(&state.chain_id))
            .encode(),
    };
    store
        .commit_block(&state, &block, &[sig])
        .expect("block commits");
    let reloaded = store.load_state().unwrap().expect("state reloads");
    assert_eq!(counter_of(&reloaded), 1);

    drop(store);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn genesis_with_contract_dags_round_trips_as_version_2() {
    let doc = genesis_doc();
    assert_eq!(doc.contract_cells.len(), 1);
    let bytes = doc.to_bytes();
    assert_eq!(
        u32::from_be_bytes(bytes[4..8].try_into().unwrap()),
        GENESIS_VERSION_CONTRACT_DAGS
    );
    let back = GenesisDocument::from_bytes(&bytes).expect("decodes");
    // Compare the canonical form, not the struct: `storage_stat.bit_count`
    // is derived on decode (ADR-0037), not carried on the wire.
    assert_eq!(back.to_bytes(), bytes);
    assert_eq!(back.genesis_hash(), doc.genesis_hash());
    assert_eq!(back.contract_cells, doc.contract_cells);
}

#[test]
fn genesis_refuses_contract_with_missing_child_content() {
    let err = build_genesis_document(&config(&[])).expect_err("missing child must be refused");
    assert!(err.contains("missing from child_cells_hex"), "{err}");

    // A listed cell nothing reaches is refused too.
    let stray = counter_data(7);
    let err = build_genesis_document(&config(&[increment_callee(), stray]))
        .expect_err("unreachable child must be refused");
    assert!(err.contains("neither its code"), "{err}");
}
