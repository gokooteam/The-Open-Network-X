//! Wave-4 integer-model tests (ADR-0035): the 257-bit `Integer` kind
//! `[-2^256, 2^256 - 1]`, exact arithmetic, and declared width/flavor
//! enforcement on every opcode of the arithmetic family.
//!
//! Each test asserts the *exception kind*, not merely "no panic": the kind is
//! what the STF maps to a bounce (see `onx-stf`, `ExecutionResult::Exception
//! -> must_bounce`), so the kind is consensus-relevant.
//!
//! Note on PUSHINT-encodability: the `PUSHINT` operand is 32 bytes plus a
//! signed flag, so bytecode-level tests can push `[0, 2^256)` (unsigned)
//! and `[-2^255, 2^255)` (signed). Negative values below `-2^255` are
//! representable by `Int257` but not producible by any opcode at declared
//! widths `<= 256`; they are covered by the `int257` unit tests and the
//! Python reference vectors (`tests/int257_vectors.rs`), not here.

use onx_data_structures::{AccountId, FullAddress, Message, MessageType, WorkchainIdent};
use onx_execution::{
    ExceptionKind, ExecutionContext, ExecutionResult, Int257, Interpreter, StackValue,
};
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

/// PUSHINT of an `i128` (signed flag = 1, 32-byte two's complement).
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

/// PUSHINT with an explicit signed flag and raw 32-byte big-endian operand.
fn pushint_raw(signed: u8, bytes: [u8; 32]) -> Vec<u8> {
    let mut code = vec![0x08, signed];
    code.extend(bytes);
    code
}

/// `2^bit` as a 32-byte big-endian unsigned operand.
fn u256be_bit(bit: u32) -> [u8; 32] {
    assert!(bit < 256);
    let mut b = [0u8; 32];
    b[(31 - bit / 8) as usize] |= 1 << (bit % 8);
    b
}

/// PUSHINT of the unsigned 256-bit value `2^bit` (signed flag = 0).
fn pushint_pow2(bit: u32) -> Vec<u8> {
    pushint_raw(0, u256be_bit(bit))
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

fn pop_int257(stack: &[StackValue], idx: usize) -> Int257 {
    match &stack[idx] {
        StackValue::Integer(v) => *v,
        other => panic!("expected Integer at stack[{idx}], got {other:?}"),
    }
}

/// Success expecting one Integer; returns it for exact comparison.
fn expect_success_int(program: Vec<u8>) -> Int257 {
    let (result, stack) = run_program(program);
    assert!(
        matches!(result, ExecutionResult::Success { .. }),
        "expected success, got {result:?}"
    );
    assert_eq!(stack.len(), 1);
    pop_int257(&stack, 0)
}

/// Narrows an `Int257` known to fit in `i128` (test assertion helper).
fn int_to_i128(v: Int257) -> i128 {
    let b = v.to_bytes33();
    let sign_byte = b[0];
    assert!(sign_byte == 0 || sign_byte == 1);
    let fill = if sign_byte == 1 { 0xFF } else { 0x00 };
    assert!(
        b[1..17].iter().all(|&x| x == fill),
        "test value does not fit i128"
    );
    let mut arr = [0u8; 16];
    arr.copy_from_slice(&b[17..33]);
    i128::from_be_bytes(arr)
}

fn expect_success_i128(program: Vec<u8>) -> i128 {
    int_to_i128(expect_success_int(program))
}

/// Success expecting the stack top to be an Integer (the stack may hold
/// more values below, e.g. LDU/LDI leave `[Slice, Integer]`).
fn expect_top_i128(program: Vec<u8>) -> i128 {
    let (result, stack) = run_program(program);
    assert!(
        matches!(result, ExecutionResult::Success { .. }),
        "expected success, got {result:?}"
    );
    assert!(!stack.is_empty(), "expected a non-empty stack");
    int_to_i128(pop_int257(&stack, stack.len() - 1))
}

// ---------------------------------------------------------------------------
// DIVMOD (0x14): the chain-halt class — unguarded division panics.
// ---------------------------------------------------------------------------

#[test]
fn divmod_unrepresentable_quotient_is_integer_overflow() {
    // The wave-3 producer-halt was (i128::MIN, -1) on a 128-bit carrier.
    // The 257-bit analog: (-2^255, -1) has true quotient 2^255, which fits
    // no signed width <= 256 — IntegerOverflow (=> bounce), never a panic.
    let mut neg = [0u8; 32];
    neg[0] = 0x80; // -2^255 as signed 256-bit
    let a_prog = pushint_raw(1, neg);
    // True quotient 2^255: fits unsigned 256, nothing else.
    let mut prog = a_prog.clone();
    prog.extend(pushint(-1));
    prog.extend([0x14, 1, 0, 0]); // width 256, unsigned -> 2^255
    let (result, stack) = run_program(prog);
    assert!(matches!(result, ExecutionResult::Success { .. }));
    assert_eq!(
        pop_int257(&stack, 0),
        Int257::from_unsigned256(&u256be_bit(255))
    );
    assert_eq!(pop_int257(&stack, 1), Int257::zero());
    for width in [64u16, 128] {
        for flavor in [0u8, 1] {
            let mut prog = a_prog.clone();
            prog.extend(pushint(-1));
            prog.extend([0x14, (width >> 8) as u8, (width & 0xff) as u8, flavor]);
            expect_exception(prog, ExceptionKind::IntegerOverflow);
        }
    }
    // Signed 256: 2^255 >= 2^255 -> raise.
    let mut prog = a_prog.clone();
    prog.extend(pushint(-1));
    prog.extend([0x14, 1, 0, 1]);
    expect_exception(prog, ExceptionKind::IntegerOverflow);
    // Wrap at 256: q = 2^255 mod 2^256 = 2^255, r = 0.
    let mut prog = a_prog.clone();
    prog.extend(pushint(-1));
    prog.extend([0x14, 1, 0, 2]);
    let (result, stack) = run_program(prog);
    assert!(
        matches!(result, ExecutionResult::Success { .. }),
        "wrap DIVMOD should succeed, got {result:?}"
    );
    assert_eq!(stack.len(), 2);
    assert_eq!(
        pop_int257(&stack, 0),
        Int257::from_unsigned256(&u256be_bit(255))
    );
    assert_eq!(pop_int257(&stack, 1), Int257::zero());
    // Wrap at 128: q = 2^255 mod 2^128 = 0, r = 0.
    let mut prog = a_prog.clone();
    prog.extend(pushint(-1));
    prog.extend([0x14, 0, 128, 2]);
    let (result, stack) = run_program(prog);
    assert!(matches!(result, ExecutionResult::Success { .. }));
    assert_eq!(pop_int257(&stack, 0), Int257::zero());
    assert_eq!(pop_int257(&stack, 1), Int257::zero());
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
        // The §3.3 shift rule (`b >= width` raises) is pinned below width
        // 128 too: a mutation relaxing the guard to `b >= 128` would let
        // `0 LSHIFT 64` at width 64 through.
        expect_exception(
            arith_program_wf(0x18, 0, 64, 64, flavor),
            ExceptionKind::IntegerOverflow,
        );
        expect_exception(
            arith_program_wf(0x19, 1, 64, 64, flavor),
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
        let got_q = int_to_i128(pop_int257(&stack, 0));
        let got_r = int_to_i128(pop_int257(&stack, 1));
        assert_eq!((got_q, got_r), (q, r), "DIVMOD({a}, {b})");
        // The invariant the spec pins, checked independently of the table:
        // a = q*b + r, |r| < |b|, sign(r) == sign(b) or r == 0.
        assert_eq!(a, got_q.wrapping_mul(b).wrapping_add(got_r));
        assert!(got_r.unsigned_abs() < b.unsigned_abs());
        assert!(got_r == 0 || (got_r < 0) == (b < 0));
    }
}

#[test]
fn divmod_floor_at_257_bit_edges() {
    // (2^200) DIVMOD 3 at width 256 unsigned. The expected quotient is
    // independently computed (Python: divmod(2**200, 3) == (q, 1)) and
    // pasted as a byte literal — not derived from the implementation:
    // q = (2^200 - 1) / 3 = 0x5555...5555 (50 hex fives, 200 bits).
    let mut q_bytes = [0u8; 32];
    q_bytes[7..32].fill(0x55);
    let mut prog = pushint_pow2(200);
    prog.extend(pushint(3));
    prog.extend([0x14, 1, 0, 0]); // DIVMOD, width 256, unsigned
    let (result, stack) = run_program(prog);
    assert!(
        matches!(result, ExecutionResult::Success { .. }),
        "DIVMOD(2^200, 3) should succeed, got {result:?}"
    );
    assert_eq!(stack.len(), 2);
    assert_eq!(pop_int257(&stack, 0), Int257::from_unsigned256(&q_bytes));
    assert_eq!(pop_int257(&stack, 1), Int257::one());
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
        assert_eq!(int_to_i128(pop_int257(&stack, 0)), q, "DIV({a}, {b})");
    }
}

// ---------------------------------------------------------------------------
// Width/flavor enforcement on the baseline family (0x10-0x14) — new in
// ADR-0035. Previously the flavor operand was ignored entirely.
// ---------------------------------------------------------------------------

#[test]
fn baseline_family_enforces_width_and_flavor() {
    // ADD(200, 100): true sum 300.
    // width 8 unsigned: 300 >= 256 -> raise; wrap -> 44; signed -> raise.
    expect_exception(
        arith_program_wf(0x10, 200, 100, 8, 0),
        ExceptionKind::IntegerOverflow,
    );
    expect_exception(
        arith_program_wf(0x10, 200, 100, 8, 1),
        ExceptionKind::IntegerOverflow,
    );
    assert_eq!(
        int_to_i128(expect_success_int(arith_program_wf(0x10, 200, 100, 8, 2))),
        44
    );
    // width 16 unsigned: 300 fits.
    assert_eq!(
        expect_success_i128(arith_program_wf(0x10, 200, 100, 16, 0)),
        300
    );
    // SUB(0, 1): true -1. unsigned -> raise; signed width 8 -> -1; wrap -> 255.
    expect_exception(
        arith_program_wf(0x11, 0, 1, 8, 0),
        ExceptionKind::IntegerOverflow,
    );
    assert_eq!(expect_success_i128(arith_program_wf(0x11, 0, 1, 8, 1)), -1);
    assert_eq!(expect_success_i128(arith_program_wf(0x11, 0, 1, 8, 2)), 255);
    // MUL(2^100, 2^100): true 2^200. width 128 unsigned -> raise;
    // wrap -> 0; width 256 unsigned -> 2^200.
    let pow2_100 = pushint_pow2(100);
    for flavor in [0u8, 1] {
        let mut prog = pow2_100.clone();
        prog.extend(pow2_100.clone());
        prog.extend([0x13, 0, 128, flavor]);
        expect_exception(prog, ExceptionKind::IntegerOverflow);
    }
    let mut prog = pow2_100.clone();
    prog.extend(pow2_100.clone());
    prog.extend([0x13, 0, 128, 2]);
    assert_eq!(expect_success_int(prog), Int257::zero());
    let mut prog = pow2_100.clone();
    prog.extend(pow2_100.clone());
    prog.extend([0x13, 1, 0, 0]); // width 256, unsigned
    assert_eq!(
        expect_success_int(prog),
        Int257::from_unsigned256(&u256be_bit(200))
    );
    // NEG(-2^127): true 2^127. width 128 signed -> raise; unsigned -> fits.
    let mut prog = pushint(i128::MIN);
    prog.extend([0x12, 0, 128, 1]);
    expect_exception(prog, ExceptionKind::IntegerOverflow);
    let mut prog = pushint(i128::MIN);
    prog.extend([0x12, 0, 128, 0]);
    assert_eq!(expect_success_int(prog), Int257::from_u128(1u128 << 127));
    // DIVMOD(-7, 2) at width 8: q = -4 fits signed; r = 1 fits.
    let (result, stack) = run_program(arith_program_wf(0x14, -7, 2, 8, 1));
    assert!(matches!(result, ExecutionResult::Success { .. }));
    assert_eq!(int_to_i128(pop_int257(&stack, 0)), -4);
    assert_eq!(int_to_i128(pop_int257(&stack, 1)), 1);
    // DIVMOD(-7, 2) at width 8 unsigned: q = -4 -> raise.
    expect_exception(
        arith_program_wf(0x14, -7, 2, 8, 0),
        ExceptionKind::IntegerOverflow,
    );
}

#[test]
fn cmp_is_total_order_at_257_bits() {
    // CMP never raises (spec §4.3 fault column "—") and now compares the
    // full 257-bit values. ADR-0031's bulkhead made CMP(2^127, 0) raise;
    // the model says 1.
    let pow2_127 = pushint_pow2(127);
    let mut prog = pow2_127.clone();
    prog.extend(pushint(0));
    prog.extend([0x15, 0, 64, 1]); // width/flavor carried but not constraining
    assert_eq!(expect_success_i128(prog), 1);
    // 2^200 > 2^100.
    let mut prog = pushint_pow2(200);
    prog.extend(pushint_pow2(100));
    prog.extend([0x15, 1, 0, 1]);
    assert_eq!(expect_success_i128(prog), 1);
    // -5 < 5, 5 == 5.
    assert_eq!(expect_success_i128(arith_program_wf(0x15, -5, 5, 8, 1)), -1);
    assert_eq!(expect_success_i128(arith_program_wf(0x15, 5, 5, 8, 1)), 0);
}

#[test]
fn add_sub_mul_neg_overflow_is_integer_overflow() {
    // True results that miss the declared width raise — even when they
    // would fit the 257-bit carrier (the width check is the rule now, not
    // the carrier).
    expect_exception(
        arith_program(0x10, i128::MAX, 1),
        ExceptionKind::IntegerOverflow,
    ); // ADD: 2^127+... > width 64
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
    // NEG of a value whose negation fits the width succeeds.
    let mut neg_five = pushint(5);
    neg_five.extend([0x12, 0, 64, 1]);
    assert_eq!(expect_success_i128(neg_five), -5);
    // ...but the same true results SUCCEED at a fitting width:
    // ADD(i128::MAX, 1) = 2^127 fits unsigned 128.
    assert_eq!(
        expect_success_int(arith_program_wf(0x10, i128::MAX, 1, 128, 0)),
        Int257::from_u128(1u128 << 127)
    );
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
    assert_eq!(int_to_i128(pop_int257(&stack, 0)), 1i128 << 62);
}

// ---------------------------------------------------------------------------
// PUSHINT signed flag, CONV, STBITS, LDI (ADR-0035 corrections).
// ---------------------------------------------------------------------------

#[test]
fn pushint_signed_flag_honored() {
    // The same 32 bytes with bit 255 set: signed=1 -> the negative value
    // -(2^255 - 1); signed=0 -> the positive value 2^255 + 1.
    let mut bytes = [0u8; 32];
    bytes[0] = 0x80;
    bytes[31] = 0x01;
    // Signed: bytes are -(2^256) + 2^255 + 1 = -(2^255 - 1). ADD 0 at
    // width 256 signed keeps it; the 257-bit encoding is
    // [0x01, 0x80, 0x00..0x01].
    let mut prog = pushint_raw(1, bytes);
    prog.extend(pushint(0));
    prog.extend([0x10, 1, 0, 1]); // ADD, width 256, signed
    let mut expected33 = [0u8; 33];
    expected33[0] = 0x01;
    expected33[1] = 0x80;
    expected33[32] = 0x01;
    assert_eq!(
        expect_success_int(prog),
        Int257::from_bytes33(&expected33).unwrap()
    );
    // NEG of it, wrap flavor at 256: (2^255 - 1).
    let mut prog = pushint_raw(1, bytes);
    prog.extend([0x12, 1, 0, 2]); // NEG, width 256, wrap
    let mut ebytes = [0xFFu8; 32];
    ebytes[0] = 0x7F; // 2^255 - 1
    assert_eq!(expect_success_int(prog), Int257::from_unsigned256(&ebytes));
    // Unsigned: the same bytes are the positive value 2^255 + 1, so NEG
    // at width 256 unsigned raises, and CMP(bytes, 0) == 1.
    let mut prog = pushint_raw(0, bytes);
    prog.extend([0x12, 1, 0, 0]); // NEG, width 256, unsigned -> raise
    expect_exception(prog, ExceptionKind::IntegerOverflow);
    let mut prog = pushint_raw(0, bytes);
    prog.extend(pushint(0));
    prog.extend([0x15, 1, 0, 1]); // CMP(bytes, 0) == 1
    assert_eq!(expect_success_i128(prog), 1);
}

#[test]
fn conv_rechecks_at_declared_width() {
    // CONV was a no-op; now it range-checks (spec §4.2, ADR-0035).
    // 2^200 does not fit unsigned 128.
    let mut prog = pushint_pow2(200);
    prog.extend([0x20, 0, 128, 0]); // CONV width 128, unsigned
    expect_exception(prog, ExceptionKind::IntegerOverflow);
    // ...but fits unsigned 256, and the value is unchanged.
    let mut prog = pushint_pow2(200);
    prog.extend([0x20, 1, 0, 0]);
    assert_eq!(
        expect_success_int(prog),
        Int257::from_unsigned256(&u256be_bit(200))
    );
    // -1 fits signed 8, not unsigned 8.
    let mut prog = pushint(-1);
    prog.extend([0x20, 0, 8, 1]);
    assert_eq!(expect_success_i128(prog), -1);
    let mut prog = pushint(-1);
    prog.extend([0x20, 0, 8, 0]);
    expect_exception(prog, ExceptionKind::IntegerOverflow);
    // Width 0 is malformed.
    let mut prog = pushint(1);
    prog.extend([0x20, 0, 0, 1]);
    expect_exception(prog, ExceptionKind::MalformedCell);
    // A signed->unsigned reinterpretation: CONV(-1, 8, unsigned) raises,
    // but the wrap path is ADD's flavor-2 job, not CONV's.
}

#[test]
fn stbits_range_checks_with_signedness() {
    // STBITS(-1, width 8, signed): fits -> byte 0xFF.
    let mut prog = vec![0x40]; // NEWC
    prog.extend(pushint(-1));
    prog.extend([0x42, 0, 8, 1]); // STBITS width 8, signed
    prog.extend([0x41]); // ENDC
    prog.extend([0x45]); // CTOS
    prog.extend([0x46, 0, 8]); // LDU 8
    assert_eq!(expect_top_i128(prog), 255);
    // STBITS(300, width 8, unsigned): 300 >= 256 -> IntegerOverflow.
    let mut prog = vec![0x40];
    prog.extend(pushint(300));
    prog.extend([0x42, 0, 8, 0]);
    expect_exception(prog, ExceptionKind::IntegerOverflow);
    // STBITS(255, width 8, signed): 255 >= 128 -> IntegerOverflow.
    let mut prog = vec![0x40];
    prog.extend(pushint(255));
    prog.extend([0x42, 0, 8, 1]);
    expect_exception(prog, ExceptionKind::IntegerOverflow);
    // STBITS(255, width 8, unsigned): fits -> 0xFF.
    let mut prog = vec![0x40];
    prog.extend(pushint(255));
    prog.extend([0x42, 0, 8, 0]);
    prog.extend([0x41]);
    prog.extend([0x45]);
    prog.extend([0x46, 0, 8]);
    assert_eq!(expect_top_i128(prog), 255);
}

#[test]
fn ldi_sign_extends() {
    // LDI was byte-identical to LDU (spec divergence). Build a cell holding
    // the byte 0xFF via STBITS(-1, 8, signed), then:
    // LDU 8 -> 255, LDI 8 -> -1.
    for (opcode, want) in [(0x46u8, 255i128), (0x47u8, -1i128)] {
        let mut prog = vec![0x40]; // NEWC
        prog.extend(pushint(-1));
        prog.extend([0x42, 0, 8, 1]); // STBITS width 8, signed -> 0xFF
        prog.extend([0x41]); // ENDC
        prog.extend([0x45]); // CTOS
        prog.extend([opcode, 0, 8]); // LDU/LDI 8
        assert_eq!(expect_top_i128(prog), want, "opcode 0x{opcode:02x}");
    }
    // LDI 16 of 0xFFFF -> -1; LDI 9 of 0x1FF -> -1 (bit 8 set).
    let mut prog = vec![0x40];
    prog.extend(pushint(-1));
    prog.extend([0x42, 0, 16, 1]); // 16 bits all set
    prog.extend([0x41]);
    prog.extend([0x45]);
    prog.extend([0x47, 0, 16]);
    assert_eq!(expect_top_i128(prog), -1);
}

// ---------------------------------------------------------------------------
// SUBBYTES (0x32): adversarial (offset, len) pairs must not panic on usize.
// ---------------------------------------------------------------------------

#[test]
fn subbytes_offset_len_overflow_is_malformed_cell() {
    // offset = i128::MAX is fine as a value but out of range for the 5-byte
    // input: MalformedCell, not a panic.
    let mut program = pushbytes(b"hello");
    program.extend(pushint(i128::MAX)); // offset
    program.extend(pushint(1)); // len
    program.push(0x32);
    expect_exception(program, ExceptionKind::MalformedCell);

    // Negative offset: MalformedCell, not a panic.
    let mut program = pushbytes(b"hello");
    program.extend(pushint(-1)); // offset
    program.extend(pushint(2)); // len
    program.push(0x32);
    expect_exception(program, ExceptionKind::MalformedCell);

    // A 257-bit offset (>= 2^128) is not a usize: MalformedCell.
    let mut program = pushbytes(b"hello");
    program.extend(pushint_pow2(200)); // offset
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
// Control-flow truthiness at 257 bits (bulkhead removal, ADR-0035).
// ---------------------------------------------------------------------------

#[test]
fn ifelse_truthiness_covers_full_257_bit_domain() {
    // Layout: PUSHINT x (34B) | 0x78 t f (3B) | PUSHINT 7 (34B) | RET (1B)
    //         | PUSHINT 9 (34B) | RET (1B).
    // pc after 0x78 is 37; true branch at 37 (offset 0), false at 72
    // (offset 35).
    for (x_prog, want) in [
        (pushint_pow2(200), 7i128), // 2^200 != 0 -> true branch
        (pushint(0), 9i128),        // 0 -> false branch
        (pushint(-3), 7i128),
    ] {
        let mut prog = x_prog;
        prog.extend([0x78, 0, 35]);
        prog.extend(pushint(7));
        prog.push(0x72); // RET
        prog.extend(pushint(9));
        prog.push(0x72); // RET
        assert_eq!(expect_success_i128(prog), want);
    }
}

// ---------------------------------------------------------------------------
// Width-boundary regression tests (0x17-0x19), now at 257 bits.
// ---------------------------------------------------------------------------

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
    // Non-negative quotients fit (2^128 > i128::MAX, no upper check needed
    // for i128-range inputs).
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
    let mut lo = [0u8; 32];
    lo[16..32].copy_from_slice(&(u128::MAX - 3).to_be_bytes());
    assert_eq!(
        expect_success_int(arith_program_wf(0x17, -7, 2, 128, 2)),
        Int257::from_unsigned256(&lo)
    );
    assert_eq!(expect_success_i128(arith_program_wf(0x17, 7, 2, 128, 2)), 3);
    // -1 RSHIFT 1 floors to -1; wrapped at width 128 that is 2^128 - 1.
    let mut ebytes = [0u8; 32];
    ebytes[16..32].fill(0xFF); // 2^128 - 1
    let mut prog = pushint(-1);
    prog.extend(pushint(1));
    prog.extend([0x19, 0, 128, 2]);
    assert_eq!(expect_success_int(prog), Int257::from_unsigned256(&ebytes));
}

#[test]
fn div_min_div_neg_one_width128_flavors() {
    // i128::MIN / -1 has true quotient 2^127.
    // Unsigned 128 holds it exactly.
    assert_eq!(
        expect_success_int(arith_program_wf(0x17, i128::MIN, -1, 128, 0)),
        Int257::from_u128(1u128 << 127)
    );
    // Signed: 2^127 is out of range at every width <= 128.
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
    assert_eq!(
        expect_success_int(arith_program_wf(0x17, i128::MIN, -1, 128, 2)),
        Int257::from_u128(1u128 << 127)
    );
    assert_eq!(
        expect_success_int(arith_program_wf(0x17, i128::MIN, -1, 64, 2)),
        Int257::zero()
    );
}

#[test]
fn div_257_bit_width256_boundaries() {
    // (2^256 - 1) / 1 at width 256 unsigned: the domain maximum survives.
    let max_bytes = [0xFFu8; 32];
    let mut prog = pushint_raw(0, max_bytes);
    prog.extend(pushint(1));
    prog.extend([0x17, 1, 0, 0]); // DIV, width 256, unsigned
    assert_eq!(
        expect_success_int(prog),
        Int257::from_unsigned256(&max_bytes)
    );
    // (-2^255) / (-1) = 2^255: fits unsigned 256, not signed 256.
    let mut neg = [0u8; 32];
    neg[0] = 0x80;
    let mut prog = pushint_raw(1, neg);
    prog.extend(pushint(-1));
    prog.extend([0x17, 1, 0, 0]);
    assert_eq!(
        expect_success_int(prog),
        Int257::from_unsigned256(&u256be_bit(255))
    );
    let mut prog = pushint_raw(1, neg);
    prog.extend(pushint(-1));
    prog.extend([0x17, 1, 0, 1]);
    expect_exception(prog, ExceptionKind::IntegerOverflow);
}

#[test]
fn lshift_lost_bits_raise_at_width128() {
    // True products that miss the declared width raise for flavors 0/1.
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
    assert_eq!(
        expect_success_int(arith_program_wf(0x18, 1, 127, 128, 0)),
        Int257::from_u128(1u128 << 127)
    );
    // (2^127 - 1) << 1 = 2^128 - 2 = 0xFFFF...FFFE.
    let mut prog = pushint(i128::MAX);
    prog.extend(pushint(1));
    prog.extend([0x18, 0, 128, 0]);
    let got = expect_success_int(prog);
    let mut ebytes = [0xFFu8; 32];
    ebytes[0..16].fill(0x00);
    ebytes[31] = 0xFE;
    assert_eq!(got, Int257::from_unsigned256(&ebytes));
    for (a, b) in [(2i128, 127i128), (4, 126)] {
        expect_exception(
            arith_program_wf(0x18, a, b, 128, 0),
            ExceptionKind::IntegerOverflow,
        );
    }
    // Wrap flavor keeps the low bits instead.
    assert_eq!(
        expect_success_int(arith_program_wf(0x18, 1, 127, 128, 2)),
        Int257::from_u128(1u128 << 127)
    );
    assert_eq!(
        expect_success_int(arith_program_wf(0x18, 2, 127, 128, 2)),
        Int257::zero()
    );
}

#[test]
fn lshift_loses_bits_at_narrow_widths() {
    // 2^100 << 63 loses bits at every width: flavors 0/1 raise.
    let operand = pushint_pow2(100);
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
fn large_operand_checked_at_width_not_at_load() {
    // ADR-0031's fail-closed load is gone: a 256-bit operand (2^200)
    // loads fine now. The *declared width* is what constrains it.
    let mut prog = pushint_pow2(200);
    prog.extend(pushint(0));
    prog.extend([0x10, 0, 64, 1]); // ADD, width 64, signed
    expect_exception(prog, ExceptionKind::IntegerOverflow);
    // At a fitting width it succeeds: ADD(2^200, 0) = 2^200.
    let mut prog = pushint_pow2(200);
    prog.extend(pushint(0));
    prog.extend([0x10, 1, 0, 0]); // width 256, unsigned
    assert_eq!(
        expect_success_int(prog),
        Int257::from_unsigned256(&u256be_bit(200))
    );
}

#[test]
fn lshift_unsigned_bound_is_exact_at_shift_127() {
    // 2^126 << 2 has true product 2^128, outside [0, 2^128): must raise.
    // (The old carrier needed a `shift >= 127` special-case for this;
    // exact arithmetic gets it structurally.)
    expect_exception(
        arith_program_wf(0x18, 1 << 126, 2, 128, 0),
        ExceptionKind::IntegerOverflow,
    );
    // Boundary controls: 2^126 << 1 = 2^127 fits; (2^127 - 1) << 1 fits.
    assert_eq!(
        expect_success_int(arith_program_wf(0x18, 1 << 126, 1, 128, 0)),
        Int257::from_u128(1u128 << 127)
    );
    let mut prog = pushint(i128::MAX);
    prog.extend(pushint(1));
    prog.extend([0x18, 0, 128, 0]);
    let mut ebytes = [0xFFu8; 32];
    ebytes[0..16].fill(0x00);
    ebytes[31] = 0xFE; // 2^128 - 2 = 0xFFFF...FFFE
    assert_eq!(expect_success_int(prog), Int257::from_unsigned256(&ebytes));
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
    // always representable: floor(-7/2) = -4; -4 mod 2^127 = 2^127 - 4
    // = 0x7FFF...FFFC.
    let mut ebytes = [0xFFu8; 32];
    ebytes[0..16].fill(0x00);
    ebytes[16] = 0x7F;
    ebytes[31] = 0xFC;
    assert_eq!(
        expect_success_int(arith_program_wf(0x17, -7, 2, 127, 2)),
        Int257::from_unsigned256(&ebytes)
    );
    assert_eq!(
        expect_success_i128(arith_program_wf(0x17, i128::MIN, 1, 127, 2)),
        0
    );
}

#[test]
fn shift_width128_signed_succeeds() {
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
