//! Regression tests for the wave-2 VM fixes:
//! 1. `LDREF` returns the actual stored child cell (not an invented
//!    placeholder), and contract data with references round-trips intact
//!    across invocations via the interpreter's cell store.
//! 2. Contract code can read the inbound message via the `0x80` message
//!    opcodes (`MSGSENDER`, `MSGVALUE`, `MSGBODY`).

use onx_data_structures::{AccountId, FullAddress, Message, MessageType, WorkchainIdent};
use onx_execution::{
    ExceptionKind, ExecutionContext, ExecutionResult, Int257, Interpreter, StackValue,
};
use onx_primitives::{Uint128, Uint256, Uint64};
use onx_state_model::Cell;
use std::collections::BTreeMap;

fn dummy_message() -> Message {
    let addr = FullAddress::new(WorkchainIdent::BASIC, AccountId::from_bytes([0x01; 32]));
    Message {
        msg_type: MessageType::Internal,
        src_address: addr,
        dest_address: addr,
        amount_nanos: Uint128::from(1000u128),
        extra_currencies: vec![],
        created_lt: Uint64::from(100u64),
        body_cell_hash: Uint256([0xAA; 32]),
    }
}

fn dummy_context(gas_limit: u64) -> ExecutionContext {
    ExecutionContext {
        gen_utime: 1700000000,
        start_lt: 100,
        end_lt: 200,
        gas_limit,
        chain_id: [0x43; 32], // test chain id (ADR-0038)
    }
}

fn empty_cell() -> Cell {
    Cell::new(vec![], vec![]).unwrap()
}

/// Builds a parent cell referencing a real child, then reads the child back
/// through CTOS + LDREF. The old code answered LDREF with an invented
/// placeholder (`Cell::new(vec![], vec![ref_hash])`); this test fails on it
/// because the placeholder's data bytes are empty.
#[test]
fn ldref_returns_actual_stored_child() {
    let child = Cell::new(vec![0xDE, 0xAD, 0xBE, 0xEF], vec![]).unwrap();
    let parent = Cell::new(vec![0x01], vec![child.hash()]).unwrap();

    // Code: CTOS (0x45), LDREF (0x48), RET (0x72).
    let code = Cell::new(vec![0x45, 0x48, 0x72], vec![]).unwrap();
    let mut interp = Interpreter::new(code, empty_cell(), dummy_message(), dummy_context(1000));
    // The host seeds the store with the data DAG.
    interp.cell_store.insert(child.hash(), child.clone());
    interp.cell_store.insert(parent.hash(), parent.clone());
    interp.stack.push(StackValue::Cell(parent));

    let res = interp.run();
    assert!(
        matches!(res, ExecutionResult::Success { .. }),
        "expected success, got {res:?}"
    );
    match interp.stack.pop() {
        Some(StackValue::Cell(c)) => assert_eq!(
            c, child,
            "LDREF must return the actual stored child, not a placeholder"
        ),
        other => panic!("expected a Cell on top of the stack, got {other:?}"),
    }
}

/// The reviewer's scenario: a contract stores data containing cell
/// references (SETDATA), the state is saved and reloaded (simulated here by
/// draining the interpreter's cell store plus `new_data`), and the next
/// invocation reads the child back intact through CTOS + LDREF.
#[test]
fn contract_data_with_refs_round_trips_across_calls() {
    // Call 1: build parent data [0xBB], then child [0xAA], STREF the child
    // into the parent, ENDC, SETDATA.
    let build_and_store: Vec<u8> = vec![
        0x40, // NEWC -> [B_parent]
        0x09, 0x00, 0x01, 0xBB, // PUSHBYTES len=1 0xBB -> [B_parent, Bytes]
        0x44, // STBYTES -> [B_parent]
        0x40, // NEWC -> [B_parent, B_child]
        0x09, 0x00, 0x01, 0xAA, // PUSHBYTES len=1 0xAA -> [B_parent, B_child, Bytes]
        0x44, // STBYTES -> [B_parent, B_child]
        0x41, // ENDC -> [B_parent, Child]
        0x43, // STREF -> [B_parent+ref]
        0x41, // ENDC -> [Parent]
        0x4D, // SETDATA
        0x72, // RET
    ];
    let mut interp = Interpreter::new(
        Cell::new(build_and_store, vec![]).unwrap(),
        empty_cell(),
        dummy_message(),
        dummy_context(10_000),
    );
    let saved_data = match interp.run() {
        ExecutionResult::Success { new_data, .. } => new_data,
        other => panic!("call 1 failed: {other:?}"),
    };
    // The parent references exactly one child.
    assert_eq!(saved_data.data_bytes(), &[0xBB]);
    assert_eq!(saved_data.cell_refs().len(), 1);

    // Simulate the persistence boundary: everything the next invocation
    // needs is `new_data` plus the drained cell store (the full DAG).
    let persisted: BTreeMap<[u8; 32], Cell> = interp.cell_store.clone();

    // Call 2: fresh interpreter, reloaded state, read the child back.
    let read_child: Vec<u8> = vec![0x45, 0x48, 0x72]; // CTOS, LDREF, RET
    let mut interp2 = Interpreter::new(
        Cell::new(read_child, vec![]).unwrap(),
        saved_data.clone(),
        dummy_message(),
        dummy_context(1000),
    );
    interp2.cell_store.extend(persisted);
    // The STF seeds the data cell on the operand stack at entry.
    interp2.stack.push(StackValue::Cell(saved_data));

    let res2 = interp2.run();
    assert!(
        matches!(res2, ExecutionResult::Success { .. }),
        "call 2 failed: {res2:?}"
    );
    match interp2.stack.pop() {
        Some(StackValue::Cell(child)) => assert_eq!(
            child.data_bytes(),
            &[0xAA],
            "child cell content must survive the save/reload round-trip"
        ),
        other => panic!("expected a Cell on top of the stack, got {other:?}"),
    }
}

/// A child reference whose content the host did not provide must fail
/// closed with `AbsentNode` — never be answered with invented data.
#[test]
fn ldref_on_unresolved_child_fails_closed_with_absent_node() {
    // Parent references a hash nobody provided.
    let parent = Cell::new(vec![0x01], vec![[0x55; 32]]).unwrap();
    let code = Cell::new(vec![0x45, 0x48, 0x72], vec![]).unwrap(); // CTOS, LDREF, RET
    let mut interp = Interpreter::new(code, empty_cell(), dummy_message(), dummy_context(1000));
    interp.stack.push(StackValue::Cell(parent));

    match interp.run() {
        ExecutionResult::Exception {
            kind: ExceptionKind::AbsentNode,
            ..
        } => {}
        other => panic!("expected AbsentNode, got {other:?}"),
    }
}

/// `tvm-instruction-set.md` §3.5.3: a *pruned* child (a special cell the
/// host did provide) is not an absent one. `LDREF` hands it back without
/// raising, `HASHCELL` and `ISEXOTIC` work on it, and only `CTOS` on the
/// pruned cell itself raises `AbsentNode`.
#[test]
fn ldref_passes_pruned_child_through_and_only_ctos_raises() {
    let pruned = Cell::new_with_special(vec![0x00], vec![], true).unwrap();
    let parent = Cell::new(vec![0x01], vec![pruned.hash()]).unwrap();

    // CTOS, LDREF, DUP (0x02), HASHCELL (0x61), SWAP (0x03), DUP,
    // ISEXOTIC (0x49), SWAP, RET.
    // Final stack: [slice, hash, is_exotic, pruned].
    let code = Cell::new(
        vec![0x45, 0x48, 0x02, 0x61, 0x03, 0x02, 0x49, 0x03, 0x72],
        vec![],
    )
    .unwrap();
    let mut interp = Interpreter::new(code, empty_cell(), dummy_message(), dummy_context(1000));
    interp.cell_store.insert(pruned.hash(), pruned.clone());
    interp.stack.push(StackValue::Cell(parent));

    let res = interp.run();
    assert!(
        matches!(res, ExecutionResult::Success { .. }),
        "LDREF/HASHCELL/ISEXOTIC on a pruned child must not raise, got {res:?}"
    );
    match interp.stack.pop() {
        Some(StackValue::Cell(c)) => assert_eq!(c, pruned),
        other => panic!("expected the pruned child on top, got {other:?}"),
    }
    assert_eq!(
        interp.stack.pop(),
        Some(StackValue::Integer(Int257::from_u64(1)))
    );
    assert_eq!(
        interp.stack.pop(),
        Some(StackValue::Integer(Int257::from_unsigned256(
            &pruned.hash()
        )))
    );

    // Dereferencing the same pruned cell's content is what raises.
    let code = Cell::new(vec![0x45, 0x72], vec![]).unwrap(); // CTOS, RET
    let mut interp = Interpreter::new(code, empty_cell(), dummy_message(), dummy_context(1000));
    interp.stack.push(StackValue::Cell(pruned));
    match interp.run() {
        ExecutionResult::Exception {
            kind: ExceptionKind::AbsentNode,
            ..
        } => {}
        other => panic!("expected AbsentNode from CTOS on a pruned cell, got {other:?}"),
    }
}

/// Contract code can read the inbound message: sender address (36 bytes:
/// workchain i32be || account id), value in nanos (u256be integer), and the
/// raw body bytes the host provided.
#[test]
fn message_opcodes_expose_sender_value_and_body() {
    let sender = FullAddress::new(WorkchainIdent::BASIC, AccountId::from_bytes([0x77; 32]));
    let msg = Message {
        msg_type: MessageType::Internal,
        src_address: sender,
        dest_address: sender,
        amount_nanos: Uint128::from(12345u128),
        extra_currencies: vec![],
        created_lt: Uint64::from(7u64),
        body_cell_hash: Uint256([0xBB; 32]),
    };
    // MSGSENDER (0x80), MSGVALUE (0x81), MSGBODY (0x82), RET (0x72).
    let code = Cell::new(vec![0x80, 0x81, 0x82, 0x72], vec![]).unwrap();
    let mut interp = Interpreter::new(code, empty_cell(), msg, dummy_context(1000));
    interp.message_body = vec![0xCA, 0xFE];

    let res = interp.run();
    assert!(
        matches!(res, ExecutionResult::Success { .. }),
        "expected success, got {res:?}"
    );

    match interp.stack.pop() {
        Some(StackValue::Bytes(b)) => assert_eq!(b, vec![0xCA, 0xFE]),
        other => panic!("expected MSGBODY bytes, got {other:?}"),
    }
    match interp.stack.pop() {
        Some(StackValue::Integer(b)) => {
            assert_eq!(b, Int257::from_u128(12345));
        }
        other => panic!("expected MSGVALUE integer, got {other:?}"),
    }
    match interp.stack.pop() {
        Some(StackValue::Bytes(b)) => assert_eq!(b, sender.to_bytes().to_vec()),
        other => panic!("expected MSGSENDER bytes, got {other:?}"),
    }
}

/// `ENDC` registers every materialized cell in the store, so the host can
/// persist the full output DAG after execution.
#[test]
fn endc_registers_materialized_cells_in_store() {
    let code = Cell::new(
        vec![
            0x40, // NEWC
            0x09, 0x00, 0x01, 0xAA, // PUSHBYTES len=1 0xAA
            0x44, // STBYTES
            0x41, // ENDC
            0x01, // DROP
            0x72, // RET
        ],
        vec![],
    )
    .unwrap();
    let mut interp = Interpreter::new(code, empty_cell(), dummy_message(), dummy_context(1000));
    let res = interp.run();
    assert!(
        matches!(res, ExecutionResult::Success { .. }),
        "expected success, got {res:?}"
    );

    let expected = Cell::new(vec![0xAA], vec![]).unwrap();
    assert_eq!(interp.cell_store.get(&expected.hash()), Some(&expected));
}
