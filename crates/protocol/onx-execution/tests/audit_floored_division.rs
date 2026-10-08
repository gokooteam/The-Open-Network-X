//! AUDIT HARNESS — floored division at the opcode level (DIVMOD 0x14, DIV 0x17,
//! and a MOD opcode if one exists).
//!
//! The oracle is the *definition* of floored division, not a second
//! implementation: (q, r) is the floored result of a / b iff
//!     a == q*b + r            (checked exactly in 256-bit arithmetic),
//!     |r| < |b|, and
//!     r == 0 || sign(r) == sign(b).
//! These three properties determine (q, r) uniquely.
//!
//! Every interpreter run is wrapped in `catch_unwind`, so any panic on any
//! input is reported as a failure (a producer panic is a chain halt).
//!
//! Expected on `main` (Euclidean): the floored-property tests FAIL for
//! negative divisors with a nonzero remainder. Expected on a correct
//! floored branch: they PASS. Width/flavor tests document spec §3.3
//! conformance and the width 127/128 limit computation.

use onx_data_structures::{AccountId, FullAddress, Message, MessageType, WorkchainIdent};
use onx_execution::{ExceptionKind, ExecutionContext, ExecutionResult, Interpreter, StackValue};
use onx_primitives::{Uint128, Uint256, Uint64};
use onx_state_model::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};

const DIVMOD: u8 = 0x14;
const DIV: u8 = 0x17;

// ---------------------------------------------------------------------------
// Program construction / execution
// ---------------------------------------------------------------------------

fn msg() -> Message {
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

fn ctx() -> ExecutionContext {
    ExecutionContext {
        gen_utime: 1_700_000_000,
        start_lt: 100,
        end_lt: 200,
        gas_limit: 10_000,
    }
}

fn pushint(v: i128) -> Vec<u8> {
    let mut code = vec![0x08, 1];
    let mut bytes = [0u8; 32];
    if v < 0 {
        bytes[..16].fill(0xff);
    }
    bytes[16..].copy_from_slice(&v.to_be_bytes());
    code.extend(bytes);
    code
}

fn binop(op: u8, a: i128, b: i128, width: u16, flavor: u8) -> Vec<u8> {
    let mut p = pushint(a);
    p.extend(pushint(b));
    p.extend([op, (width >> 8) as u8, width as u8, flavor]);
    p
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Out {
    Ok(Vec<i128>),
    /// Success, but the final stack holds a non-Integer value (possible when
    /// probing opcodes whose operand bytes then execute as instructions).
    OkNonInteger,
    Exc(ExceptionKind),
    Panic(String),
}

fn run(program: Vec<u8>) -> Out {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut it = Interpreter::new(
            Cell::new(program, vec![]).unwrap(),
            Cell::new(vec![], vec![]).unwrap(),
            msg(),
            ctx(),
        );
        let res = it.run();
        (res, it.stack.clone())
    }));
    match r {
        Err(e) => Out::Panic(
            e.downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default(),
        ),
        Ok((ExecutionResult::Exception { kind, .. }, _)) => Out::Exc(kind),
        Ok((ExecutionResult::Success { .. }, stack)) => {
            let ints: Option<Vec<i128>> = stack
                .iter()
                .map(|v| match v {
                    StackValue::Integer(b) => StackValue::Integer(*b).to_i128().ok(),
                    _ => None,
                })
                .collect();
            ints.map_or(Out::OkNonInteger, Out::Ok)
        }
    }
}

// ---------------------------------------------------------------------------
// Exact 256-bit check of a == q*b + r (no division used anywhere)
// ---------------------------------------------------------------------------

/// Two's-complement 256-bit value as (hi: i128, lo: u128).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct I256 {
    hi: i128,
    lo: u128,
}

impl I256 {
    fn from_i128(x: i128) -> Self {
        I256 {
            hi: if x < 0 { -1 } else { 0 },
            lo: x as u128,
        }
    }
    fn add(self, o: I256) -> I256 {
        let (lo, c) = self.lo.overflowing_add(o.lo);
        I256 {
            hi: self.hi.wrapping_add(o.hi).wrapping_add(c as i128),
            lo,
        }
    }
    fn neg(self) -> I256 {
        let lo = (!self.lo).wrapping_add(1);
        let hi = (!self.hi).wrapping_add((lo == 0) as i128);
        I256 { hi, lo }
    }
    /// Exact signed product of two i128 values.
    fn mul(x: i128, y: i128) -> I256 {
        let (ux, uy) = (x.unsigned_abs(), y.unsigned_abs());
        let (x1, x0) = (ux >> 64, ux & u64::MAX as u128);
        let (y1, y0) = (uy >> 64, uy & u64::MAX as u128);
        let p00 = x0 * y0;
        let p01 = x0 * y1;
        let p10 = x1 * y0;
        let p11 = x1 * y1;
        let (mid, mc) = p01.overflowing_add(p10);
        let (lo, c1) = p00.overflowing_add(mid << 64);
        let hi_u = p11 + (mid >> 64) + ((mc as u128) << 64) + c1 as u128;
        let mag = I256 {
            hi: hi_u as i128,
            lo,
        };
        if (x < 0) != (y < 0) {
            mag.neg()
        } else {
            mag
        }
    }
}

/// True iff (q, r) is THE floored division result of a / b (b != 0).
fn is_floored(a: i128, b: i128, q: i128, r: i128) -> bool {
    let exact = I256::mul(q, b).add(I256::from_i128(r)) == I256::from_i128(a);
    let bounded = r.unsigned_abs() < b.unsigned_abs();
    let signed_ok = r == 0 || (r < 0) == (b < 0);
    exact && bounded && signed_ok
}

#[test]
fn oracle_self_check() {
    // Hand-checked floored results, including the cases Euclidean gets wrong.
    for (a, b, q, r) in [
        (7i128, 2i128, 3i128, 1i128),
        (-7, 2, -4, 1),
        (7, -2, -4, -1),
        (-7, -2, 3, -1),
        (i128::MIN, 3, -56713727820156410577229101238628035243, 1),
        (i128::MIN, -3, 56713727820156410577229101238628035242, -2),
        (i128::MAX, -2, -85070591730234615865843651857942052864, -1),
        (i128::MIN, 1, i128::MIN, 0),
        (i128::MAX, i128::MIN, -1, -1),
        (i128::MIN, i128::MAX, -2, i128::MAX - 1),
    ] {
        assert!(
            is_floored(a, b, q, r),
            "oracle rejects true floor ({a},{b})->({q},{r})"
        );
    }
    // ...and rejects the Euclidean answers for negative divisors.
    assert!(!is_floored(7, -2, -3, 1));
    assert!(!is_floored(-7, -2, 4, 1));
    // I256::mul spot checks at the extremes.
    assert_eq!(
        I256::mul(i128::MIN, i128::MIN),
        I256 {
            hi: 1i128 << 126,
            lo: 0
        }
    );
    assert_eq!(
        I256::mul(i128::MIN, -1),
        I256 {
            hi: 0,
            lo: 1u128 << 127
        }
    );
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

fn boundary_values() -> Vec<i128> {
    let mut v = vec![
        i128::MIN,
        i128::MIN + 1,
        i128::MIN + 2,
        i128::MIN / 2 - 1,
        i128::MIN / 2,
        i128::MIN / 2 + 1,
        -(1i128 << 64) - 1,
        -(1i128 << 64),
        -(1i128 << 63),
        -(1i128 << 32),
        -1_000_000_007,
        -8,
        -7,
        -3,
        -2,
        -1,
        0,
        1,
        2,
        3,
        7,
        8,
        1_000_000_007,
        1i128 << 32,
        (1i128 << 63) - 1,
        1i128 << 63,
        1i128 << 64,
        i128::MAX / 2,
        i128::MAX / 2 + 1,
        i128::MAX - 2,
        i128::MAX - 1,
        i128::MAX,
        -(1i128 << 126),
        (1i128 << 126) - 1,
        1i128 << 126,
    ];
    v.sort();
    v.dedup();
    v
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn i128(&mut self) -> i128 {
        match self.next() % 4 {
            0 => ((self.next() as u128) << 64 | self.next() as u128) as i128,
            1 => (self.next() as i64 % 1000) as i128,
            2 => {
                let b = boundary_values();
                let base = b[(self.next() % b.len() as u64) as usize];
                base.wrapping_add((self.next() % 5) as i128 - 2)
            }
            _ => (self.next() as i64) as i128,
        }
    }
}

fn pairs() -> Vec<(i128, i128)> {
    let b = boundary_values();
    let mut out: Vec<_> = b
        .iter()
        .flat_map(|&x| b.iter().map(move |&y| (x, y)))
        .collect();
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for _ in 0..60_000 {
        out.push((rng.i128(), rng.i128()));
    }
    out
}

// ---------------------------------------------------------------------------
// DIVMOD: floored semantics, error mapping, no panics
// ---------------------------------------------------------------------------

#[test]
fn divmod_is_floored_for_every_input_and_never_panics() {
    let mut wrong = Vec::new();
    let mut panics = Vec::new();
    let mut n = 0usize;
    for (a, b) in pairs() {
        n += 1;
        let out = run(binop(DIVMOD, a, b, 128, 1));
        let overflow_case = b == 0 || (a == i128::MIN && b == -1);
        match (&out, overflow_case) {
            (Out::Panic(m), _) => panics.push(format!("({a},{b}): {m}")),
            (Out::Exc(ExceptionKind::IntegerOverflow), true) => {}
            (Out::Ok(s), false) if s.len() == 2 && is_floored(a, b, s[0], s[1]) => {}
            _ => wrong.push(format!("DIVMOD({a}, {b}) -> {out:?}")),
        }
    }
    assert!(
        panics.is_empty(),
        "DIVMOD PANICKED on {} inputs: {:?}",
        panics.len(),
        &panics[..panics.len().min(5)]
    );
    assert!(
        wrong.is_empty(),
        "DIVMOD is not floored on {}/{n} inputs, e.g.:\n{}",
        wrong.len(),
        wrong[..wrong.len().min(8)].join("\n")
    );
}

#[test]
fn divmod_floored_spot_checks_negative_divisors() {
    for (a, b, q, r) in [
        (7i128, -2i128, -4i128, -1i128),
        (-7, -2, 3, -1),
        (1, -3, -1, -2),
        (-1, -3, 0, -1),
    ] {
        assert_eq!(
            run(binop(DIVMOD, a, b, 128, 1)),
            Out::Ok(vec![q, r]),
            "DIVMOD({a}, {b})"
        );
    }
}

#[test]
fn divmod_zero_divisor_and_min_neg_one_are_integer_overflow() {
    for a in boundary_values() {
        assert_eq!(
            run(binop(DIVMOD, a, 0, 128, 1)),
            Out::Exc(ExceptionKind::IntegerOverflow),
            "DIVMOD({a}, 0)"
        );
    }
    assert_eq!(
        run(binop(DIVMOD, i128::MIN, -1, 128, 1)),
        Out::Exc(ExceptionKind::IntegerOverflow)
    );
}

// ---------------------------------------------------------------------------
// DIV vs DIVMOD: one quotient
// ---------------------------------------------------------------------------

#[test]
fn div_equals_divmod_quotient_at_width_128_signed() {
    // Width 128, signed: every i128 quotient fits, so DIV must succeed
    // exactly where DIVMOD does and return the same quotient.
    let mut wrong = Vec::new();
    let mut n = 0usize;
    for (a, b) in pairs().into_iter().step_by(7) {
        n += 1;
        let dm = run(binop(DIVMOD, a, b, 128, 1));
        let d = run(binop(DIV, a, b, 128, 1));
        let ok = match (&dm, &d) {
            (Out::Ok(s), Out::Ok(t)) => t.len() == 1 && t[0] == s[0],
            (Out::Exc(x), Out::Exc(y)) => x == y,
            _ => false,
        };
        if !ok {
            wrong.push(format!("({a},{b}): DIVMOD={dm:?} DIV={d:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "DIV disagrees with DIVMOD's quotient on {}/{n} inputs at width=128 signed, e.g.:\n{}",
        wrong.len(),
        wrong[..wrong.len().min(6)].join("\n")
    );
}

#[test]
fn div_equals_divmod_quotient_where_it_fits_width_64() {
    let mut wrong = Vec::new();
    for (a, b) in pairs().into_iter().step_by(5) {
        let dm = run(binop(DIVMOD, a, b, 64, 1));
        let d = run(binop(DIV, a, b, 64, 1));
        if let Out::Ok(s) = &dm {
            let fits = s[0] >= i64::MIN as i128 && s[0] <= i64::MAX as i128;
            let want = if fits {
                Out::Ok(vec![s[0]])
            } else {
                Out::Exc(ExceptionKind::IntegerOverflow)
            };
            if d != want {
                wrong.push(format!(
                    "({a},{b}): DIVMOD.q={} DIV={d:?} want={want:?}",
                    s[0]
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "DIV/DIVMOD mismatch at width 64 on {} inputs, e.g.:\n{}",
        wrong.len(),
        wrong[..wrong.len().min(6)].join("\n")
    );
}

// ---------------------------------------------------------------------------
// DIV width/flavor (spec §3.3): includes the 127/128 limit computation
// ---------------------------------------------------------------------------

enum Spec {
    Value(i128),
    Overflow,
    /// The spec's 257-bit result has no i128 representation (known model
    /// gap, ADR-0028 Non-goals) — not asserted either way.
    Unrepresentable,
}

/// Spec §3.3 for a quotient q at (width, flavor).
fn spec_fit(q: i128, width: u32, flavor: u8) -> Spec {
    match flavor {
        // unsigned: 0 <= q < 2^width (every non-negative i128 fits at width >= 127)
        0 => {
            if q >= 0 && (width >= 127 || q < (1i128 << width)) {
                Spec::Value(q)
            } else {
                Spec::Overflow
            }
        }
        // signed: -2^(w-1) <= q < 2^(w-1) (every i128 fits at width >= 128)
        1 => {
            if width >= 128 {
                return Spec::Value(q);
            }
            let lim = 1i128 << (width - 1);
            if q >= -lim && q < lim {
                Spec::Value(q)
            } else {
                Spec::Overflow
            }
        }
        // modulo: never overflows; q mod 2^width as an unsigned bit pattern
        2 => {
            if width <= 127 {
                Spec::Value(((q as u128) & ((1u128 << width) - 1)) as i128)
            } else if q >= 0 {
                Spec::Value(q)
            } else {
                Spec::Unrepresentable
            }
        }
        _ => Spec::Overflow,
    }
}

#[test]
fn div_width_flavor_matches_spec_including_widths_127_and_128() {
    let mut wrong = Vec::new();
    for width in [1u16, 2, 7, 8, 63, 64, 65, 126, 127, 128] {
        for flavor in [0u8, 1, 2] {
            for (a, b) in [
                (7i128, 2i128),
                (-7, 2),
                (100, 1),
                (-100, 1),
                (1i128 << 100, 1),
                (5, 5),
            ] {
                let q = match run(binop(DIVMOD, a, b, 128, 1)) {
                    Out::Ok(s) => s[0],
                    _ => continue,
                };
                let want = match spec_fit(q, width as u32, flavor) {
                    Spec::Value(v) => Out::Ok(vec![v]),
                    Spec::Overflow => Out::Exc(ExceptionKind::IntegerOverflow),
                    Spec::Unrepresentable => continue,
                };
                let got = run(binop(DIV, a, b, width, flavor));
                if got != want {
                    wrong.push(format!(
                        "DIV({a},{b}) w={width} f={flavor}: got {got:?} want {want:?}"
                    ));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} DIV width/flavor mismatches:\n{}",
        wrong.len(),
        wrong[..wrong.len().min(12)].join("\n")
    );
}

#[test]
#[ignore = "known deviation: DIVMOD ignores width/flavor (ADR-0028 Non-goals); spec 3.3 conformance"]
fn div_and_divmod_agree_on_declared_width_overflow() {
    // Spec §3.3 applies the declared width to BOTH opcodes. ADR-0028 lists
    // DIVMOD's unenforced width as a known deviation; this makes it visible.
    let cases = [
        (1000i128, 1i128, 8u16, 1u8),
        (300, 1, 8, 0),
        (-1, 1, 8, 0),
        (1i128 << 70, 1, 64, 1),
    ];
    let mut wrong = Vec::new();
    for (a, b, w, f) in cases {
        let d = run(binop(DIV, a, b, w, f));
        let dm = run(binop(DIVMOD, a, b, w, f));
        let d_over = d == Out::Exc(ExceptionKind::IntegerOverflow);
        let dm_over = dm == Out::Exc(ExceptionKind::IntegerOverflow);
        if d_over != dm_over {
            wrong.push(format!("({a},{b}) w={w} f={f}: DIV={d:?} DIVMOD={dm:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "DIV and DIVMOD disagree on width overflow:\n{}",
        wrong.join("\n")
    );
}

// ---------------------------------------------------------------------------
// MOD: locate it, then require it to equal DIVMOD's remainder
// ---------------------------------------------------------------------------

fn find_mod_opcode() -> Option<u8> {
    (0x00u8..=0xFF).find(|&op| {
        if op == DIVMOD || op == DIV {
            return false;
        }
        run(binop(op, 7, 2, 128, 1)) == Out::Ok(vec![1])
            && run(binop(op, 9, 5, 128, 1)) == Out::Ok(vec![4])
            && run(binop(op, 20, 7, 128, 1)) == Out::Ok(vec![6])
    })
}

#[test]
#[ignore = "no MOD opcode exists in this branch: the interpreter implements \
            DIV (0x17) and DIVMOD (0x14) only. The branch commit message's \
            'DIV, MOD and DIVMOD' wording was incorrect. Un-ignore if a MOD \
            opcode is ever specified and implemented."]
fn mod_opcode_exists_and_equals_divmod_remainder() {
    let op = find_mod_opcode().expect(
        "no MOD opcode found anywhere in 0x00..=0xFF (spec §4.3 lists none; \
         the branch claims DIV, MOD and DIVMOD share one quotient)",
    );
    let mut wrong = Vec::new();
    for (a, b) in pairs().into_iter().step_by(7) {
        let dm = run(binop(DIVMOD, a, b, 128, 1));
        let m = run(binop(op, a, b, 128, 1));
        let ok = match (&dm, &m) {
            (Out::Ok(s), Out::Ok(t)) => t == &vec![s[1]],
            (Out::Exc(x), Out::Exc(y)) => x == y,
            _ => false,
        };
        if !ok {
            wrong.push(format!("({a},{b}): DIVMOD={dm:?} MOD={m:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "MOD (0x{op:02x}) != DIVMOD.r on {} inputs:\n{}",
        wrong.len(),
        wrong[..wrong.len().min(6)].join("\n")
    );
    eprintln!("MOD found at 0x{op:02x}; MOD(MIN,-1) = {:?} (true value 0 fits; spec §3.3 says overflow only if the result does not fit)", run(binop(op, i128::MIN, -1, 128, 1)));
}

// ---------------------------------------------------------------------------
// Whole-family no-panic sweep (all arithmetic opcodes, all widths/flavors)
// ---------------------------------------------------------------------------

#[test]
fn arithmetic_family_never_panics_on_boundary_grid() {
    let vals = boundary_values();
    let mut panics = Vec::new();
    for op in (0x10u8..=0x1F).chain([0x20]) {
        for width in [0u16, 1, 63, 64, 127, 128, 129, 256, 257, u16::MAX] {
            for flavor in [0u8, 1, 2, 3, 255] {
                for &a in vals.iter().step_by(3) {
                    for &b in vals.iter().step_by(3) {
                        if let Out::Panic(m) = run(binop(op, a, b, width, flavor)) {
                            panics
                                .push(format!("op=0x{op:02x} w={width} f={flavor} ({a},{b}): {m}"));
                        }
                    }
                }
            }
        }
    }
    assert!(
        panics.is_empty(),
        "{} panics, e.g.:\n{}",
        panics.len(),
        panics[..panics.len().min(8)].join("\n")
    );
}

#[test]
fn shifts_share_the_width_limit_computation() {
    // LSHIFT (0x18) / RSHIFT (0x19) go through the same width/flavor block
    // as DIV (interpreter.rs 478-500). Signed width 128 must accept small values.
    let mut wrong = Vec::new();
    for (op, a, s, want) in [
        (0x18u8, 1i128, 3i128, 8i128),
        (0x19, 8, 1, 4),
        (0x19, -8, 1, -4),
    ] {
        for (w, f) in [(128u16, 1u8), (127, 0), (127, 2), (126, 1)] {
            if f == 0 && want < 0 {
                continue;
            }
            let got = run(binop(op, a, s, w, f));
            if got
                != Out::Ok(vec![if f == 2 && want < 0 {
                    ((want as u128) & ((1u128 << w) - 1)) as i128
                } else {
                    want
                }])
            {
                wrong.push(format!("op=0x{op:02x}({a},{s}) w={w} f={f}: {got:?}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} shift width mismatches:\n{}",
        wrong.len(),
        wrong.join("\n")
    );
}
