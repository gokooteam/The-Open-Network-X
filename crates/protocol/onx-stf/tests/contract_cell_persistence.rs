//! End-to-end regression test for the wave-2 VM cell-store wiring.
//!
//! Exercises the STF's host contract together with the VM's child-cell
//! fix: a contract stores a child cell, gets re-invoked, and reads it back
//! via `LDREF`. The test proves:
//! 1. The interpreter's cell store is seeded from the persisted DAGs on
//!    contract load (otherwise `LDREF` fails closed with `AbsentNode` and
//!    the delivery bounces).
//! 2. The drained cell store is persisted after execution (the next
//!    invocation resolves the previously stored child).
//! 3. `LDREF` returns the *real* child (the `LDU 8` fails on the old
//!    invented placeholder, which carries no data).
//! 4. `message_body` is set from the internal message payload (the
//!    `MSGBODY` opcode exposes it; asserted via a dedicated contract).
//!
//! The bounce mechanism is the signal: any failure in the chain above
//! bounces the delivery instead of applying. A non-bounced delivery with
//! the expected new data proves the whole path.

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

fn test_secret() -> SecretKey {
    SecretKey::from_seed(&[0xA1; 32]).expect("fixed test seed is valid")
}

/// Contract A: reads the old child (proving the seed), then stores a new
/// child `[0xBB]`.
///
/// ```text
/// CTOS            ; [data_slice]
/// LDREF           ; [data_slice, child_cell]  (AbsentNode if the DAG wasn't seeded)
/// CTOS            ; [data_slice, child_slice]
/// LDU 8           ; [data_slice, child_slice, byte] (fails on the empty placeholder)
/// DROP, DROP, DROP; []
/// NEWC            ; [B_parent]
/// PUSHBYTES 1 0xBB; [B_parent, Bytes]
/// STBYTES         ; [B_parent]
/// NEWC            ; [B_parent, B_child]
/// PUSHBYTES 1 0xBB; [B_parent, B_child, Bytes]
/// STBYTES         ; [B_parent, B_child]
/// ENDC            ; [B_parent, Child]
/// STREF           ; [B_parent+ref]
/// ENDC            ; [Parent]
/// SETDATA
/// RET
/// ```
fn read_then_store_code() -> Cell {
    let code = vec![
        0x45, // CTOS
        0x48, // LDREF
        0x45, // CTOS (child cell -> slice)
        0x46, 0x00, 0x08, // LDU 8
        0x01, // DROP (byte)
        0x01, // DROP (child_slice)
        0x01, // DROP (data_slice)
        0x40, // NEWC
        0x09, 0x00, 0x01, 0xBB, // PUSHBYTES 1 0xBB
        0x44, // STBYTES
        0x40, // NEWC
        0x09, 0x00, 0x01, 0xBB, // PUSHBYTES 1 0xBB
        0x44, // STBYTES
        0x41, // ENDC
        0x43, // STREF
        0x41, // ENDC
        0x4D, // SETDATA
        0x72, // RET
    ];
    Cell::new(code, vec![]).unwrap()
}

/// Contract B: stores a child `[0xCC]` unconditionally (no read). Used to
/// prove the store-from-scratch path: the first execution has no persisted
/// DAGs at all, yet afterwards the DAG must exist.
fn store_only_code() -> Cell {
    let code = vec![
        0x40, // NEWC
        0x09, 0x00, 0x01, 0xBB, // PUSHBYTES 1 0xBB (parent data)
        0x44, // STBYTES
        0x40, // NEWC
        0x09, 0x00, 0x01, 0xCC, // PUSHBYTES 1 0xCC (child data)
        0x44, // STBYTES
        0x41, // ENDC
        0x43, // STREF
        0x41, // ENDC
        0x4D, // SETDATA
        0x72, // RET
    ];
    Cell::new(code, vec![]).unwrap()
}

/// Contract C: pushes MSGBODY and stores its length-prefixed copy as data.
/// Proves `message_body` is wired (without it, MSGBODY yields empty bytes
/// and the data cell would be empty).
fn msgbody_code() -> Cell {
    let code = vec![
        0x82, // MSGBODY -> [Bytes]
        0x40, // NEWC -> [Bytes, B]
        0x03, // SWAP -> [B, Bytes]
        0x44, // STBYTES -> [B]
        0x41, // ENDC -> [Cell]
        0x4D, // SETDATA
        0x72, // RET
    ];
    Cell::new(code, vec![]).unwrap()
}

fn child_cell(byte: u8) -> Cell {
    Cell::new(vec![byte], vec![]).unwrap()
}

fn parent_with_child(parent_byte: u8, child: &Cell) -> Cell {
    Cell::new(vec![parent_byte], vec![child.hash()]).unwrap()
}

/// Build a `ContractCellDags` from explicit cell sets (simulating what a
/// previous execution's drained store would have produced).
fn make_dags(code: &Cell, data: &Cell, extra: &[Cell]) -> ContractCellDags {
    let mut code_cells = BTreeMap::new();
    code_cells.insert(code.hash(), code.clone());
    let mut data_cells = BTreeMap::new();
    for c in extra {
        data_cells.insert(c.hash(), c.clone());
    }
    data_cells.insert(data.hash(), data.clone());
    ContractCellDags {
        code: BagOfCells::new(code.hash(), code_cells).unwrap(),
        data: BagOfCells::new(data.hash(), data_cells).unwrap(),
    }
}

fn active_contract(code: Cell, data: Cell) -> AccountState {
    AccountState::Active {
        balance_nanos: 1_000_000,
        last_trans_lt: 0,
        code: Some(code),
        data: Some(data),
        storage_stat: StorageStat {
            cell_count: 2,
            byte_count: 0,
        },
        pubkey: [0u8; 32],
        nonce: 0,
    }
}

fn funded_sender(secret: &SecretKey) -> (AccountId, AccountState) {
    let sender = AccountId::from_bytes([0xA1; 32]);
    let st = AccountState::Active {
        balance_nanos: 10_000_000,
        last_trans_lt: 0,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 0,
            byte_count: 0,
        },
        pubkey: secret.public_key().encode(),
        nonce: 0,
    };
    (sender, st)
}

fn call_msg(
    chain_id: [u8; 32],
    sender: AccountId,
    nonce: u64,
    contract: AccountId,
    payload: Vec<u8>,
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
        payload,
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
    let block = propose_block(state, vec![msg], lt, collector)?;
    let (next, receipts) = apply_block(state, &block)?;
    Ok((next, receipts))
}

fn contract_data_of(state: &State, contract: &AccountId) -> Cell {
    match state.tree.get(contract).expect("contract exists") {
        AccountState::Active {
            data: Some(data), ..
        } => data.clone(),
        other => panic!("contract account malformed: {other:?}"),
    }
}

fn assert_not_bounced(receipts: &Receipts) {
    assert_eq!(receipts.0.len(), 1);
    let d = &receipts.0[0].deliveries[0];
    assert!(!d.bounced, "delivery must not bounce: {d:?}");
    assert!(d.gas_used > 0);
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// The full cycle: seed -> execute (LDREF resolves) -> persist -> execute
/// again (LDREF resolves the previously stored child).
#[test]
fn contract_child_cell_survives_reinvocation() {
    let secret = test_secret();
    let (sender, sender_st) = funded_sender(&secret);
    let contract = AccountId::from_bytes([0xC0; 32]);
    let collector = AccountId::from_bytes([0xCC; 32]);

    let code = read_then_store_code();
    // Genesis data: parent [0xAA] with child [0xAA].
    let genesis_child = child_cell(0xAA);
    let genesis_data = parent_with_child(0xAA, &genesis_child);

    let mut tree = ShardStateTree::new();
    tree.insert(sender, sender_st).unwrap();
    tree.insert(
        contract,
        active_contract(code.clone(), genesis_data.clone()),
    )
    .unwrap();
    // Simulate a previous execution's persistence: the DAG is seeded.
    tree.set_contract_cells(
        contract,
        make_dags(&code, &genesis_data, std::slice::from_ref(&genesis_child)),
    );
    let state = State {
        tree,
        workchain: 0,
        chain_id: [0x99; 32],
        seqno: 0,
        last_lt: 0,
        last_hash: [0x99; 32],
    };

    // Call 1: LDREF must resolve the seeded [0xAA] child (else bounce),
    // then stores a new [0xBB] child.
    let msg = call_msg(
        state.chain_id,
        sender,
        0,
        contract,
        b"call1".to_vec(),
        &secret,
    );
    let (state, receipts) = apply_one(&state, msg, collector, 1).expect("call 1 must apply");
    assert_not_bounced(&receipts);

    let data1 = contract_data_of(&state, &contract);
    assert_eq!(data1.data_bytes(), &[0xBB]);
    assert_eq!(data1.cell_refs().len(), 1);
    // The persisted DAG now carries the [0xBB] child content (not just
    // the root hash).
    let dags1 = state
        .tree
        .contract_cells(&contract)
        .expect("DAGs persisted");
    let child_bb_hash = data1.cell_refs()[0];
    let child_bb = dags1.data.get_cell(&child_bb_hash).expect("child in DAG");
    assert_eq!(child_bb.data_bytes(), &[0xBB]);

    // Call 2: LDREF must resolve the [0xBB] child stored by call 1. If the
    // drained store had not been persisted, this bounces with AbsentNode.
    let msg = call_msg(
        state.chain_id,
        sender,
        1,
        contract,
        b"call2".to_vec(),
        &secret,
    );
    let (state, receipts) = apply_one(&state, msg, collector, 2).expect("call 2 must apply");
    assert_not_bounced(&receipts);

    // And the DAG was rebuilt again (still complete).
    let data2 = contract_data_of(&state, &contract);
    let dags2 = state
        .tree
        .contract_cells(&contract)
        .expect("DAGs persisted");
    let child2 = dags2
        .data
        .get_cell(&data2.cell_refs()[0])
        .expect("child in DAG");
    assert_eq!(child2.data_bytes(), &[0xBB]);
}

/// Store-from-scratch: the first execution has no persisted DAGs at all.
/// Afterwards the DAG must exist and be complete.
#[test]
fn first_execution_persists_dag_from_scratch() {
    let secret = test_secret();
    let (sender, sender_st) = funded_sender(&secret);
    let contract = AccountId::from_bytes([0xC1; 32]);
    let collector = AccountId::from_bytes([0xCC; 32]);

    let code = store_only_code();
    let empty = Cell::new(vec![], vec![]).unwrap();

    let mut tree = ShardStateTree::new();
    tree.insert(sender, sender_st).unwrap();
    tree.insert(contract, active_contract(code.clone(), empty))
        .unwrap();
    // Deliberately NO contract_cells entry: first execution ever.
    assert!(tree.contract_cells(&contract).is_none());
    let state = State {
        tree,
        workchain: 0,
        chain_id: [0x99; 32],
        seqno: 0,
        last_lt: 0,
        last_hash: [0x99; 32],
    };

    let msg = call_msg(state.chain_id, sender, 0, contract, b"go".to_vec(), &secret);
    let (state, receipts) = apply_one(&state, msg, collector, 1).expect("call must apply");
    assert_not_bounced(&receipts);

    let data = contract_data_of(&state, &contract);
    assert_eq!(data.cell_refs().len(), 1);
    let dags = state.tree.contract_cells(&contract).expect("DAG persisted");
    let child = dags
        .data
        .get_cell(&data.cell_refs()[0])
        .expect("child content persisted");
    assert_eq!(child.data_bytes(), &[0xCC]);
    // The code DAG is there too (interpreter seeds both roots).
    assert!(dags.code.get_cell(&code.hash()).is_some());
}

/// `message_body` is wired: MSGBODY exposes the internal message payload.
#[test]
fn msgbody_exposes_internal_payload() {
    let secret = test_secret();
    let (sender, sender_st) = funded_sender(&secret);
    let contract = AccountId::from_bytes([0xC2; 32]);
    let collector = AccountId::from_bytes([0xCC; 32]);

    let code = msgbody_code();
    let empty = Cell::new(vec![], vec![]).unwrap();

    let mut tree = ShardStateTree::new();
    tree.insert(sender, sender_st).unwrap();
    tree.insert(contract, active_contract(code, empty)).unwrap();
    let state = State {
        tree,
        workchain: 0,
        chain_id: [0x99; 32],
        seqno: 0,
        last_lt: 0,
        last_hash: [0x99; 32],
    };

    let payload = b"hello-msgbody".to_vec();
    let msg = call_msg(
        state.chain_id,
        sender,
        0,
        contract,
        payload.clone(),
        &secret,
    );
    let (state, receipts) = apply_one(&state, msg, collector, 1).expect("call must apply");
    assert_not_bounced(&receipts);

    // The contract stored MSGBODY's bytes as its data: they must equal the
    // internal message payload (not empty, not a hash).
    let data = contract_data_of(&state, &contract);
    assert_eq!(data.data_bytes(), &payload);
}
