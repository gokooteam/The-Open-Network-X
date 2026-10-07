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
    arith_program_wf(opcode, a, b, 64, 1)
}

/// Variant of [`arith_program`] with explicit width and flavor.
fn arith_program_wf(opcode: u8, a: i128, b: i128, width: u16, flavor: u8) -> Vec<u8> {
    let mut program = pushint(a);
    program.extend(pushint(b));
    program.extend([opcode, (width >> 8) as u8, (width & 0xff) as u8, flavor]);
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

/// Success expecting a raw u128 stack value (for wrap-flavor results at
/// width 128 that exceed i128::MAX). Compares the pushed bytes, not a
/// truncated i128.
fn expect_success_u128(program: Vec<u8>, expected: u128) {
    let (result, stack) = run_program(program);
    assert!(
        matches!(result, ExecutionResult::Success { .. }),
        "expected success, got {result:?}"
    );
    assert_eq!(stack.len(), 1);
    // Pin the FULL 32-byte representation, not just the low half: the u128
    // carrier must be zero-padded, never sign-extended. A mutation rewriting
    // `from_u128` as sign-extended `from_i128` pushes 2^127 with a 0xFF high
    // half (reads back as -2^127) and must fail here.
    match &stack[0] {
        StackValue::Integer(bytes) => {
            assert_eq!(
                &bytes[0..16],
                &[0u8; 16],
                "u128 carrier high half must be zero-padded"
            );
            assert_eq!(&bytes[16..32], &expected.to_be_bytes());
        }
        other => panic!("expected Integer at stack[0], got {other:?}"),
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
    // 0x17 DIV keeps the same zero-divisor mapping as DIVMOD — for every
    // flavor, including wrap (spec §3.3: range checks precede flavor).
    expect_exception(arith_program(0x17, 5, 0), ExceptionKind::IntegerOverflow);
    for flavor in [0u8, 1, 2] {
        expect_exception(
            arith_program_wf(0x17, 5, 0, 128, flavor),
            ExceptionKind::IntegerOverflow,
        );
        expect_exception(
            arith_program_wf(0x17, -7, 0, 64, flavor),
            ExceptionKind::IntegerOverflow,
        );
    }
    // Same for LSHIFT/RSHIFT: bad shift amounts raise in every flavor.
    for flavor in [0u8, 1, 2] {
        expect_exception(
            arith_program_wf(0x18, 1, -1, 128, flavor),
            ExceptionKind::IntegerOverflow,
        );
        expect_exception(
            arith_program_wf(0x18, 1, 128, 128, flavor),
            ExceptionKind::IntegerOverflow,
        );
        expect_exception(
            arith_program_wf(0x19, 1, -1, 128, flavor),
            ExceptionKind::IntegerOverflow,
        );
    }
}

#[test]
fn divmod_is_true_floor() {
    // Spec §4.3 / ADR-0030: q = floor(a/b), r = a - q*b, with
    // sign(r) == sign(b) or r == 0. ADR-0028's Euclidean pin was wrong:
    // 7 DIVMOD -2 -> (-4, -1) (was (-3, 1)); -7 DIVMOD -2 -> (3, -1)
    // (was (4, 1)).
    for (a, b, q, r) in [
        (7i128, 2i128, 3i128, 1i128),
        (-7, 2, -4, 1),
        (7, -2, -4, -1),
        (-7, -2, 3, -1),
        (-8, 2, -4, 0),
        (0, -5, 0, 0),
        (0, -1, 0, 0),
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
        // The invariant the spec pins, checked independently of the table:
        // a = q*b + r, |r| < |b|, sign(r) == sign(b) or r == 0.
        assert_eq!(a, got_q.wrapping_mul(b).wrapping_add(got_r));
        assert!(got_r.unsigned_abs() < b.unsigned_abs());
        assert!(got_r == 0 || (got_r < 0) == (b < 0));
    }
}

#[test]
fn div_opcode_matches_divmod_quotient() {
    // 0x17 DIV returns only the quotient; it must agree with DIVMOD's
    // true-floor quotient (the old truncating `checked_div` did not).
    for (a, b, q) in [(7i128, 2i128, 3i128), (-7, 2, -4), (7, -2, -4), (-7, -2, 3)] {
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

// ---------------------------------------------------------------------------
// Width-boundary regression tests (0x17-0x19): 2^127 / 2^128 are
// unrepresentable as i128, so the width-127/128 limit paths are special.
// The old shift-based limit made EVERY width-128 op and every width-127
// unsigned-flavor op raise IntegerOverflow.
// ---------------------------------------------------------------------------

fn expect_success_i128(program: Vec<u8>) -> i128 {
    let (result, stack) = run_program(program);
    assert!(
        matches!(result, ExecutionResult::Success { .. }),
        "expected success, got {result:?}"
    );
    assert_eq!(stack.len(), 1);
    pop_i128(&stack, 0)
}

#[test]
fn div_width128_signed_accepts_full_i128_range() {
    // Signed 128 == the i128 range: extremes must succeed exactly.
    assert_eq!(
        expect_success_i128(arith_program_wf(0x17, -7, 2, 128, 1)),
        -4
    );
    assert_eq!(
        expect_success_i128(arith_program_wf(0x17, i128::MIN, 1, 128, 1)),
        i128::MIN
    );
    assert_eq!(
        expect_success_i128(arith_program_wf(0x17, i128::MAX, 1, 128, 1)),
        i128::MAX
    );
}

#[test]
fn div_width128_unsigned_rejects_negative_result() {
    // Unsigned 128: negative quotients are unrepresentable.
    expect_exception(
        arith_program_wf(0x17, -7, 2, 128, 0),
        ExceptionKind::IntegerOverflow,
    );
    // Non-negative quotients fit (2^128 > i128::MAX, no upper check needed).
    assert_eq!(expect_success_i128(arith_program_wf(0x17, 7, 2, 128, 0)), 3);
    assert_eq!(
        expect_success_i128(arith_program_wf(0x17, i128::MAX, 1, 128, 0)),
        i128::MAX
    );
}

#[test]
fn div_width128_wrap_unsigned_wraps_negative_result() {
    // Flavor 2 never raises on the result (spec §3.3): -7/2 floors to -4,
    // and -4 mod 2^128 = 2^128 - 4. (Operand range checks — e.g. division
    // by zero — still raise for every flavor.)
    expect_success_u128(
        arith_program_wf(0x17, -7, 2, 128, 2),
        u128::MAX - 3, // 2^128 - 4
    );
    expect_success_u128(arith_program_wf(0x17, 7, 2, 128, 2), 3);
    // -1 RSHIFT 1 floors to -1; wrapped at width 128 that is 2^128 - 1.
    expect_success_u128(arith_program_wf(0x19, -1, 1, 128, 2), u128::MAX);
}

#[test]
fn div_min_div_neg_one_width128_flavors() {
    // MIN / -1 has true quotient 2^127 (F2).
    // Unsigned 128 holds it exactly.
    expect_success_u128(arith_program_wf(0x17, i128::MIN, -1, 128, 0), 1u128 << 127);
    // Signed: 2^127 is out of range at every width.
    expect_exception(
        arith_program_wf(0x17, i128::MIN, -1, 128, 1),
        ExceptionKind::IntegerOverflow,
    );
    expect_exception(
        arith_program_wf(0x17, i128::MIN, -1, 64, 1),
        ExceptionKind::IntegerOverflow,
    );
    // Unsigned below 128: 2^127 >= 2^width.
    expect_exception(
        arith_program_wf(0x17, i128::MIN, -1, 64, 0),
        ExceptionKind::IntegerOverflow,
    );
    // Wrap: 2^127 mod 2^128 = 2^127; mod 2^64 = 0.
    expect_success_u128(arith_program_wf(0x17, i128::MIN, -1, 128, 2), 1u128 << 127);
    expect_success_u128(arith_program_wf(0x17, i128::MIN, -1, 64, 2), 0);
}

#[test]
fn lshift_lost_bits_raise_at_width128() {
    // F1 regressions: the old `checked_shl` silently discarded shifted-out
    // bits (only rejecting amounts >= 128).
    // Signed flavor: any lost bits mean the true product is outside
    // [-2^127, 2^127) — always IntegerOverflow here.
    for (a, b) in [(1i128, 127i128), (2, 127), (4, 126), (i128::MAX, 1)] {
        expect_exception(
            arith_program_wf(0x18, a, b, 128, 1),
            ExceptionKind::IntegerOverflow,
        );
        // Narrow widths raise for both flavors (true product >> 2^64).
        expect_exception(
            arith_program_wf(0x18, a, b, 64, 0),
            ExceptionKind::IntegerOverflow,
        );
        expect_exception(
            arith_program_wf(0x18, a, b, 64, 1),
            ExceptionKind::IntegerOverflow,
        );
    }
    // Unsigned flavor at width 128 keeps true products in [0, 2^128):
    // 1<<127 = 2^127 and (2^127-1)<<1 = 2^128-2 succeed; 2<<127 and
    // 4<<126 hit exactly 2^128 and raise.
    expect_success_u128(arith_program_wf(0x18, 1, 127, 128, 0), 1u128 << 127);
    expect_success_u128(arith_program_wf(0x18, i128::MAX, 1, 128, 0), u128::MAX - 1);
    for (a, b) in [(2i128, 127i128), (4, 126)] {
        expect_exception(
            arith_program_wf(0x18, a, b, 128, 0),
            ExceptionKind::IntegerOverflow,
        );
    }
    // Wrap flavor keeps the low bits instead.
    expect_success_u128(arith_program_wf(0x18, 1, 127, 128, 2), 1u128 << 127);
    expect_success_u128(arith_program_wf(0x18, 2, 127, 128, 2), 0);
}

#[test]
fn lshift_loses_bits_at_narrow_widths() {
    // 2^100 << 63 loses bits at every width: flavors 0/1 raise.
    // (2^100 itself fits i128, so the operand loads fine.)
    let mut operand = vec![0x08u8, 1]; // PUSHINT, signed
    let mut bytes = [0u8; 32];
    let bit: u32 = 100;
    bytes[(31 - bit / 8) as usize] |= 1 << (bit % 8);
    operand.extend(bytes);
    for width in [64u16, 127, 128] {
        for flavor in [0u8, 1] {
            let mut p = operand.clone();
            p.extend(pushint(63));
            p.extend([0x18, (width >> 8) as u8, (width & 0xff) as u8, flavor]);
            expect_exception(p, ExceptionKind::IntegerOverflow);
        }
    }
}

#[test]
fn oversize_operand_fails_closed() {
    // P1-minimal: a 256-bit operand (2^200) raises IntegerOverflow at load
    // instead of silently truncating to its low 128 bits (which was 0,
    // making ADD return the other operand unchanged).
    let mut prog = vec![0x08u8, 1]; // PUSHINT, signed flag
    let mut bytes = [0u8; 32];
    let bit: u32 = 200;
    bytes[(31 - bit / 8) as usize] |= 1 << (bit % 8);
    prog.extend(bytes);
    prog.extend(pushint(0));
    prog.extend([0x10, 0, 64, 1]); // ADD, width 64, flavor 1
    expect_exception(prog, ExceptionKind::IntegerOverflow);
}

#[test]
fn to_i128_rejects_bad_sign_extension_not_just_huge_values() {
    // The subtle case: high half all zero but bit 127 set (the encoding of
    // 2^127 as *unsigned*). A mutation moving the sign check from byte 16
    // to byte 0 accepts this as a non-negative value; the correct check
    // demands the high half be the sign extension of the low half.
    let mut bad = [0u8; 32];
    bad[16] = 0x80; // bit 127 set, high half zero -> not a sign extension
    let mut prog = vec![0x08u8, 1];
    prog.extend(bad);
    prog.extend(pushint(0));
    prog.extend([0x10, 0, 64, 1]); // ADD reads both operands via to_i128
    expect_exception(prog, ExceptionKind::IntegerOverflow);

    // Control: proper sign extensions still load. -1 (all 0xFF) + 0 = -1.
    let mut prog = vec![0x08u8, 1];
    prog.extend([0xffu8; 32]);
    prog.extend(pushint(0));
    prog.extend([0x10, 0, 64, 1]);
    assert_eq!(expect_success_i128(prog), -1);
}

#[test]
fn lshift_unsigned_bound_is_exact_at_shift_127() {
    // The unsigned fits-check `a < 2^(w-b)` is exact only with the bound
    // `shift >= 127`. Widening it to `>= 126` lets 2^126 << 2 through, and
    // `u128::checked_shl` does NOT catch the overflow (it wraps to 0):
    // true product 2^128 is outside [0, 2^128) and must raise.
    expect_exception(
        arith_program_wf(0x18, 1 << 126, 2, 128, 0),
        ExceptionKind::IntegerOverflow,
    );
    // Boundary controls: 2^126 << 1 = 2^127 fits; (2^127 - 1) << 1 fits.
    expect_success_u128(arith_program_wf(0x18, 1 << 126, 1, 128, 0), 1u128 << 127);
    expect_success_u128(arith_program_wf(0x18, i128::MAX, 1, 128, 0), u128::MAX - 1);
}

#[test]
fn div_width127_unsigned_flavors_no_longer_reject_everything() {
    // Width 127, flavor 0: [0, 2^127); every non-negative i128 fits.
    assert_eq!(expect_success_i128(arith_program_wf(0x17, 7, 2, 127, 0)), 3);
    assert_eq!(
        expect_success_i128(arith_program_wf(0x17, i128::MAX, 1, 127, 0)),
        i128::MAX
    );
    expect_exception(
        arith_program_wf(0x17, -7, 2, 127, 0),
        ExceptionKind::IntegerOverflow,
    );
    // Width 127, flavor 2: wrap of a negative lands in [0, 2^127),
    // always representable: -7 mod 2^127 == 2^127 - 7.
    assert_eq!(
        expect_success_i128(arith_program_wf(0x17, -7, 2, 127, 2)),
        i128::MAX - 3 // floor(-7/2) = -4; -4 mod 2^127 = 2^127 - 4
    );
    assert_eq!(
        expect_success_i128(arith_program_wf(0x17, i128::MIN, 1, 127, 2)),
        0
    );
}

#[test]
fn shift_width128_signed_succeeds() {
    // 0x18 SHL at width 128 used to die in the limit computation.
    assert_eq!(
        expect_success_i128(arith_program_wf(0x18, 1, 100, 128, 1)),
        1i128 << 100
    );
    // Shift-amount bound still enforced at width 128.
    expect_exception(
        arith_program_wf(0x18, 1, 128, 128, 1),
        ExceptionKind::IntegerOverflow,
    );
}

#[test]
fn div_width64_unsigned_bound_still_enforced() {
    // Narrow widths keep their exact bounds: 2^64 - 1 is the max for
    // flavor 0 at width 64, so i128::MAX / 1 must still fail.
    expect_exception(
        arith_program_wf(0x17, i128::MAX, 1, 64, 0),
        ExceptionKind::IntegerOverflow,
    );
    assert_eq!(
        expect_success_i128(arith_program_wf(0x17, u64::MAX as i128, 1, 64, 0)),
        u64::MAX as i128
    );
}
