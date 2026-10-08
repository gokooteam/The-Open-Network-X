//! 257-bit signed integers (Wave 4, ADR-0035).
//!
//! The spec's `Integer` stack kind is the closed range `[-2^256, 2^256 - 1]`
//! (`docs/specification/tvm-instruction-set.md` §3.2). [`Int257`] is the
//! canonical carrier: 5 little-endian `u64` limbs holding a 320-bit
//! two's-complement value whose bits 257..320 are always the sign extension
//! of bit 256. There is no public constructor that can produce a
//! non-canonical value — canonicality bugs would be consensus bugs, so the
//! type enforces the invariant, not the call sites.
//!
//! Arithmetic is two-phase, mirroring the spec (§3.3): every operation first
//! computes the **true mathematical result exactly** in the 640-bit
//! two's-complement working type [`Wide`] (a `MUL` of two 257-bit values
//! needs at most 514 bits; an `LSHIFT` by `< 256` at most 512 — nothing the
//! instruction set can express overflows 640 bits), then
//! [`Wide::apply`] enforces the opcode's declared width/flavor:
//! unsigned/signed flavors raise `IntegerOverflow` when the true result does
//! not fit, the modulo flavor reduces mod `2^width`. Range checks (division
//! by zero, out-of-range shift amounts) precede flavor selection and raise
//! for every flavor.
//!
//! No new dependencies: the limb arithmetic is hand-rolled (schoolbook
//! multiply, binary long division), matching the crate's dependency-free
//! stance. Python's arbitrary-precision integers are the independent oracle
//! (`reference/gen_int257_vectors.py`); `tests/int257_vectors.rs` asserts
//! agreement.
//!
//! Every arithmetic site names its overflow behavior explicitly per the
//! crate's `arithmetic_side_effects` deny: `wrapping_*` marks operations
//! proven exact in the comments (fixed-width two's-complement closure or a
//! statically bounded accumulator), `overflowing_*` marks manual
//! carry/borrow plumbing, and `checked`/fallible conversions mark the
//! consensus-visible failure points.

use crate::types::ExceptionKind;
use std::cmp::Ordering;

/// Canonical 257-bit signed integer: the spec's `Integer` kind.
///
/// Internal representation: 5 little-endian `u64` limbs of a 320-bit
/// two's-complement value, with the invariant that bits 257..320 equal bit
/// 256 (the sign). Concretely `limbs[4]` is `0`/`1` for non-negative values
/// and `0xFFFF_FFFF_FFFF_FFFE`/`0xFFFF_FFFF_FFFF_FFFF` for negative ones.
/// The canonical 33-byte big-endian stack encoding is `to_bytes33`:
/// `byte[0] & 0xFE == 0`, bit 256 is the sign.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Int257([u64; 5]);

impl Int257 {
    /// Zero (canonical: all limbs zero).
    pub fn zero() -> Self {
        Self([0; 5])
    }

    /// One.
    pub fn one() -> Self {
        Self([1, 0, 0, 0, 0])
    }

    /// Small unsigned constant.
    pub fn from_u64(v: u64) -> Self {
        Self([v, 0, 0, 0, 0])
    }

    /// Small signed constant, sign-extended.
    pub fn from_i64(v: i64) -> Self {
        if v < 0 {
            Self([v as u64, !0u64, !0u64, !0u64, !0u64])
        } else {
            Self::from_u64(v as u64)
        }
    }

    /// Sign-extended from `i128` (always in-domain).
    pub fn from_i128(v: i128) -> Self {
        let lo = v as u128;
        let fill = if v < 0 { !0u64 } else { 0u64 };
        Self([lo as u64, (lo >> 64) as u64, fill, fill, fill])
    }

    /// Zero-extended from `u128` (always in-domain).
    pub fn from_u128(v: u128) -> Self {
        Self([v as u64, (v >> 64) as u64, 0, 0, 0])
    }

    /// Canonical 33-byte big-endian encoding (`byte[0] & 0xFE == 0`).
    /// Returns `None` on a non-canonical encoding.
    pub fn from_bytes33(bytes: &[u8; 33]) -> Option<Self> {
        if bytes[0] & 0xFE != 0 {
            return None;
        }
        let mut limbs = [0u64; 5];
        // bytes[1..33] are the 256 value bits, big-endian; assemble
        // little-endian from the tail.
        for (i, chunk) in bytes[1..33].rchunks(8).enumerate() {
            let mut w = 0u64;
            for &b in chunk {
                w = (w << 8) | b as u64;
            }
            limbs[i] = w;
        }
        // Bit 256 is the sign; extend it over bits 257..320.
        let sign = (bytes[0] & 1) as u64;
        limbs[4] = sign | if sign == 1 { !1u64 } else { 0 };
        Some(Self(limbs))
    }

    /// Canonical 33-byte big-endian encoding of this value.
    pub fn to_bytes33(&self) -> [u8; 33] {
        debug_assert!(self.is_canonical());
        let mut out = [0u8; 33];
        out[0] = (self.0[4] & 1) as u8;
        for (i, chunk) in out[1..33].rchunks_mut(8).enumerate() {
            let mut w = self.0[i];
            for slot in chunk.iter_mut().rev() {
                *slot = w as u8;
                w >>= 8;
            }
        }
        out
    }

    /// Unsigned 256-bit big-endian interpretation. `None` iff negative —
    /// non-negative domain values always fit in 32 bytes.
    pub fn from_unsigned256(bytes: &[u8; 32]) -> Self {
        let mut limbs = [0u64; 5];
        for (i, chunk) in bytes.rchunks(8).enumerate() {
            let mut w = 0u64;
            for &b in chunk {
                w = (w << 8) | b as u64;
            }
            limbs[i] = w;
        }
        // limbs[4] stays 0: value < 2^256, non-negative, canonical.
        Self(limbs)
    }

    /// 256-bit two's-complement big-endian, sign-extended to 257 bits.
    /// Used by `PUSHINT`'s signed form.
    pub fn from_signed256(bytes: &[u8; 32]) -> Self {
        let mut v = Self::from_unsigned256(bytes);
        if bytes[0] & 0x80 != 0 {
            // Bit 255 is the sign: extend it over bits 256..320.
            // limbs[4] = all-ones keeps bit 256 = 1 = the sign: canonical.
            v.0[4] = !0u64;
        }
        v
    }

    /// 32-byte big-endian form. `None` iff negative: only values in
    /// `[0, 2^256)` name a 32-byte string (used for `CHKSIGNU` hashes).
    pub fn to_unsigned256(&self) -> Option<[u8; 32]> {
        if self.is_negative() {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, chunk) in out.rchunks_mut(8).enumerate() {
            let mut w = self.0[i];
            for slot in chunk.iter_mut().rev() {
                *slot = w as u8;
                w >>= 8;
            }
        }
        Some(out)
    }

    /// `None` iff negative or larger than `usize::MAX` (used for
    /// `SUBBYTES` offset/length, which index host memory).
    pub fn to_usize_checked(&self) -> Option<usize> {
        if self.is_negative() {
            return None;
        }
        if self.0[1] != 0 || self.0[2] != 0 || self.0[3] != 0 || self.0[4] != 0 {
            return None;
        }
        Some(self.0[0] as usize)
    }

    /// The 257-bit invariant: bits 257..320 equal bit 256.
    fn is_canonical(&self) -> bool {
        let sign = self.0[4] & 1;
        self.0[4] == sign | if sign == 1 { !1u64 } else { 0 }
    }

    /// True iff the value is zero.
    pub fn is_zero(&self) -> bool {
        self.0 == [0; 5]
    }

    /// True iff the value is negative (bit 256 set).
    pub fn is_negative(&self) -> bool {
        self.0[4] & 1 == 1
    }

    fn sign(&self) -> i8 {
        if self.is_zero() {
            0
        } else if self.is_negative() {
            -1
        } else {
            1
        }
    }

    /// `(sign, magnitude)`: the magnitude is the absolute value as 5
    /// little-endian limbs (`<= 2^256`, so it always fits).
    fn sign_magnitude(&self) -> (i8, [u64; 5]) {
        if !self.is_negative() {
            return (self.sign(), self.0);
        }
        // Two's-complement negation over the 5 limbs; `overflowing_add`
        // is the manual carry plumbing, exact by fixed width.
        let mut m = [0u64; 5];
        let mut carry = 1u64;
        for (d, &s) in m.iter_mut().zip(self.0.iter()) {
            let (w, c) = (!s).overflowing_add(carry);
            *d = w;
            carry = c as u64;
        }
        (-1, m)
    }

    fn to_wide(self) -> Wide {
        let fill = if self.is_negative() { !0u64 } else { 0u64 };
        let mut w = [fill; 10];
        w[..5].copy_from_slice(&self.0);
        Wide(w)
    }

    /// Exact sum, as a [`Wide`] (true result, no width check yet).
    pub fn add_exact(self, o: Self) -> Wide {
        Wide(add10(&self.to_wide().0, &o.to_wide().0))
    }

    /// Exact difference.
    pub fn sub_exact(self, o: Self) -> Wide {
        Wide(add10(&self.to_wide().0, &neg10(&o.to_wide().0)))
    }

    /// Exact negation.
    pub fn neg_exact(self) -> Wide {
        Wide(neg10(&self.to_wide().0))
    }

    /// Exact product. Magnitudes are `<= 2^256`, so `|product| < 2^514`
    /// `< 2^576`: the sign-magnitude product is exact, and the sign is
    /// applied afterward.
    pub fn mul_exact(self, o: Self) -> Wide {
        let (sa, ma) = self.sign_magnitude();
        let (sb, mb) = o.sign_magnitude();
        let mag = mul_mag5(&ma, &mb);
        let mut w = [0u64; 10];
        w[..9].copy_from_slice(&mag);
        let wide = Wide(w);
        // Negation of zero is zero, so the sa == 0 case needs no care.
        if sa == sb {
            wide
        } else {
            wide.neg()
        }
    }

    /// Exact `self * 2^bits` (`bits < 256` — enforced by the caller via
    /// the shift-amount range check, which precedes flavor per spec §3.3).
    pub fn shl_exact(self, bits: u32) -> Wide {
        debug_assert!(bits < 256);
        Wide(shl10(&self.to_wide().0, bits))
    }

    /// Exact arithmetic shift right: two's-complement `shr` is
    /// `floor(self / 2^bits)`, always in-domain. `bits < 256` (caller
    /// range-checks before calling).
    pub fn shr_exact(self, bits: u32) -> Self {
        debug_assert!(bits < 256);
        let fill = if self.is_negative() { !0u64 } else { 0u64 };
        // Sign-extend to 576 bits so the word shift never runs off the end:
        // source bit j >= 320 is the sign, exactly what arithmetic shift
        // must bring in from the top.
        let mut ext = [fill; 9];
        ext[..5].copy_from_slice(&self.0);
        let word = (bits / 64) as usize;
        let bit = bits % 64;
        let mut out = [0u64; 5];
        for (i, d) in out.iter_mut().enumerate() {
            // i + word + 1 <= 4 + 3 + 1 = 8 < 9: in-bounds by bits < 256.
            // wrapping_add is exact here (no wrap possible), named
            // explicitly for the arithmetic lint.
            let s0 = i.wrapping_add(word);
            let a = ext[s0];
            *d = if bit == 0 {
                a
            } else {
                let b = ext[s0.wrapping_add(1)];
                (a >> bit) | (b << 64u32.wrapping_sub(bit))
            };
        }
        // The source is properly sign-extended and bits < 256, so result
        // bits 257..320 equal the result's bit 256: canonical by
        // construction.
        Self(out)
    }

    /// True floor division: `q = floor(a/b)`, `r = a - q*b`, so
    /// `sign(r) = sign(b)` or `r = 0` (spec §4.3, ADR-0030 — TON's
    /// round-toward-negative-infinity, fully defined for negative
    /// divisors). `None` iff `b == 0`; the caller maps that to
    /// `IntegerOverflow` before any flavor selection.
    ///
    /// Both results are exact [`Wide`] values: the quotient can be `2^256`
    /// (for `a = -2^256, b = -1`), which is outside the `Int257` domain —
    /// `apply` then raises or wraps it per the declared width/flavor.
    pub fn floored_divmod_exact(a: Self, b: Self) -> Option<(Wide, Wide)> {
        if b.is_zero() {
            return None;
        }
        let (sa, ma) = a.sign_magnitude();
        let (sb, mb) = b.sign_magnitude();
        let (q0, r0) = udivmod_320(ma, mb);
        let q0w = Wide::from_mag(q0);
        let r0w = Wide::from_mag(r0);
        if r0w.is_zero() {
            // Exact division.
            let q = if sa == sb { q0w } else { q0w.neg() };
            return Some((q, Wide::ZERO));
        }
        // Truncated quotient/remainder, then the floor adjustment (the same
        // correction the i128 implementation used, generalized): when the
        // truncated remainder's sign disagrees with the divisor's, step the
        // quotient down one and hand the remainder one divisor. Then
        // |r| = |b| - |r0| < |b| and sign(r) = sign(b).
        let q_t = if sa == sb { q0w } else { q0w.neg() };
        let r_t = if sa == 1 { r0w } else { r0w.neg() };
        if sa != sb {
            Some((q_t.sub(&Wide::ONE), r_t.add(&b.to_wide())))
        } else {
            Some((q_t, r_t))
        }
    }

    /// Range-checks this value at the declared width/flavor without
    /// changing it (used by `CONV`/`STBITS`, which re-check an existing
    /// value rather than computing a new one).
    pub fn check_width(self, width: u16, flavor: u8) -> Result<Self, ExceptionKind> {
        self.to_wide().apply(width, flavor)
    }
}

impl Ord for Int257 {
    fn cmp(&self, other: &Self) -> Ordering {
        // Fixed-width two's complement: within one sign class, numeric
        // order is unsigned lexicographic order of the limbs.
        match (self.is_negative(), other.is_negative()) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => {
                for i in (0..5).rev() {
                    match self.0[i].cmp(&other.0[i]) {
                        Ordering::Equal => continue,
                        ord => return ord,
                    }
                }
                Ordering::Equal
            }
        }
    }
}

impl PartialOrd for Int257 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// 640-bit two's-complement working value (10 little-endian limbs).
///
/// Every exact operation the instruction set can express on [`Int257`]
/// lands here without loss: `ADD`/`SUB`/`NEG` need ≤ 258 bits, `MUL`
/// ≤ 514 bits, `LSHIFT` by `< 256` ≤ 512 bits, `DIVMOD`'s quotient
/// ≤ 257 bits. Fixed-width `add`/`sub`/`neg` wrap mod 2^640, which is the
/// correct two's-complement semantic; the callers only feed values whose
/// true results fit, so no precision is ever lost.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Wide([u64; 10]);

impl Wide {
    const ZERO: Wide = Wide([0; 10]);
    const ONE: Wide = Wide([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);

    fn from_mag(mag: [u64; 5]) -> Self {
        let mut w = [0u64; 10];
        w[..5].copy_from_slice(&mag);
        Self(w)
    }

    fn is_zero(&self) -> bool {
        self.0 == [0; 10]
    }

    fn is_negative(&self) -> bool {
        self.0[9] >> 63 == 1
    }

    fn neg(&self) -> Self {
        Wide(neg10(&self.0))
    }

    fn add(&self, o: &Self) -> Self {
        Wide(add10(&self.0, &o.0))
    }

    fn sub(&self, o: &Self) -> Self {
        Wide(add10(&self.0, &neg10(&o.0)))
    }

    /// Applies the declared width/flavor to an exact result (spec §3.3):
    /// unsigned/signed flavors raise `IntegerOverflow` when the true result
    /// does not fit; the modulo flavor reduces mod `2^width` and never
    /// raises on the result. Callers validate `1 <= width <= 256` and
    /// `flavor <= 2` as bytecode well-formedness (`MalformedCell`) before
    /// calling.
    pub fn apply(self, width: u16, flavor: u8) -> Result<Int257, ExceptionKind> {
        debug_assert!((1..=256).contains(&width));
        debug_assert!(flavor <= 2);
        let w = width as u32;
        match flavor {
            // Unsigned: 0 <= v < 2^width.
            0 => {
                if self.is_negative() || !self.all_zero_from(w) {
                    return Err(ExceptionKind::IntegerOverflow);
                }
                Ok(self.wrap_bits(w))
            }
            // Signed: -2^(w-1) <= v < 2^(w-1).
            1 => {
                // w >= 1, so w-1 is exact; wrapping_sub names it for the lint.
                let bound_ok = if self.is_negative() {
                    self.all_one_from(w.wrapping_sub(1))
                } else {
                    self.all_zero_from(w.wrapping_sub(1))
                };
                if !bound_ok {
                    return Err(ExceptionKind::IntegerOverflow);
                }
                Ok(self.narrow_signed())
            }
            // Modulo: v mod 2^width as the unsigned bit pattern.
            _ => Ok(self.wrap_bits(w)),
        }
    }

    /// True iff every bit at position `>= from` is zero.
    fn all_zero_from(&self, from: u32) -> bool {
        let mut limb = (from / 64) as usize;
        let bit = from % 64;
        if bit != 0 {
            // bit < 64 here, so the shift is exact.
            if self.0[limb] & (!0u64 << bit) != 0 {
                return false;
            }
            limb = limb.wrapping_add(1);
        }
        let mut i = limb;
        while i < 10 {
            if self.0[i] != 0 {
                return false;
            }
            i = i.wrapping_add(1);
        }
        true
    }

    /// True iff every bit at position `>= from` is one.
    fn all_one_from(&self, from: u32) -> bool {
        let mut limb = (from / 64) as usize;
        let bit = from % 64;
        if bit != 0 {
            let mask = !0u64 << bit;
            if self.0[limb] & mask != mask {
                return false;
            }
            limb = limb.wrapping_add(1);
        }
        let mut i = limb;
        while i < 10 {
            if self.0[i] != !0u64 {
                return false;
            }
            i = i.wrapping_add(1);
        }
        true
    }

    /// The low `w` bits (`w <= 256`) as an unsigned — hence non-negative —
    /// [`Int257`]. This is both the modulo-flavor reduction and the
    /// unsigned-flavor narrowing (the fit check already passed there).
    fn wrap_bits(&self, w: u32) -> Int257 {
        debug_assert!(w <= 256);
        let mut limbs = [0u64; 5];
        let full = (w / 64) as usize;
        let mut i = 0;
        while i < full && i < 5 {
            limbs[i] = self.0[i];
            i = i.wrapping_add(1);
        }
        let bit = w % 64;
        // w <= 256, so full <= 4 < 5: the partial limb always exists.
        if bit != 0 {
            // Low-`bit` mask; 64 - bit is exact (bit < 64).
            limbs[full] = self.0[full] & (!0u64 >> 64u32.wrapping_sub(bit));
        }
        // Bits taken are all < 256, so limbs[4] == 0: non-negative and
        // canonical by construction.
        Int257(limbs)
    }

    /// Truncates to the low 257 bits with sign extension. The caller must
    /// have established via a fit check that bits `[257, 640)` are the sign
    /// extension of bit 256, so the truncation is exact.
    fn narrow_signed(&self) -> Int257 {
        let mut limbs = [0u64; 5];
        limbs[..4].copy_from_slice(&self.0[..4]);
        let sign = self.0[4] & 1;
        limbs[4] = sign | if self.is_negative() { !1u64 } else { 0 };
        Int257(limbs)
    }
}

/// Fixed-width two's-complement addition over 10 limbs. The `u128`
/// accumulator satisfies `(2^64-1) + (2^64-1) + 1 < 2^65`, so `wrapping`
/// is exact, not a silent clamp.
fn add10(a: &[u64; 10], b: &[u64; 10]) -> [u64; 10] {
    let mut out = [0u64; 10];
    let mut carry = 0u64;
    for ((&x, &y), d) in a.iter().zip(b.iter()).zip(out.iter_mut()) {
        let t = (x as u128)
            .wrapping_add(y as u128)
            .wrapping_add(carry as u128);
        *d = t as u64;
        carry = (t >> 64) as u64;
    }
    out
}

/// Fixed-width two's-complement negation over 10 limbs; `overflowing_add`
/// is the manual carry plumbing, exact by fixed width.
fn neg10(a: &[u64; 10]) -> [u64; 10] {
    let mut out = [0u64; 10];
    let mut carry = 1u64;
    for i in 0..10 {
        let (w, c) = (!a[i]).overflowing_add(carry);
        out[i] = w;
        carry = c as u64;
    }
    out
}

/// Exact unsigned product of two `<= 257`-bit magnitudes into 9 limbs.
/// The true product is `< 2^514 < 2^576`, so nothing is lost. Schoolbook
/// multiply; each partial `t = a[i]*b[j] + acc + carry` satisfies
/// `t < 2^128` (carry `< 2^64` by induction), so the `u128` accumulator
/// never overflows and `wrapping` is exact.
fn mul_mag5(a: &[u64; 5], b: &[u64; 5]) -> [u64; 9] {
    let mut out = [0u64; 9];
    for (i, &ai) in a.iter().enumerate() {
        let mut carry = 0u64;
        let mut j = 0usize;
        while j < 5 {
            // i + j <= 8 < 9: wrapping_add is exact (no wrap possible).
            let k = i.wrapping_add(j);
            let t = (ai as u128)
                .wrapping_mul(b[j] as u128)
                .wrapping_add(out[k] as u128)
                .wrapping_add(carry as u128);
            out[k] = t as u64;
            carry = (t >> 64) as u64;
            j = j.wrapping_add(1);
        }
        // Drain the row carry. i + 5 <= 9, and the true product is < 2^514,
        // so the chain always terminates in-bounds with carry 0; the guard
        // is belt-and-braces against an indexing panic, not a reachable path.
        let mut k = i.wrapping_add(5);
        while carry != 0 {
            debug_assert!(k < 9);
            if k >= 9 {
                break;
            }
            let t = (out[k] as u128).wrapping_add(carry as u128);
            out[k] = t as u64;
            carry = (t >> 64) as u64;
            k = k.wrapping_add(1);
        }
        debug_assert_eq!(carry, 0);
    }
    out
}

/// Logical shift left over 10 limbs. Bits shifted at or past position 640
/// are dropped — every caller shifts a sign-extended 257-bit value by
/// `< 256` (value `< 2^513`), so nothing is lost.
fn shl10(a: &[u64; 10], bits: u32) -> [u64; 10] {
    debug_assert!(bits < 640);
    let word = (bits / 64) as usize;
    let bit = bits % 64;
    let mut out = [0u64; 10];
    for (i, d) in out.iter_mut().enumerate() {
        if i >= word {
            // i - word <= 9: wrapping_sub is exact (no wrap possible).
            let s = i.wrapping_sub(word);
            let mut v = a[s] << bit;
            if bit != 0 && s > 0 {
                // 64 - bit is exact (0 < bit < 64).
                v |= a[s.wrapping_sub(1)] >> 64u32.wrapping_sub(bit);
            }
            *d = v;
        }
    }
    out
}

/// Unsigned `>=` over 5 limbs.
fn ge5(a: &[u64; 5], b: &[u64; 5]) -> bool {
    for i in (0..5).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

/// Unsigned subtraction over 5 limbs. Precondition: `a >= b` (the caller
/// establishes it via [`ge5`]); the borrow chain provably terminates with
/// borrow 0, so `overflowing_sub` is exact manual plumbing.
fn sub5(a: &[u64; 5], b: &[u64; 5]) -> [u64; 5] {
    let mut out = [0u64; 5];
    let mut borrow = 0u64;
    for i in 0..5 {
        let (r1, b1) = a[i].overflowing_sub(b[i]);
        let (r2, b2) = r1.overflowing_sub(borrow);
        out[i] = r2;
        borrow = (b1 as u64).wrapping_add(b2 as u64);
    }
    debug_assert_eq!(borrow, 0);
    out
}

/// Unsigned division with remainder over 5 limbs (`d != 0`): binary long
/// division, 320 iterations. Returns `(quotient, remainder)` with
/// `n = q*d + r`, `r < d`.
fn udivmod_320(n: [u64; 5], d: [u64; 5]) -> ([u64; 5], [u64; 5]) {
    let mut q = [0u64; 5];
    let mut r = [0u64; 5];
    for bit in (0..320u32).rev() {
        // r = (r << 1) | bit_{bit}(n).
        let incoming = (n[(bit >> 6) as usize] >> (bit & 63)) & 1;
        let mut carry = incoming;
        for w in r.iter_mut() {
            let next_carry = *w >> 63;
            *w = (*w << 1) | carry;
            carry = next_carry;
        }
        // If r >= d, subtract and set the quotient bit.
        if ge5(&r, &d) {
            r = sub5(&r, &d);
            q[(bit >> 6) as usize] |= 1 << (bit & 63);
        }
    }
    (q, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_canonical(v: &Int257) {
        assert!(v.is_canonical(), "non-canonical Int257: {v:?}");
    }

    #[test]
    fn limbs_round_trip_bytes33() {
        for v in [
            Int257::zero(),
            Int257::one(),
            Int257::from_i64(-1),
            Int257::from_i128(i128::MIN),
            Int257::from_i128(i128::MAX),
            Int257::from_u128(u128::MAX),
        ] {
            let b = v.to_bytes33();
            let w = Int257::from_bytes33(&b).expect("canonical");
            assert_eq!(v, w);
            assert_canonical(&w);
        }
        // Non-canonical encodings are rejected.
        let mut bad = [0u8; 33];
        bad[0] = 0x02;
        assert!(Int257::from_bytes33(&bad).is_none());
        // -2^256: byte0 = 0x01, rest zero.
        let mut min = [0u8; 33];
        min[0] = 0x01;
        let v = Int257::from_bytes33(&min).expect("canonical -2^256");
        assert!(v.is_negative());
        assert_eq!(v.to_bytes33(), min);
        // 2^256 - 1: byte0 = 0x00, rest 0xFF.
        let max = [
            0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
            0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ];
        let v = Int257::from_bytes33(&max).expect("canonical 2^256-1");
        assert!(!v.is_negative());
        assert_eq!(v.to_unsigned256().unwrap(), [0xFF; 32]);
    }

    #[test]
    fn ordering_spot_checks() {
        let neg = Int257::from_i64(-5);
        let pos = Int257::from_i64(5);
        assert!(neg < Int257::zero());
        assert!(Int257::zero() < pos);
        assert!(neg < pos);
        assert!(Int257::from_i128(i128::MIN) < Int257::from_i128(i128::MAX));
        // -2^256 is the domain minimum.
        let mut min_bytes = [0u8; 33];
        min_bytes[0] = 0x01;
        let min = Int257::from_bytes33(&min_bytes).unwrap();
        assert!(min < Int257::from_i128(i128::MIN));
        assert!(min < Int257::from_i64(-1));
    }

    #[test]
    fn exact_arithmetic_spot_checks() {
        // (2^200) * (2^100) = 2^300: needs > 257 bits of exactness.
        let a = Int257::one().shl_exact(200).apply(256, 0).unwrap();
        let b = Int257::one().shl_exact(100).apply(256, 0).unwrap();
        let w = a.mul_exact(b);
        // 2^300 mod 2^256 = 0 ...
        let wrapped = w.apply(256, 2).unwrap();
        assert_eq!(wrapped, Int257::zero());
        // ...and it does NOT fit unsigned width 256.
        assert!(w.apply(256, 0).is_err());
    }

    #[test]
    fn floored_division_extremes() {
        // (-2^256) / (-1) = 2^256: exact Wide, unrepresentable as Int257.
        let mut min_bytes = [0u8; 33];
        min_bytes[0] = 0x01;
        let min = Int257::from_bytes33(&min_bytes).unwrap();
        let (q, r) = Int257::floored_divmod_exact(min, Int257::from_i64(-1)).unwrap();
        assert_eq!(r, Wide::ZERO);
        // 2^256 fits no declared width: flavors 0/1 raise, flavor 2 wraps to 0.
        assert!(q.apply(256, 0).is_err());
        assert!(q.apply(256, 1).is_err());
        assert_eq!(q.apply(256, 2).unwrap(), Int257::zero());
        // Floor with negative divisor: 7 / -2 = (-4, -1).
        let (q, r) =
            Int257::floored_divmod_exact(Int257::from_i64(7), Int257::from_i64(-2)).unwrap();
        assert_eq!(q.apply(8, 1).unwrap(), Int257::from_i64(-4));
        assert_eq!(r.apply(8, 1).unwrap(), Int257::from_i64(-1));
        // Division by zero.
        assert!(Int257::floored_divmod_exact(Int257::one(), Int257::zero()).is_none());
    }
}
