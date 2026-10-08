//! `CALLREF` from a real contract, end to end through the STF (ADR-0039,
//! Wave 4 step 7 — the M4 "`JMPREF`/`CALLREF` work from real contracts"
//! evidence).
//!
//! Before ADR-0039 the STF never filled `Interpreter::code_refs` (only test
//! code pushed to it), so any `CALLREF` in a deployed contract raised
//! `MalformedCell` and the delivery bounced. Here a contract whose root code
//! cell `CALLREF`s a child code cell is executed through `apply_block` — no
//! hand-built interpreter, no manual `code_refs` push. The test proves:
//! 1. The STF seeds the code DAG and `code_refs` resolves `ref 0` to the
//!    child (otherwise the delivery bounces).
//! 2. The callee runs and `RET` returns to the caller, which finishes the
//!    job (the new data cell is only written after the return).
//! 3. The result is right: the callee does the arithmetic, and the counter
//!    advances by exactly one per call.
//! 4. It keeps working on re-invocation: the code DAG persisted after the
//!    first call still carries the child.
//!
//! A negative control shows the resolution really goes through the
//! persisted code DAG: without the child cell's content, the same contract
//! bounces and the counter does not move.

use onx_data_structures::AccountId;
use onx_primitives::SecretKey;
use onx_state_model::{
    AccountState, BagOfCells, Cell, ContractCellDags, ShardStateTree, StorageStat,
};
use onx_stf::{apply_block, propose_block, ExternalMessage, MsgKind, Receipts, State, StfError};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

/// `PUSHINT 1` (unsigned, 32-byte big-endian immediate).
fn push_one() -> Vec<u8> {
    let mut code = vec![0x08, 0x00];
    let mut one = [0u8; 32];
    one[31] = 1;
    code.extend_from_slice(&one);
    code
}

/// Callee: increments the integer on top of the stack and returns.
///
/// ```text
/// PUSHINT 1       ; counter, 1
/// ADD 128         ; counter+1
/// RET             ; back to the caller
/// ```
fn increment_callee() -> Cell {
    let mut code = push_one();
    code.extend_from_slice(&[
        0x10, 0x00, 0x80, 0x00, // ADD width=128 flavor=0
        0x72, // RET
    ]);
    Cell::new(code, vec![]).unwrap()
}

/// Caller: unpacks the counter, `CALLREF`s its first code child to do the
/// increment, then packs and stores the result. The data cell is on the
/// stack at entry (STF calling convention).
///
/// ```text
/// CTOS            ; data_slice
/// LDU 64          ; data_slice, counter
/// SWAP            ; counter, data_slice
/// DROP            ; counter
/// CALLREF 0       ; counter+1        (runs the callee, which RETs here)
/// NEWC            ; counter+1, builder
/// SWAP            ; builder, counter+1
/// STBITS 64       ; builder
/// ENDC            ; new_data_cell
/// SETDATA
/// ```
fn caller_code(callee: &Cell) -> Cell {
    let code = vec![
        0x45, // CTOS
        0x46, 0x00, 0x40, // LDU 64
        0x03, // SWAP
        0x01, // DROP
        0x71, 0x00, // CALLREF ref 0
        0x40, // NEWC
        0x03, // SWAP
        0x42, 0x00, 0x40, 0x00, // STBITS width=64 signed=0
        0x41, // ENDC
        0x4D, // SETDATA
    ];
    Cell::new(code, vec![callee.hash()]).unwrap()
}

fn counter_data(value: u64) -> Cell {
    Cell::new(value.to_be_bytes().to_vec(), vec![]).unwrap()
}

fn read_counter(cell: &Cell) -> u64 {
    let bytes = cell.data_bytes();
    assert!(bytes.len() >= 8, "counter cell too short");
    u64::from_be_bytes(bytes[..8].try_into().unwrap())
}

/// Persisted cell DAGs for the contract: the code DAG holds the root and
/// `code_children`; the data DAG holds just the data root.
fn make_dags(code: &Cell, code_children: &[Cell], data: &Cell) -> ContractCellDags {
    let mut code_cells = BTreeMap::new();
    for c in code_children {
        code_cells.insert(c.hash(), c.clone());
    }
    code_cells.insert(code.hash(), code.clone());
    let mut data_cells = BTreeMap::new();
    data_cells.insert(data.hash(), data.clone());
    ContractCellDags {
        code: BagOfCells::new(code.hash(), code_cells).unwrap(),
        data: BagOfCells::new(data.hash(), data_cells).unwrap(),
    }
}

fn test_secret() -> SecretKey {
    SecretKey::from_seed(&[0xA1; 32]).expect("fixed test seed is valid")
}

/// Genesis: a funded keyed sender and the caller contract with counter 0.
/// `code_children` is what the contract's persisted code DAG carries
/// besides the root. Returns (state, sender, contract, collector, secret).
fn genesis(code_children: &[Cell]) -> (State, AccountId, AccountId, AccountId, SecretKey) {
    let sender = AccountId::from_bytes([0xA1; 32]);
    let contract = AccountId::from_bytes([0xC0; 32]);
    let collector = AccountId::from_bytes([0xCC; 32]);
    let secret = test_secret();
    let callee = increment_callee();
    let code = caller_code(&callee);
    let data = counter_data(0);

    let mut tree = ShardStateTree::new();
    tree.insert(
        sender,
        AccountState::Active {
            balance_nanos: 10_000_000,
            last_trans_lt: 0,
            code: None,
            data: None,
            storage_stat: StorageStat {
                cell_count: 0,
                byte_count: 0,
                bit_count: 0,
            },
            pubkey: secret.public_key().encode(),
            nonce: 0,
        },
    )
    .unwrap();
    tree.insert(
        contract,
        AccountState::Active {
            balance_nanos: 1_000_000,
            last_trans_lt: 0,
            code: Some(code.clone()),
            data: Some(data.clone()),
            storage_stat: StorageStat {
                cell_count: 3,
                byte_count: (code.to_bytes().len()
                    + callee.to_bytes().len()
                    + data.to_bytes().len()) as u64,
                bit_count: (code.bit_len() + callee.bit_len() + data.bit_len()) as u64,
            },
            pubkey: [0u8; 32],
            nonce: 0,
        },
    )
    .unwrap();
    tree.set_contract_cells(contract, make_dags(&code, code_children, &data));
    let state = State {
        tree,
        workchain: 0,
        chain_id: [0x99; 32],
        seqno: 0,
        last_lt: 0,
        last_hash: [0x99; 32],
    };
    (state, sender, contract, collector, secret)
}

fn call_msg(
    chain_id: [u8; 32],
    sender: AccountId,
    nonce: u64,
    contract: AccountId,
    secret: &SecretKey,
) -> ExternalMessage {
    ExternalMessage::new_signed(
        chain_id,
        MsgKind::ContractCall,
        sender,
        nonce,
        contract,
        1_000,
        100_000,
        b"increment".to_vec(),
        [0u8; 32],
        secret,
    )
}

fn apply_one(
    state: &State,
    msg: ExternalMessage,
    collector: AccountId,
    lt: u64,
) -> Result<(State, Receipts), StfError> {
    let block = propose_block(state, vec![msg], lt, collector, 1, 0)?;
    apply_block(state, &block)
}

fn counter_of(state: &State, contract: &AccountId) -> u64 {
    match state.tree.get(contract).expect("contract exists") {
        AccountState::Active {
            data: Some(data), ..
        } => read_counter(data),
        other => panic!("contract account malformed: {other:?}"),
    }
}

/// The delivery of the call itself (a bounce appends the refund after it).
fn call_delivery(receipts: &Receipts) -> &onx_stf::DeliveryReceipt {
    assert_eq!(receipts.0.len(), 1);
    &receipts.0[0].deliveries[0]
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[test]
fn stf_contract_callref_returns_right_result() {
    let (state, sender, contract, collector, secret) = genesis(&[increment_callee()]);
    assert_eq!(counter_of(&state, &contract), 0);

    // Call 1: CALLREF 0 must resolve the callee through the STF-seeded
    // code DAG, the callee increments, RET returns, the caller stores.
    let msg = call_msg(state.chain_id, sender, 0, contract, &secret);
    let (state, receipts) = apply_one(&state, msg, collector, 1).expect("call 1 must apply");
    assert_eq!(receipts.0[0].deliveries.len(), 1, "no bounce refund");
    let d = call_delivery(&receipts);
    assert!(
        !d.bounced,
        "CALLREF from a real contract must not bounce: {d:?}"
    );
    assert!(d.gas_used > 0);
    assert_eq!(counter_of(&state, &contract), 1);

    // The persisted code DAG still carries the callee after execution.
    let dags = state
        .tree
        .contract_cells(&contract)
        .expect("DAGs persisted");
    assert!(
        dags.code.get_cell(&increment_callee().hash()).is_some(),
        "callee must survive in the persisted code DAG"
    );

    // Call 2: the re-invocation resolves the callee from the DAG the first
    // call persisted, and the result is again exactly +1.
    let msg = call_msg(state.chain_id, sender, 1, contract, &secret);
    let (state, receipts) = apply_one(&state, msg, collector, 2).expect("call 2 must apply");
    let d = call_delivery(&receipts);
    assert!(!d.bounced, "second CALLREF must not bounce: {d:?}");
    assert_eq!(counter_of(&state, &contract), 2);
}

#[test]
fn stf_contract_callref_without_callee_content_bounces() {
    // Negative control: same contract, but the persisted code DAG lacks the
    // callee's content. code_refs cannot resolve ref 0, execution fails
    // closed, the delivery bounces (ADR-0037: not fatal) and the counter
    // stays put — so the success above really came from the CALLREF path.
    let (state, sender, contract, collector, secret) = genesis(&[]);
    let msg = call_msg(state.chain_id, sender, 0, contract, &secret);
    let (state, receipts) = apply_one(&state, msg, collector, 1).expect("block must still apply");
    let d = call_delivery(&receipts);
    assert!(d.bounced, "unresolvable CALLREF target must bounce: {d:?}");
    assert_eq!(counter_of(&state, &contract), 0);
}
