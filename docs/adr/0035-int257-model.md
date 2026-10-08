# ADR-0035 — 257-bit Integer Model (Wave 4, step 3)

**Status:** Accepted (2026-10-08; proposed 2026-10-07). Implemented in #32
(`onx-execution/src/int257.rs`), pinned by `reference/vectors/int257.json`.
**Decider:** Gokoo (design authority between audits; flagged for Claude's Wave 4 audit)
**Supersedes:** ADR-0028 Non-goals (the "integer-model wave" deferral), ADR-0031
  "u128 carrier containment" (the bulkhead is removed by the real model)

## Context

The spec (`docs/specification/tvm-instruction-set.md` §3.2) defines the
`Integer` stack kind as the closed range `[-2^256, 2^256 - 1]` — TON-style
257-bit signed integers. The implementation never matched it:

- Wave 2/3 computed on `i128` with width/flavor parsed but ignored
  (ADR-0024's "fixed 257-bit" range was aspirational).
- ADR-0028 contained the divergence behind the deploy gate (explicit
  non-goal: "the 257-bit integer-model correction is a LATER wave").
- ADR-0031 (PR #10 hotfix) built a fail-closed bulkhead instead of the
  model: `to_i128` raised on any operand outside `[-2^127, 2^127)`, and a
  `u128` carrier held wrap-flavor results in `[0, 2^128)` that no other
  opcode could consume. Consequences, stated honestly in ADR-0031: the
  bulkhead "differs from the spec's Integer model and must be revisited by
  the Wave 4 integer-model work — it is a containment bulkhead, not the
  final semantics."

Wave 4 step 3 is that revisit. Claude's Wave 4 order
(`claude-onx-review-2026-10-06.md`) puts it after gas caps (done, ADR-0034)
and before builder/bit-length.

## Decision

### 1. Carrier: `Int257`, canonical 257-bit two's complement

New type `crates/protocol/onx-execution/src/int257.rs`:

- Internal: 5 little-endian `u64` limbs holding a 320-bit two's complement
  value, with the invariant that bits 257..320 are the sign extension of
  bit 256 (so `limbs[4]` is `0`/`1` for non-negative values,
  `0xFFFF_FFFF_FFFF_FFFE`/`0xFFFF_FFFF_FFFF_FFFF` for negative ones).
- Canonical stack encoding: 33-byte big-endian, `byte[0] & 0xFE == 0`
  (bit 256 is the sign). `StackValue::Integer` now carries `Int257`,
  not `[u8; 32]`.
- Every constructor maintains the invariant; `from_bytes33` rejects
  non-canonical encodings. There is no way to build a non-canonical
  `Int257` through the public API — canonicality bugs would be consensus
  bugs, so the type enforces it, not the call sites.

No new dependencies: the arithmetic is hand-rolled on fixed limbs
(schoolbook multiply, binary long division), matching the crate's
dependency-free stance (cf. the fuzz scaffold, ADR-0028 §5). Python's
arbitrary-precision ints are the independent oracle
(`reference/gen_int257_vectors.py`).

### 2. Exact arithmetic, then width/flavor application

All arithmetic computes the **true mathematical result exactly** in a
640-bit two's-complement working type (`Wide`, 10 limbs — every op is
exact: `MUL` of 257-bit values needs ≤ 514 bits, `LSHIFT` by < 256 needs
≤ 512 bits), then applies the declared width/flavor per spec §3.3:

- flavor 0 (unsigned): result must satisfy `0 ≤ v < 2^width`, else
  `IntegerOverflow`;
- flavor 1 (signed): `-2^(width-1) ≤ v < 2^(width-1)`, else
  `IntegerOverflow`;
- flavor 2 (modulo): `v mod 2^width`, stored as the unsigned bit pattern,
  never raises on the result.

**Range checks precede flavor** (spec §3.3, pinned by ADR-0031 §3):
division by zero and out-of-range shift amounts raise `IntegerOverflow`
for every flavor, before flavor selection.

`0x10`–`0x14` (`ADD`/`SUB`/`NEG`/`MUL`/`DIVMOD`) now enforce their
declared width/flavor — ADR-0028/ADR-0031's "ignore the flavor operand
entirely" non-goal is closed. `0x17`–`0x19` accept width `1..=256`
(was `1..=128`); the F1/F2 hotfix's `Raw::{I128,U128}` carrier machinery
is deleted, replaced by exact arithmetic.

### 3. Division: floored, fully defined on 257 bits

`q = floor(a/b)`, `r = a - q·b`, `sign(r) = sign(b)` or `r = 0` (spec §4.3,
ADR-0030 — TON's round-toward-negative-infinity). Implemented on
magnitudes via binary long division with the same floor adjustment the
i128 code used, generalized. The old `MIN / -1` special case generalizes:
`(-2^256) / (-1)` has true quotient `2^256`, which fits no declared width
(`≤ 256`), so flavors 0/1 raise and flavor 2 yields `0`.

### 4. Opcode semantics corrected to the spec

- `PUSHINT` (0x08): the `signed` flag is now honored (was ignored):
  `signed != 0` interprets the 32-byte operand as 256-bit two's
  complement, `signed == 0` as unsigned. Both fit the domain.
- `CONV` (0x20): was a no-op ignoring width/signedness; now re-checks the
  value at the declared width (`IntegerOverflow` if it doesn't fit).
- `STBITS` (0x42): was unchecked and ignored `signed`; now range-checks
  with the declared signedness, then appends the low `width` bits of the
  two's-complement encoding.
- `LDI` (0x47): now sign-extends from `width` bits (was byte-identical to
  `LDU` — a spec divergence noted since ADR-0028).
- `HASHBYTES`/`HASHCELL` (0x60/0x61): 256-bit digests are pushed as
  unsigned 256-bit Integers — under the old carrier a top-bit-set hash
  was negative or unrepresentable; now it is exactly what the spec says.
- `CHKSIGNU` (0x62): the hash operand must be in `[0, 2^256)` to name a
  32-byte preimage; a negative Integer is `TypeMismatch` (deterministic;
  a hash is a 256-bit string, not a signed value).
- Control-flow truthiness (`IFJMP`/`IFNOTJMP`/`IFELSE`/`IFRET`/`UNTIL`
  family): any nonzero 257-bit value is true. The ADR-0031 bulkhead
  (operands `≥ 2^127` raised instead of branching) is gone — this is the
  intended model change, not a regression.

### 5. What the domain bound means in practice

Declared widths are `1..=256`, so no opcode can *produce* a value outside
`[-2^255, 2^256)` — but the carrier accepts the full spec domain, and
every value the stack can hold satisfies it by construction:
`PUSHINT`/`HASHBYTES`/`HASHCELL`/`MSGVALUE`/`LDU`/`LDI` all produce
in-domain values, and every arithmetic result passes through
width/flavor application before it is stored. `-2^256` is representable
but not currently producible by any opcode (widths cap at 256); the
implementation handles it correctly anyway (notably `NEG(-2^256)` →
true `2^256` → flavors 0/1 raise, flavor 2 wraps to `0`).

## Consequences

- The ADR-0031 bulkhead behaviors change where the spec requires it:
  `CMP(2^127, 0)` is `1` (was `IntegerOverflow`); arithmetic on operands
  in `[2^127, 2^256)` works instead of raising; `DIV`'s `MIN/-1`
  u128-carrier special case is subsumed by exact arithmetic.
- `StackValue::Integer`'s payload type changes (`[u8; 32]` → `Int257`);
  the compiler enumerates every affected site (all inside
  `onx-execution` plus its tests — verified by grep before the change).
- Spec amendments: `tvm-instruction-set.md` §3.2 (Integer representation),
  §3.3 (the "0x10–0x14 ignore flavor" note is removed), §4.2 (`PUSHINT`
  signed flag, `CONV` range check, `0x17`–`0x19` width `1..=256`), §4.4
  (`STBITS` signed range check, `LDI` sign extension).
- Python reference: `reference/gen_int257_vectors.py` +
  `reference/vectors/int257.json` (independent oracle; `--check` drift
  gate); Rust `tests/int257_vectors.rs` asserts agreement.
- The old `divmod.json` (`integer_bits: 128`) is superseded by
  `int257.json` for the integer model; it is regenerated at 257 bits to
  avoid a stale doc artifact.
- Audit surface for Claude's Wave 4 audit: the limb arithmetic
  (`int257.rs` — add/sub/mul/divmod/shl/shr, the fit/wrap bit tests),
  canonicality enforcement, and the width/flavor application on every
  Integer-producing opcode.

## Non-goals

- Bit-length bookkeeping for cells/builders (Wave 4 step 4).
- `code_refs` call-stack depth limit (lands with step 7).
- Gas repricing of arithmetic (unchanged costs).
