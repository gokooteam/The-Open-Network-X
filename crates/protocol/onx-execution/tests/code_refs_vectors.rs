//! code_refs agreement (ADR-0039, Wave 4 step 7 "code_refs LAST"): the Rust
//! implementation must agree with the independent Python reference
//! (`reference/gen_code_refs_vectors.py`, `reference/vectors/code_refs.json`).
//!
//! The vectors were generated from the spec text, not from the Rust code.
//! Two halves are pinned here:
//!   A. Live code_refs wiring — `ref_index` N addresses the Nth child of
//!      the current code cell, resolved via `cell_store`; a missing child
//!      fails closed with `AbsentNode` at `run()` entry.
//!   B. Call-stack depth limit — a taken CALL variant fails closed with
//!      `CallStackOverflow` at `MAX_CALL_STACK_DEPTH` (256).
//!
//! Each test asserts the *exception kind*, not merely "no panic": the kind is
//! what the STF maps to a bounce (see `onx-stf`), so the kind is
//! consensus-relevant.
//!
//! Note on recursion: content-addressed cells cannot reference themselves
//! (hash cycle), so unbounded self-recursion is expressed as a long chain
//! of distinct cells, built leaves-up. A hostile chain longer than the cap
//! is exactly the OOM vector ADR-0039 closes.

use onx_data_structures::{AccountId, FullAddress, Message, MessageType, WorkchainIdent};
use onx_execution::{
    Continuation, ExceptionKind, ExecutionContext, ExecutionResult, Int257, Interpreter, StackValue,
};
use onx_primitives::{Uint128, Uint256, Uint64};
use onx_state_model::Cell;
use serde_json::Value;

fn vectors() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../reference/vectors/code_refs.json"
    );
    let text = std::fs::read_to_string(path).expect("code_refs.json must exist");
    serde_json::from_str(&text).expect("code_refs.json must parse")
}

fn max_depth() -> usize {
    vectors()["max_call_stack_depth"]
        .as_u64()
        .expect("max_call_stack_depth") as usize
}

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

fn dummy_context() -> ExecutionContext {
    ExecutionContext {
        gen_utime: 1700000000,
        start_lt: 100,
        end_lt: 200,
        gas_limit: 10_000,
        chain_id: [0x43; 32], // test chain id (ADR-0038)
    }
}

/// A dummy return-address frame, as if left by earlier nested calls.
fn dummy_frame() -> (Cell, usize) {
    (Cell::new(vec![], vec![]).unwrap(), 0)
}

/// Seed `interp`'s cell store with every cell of the DAG rooted at `code`
/// (the interpreter only seeds the roots itself; the host — here, the
/// test — must provide the rest, per the `cell_store` calling convention).
fn seed_store(interp: &mut Interpreter, cells: &[Cell]) {
    for cell in cells {
        interp.cell_store.insert(cell.hash(), cell.clone());
    }
}

/// Build a cell that CALLREFs *itself* via store aliasing (the same pattern
/// as the operand-stack flood test): the cell declares child hash
/// `SELF_HASH`, and the test inserts the cell itself under that hash in
/// `cell_store`. Content-addressed cells cannot truly self-reference (hash
/// cycle); the interpreter trusts the store, and this is exactly the
/// hostile shape ADR-0039 closes — unbounded self-recursion.
const SELF_HASH: [u8; 32] = [0xAA; 32];

fn self_calling_cell() -> Cell {
    Cell::new(vec![0x71, 0x00], vec![SELF_HASH]).unwrap() // CALLREF ref 0 (itself, via the store)
}

fn interpreter_for_self_call() -> Interpreter {
    let cell = self_calling_cell();
    let mut interp = Interpreter::new(
        cell.clone(),
        Cell::new(vec![], vec![]).unwrap(),
        dummy_message(),
        dummy_context(),
    );
    interp.cell_store.insert(SELF_HASH, cell);
    interp
}

#[test]
fn vectors_pin_the_wiring_and_depth_rules() {
    let v = vectors();
    assert_eq!(v["adr"].as_str(), Some("ADR-0039"));
    // A. wiring rules.
    let wiring = &v["ref_resolution"];
    assert!(wiring["rule"].as_str().unwrap().contains("Nth child"));
    assert_eq!(wiring["missing_child"].as_str(), Some("AbsentNode"));
    // B. depth rule table: below the cap a CALL proceeds, at and above it
    // the CALL raises CallStackOverflow.
    assert_eq!(max_depth(), 256, "reference must pin 256");
    for case in v["call_rule"].as_array().expect("call_rule array") {
        let depth = case["depth_before"].as_u64().unwrap() as usize;
        let outcome = case["outcome"].as_str().unwrap();
        let expected = if depth < 256 {
            "ok"
        } else {
            "CallStackOverflow"
        };
        assert_eq!(outcome, expected, "rule mismatch at depth {depth}");
    }
    // THROW's operand domain is unchanged by ADR-0039 (VM-raised only).
    let throw_ops: Vec<u64> = v["throw_operands"]
        .as_array()
        .expect("throw_operands array")
        .iter()
        .map(|c| c["operand"].as_u64().unwrap())
        .collect();
    assert_eq!(throw_ops, vec![0, 1, 2, 3]);
    // c2 discriminator for CallStackOverflow is 5, append-only.
    let disc = v["c2_discriminators"]
        .as_array()
        .expect("c2_discriminators array")
        .iter()
        .find(|c| c["kind"] == "CallStackOverflow")
        .expect("CallStackOverflow discriminator");
    assert_eq!(disc["code"].as_u64(), Some(5));
}

#[test]
fn ref_index_addresses_children_in_index_order() {
    // A = CALLREF ref 1 with children [B, C]: ref 1 must address the
    // SECOND child. B throws, C returns — Success proves index order.
    let thrower = Cell::new(vec![0x77, 0x00], vec![]).unwrap(); // THROW IntegerOverflow
    let returner = Cell::new(vec![0x72], vec![]).unwrap(); // RET
    let root = Cell::new(vec![0x71, 0x01], vec![thrower.hash(), returner.hash()]).unwrap();
    let mut interp = Interpreter::new(
        root,
        Cell::new(vec![], vec![]).unwrap(),
        dummy_message(),
        dummy_context(),
    );
    seed_store(&mut interp, &[thrower, returner]);
    match interp.run() {
        ExecutionResult::Success { gas_used, .. } => {
            assert_eq!(gas_used, 8, "CALLREF 4 + RET 4");
        }
        ExecutionResult::Exception { kind, .. } => {
            panic!("CALLREF ref 1 must address the second child, got {kind:?}")
        }
    }
}

#[test]
fn missing_child_fails_fast_with_absent_node() {
    // The target child is not in the cell store: run() must fail closed at
    // entry with AbsentNode (never invented data), before any gas is spent.
    let missing = Cell::new(vec![0x72], vec![]).unwrap();
    let root = Cell::new(vec![0x71, 0x00], vec![missing.hash()]).unwrap();
    let mut interp = Interpreter::new(
        root,
        Cell::new(vec![], vec![]).unwrap(),
        dummy_message(),
        dummy_context(),
    );
    // Deliberately NOT seeding `missing`.
    match interp.run() {
        ExecutionResult::Exception { kind, gas_used } => {
            assert_eq!(kind, ExceptionKind::AbsentNode);
            assert_eq!(gas_used, 0);
        }
        ExecutionResult::Success { .. } => panic!("unresolvable child must fail closed"),
    }
}

#[test]
fn call_succeeds_one_below_the_cap_and_fails_at_it() {
    let cap = max_depth();
    let callee = Cell::new(vec![0x72], vec![]).unwrap(); // RET
    let root = Cell::new(vec![0x71, 0x00], vec![callee.hash()]).unwrap(); // CALLREF ref 0

    // One below the cap: the CALL pushes the cap-th frame, the callee
    // returns via c0, and execution falls off the root successfully.
    let mut interp = Interpreter::new(
        root.clone(),
        Cell::new(vec![], vec![]).unwrap(),
        dummy_message(),
        dummy_context(),
    );
    seed_store(&mut interp, std::slice::from_ref(&callee));
    for _ in 0..cap - 1 {
        interp.call_stack.push(dummy_frame());
    }
    match interp.run() {
        ExecutionResult::Success { gas_used, .. } => {
            assert_eq!(gas_used, 8, "CALLREF 4 + RET 4");
        }
        ExecutionResult::Exception { kind, .. } => {
            panic!("CALL at depth cap-1 must succeed, got {kind:?}")
        }
    }
    // The fall-off-the-end implicit return pops the pre-seeded dummy
    // frames one by one (each restores an empty cell, which immediately
    // falls off again) — deterministic, gas-free, terminating.
    assert!(
        interp.call_stack.is_empty(),
        "implicit returns must drain the pre-seeded frames"
    );

    // At the cap: the CALL fails closed with CallStackOverflow, costing
    // only the opcode's 4 gas (already consumed before the check), and no
    // frame is pushed.
    let mut interp = Interpreter::new(
        root,
        Cell::new(vec![], vec![]).unwrap(),
        dummy_message(),
        dummy_context(),
    );
    seed_store(&mut interp, &[callee]);
    for _ in 0..cap {
        interp.call_stack.push(dummy_frame());
    }
    match interp.run() {
        ExecutionResult::Exception { kind, gas_used } => {
            assert_eq!(kind, ExceptionKind::CallStackOverflow);
            assert_eq!(gas_used, 4, "only the opcode cost is charged");
        }
        ExecutionResult::Success { .. } => panic!("CALL at the cap must fail closed"),
    }
    assert_eq!(
        interp.call_stack.len(),
        cap,
        "a failed CALL must not push a frame"
    );
}

#[test]
fn self_recursive_call_terminates_with_overflow() {
    // The unbounded-memory vector from Claude's Wave 4 finding: a cell
    // that CALLREFs itself. Before ADR-0039 this grew the call stack until
    // the gas limit (~2.5M frames, ~340MB of heap); now it fails closed at
    // the depth cap and the run terminates.
    let cap = max_depth();
    let mut interp = interpreter_for_self_call();
    match interp.run() {
        ExecutionResult::Exception { kind, gas_used } => {
            assert_eq!(kind, ExceptionKind::CallStackOverflow);
            // cap CALLs succeed, then the (cap+1)-th fails: 4 gas each.
            assert_eq!(gas_used, (cap as u64 + 1) * 4);
        }
        ExecutionResult::Success { .. } => panic!("unbounded self-call must hit the depth cap"),
    }
    assert!(
        interp.call_stack.len() <= cap,
        "call stack exceeded the cap: {}",
        interp.call_stack.len()
    );
}

#[test]
fn jmp_variants_are_not_subject_to_the_depth_cap() {
    // JMPREF never touches the call stack, so a full call stack must not
    // stop a jump.
    let cap = max_depth();
    let target = Cell::new(vec![0x00], vec![]).unwrap(); // NOP
    let root = Cell::new(vec![0x70, 0x00], vec![target.hash()]).unwrap(); // JMPREF ref 0
    let mut interp = Interpreter::new(
        root,
        Cell::new(vec![], vec![]).unwrap(),
        dummy_message(),
        dummy_context(),
    );
    seed_store(&mut interp, &[target]);
    for _ in 0..cap {
        interp.call_stack.push(dummy_frame());
    }
    match interp.run() {
        ExecutionResult::Success { gas_used, .. } => {
            assert_eq!(gas_used, 5, "JMPREF 4 + NOP 1");
        }
        ExecutionResult::Exception { kind, .. } => {
            panic!("JMPREF at a full call stack must not raise, got {kind:?}")
        }
    }
}

#[test]
fn c2_handler_receives_discriminator_5() {
    // The c2 exception handler gets the deterministic discriminator for
    // CallStackOverflow (append-only: 5, existing 0-4 unmoved).
    let cap = max_depth();
    let handler = Cell::new(vec![0x00], vec![]).unwrap(); // NOP then finish
    let mut interp = interpreter_for_self_call();
    interp.set_exception_handler(Continuation::new(handler, 0));
    match interp.run() {
        ExecutionResult::Success { gas_used, .. } => {
            // (cap+1) CALLs at 4 gas each, then the handler's NOP at 1.
            assert_eq!(gas_used, (cap as u64 + 1) * 4 + 1);
        }
        ExecutionResult::Exception { kind, .. } => {
            panic!("c2 handler should catch the overflow, got {kind:?}")
        }
    }
    assert_eq!(
        interp.stack.last(),
        Some(&StackValue::Integer(Int257::from_i64(5))),
        "c2 discriminator for CallStackOverflow must be 5"
    );
}
