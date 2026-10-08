//! AUDIT HARNESS — floored division at the opcode level (DIVMOD 0x14, DIV 0x17),
//! now over the 257-bit `Integer` model (ADR-0035).
//!
//! The oracle is the *definition* of floored division, not a second
//! implementation of the interpreter: (q, r) is the floored result of a / b
//! iff
//!     a == q*b + r            (checked exactly, see `SInt`),
//!     |r| < |b|, and
//!     r == 0 || sign(r) == sign(b).
//! These three properties determine (q, r) uniquely.
//!
//! `SInt` is a test-only exact integer (sign + magnitude limbs), independent
//! of `int257.rs`'s two's-complement limb code; `oracle_self_check`
//! validates it against hand-computed values. Width/flavor application is
//! re-derived in the test (`SInt::fit`) from the spec text, not from the
//! Rust code.
//!
//! Every interpreter run is wrapped in `catch_unwind`, so any panic on any
//! input is reported as a failure (a producer panic is a chain halt).

use onx_data_structures::{AccountId, FullAddress, Message, MessageType, WorkchainIdent};
use onx_execution::{
    ExceptionKind, ExecutionContext, ExecutionResult, Int257, Interpreter, StackValue,
};
use onx_primitives::{Uint128, Uint256, Uint64};
use onx_state_model::Cell;
use std::cmp::Ordering;
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

/// PUSHINT of an `Int257`. Panics for values the 32-byte operand cannot
/// encode (negatives below `-2^255`); the sweep only uses encodable values.
fn pushint257(v: Int257) -> Vec<u8> {
    let b = v.to_bytes33();
    let mut raw = [0u8; 32];
    raw.copy_from_slice(&b[1..33]);
    let signed = if v.is_negative() {
        assert_eq!(
            Int257::from_signed256(&raw),
            v,
            "test value {v:?} is not PUSHINT-encodable"
        );
        1
    } else {
        0
    };
    let mut code = vec![0x08, signed];
    code.extend(raw);
    code
}

fn binop(op: u8, a: Int257, b: Int257, width: u16, flavor: u8) -> Vec<u8> {
    let mut p = pushint257(a);
    p.extend(pushint257(b));
    p.extend([op, (width >> 8) as u8, width as u8, flavor]);
    p
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Out {
    Ok(Vec<SInt>),
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
            let ints: Option<Vec<SInt>> = stack
                .iter()
                .map(|v| match v {
                    StackValue::Integer(i) => Some(SInt::from_int257(*i)),
                    _ => None,
                })
                .collect();
            ints.map_or(Out::OkNonInteger, Out::Ok)
        }
    }
}

// ---------------------------------------------------------------------------
// Test-only exact integer oracle (sign + magnitude, independent of int257.rs)
// ---------------------------------------------------------------------------

/// Exact signed integer for the oracle: sign plus a 576-bit magnitude.
/// Deliberately sign-magnitude (not two's complement) so it shares no
/// implementation lineage with `int257.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SInt {
    neg: bool, // false when the value is zero
    mag: [u64; 9],
}

impl SInt {
    fn zero() -> Self {
        SInt {
            neg: false,
            mag: [0; 9],
        }
    }

    fn one() -> Self {
        let mut mag = [0u64; 9];
        mag[0] = 1;
        SInt { neg: false, mag }
    }

    fn is_zero(&self) -> bool {
        self.mag == [0; 9]
    }

    fn from_int257(v: Int257) -> Self {
        let b = v.to_bytes33();
        let neg = b[0] & 1 == 1;
        // Low 256 bits, little-endian (bytes[1..33]); limb 4 holds only
        // bit 256 and is set below.
        let mut tw = [0u64; 5];
        for i in 0..4usize {
            tw[i] = u64::from_be_bytes(b[33 - 8 * (i + 1)..33 - 8 * i].try_into().unwrap());
        }
        if !neg {
            let mut mag = [0u64; 9];
            mag[..5].copy_from_slice(&tw);
            // tw[4] is 0 for non-negative (canonical encoding).
            debug_assert_eq!(tw[4], 0);
            return SInt { neg: false, mag };
        }
        // Negative: the magnitude is the two's-complement negation.
        // Sign-extend bit 256 over bits 257..320 first: for a negative
        // value bit 256 is 1, so tw[4] becomes all-ones.
        tw[4] = !0u64;
        let mut mag = [0u64; 9];
        let mut carry = 1u64;
        for i in 0..5usize {
            let (w, c) = (!tw[i]).overflowing_add(carry);
            mag[i] = w;
            carry = c as u64;
        }
        // |v| <= 2^256 < 2^320: the negation is exact in 5 limbs.
        SInt { neg: true, mag }
    }

    fn cmp_mag(a: &[u64; 9], b: &[u64; 9]) -> Ordering {
        for i in (0..9).rev() {
            if a[i] != b[i] {
                return a[i].cmp(&b[i]);
            }
        }
        Ordering::Equal
    }

    fn add_mag(a: &[u64; 9], b: &[u64; 9]) -> [u64; 9] {
        // Caller guarantees the sum fits in 576 bits.
        let mut out = [0u64; 9];
        let mut carry = 0u64;
        for i in 0..9 {
            let t = (a[i] as u128) + (b[i] as u128) + (carry as u128);
            debug_assert!(t < (1u128 << 65));
            out[i] = t as u64;
            carry = (t >> 64) as u64;
        }
        debug_assert_eq!(carry, 0);
        out
    }

    fn sub_mag(a: &[u64; 9], b: &[u64; 9]) -> [u64; 9] {
        // Caller guarantees a >= b.
        let mut out = [0u64; 9];
        let mut borrow = 0u64;
        for i in 0..9 {
            let (r1, b1) = a[i].overflowing_sub(b[i]);
            let (r2, b2) = r1.overflowing_sub(borrow);
            out[i] = r2;
            borrow = (b1 as u64) + (b2 as u64);
        }
        debug_assert_eq!(borrow, 0);
        out
    }

    fn mul_mag(a: &[u64; 9], b: &[u64; 9]) -> [u64; 9] {
        // Caller guarantees the product fits in 576 bits.
        let mut wide = [0u64; 18];
        for (i, &x) in a.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &y) in b.iter().enumerate() {
                let k = i + j;
                let t = (x as u128) * (y as u128) + (wide[k] as u128) + (carry as u128);
                wide[k] = t as u64;
                carry = (t >> 64) as u64;
            }
            let mut k = i + 9;
            while carry != 0 {
                let t = (wide[k] as u128) + (carry as u128);
                wide[k] = t as u64;
                carry = (t >> 64) as u64;
                k += 1;
            }
        }
        debug_assert!(wide[9..18].iter().all(|&w| w == 0));
        let mut out = [0u64; 9];
        out.copy_from_slice(&wide[..9]);
        out
    }

    fn neg(&self) -> Self {
        if self.is_zero() {
            *self
        } else {
            SInt {
                neg: !self.neg,
                mag: self.mag,
            }
        }
    }

    fn add(&self, o: &Self) -> Self {
        if self.neg == o.neg {
            SInt {
                neg: self.neg,
                mag: Self::add_mag(&self.mag, &o.mag),
            }
        } else {
            match Self::cmp_mag(&self.mag, &o.mag) {
                Ordering::Greater => SInt {
                    neg: self.neg,
                    mag: Self::sub_mag(&self.mag, &o.mag),
                },
                Ordering::Less => SInt {
                    neg: o.neg,
                    mag: Self::sub_mag(&o.mag, &self.mag),
                },
                Ordering::Equal => SInt::zero(),
            }
        }
    }

    fn sub(&self, o: &Self) -> Self {
        self.add(&o.neg())
    }

    fn mul(&self, o: &Self) -> Self {
        if self.is_zero() || o.is_zero() {
            return SInt::zero();
        }
        SInt {
            neg: self.neg != o.neg,
            mag: Self::mul_mag(&self.mag, &o.mag),
        }
    }

    /// Unsigned division with remainder over 9 limbs (`d != 0`): binary
    /// long division. Returns `(quotient, remainder)`.
    fn udivmod_mag(n: &[u64; 9], d: &[u64; 9]) -> ([u64; 9], [u64; 9]) {
        let mut q = [0u64; 9];
        let mut r = [0u64; 9];
        for bit in (0..576u32).rev() {
            let incoming = (n[(bit >> 6) as usize] >> (bit & 63)) & 1;
            let mut carry = incoming;
            for w in r.iter_mut() {
                let next_carry = *w >> 63;
                *w = (*w << 1) | carry;
                carry = next_carry;
            }
            if Self::cmp_mag(&r, d) != Ordering::Less {
                r = Self::sub_mag(&r, d);
                q[(bit >> 6) as usize] |= 1 << (bit & 63);
            }
        }
        (q, r)
    }

    /// `(q, r)` with `q = floor(a/b)`, `r = a - q*b`; `b != 0`.
    fn floored_div(a: &Self, b: &Self) -> (Self, Self) {
        assert!(!b.is_zero());
        let (q0, r0) = Self::udivmod_mag(&a.mag, &b.mag);
        let q0s = SInt {
            neg: false,
            mag: q0,
        };
        let r0s = SInt {
            neg: false,
            mag: r0,
        };
        if r0s.is_zero() {
            let q = if a.neg == b.neg { q0s } else { q0s.neg() };
            return (q, SInt::zero());
        }
        let q_t = if a.neg == b.neg { q0s } else { q0s.neg() };
        let r_t = if a.neg { r0s.neg() } else { r0s };
        if a.neg != b.neg {
            // Truncation rounded up past the floor: step down, hand the
            // remainder one divisor. Then |r| = |b| - |r0| < |b| and
            // sign(r) = sign(b).
            (q_t.sub(&SInt::one()), r_t.add(b))
        } else {
            (q_t, r_t)
        }
    }

    fn bitlen(mag: &[u64; 9]) -> u32 {
        for i in (0..9).rev() {
            if mag[i] != 0 {
                return i as u32 * 64 + (64 - mag[i].leading_zeros());
            }
        }
        0
    }

    /// Applies `(width, flavor)` per spec §3.3, re-derived from the spec
    /// text (not from the Rust code). `None` = `IntegerOverflow`.
    fn fit(&self, width: u32, flavor: u8) -> Option<Self> {
        match flavor {
            // Unsigned: 0 <= v < 2^width.
            0 => {
                if self.neg || Self::bitlen(&self.mag) > width {
                    return None;
                }
                Some(*self)
            }
            // Signed: -2^(w-1) <= v < 2^(w-1).
            1 => {
                let bl = Self::bitlen(&self.mag);
                if self.neg {
                    // |v| <= 2^(w-1): bitlen < w, or exactly 2^(w-1).
                    let mut pow = [0u64; 9];
                    pow[((width - 1) / 64) as usize] = 1u64 << ((width - 1) % 64);
                    if !(bl < width || (bl == width && self.mag == pow)) {
                        return None;
                    }
                } else {
                    // v <= 2^(w-1) - 1  <=>  bitlen(v) <= w - 1.
                    if bl > width - 1 {
                        return None;
                    }
                }
                Some(*self)
            }
            // Modulo: v mod 2^width as the unsigned bit pattern.
            2 => {
                let mut m = [0u64; 9];
                let full = (width / 64) as usize;
                m[..full].copy_from_slice(&self.mag[..full]);
                let rem = width % 64;
                if rem != 0 {
                    m[full] = self.mag[full] & ((1u64 << rem) - 1);
                }
                if !self.neg {
                    Some(SInt { neg: false, mag: m })
                } else if m == [0; 9] {
                    Some(SInt::zero())
                } else {
                    // 2^width - m.
                    let mut pow2 = [0u64; 9];
                    if rem != 0 {
                        pow2[full] = 1u64 << rem;
                    } else {
                        pow2[full] = 1;
                    }
                    Some(SInt {
                        neg: false,
                        mag: Self::sub_mag(&pow2, &m),
                    })
                }
            }
            _ => None,
        }
    }
}

/// True iff (q, r) is THE floored division result of a / b (b != 0):
/// the definition, checked exactly.
fn is_floored(a: &SInt, b: &SInt, q: &SInt, r: &SInt) -> bool {
    let exact = r.add(&q.mul(b)) == *a;
    let bounded = SInt::cmp_mag(&r.mag, &b.mag) == Ordering::Less;
    let signed_ok = r.is_zero() || r.neg == b.neg;
    exact && bounded && signed_ok
}

#[test]
fn oracle_self_check() {
    // Hand-checked floored results, including the cases Euclidean gets wrong.
    for (a, b, q, r) in [
        (7i64, 2i64, 3i64, 1i64),
        (-7, 2, -4, 1),
        (7, -2, -4, -1),
        (-7, -2, 3, -1),
        (0, -5, 0, 0),
    ] {
        let (aq, ar) = SInt::floored_div(
            &SInt::from_int257(Int257::from_i64(a)),
            &SInt::from_int257(Int257::from_i64(b)),
        );
        assert_eq!(aq, SInt::from_int257(Int257::from_i64(q)), "q({a},{b})");
        assert_eq!(ar, SInt::from_int257(Int257::from_i64(r)), "r({a},{b})");
        assert!(
            is_floored(
                &SInt::from_int257(Int257::from_i64(a)),
                &SInt::from_int257(Int257::from_i64(b)),
                &aq,
                &ar
            ),
            "definition rejects oracle result ({a},{b})"
        );
    }
    // ...and the definition rejects the Euclidean answers for negative divisors.
    let a = SInt::from_int257(Int257::from_i64(7));
    let b = SInt::from_int257(Int257::from_i64(-2));
    assert!(!is_floored(
        &a,
        &b,
        &SInt::from_int257(Int257::from_i64(-3)),
        &SInt::from_int257(Int257::from_i64(1))
    ));
    // Magnitude extremes through the oracle: (2^200) * (2^100) = 2^300.
    let p200 = SInt::from_int257(Int257::from_unsigned256(&u256be_bit(200)));
    let p100 = SInt::from_int257(Int257::from_unsigned256(&u256be_bit(100)));
    let prod = p200.mul(&p100);
    assert_eq!(SInt::bitlen(&prod.mag), 301);
    // fit() pins: 2^300 needs unsigned width >= 301.
    assert!(prod.fit(300, 0).is_none());
    assert!(prod.fit(301, 0).is_some());
    assert!(prod.fit(256, 2).is_some());
    let wrapped = prod.fit(256, 2).unwrap();
    assert_eq!(wrapped, SInt::zero()); // 2^300 mod 2^256 = 0
                                       // Signed fit edges: -2^255 fits width 256 signed, 2^255 does not.
    let neg_pow255 = SInt::from_int257(Int257::from_signed256(&neg_pow255_bytes()));
    assert!(neg_pow255.fit(256, 1).is_some());
    assert!(neg_pow255.neg().fit(256, 1).is_none());
    assert!(neg_pow255.neg().fit(256, 0).is_some());
}

fn u256be_bit(bit: u32) -> [u8; 32] {
    assert!(bit < 256);
    let mut b = [0u8; 32];
    b[(31 - bit / 8) as usize] |= 1 << (bit % 8);
    b
}

fn neg_pow255_bytes() -> [u8; 32] {
    let mut b = [0u8; 32];
    b[0] = 0x80; // -2^255 as signed 256-bit
    b
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

fn boundary_values() -> Vec<Int257> {
    let mut v = vec![
        Int257::from_i128(i128::MIN),
        Int257::from_i128(i128::MIN + 1),
        Int257::from_i128(-(1i128 << 100)),
        Int257::from_i64(-1_000_000_007),
        Int257::from_i64(-8),
        Int257::from_i64(-7),
        Int257::from_i64(-3),
        Int257::from_i64(-2),
        Int257::from_i64(-1),
        Int257::zero(),
        Int257::from_i64(1),
        Int257::from_i64(2),
        Int257::from_i64(3),
        Int257::from_i64(7),
        Int257::from_i64(8),
        Int257::from_i64(1_000_000_007),
        Int257::from_i128(1i128 << 100),
        Int257::from_i128(i128::MAX - 1),
        Int257::from_i128(i128::MAX),
        Int257::from_u128(1u128 << 127),
        Int257::from_u128(u128::MAX),
        // 257-bit edges (unsigned PUSHINT-encodable):
        Int257::from_unsigned256(&u256be_bit(200)),
        Int257::from_unsigned256(&u256be_bit(255)),
        Int257::from_unsigned256(&[0xFF; 32]), // 2^256 - 1
        // -2^255 (signed PUSHINT-encodable):
        Int257::from_signed256(&neg_pow255_bytes()),
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
    fn int257(&mut self) -> Int257 {
        match self.next() % 4 {
            0 => {
                // Random 256-bit pattern, signed or unsigned interpretation.
                let mut b = [0u8; 32];
                for i in 0..4 {
                    b[i * 8..(i + 1) * 8].copy_from_slice(&self.next().to_be_bytes());
                }
                if self.next().is_multiple_of(2) {
                    Int257::from_unsigned256(&b)
                } else {
                    Int257::from_signed256(&b)
                }
            }
            1 => Int257::from_i64((self.next() % 2000) as i64 - 1000),
            2 => {
                let b = boundary_values();
                b[(self.next() % b.len() as u64) as usize]
            }
            _ => Int257::from_i128(self.next() as i128),
        }
    }
}

fn pairs() -> Vec<(Int257, Int257)> {
    let b = boundary_values();
    let mut out: Vec<_> = b
        .iter()
        .flat_map(|&x| b.iter().map(move |&y| (x, y)))
        .collect();
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for _ in 0..20_000 {
        out.push((rng.int257(), rng.int257()));
    }
    out
}

// ---------------------------------------------------------------------------
// Expected outcomes from the oracle
// ---------------------------------------------------------------------------

/// The oracle's expected `Out` for DIVMOD(a, b) at (width, flavor).
fn expected_divmod(a: Int257, b: Int257, width: u16, flavor: u8) -> Out {
    let sa = SInt::from_int257(a);
    let sb = SInt::from_int257(b);
    if sb.is_zero() {
        return Out::Exc(ExceptionKind::IntegerOverflow);
    }
    let (q, r) = SInt::floored_div(&sa, &sb);
    match (q.fit(width as u32, flavor), r.fit(width as u32, flavor)) {
        (Some(qq), Some(rr)) => Out::Ok(vec![qq, rr]),
        _ => Out::Exc(ExceptionKind::IntegerOverflow),
    }
}

/// The oracle's expected `Out` for DIV(a, b) at (width, flavor).
fn expected_div(a: Int257, b: Int257, width: u16, flavor: u8) -> Out {
    let sa = SInt::from_int257(a);
    let sb = SInt::from_int257(b);
    if sb.is_zero() {
        return Out::Exc(ExceptionKind::IntegerOverflow);
    }
    let (q, _) = SInt::floored_div(&sa, &sb);
    match q.fit(width as u32, flavor) {
        Some(qq) => Out::Ok(vec![qq]),
        None => Out::Exc(ExceptionKind::IntegerOverflow),
    }
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
        let got = run(binop(DIVMOD, a, b, 256, 1));
        let want = expected_divmod(a, b, 256, 1);
        match (&got, &want) {
            (Out::Panic(m), _) => panics.push(format!("({a:?},{b:?}): {m}")),
            (Out::Ok(s), Out::Ok(t)) if s.len() == 2 && t.len() == 2 => {
                // The definition must hold for the returned pair too
                // (oracle self-consistency, not just oracle agreement).
                let sa = SInt::from_int257(a);
                let sb = SInt::from_int257(b);
                if !(is_floored(&sa, &sb, &s[0], &s[1]) && s == t) {
                    wrong.push(format!("DIVMOD({a:?}, {b:?}) -> {got:?}, want {want:?}"));
                }
            }
            _ => {
                if got != want {
                    wrong.push(format!("DIVMOD({a:?}, {b:?}) -> {got:?}, want {want:?}"));
                }
            }
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
        "DIVMOD disagrees with the oracle on {}/{n} inputs, e.g.:\n{}",
        wrong.len(),
        wrong[..wrong.len().min(8)].join("\n")
    );
}

#[test]
fn divmod_floored_spot_checks_negative_divisors() {
    for (a, b, q, r) in [
        (7i64, -2i64, -4i64, -1i64),
        (-7, -2, 3, -1),
        (1, -3, -1, -2),
        (-1, -3, 0, -1),
    ] {
        let (a, b, q, r) = (
            Int257::from_i64(a),
            Int257::from_i64(b),
            Int257::from_i64(q),
            Int257::from_i64(r),
        );
        assert_eq!(
            run(binop(DIVMOD, a, b, 256, 1)),
            Out::Ok(vec![SInt::from_int257(q), SInt::from_int257(r)]),
            "DIVMOD({a:?}, {b:?})"
        );
    }
}

#[test]
fn divmod_zero_divisor_is_integer_overflow_in_every_flavor() {
    for a in boundary_values() {
        for flavor in [0u8, 1, 2] {
            assert_eq!(
                run(binop(DIVMOD, a, Int257::zero(), 256, flavor)),
                Out::Exc(ExceptionKind::IntegerOverflow),
                "DIVMOD({a:?}, 0) flavor {flavor}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// DIV vs DIVMOD: one quotient
// ---------------------------------------------------------------------------

#[test]
fn div_equals_divmod_quotient() {
    // DIV must return exactly DIVMOD's quotient (and agree on
    // overflow) at every width/flavor — the two opcodes share the
    // quotient by construction (ADR-0030).
    let mut wrong = Vec::new();
    let mut n = 0usize;
    for (a, b) in pairs().into_iter().step_by(7) {
        for (width, flavor) in [(256u16, 1u8), (128, 1), (64, 0), (8, 2)] {
            n += 1;
            let dm = run(binop(DIVMOD, a, b, width, flavor));
            let d = run(binop(DIV, a, b, width, flavor));
            let want_dm = expected_divmod(a, b, width, flavor);
            let want_d = expected_div(a, b, width, flavor);
            if dm != want_dm || d != want_d {
                wrong.push(format!(
                    "({a:?},{b:?}) w={width} f={flavor}: DIVMOD={dm:?} (want {want_dm:?}) DIV={d:?} (want {want_d:?})"
                ));
            }
            // And DIV's quotient is DIVMOD's quotient, structurally.
            if let (Out::Ok(s), Out::Ok(t)) = (&dm, &d) {
                if t.len() != 1 || t[0] != s[0] {
                    wrong.push(format!("({a:?},{b:?}): DIV quotient != DIVMOD quotient"));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "DIV/DIVMOD mismatch on {}/{n} cases, e.g.:\n{}",
        wrong.len(),
        wrong[..wrong.len().min(6)].join("\n")
    );
}

// ---------------------------------------------------------------------------
// DIV and DIVMOD agree on the declared width (spec §3.3)
// ---------------------------------------------------------------------------

#[test]
fn div_and_divmod_agree_on_declared_width_overflow() {
    // ADR-0028 recorded DIVMOD's unenforced width as a known deviation and
    // this test was ignored. ADR-0035 closes it: both opcodes enforce the
    // declared width/flavor, so they must agree on overflow.
    let cases = [
        (Int257::from_i64(1000), Int257::from_i64(1), 8u16, 1u8),
        (Int257::from_i64(300), Int257::from_i64(1), 8, 0),
        (Int257::from_i64(-1), Int257::from_i64(1), 8, 0),
        (
            Int257::from_unsigned256(&u256be_bit(70)),
            Int257::from_i64(1),
            64,
            1,
        ),
    ];
    let mut wrong = Vec::new();
    for (a, b, w, f) in cases {
        let d = run(binop(DIV, a, b, w, f));
        let dm = run(binop(DIVMOD, a, b, w, f));
        let want_d = expected_div(a, b, w, f);
        let want_dm = expected_divmod(a, b, w, f);
        if d != want_d || dm != want_dm {
            wrong.push(format!(
                "({a:?},{b:?}) w={w} f={f}: DIV={d:?} (want {want_d:?}) DIVMOD={dm:?} (want {want_dm:?})"
            ));
        }
        let d_over = d == Out::Exc(ExceptionKind::IntegerOverflow);
        let dm_over = dm == Out::Exc(ExceptionKind::IntegerOverflow);
        if d_over != dm_over {
            wrong.push(format!(
                "({a:?},{b:?}) w={w} f={f}: DIV={d:?} DIVMOD={dm:?} disagree"
            ));
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
    let (seven, two, nine, five, twenty) = (
        Int257::from_i64(7),
        Int257::from_i64(2),
        Int257::from_i64(9),
        Int257::from_i64(5),
        Int257::from_i64(20),
    );
    (0x00u8..=0xFF).find(|&op| {
        if op == DIVMOD || op == DIV {
            return false;
        }
        run(binop(op, seven, two, 256, 1)) == Out::Ok(vec![SInt::from_int257(Int257::from_i64(1))])
            && run(binop(op, nine, five, 256, 1))
                == Out::Ok(vec![SInt::from_int257(Int257::from_i64(4))])
            && run(binop(op, twenty, seven, 256, 1))
                == Out::Ok(vec![SInt::from_int257(Int257::from_i64(6))])
            && run(binop(op, seven, two, 256, 1))
                == Out::Ok(vec![SInt::from_int257(Int257::from_i64(1))])
    })
}

#[test]
#[ignore = "no MOD opcode exists: the interpreter implements DIV (0x17) and DIVMOD (0x14) only. Un-ignore if a MOD opcode is ever specified and implemented."]
fn mod_opcode_exists_and_equals_divmod_remainder() {
    let op = find_mod_opcode().expect("no MOD opcode found anywhere in 0x00..=0xFF");
    panic!("MOD found at 0x{op:02x} — un-ignore and pin its semantics");
}

// ---------------------------------------------------------------------------
// Whole-family no-panic sweep (all arithmetic opcodes, all widths/flavors)
// ---------------------------------------------------------------------------

#[test]
fn arithmetic_family_never_panics_on_boundary_grid() {
    let vals = boundary_values();
    let mut panics = Vec::new();
    for op in (0x10u8..=0x1F).chain([0x20]) {
        for width in [0u16, 1, 63, 64, 127, 128, 129, 255, 256, 257, u16::MAX] {
            for flavor in [0u8, 1, 2, 3, 255] {
                for &a in vals.iter().step_by(3) {
                    for &b in vals.iter().step_by(3) {
                        if let Out::Panic(m) = run(binop(op, a, b, width, flavor)) {
                            panics.push(format!(
                                "op=0x{op:02x} w={width} f={flavor} ({a:?},{b:?}): {m}"
                            ));
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
    // LSHIFT (0x18) / RSHIFT (0x19) go through the same width/flavor
    // application as DIV. Pin a few hand-checked cases via the oracle.
    let cases: Vec<(u8, Int257, Int257, u16, u8)> = vec![
        (0x18, Int257::from_i64(1), Int257::from_i64(3), 128, 1), // 8
        (0x19, Int257::from_i64(8), Int257::from_i64(1), 128, 1), // 4
        (0x19, Int257::from_i64(-8), Int257::from_i64(1), 128, 1), // -4
        (
            0x18,
            Int257::from_unsigned256(&u256be_bit(200)),
            Int257::from_i64(55),
            256,
            0,
        ), // 2^255
        (
            0x19,
            Int257::from_signed256(&neg_pow255_bytes()),
            Int257::from_i64(1),
            256,
            1,
        ), // -2^254
    ];
    for (op, a, b, w, f) in cases {
        let got = run(binop(op, a, b, w, f));
        let want = match op {
            0x18 => oracle_shift_left(a, b, w, f),
            _ => oracle_shift_right(a, b, w, f),
        };
        assert_eq!(got, want, "op=0x{op:02x}({a:?},{b:?}) w={w} f={f}");
    }
}

/// Test-oracle for LSHIFT: range-check the amount, multiply by 2^shift
/// exactly, apply width/flavor.
fn oracle_shift_left(a: Int257, b: Int257, width: u16, flavor: u8) -> Out {
    let sb = SInt::from_int257(b);
    let shift: u32 = match as_u32(&sb) {
        Some(s) if s < width as u32 => s,
        _ => return Out::Exc(ExceptionKind::IntegerOverflow),
    };
    let sa = SInt::from_int257(a);
    // exact: sa * 2^shift via magnitude shift.
    let word = (shift / 64) as usize;
    let bit = shift % 64;
    let mut mag = [0u64; 9];
    let mut carry = 0u64;
    for (i, slot) in mag.iter_mut().enumerate() {
        let src = if i >= word { sa.mag[i - word] } else { 0 };
        let lo = src << bit;
        let hi = if bit != 0 && i >= word && i - word > 0 {
            sa.mag[i - word - 1] >> (64 - bit)
        } else {
            0
        };
        let t = (lo as u128) + (hi as u128) + (carry as u128);
        *slot = t as u64;
        carry = (t >> 64) as u64;
    }
    if carry != 0 {
        // Beyond 576 bits: only for absurd shifts, which the range check
        // (shift < width <= 256) already excludes. Fail the test loudly.
        panic!("oracle shift overflow: shift={shift}");
    }
    let exact = SInt { neg: sa.neg, mag };
    match exact.fit(width as u32, flavor) {
        Some(v) => Out::Ok(vec![v]),
        None => Out::Exc(ExceptionKind::IntegerOverflow),
    }
}

/// Test-oracle for RSHIFT: arithmetic shift right is floor(a / 2^shift).
fn oracle_shift_right(a: Int257, b: Int257, width: u16, flavor: u8) -> Out {
    let sb = SInt::from_int257(b);
    let shift: u32 = match as_u32(&sb) {
        Some(s) if s < width as u32 => s,
        _ => return Out::Exc(ExceptionKind::IntegerOverflow),
    };
    let sa = SInt::from_int257(a);
    // floor(a / 2^shift) via floored_div by 2^shift.
    let mut den_mag = [0u64; 9];
    den_mag[(shift / 64) as usize] = 1u64 << (shift % 64);
    let den = SInt {
        neg: false,
        mag: den_mag,
    };
    let (q, _) = SInt::floored_div(&sa, &den);
    match q.fit(width as u32, flavor) {
        Some(v) => Out::Ok(vec![v]),
        None => Out::Exc(ExceptionKind::IntegerOverflow),
    }
}

/// Small non-negative SInt as u32; None otherwise.
fn as_u32(v: &SInt) -> Option<u32> {
    if v.neg || v.mag[1..9].iter().any(|&w| w != 0) || v.mag[0] > u32::MAX as u64 {
        return None;
    }
    Some(v.mag[0] as u32)
}
