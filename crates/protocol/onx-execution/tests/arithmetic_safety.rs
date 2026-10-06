//! Wave-3 chain-safety tests (ADR-0028): every arithmetic opcode must map
//! adversarial inputs to [`ExceptionKind`], never panic the producer.
//!
//! Each test asserts the *exception kind*, not merely "no panic": the kind is
//! what the STF maps to a bounce (see `onx-stf`, `ExecutionResult::Exception
//! -> must_bounce`), so the kind is consensus-relevant.

use onx_data_structures::{AccountId, FullAddress, Message, MessageType, WorkchainIdent};
use onx_execution::{ExceptionKind, ExecutionContext, ExecutionResult, Interpreter, StackValue};
use onx_primitives::{Uint128, Uint256, Uint64};
use onx_state_model::Cell;

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
    }
}

/// PUSHINT encoding: 0x08, signed flag byte, then the 32-byte big-endian value
/// (upper 16 bytes sign-extended), matching `StackValue::from_i128`.
fn pushint(value: i128) -> Vec<u8> {
    let mut code = vec![0x08, 1];
    let mut bytes = [0u8; 32];
    if value < 0 {
        bytes[..16].fill(0xff);
    }
    bytes[16..].copy_from_slice(&value.to_be_bytes());
    code.extend(bytes);
    code
}

fn pushbytes(bytes: &[u8]) -> Vec<u8> {
    let mut code = vec![0x09, (bytes.len() >> 8) as u8, (bytes.len() & 0xff) as u8];
    code.extend_from_slice(bytes);
    code
}

/// Builds `(a, b) -> op` for the width/flavor-parameterized arithmetic family
/// (0x10-0x15, 0x17-0x19): operands then `opcode, width_hi, width_lo, flavor`.
fn arith_program(opcode: u8, a: i128, b: i128) -> Vec<u8> {
    let mut program = pushint(a);
    program.extend(pushint(b));
    program.extend([opcode, 0, 64, 1]); // width = 64, flavor = signed
    program
}

fn run_program(program: Vec<u8>) -> (ExecutionResult, Vec<StackValue>) {
    let mut interpreter = Interpreter::new(
        Cell::new(program, vec![]).unwrap(),
        Cell::new(vec![], vec![]).unwrap(),
        dummy_message(),
        dummy_context(),
    );
    let result = interpreter.run();
    let stack = interpreter.stack.clone();
    (result, stack)
}

fn expect_exception(program: Vec<u8>, kind: ExceptionKind) {
    match run_program(program).0 {
        ExecutionResult::Exception { kind: got, .. } => {
            assert_eq!(got, kind, "wrong exception kind");
        }
        ExecutionResult::Success { .. } => panic!("expected {kind:?}, got success"),
    }
}

fn pop_i128(stack: &[StackValue], idx: usize) -> i128 {
    match &stack[idx] {
        StackValue::Integer(bytes) => StackValue::Integer(*bytes).to_i128().unwrap(),
        other => panic!("expected Integer at stack[{idx}], got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// DIVMOD (0x14): the chain-halt class — i128::MIN / -1 panics in plain `/`.
// ---------------------------------------------------------------------------

#[test]
fn divmod_min_div_neg_one_is_integer_overflow() {
    // The exact producer-halt from the wave-3 review: `a / b` on
    // (i128::MIN, -1) panics. Must be IntegerOverflow (=> bounce).
    expect_exception(
        arith_program(0x14, i128::MIN, -1),
        ExceptionKind::IntegerOverflow,
    );
}

#[test]
fn divmod_min_rem_neg_one_is_integer_overflow() {
    // Same pair through the remainder path: the quotient overflows first,
    // and the observable behavior is still IntegerOverflow, never a panic.
    expect_exception(
        arith_program(0x14, i128::MIN, -1),
        ExceptionKind::IntegerOverflow,
    );
}

#[test]
fn divmod_by_zero_still_integer_overflow() {
    // Pre-existing mapping (ADR-0024 §3.3): division by zero has no
    // representable result, so it stays IntegerOverflow.
    expect_exception(arith_program(0x14, 5, 0), ExceptionKind::IntegerOverflow);
    expect_exception(arith_program(0x14, -5, 0), ExceptionKind::IntegerOverflow);
    expect_exception(arith_program(0x14, 0, 0), ExceptionKind::IntegerOverflow);
}

#[test]
fn div_by_zero_still_integer_overflow() {
    // 0x17 DIV keeps the same zero-divisor mapping as DIVMOD.
    expect_exception(arith_program(0x17, 5, 0), ExceptionKind::IntegerOverflow);
}

#[test]
fn divmod_is_floored_with_nonnegative_remainder() {
    // Spec §4.2: (a, b) -> (a div b, a mod b) with a = q*b + r and
    // 0 <= r < |b|. Pinned by ADR-0028 for negative divisors too.
    for (a, b, q, r) in [
        (-7i128, 2i128, -4i128, 1i128),
        (7, -2, -3, 1),
        (-7, -2, 4, 1),
        (7, 2, 3, 1),
        (-8, 2, -4, 0),
        (0, -5, 0, 0),
    ] {
        let (result, stack) = run_program(arith_program(0x14, a, b));
        assert!(
            matches!(result, ExecutionResult::Success { .. }),
            "DIVMOD({a}, {b}) should succeed, got {result:?}"
        );
        assert_eq!(stack.len(), 2, "DIVMOD pushes (q, r)");
        let got_q = pop_i128(&stack, 0);
        let got_r = pop_i128(&stack, 1);
        assert_eq!((got_q, got_r), (q, r), "DIVMOD({a}, {b})");
        // The invariant the spec pins, checked independently of the table.
        assert_eq!(a, got_q.wrapping_mul(b).wrapping_add(got_r));
        assert!((0..b.abs()).contains(&got_r));
    }
}

#[test]
fn div_opcode_matches_divmod_quotient() {
    // 0x17 DIV returns only the quotient; it must agree with DIVMOD's
    // floored quotient (the old truncating `checked_div` did not).
    for (a, b, q) in [(-7i128, 2i128, -4i128), (7, -2, -3), (-7, -2, 4), (7, 2, 3)] {
        let (result, stack) = run_program(arith_program(0x17, a, b));
        assert!(
            matches!(result, ExecutionResult::Success { .. }),
            "DIV({a}, {b}) should succeed, got {result:?}"
        );
        assert_eq!(stack.len(), 1);
        assert_eq!(pop_i128(&stack, 0), q, "DIV({a}, {b})");
    }
}

#[test]
fn divmod_min_max_edges() {
    // Extreme magnitudes that stay representable must succeed exactly.
    for (a, b, q, r) in [
        (i128::MAX, 1, i128::MAX, 0),
        (i128::MIN, 1, i128::MIN, 0),
        (i128::MAX, -1, -i128::MAX, 0),
        (i128::MIN, 2, i128::MIN / 2, 0),
        (i128::MIN, -2, 1i128 << 126, 0),
    ] {
        let (result, stack) = run_program(arith_program(0x14, a, b));
        assert!(
            matches!(result, ExecutionResult::Success { .. }),
            "DIVMOD edge ({a}, {b}) should succeed, got {result:?}"
        );
        assert_eq!((pop_i128(&stack, 0), pop_i128(&stack, 1)), (q, r));
    }
}

// ---------------------------------------------------------------------------
// Sibling arithmetic opcodes: same panic class, checked_* -> IntegerOverflow.
// ---------------------------------------------------------------------------

#[test]
fn add_sub_mul_neg_overflow_is_integer_overflow() {
    expect_exception(
        arith_program(0x10, i128::MAX, 1),
        ExceptionKind::IntegerOverflow,
    ); // ADD
    expect_exception(
        arith_program(0x10, i128::MIN, -1),
        ExceptionKind::IntegerOverflow,
    );
    expect_exception(
        arith_program(0x11, i128::MIN, 1),
        ExceptionKind::IntegerOverflow,
    ); // SUB
    expect_exception(
        arith_program(0x11, i128::MAX, -1),
        ExceptionKind::IntegerOverflow,
    );
    expect_exception(
        arith_program(0x13, i128::MAX, 2),
        ExceptionKind::IntegerOverflow,
    ); // MUL
    expect_exception(
        arith_program(0x13, i128::MIN, -1),
        ExceptionKind::IntegerOverflow,
    );
    // NEG takes one operand: reuse the builder with a dummy second push.
    let mut neg_min = pushint(i128::MIN);
    neg_min.extend([0x12, 0, 64, 1]);
    expect_exception(neg_min, ExceptionKind::IntegerOverflow);
    let mut neg_max = pushint(i128::MAX);
    neg_max.extend([0x12, 0, 64, 1]);
    let (result, stack) = run_program(neg_max);
    assert!(matches!(result, ExecutionResult::Success { .. }));
    assert_eq!(pop_i128(&stack, 0), -i128::MAX);
}

#[test]
fn lshift_overflow_and_bad_shift_is_integer_overflow() {
    // 1 << 100 overflows a signed 64-bit width (flavor=1) -> IntegerOverflow.
    expect_exception(arith_program(0x18, 1, 100), ExceptionKind::IntegerOverflow);
    // Negative shift amounts are rejected, not performed.
    expect_exception(arith_program(0x18, 1, -1), ExceptionKind::IntegerOverflow);
    // Shift >= width is rejected.
    expect_exception(arith_program(0x18, 1, 64), ExceptionKind::IntegerOverflow);
    // A shift that fits the width succeeds: 1 << 62 at width 64 signed.
    let (result, stack) = run_program(arith_program(0x18, 1, 62));
    assert!(
        matches!(result, ExecutionResult::Success { .. }),
        "{result:?}"
    );
    assert_eq!(pop_i128(&stack, 0), 1i128 << 62);
}

// ---------------------------------------------------------------------------
// SUBBYTES (0x32): adversarial (offset, len) pairs must not panic on usize.
// ---------------------------------------------------------------------------

#[test]
fn subbytes_offset_len_overflow_is_malformed_cell() {
    // offset = i128::MAX wraps to usize::MAX; +1 overflows usize and used
    // to panic the producer in debug builds. Stack order for SUBBYTES is
    // (bytes, offset, len) with len on top.
    let mut program = pushbytes(b"hello");
    program.extend(pushint(i128::MAX)); // offset
    program.extend(pushint(1)); // len
    program.push(0x32);
    expect_exception(program, ExceptionKind::MalformedCell);

    // Negative offset wraps to a huge usize: out of range, not a panic.
    let mut program = pushbytes(b"hello");
    program.extend(pushint(-1)); // offset
    program.extend(pushint(2)); // len
    program.push(0x32);
    expect_exception(program, ExceptionKind::MalformedCell);
}

#[test]
fn subbytes_happy_path_unchanged() {
    let mut program = pushbytes(b"hello world");
    program.extend(pushint(6)); // offset
    program.extend(pushint(5)); // len
    program.push(0x32);
    let (result, stack) = run_program(program);
    assert!(
        matches!(result, ExecutionResult::Success { .. }),
        "{result:?}"
    );
    assert_eq!(stack.len(), 1);
    match &stack[0] {
        StackValue::Bytes(bytes) => assert_eq!(bytes, b"world"),
        other => panic!("expected Bytes, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Stack-depth invariant: adversarial PUSHINT floods respect the 1023 cap.
// ---------------------------------------------------------------------------

#[test]
fn stack_depth_cap_holds_under_pushint_flood() {
    // A single cell holds at most 128 bytes, so flood the stack with a
    // self-recursive cell: 3x PUSHINT + CALLREF to itself. Each iteration
    // pushes 3 values for 7 gas; the 1023 cap trips (~341 iterations,
    // ~2.4k gas) long before the 10k gas limit, and `push` fails closed
    // with MalformedCell instead of growing the stack unboundedly.
    let mut cell_bytes = Vec::new();
    for _ in 0..3 {
        cell_bytes.extend(pushint(1));
    }
    cell_bytes.extend([0x71, 0x00]); // CALLREF ref 0 (itself)
    let cell = Cell::new(cell_bytes, vec![]).unwrap();

    let mut interpreter = Interpreter::new(
        cell.clone(),
        Cell::new(vec![], vec![]).unwrap(),
        dummy_message(),
        dummy_context(),
    );
    interpreter.code_refs.push(cell);
    match interpreter.run() {
        ExecutionResult::Exception { kind, .. } => {
            assert_eq!(kind, ExceptionKind::MalformedCell);
        }
        ExecutionResult::Success { .. } => panic!("recursive push flood must hit the stack cap"),
    }
    assert!(
        interpreter.stack.len() <= 1023,
        "stack exceeded cap: {}",
        interpreter.stack.len()
    );
}
