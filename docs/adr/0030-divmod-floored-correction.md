# ADR-0030 — DIVMOD Floored-Division Correction

**Status:** Accepted

**Date:** 2026-10-06

**Supersedes:** ADR-0028 §2 (the floored-division pin)

## Context

ADR-0028 §2 fixed the wave-3 DIVMOD panic by switching the interpreter to
`checked_div_euclid` / `checked_rem_euclid`, on the premise that the spec's
word "floored" (`docs/specification/tvm-instruction-set.md` §4.3) was
*underspecified for negative divisors*, so the ADR pinned the
non-negative-remainder invariant `0 <= r < |b|` for all divisors:
`7 DIVMOD -2 → (-3, 1)`, `-7 DIVMOD -2 → (4, 1)`.

That premise was wrong. "Floored" was never underspecified: `q = floor(a/b)`
— the greatest integer ≤ the exact quotient — is fully defined for negative
divisors, and it disagrees with Euclidean division exactly there. The
Euclidean pin was a spec-divergence bug, not a clarification:

| Pair | Euclidean (ADR-0028, wrong) | True floor (spec, correct) |
|---|---|---|
| `7 DIVMOD -2` | `(-3, 1)` | `(-4, -1)` |
| `-7 DIVMOD -2` | `(4, 1)` | `(3, -1)` |
| `-7 DIVMOD 2` | `(-4, 1)` | `(-4, 1)` (agree) |

The spec is the contract; the code must change, not the spec.

## Decision

1. **True floor division everywhere.** `DIVMOD` (0x14) and `DIV` (0x17) now
   compute `q = floor(a/b)`, `r = a − q·b`, with `sign(r) == sign(b)` or
   `r == 0`. The implementation starts from the truncated `checked_div` /
   `checked_rem` and applies the one-step floor correction
   (`q − 1`, `r + b`) exactly when the truncated remainder is non-zero and
   its sign disagrees with the divisor's — i.e. exactly when truncation
   rounded toward zero *up* past the floor. Both steps stay `checked_*`
   (the crate denies `clippy::arithmetic_side_effects`); each is provably
   non-overflowing (the `q − 1` site is unreachable for `q_trunc = MIN`,
   the `r + b` site strictly shrinks toward zero), and the checks are kept
   for the lint, documented at the call site.
2. **Error behavior unchanged.** `b = 0` and `MIN / -1` still map to
   `IntegerOverflow` (the latter because the true floor quotient, 2^127,
   is unrepresentable in `i128` — the same reason the old code raised).
3. **Spec text made explicit.** `tvm-instruction-set.md` §4.3 now states
   the formula in the `DIVMOD`/`DIV` rows (`q = floor(a/b)`, `r = a − q·b`,
   `sign(r) = sign(b)` or `r = 0`) plus a "stated exactly" paragraph with
   the four sign-combination examples. The word "floored" was already
   right; it is now unambiguous. The semantic change and the spec edit
   merge together, per repo rule.
4. **ADR-0028 §2 is superseded, not rewritten.** ADR-0028 remains the
   historical record of the wave-3 panic fix; its §2 pin is corrected by
   this ADR. Its test-count line is corrected separately to the real
   `cargo test -p onx-execution` count (see below).

## Why this is safe to do now

- **Zero switching cost.** There are no deployed contracts, no transaction
  history, and no downstream consumers exercising `DIVMOD` with negative
  divisors — the network is live but empty. The semantic change touches
  nothing observable in the wild.
- **TON compatibility.** TON's own integer division rounds toward
  negative infinity; true floor is the TON-compatible convention, and
  ONX is a spec-first reimplementation of that lineage.
- **Spec primacy.** The review's standing rule is that the spec is the
  contract and the code follows. ADR-0028 inverted that for one operator;
  this ADR restores it.

## Consequences

- `DIV` (0x17) changes observable semantics for negative divisors
  (`7 / -2`: `-3` → `-4`). Same deliberate-consensus-alignment reasoning
  as ADR-0028's `-7/2` note; nothing in the wild consumes it.
- The Python reference gains `reference/vectors/divmod.json` — the first
  DIVMOD vectors, computed from the spec formula independently of the Rust
  code and covered by `gen_vectors.py --check`. (TVM execution remains a
  deliberate non-goal of the Python reference; the vectors are spec-derived
  arithmetic cases, not an interpreter.)
- `crates/protocol/onx-execution/tests/arithmetic_safety.rs`:
  `divmod_is_floored_with_nonnegative_remainder` →
  `divmod_is_true_floor` with the corrected vectors and the
  `sign(r) == sign(b)` invariant; `div_opcode_matches_divmod_quotient`
  updated (`7/-2 → -4`).

## Implementation

- `crates/protocol/onx-execution/src/interpreter.rs`: new private
  `floored_divmod(a, b) -> Option<(i128, i128)>` used by both `0x14`
  and `0x17`; Euclidean calls removed.
- `docs/specification/tvm-instruction-set.md`: explicit floor formula in
  the `DIVMOD`/`DIV` rows + "stated exactly" paragraph.
- `crates/protocol/onx-execution/tests/arithmetic_safety.rs`: corrected
  vectors.
- `reference/gen_vectors.py` + `reference/vectors/divmod.json`: new
  spec-derived vectors, wired into `--check`.

## Tests

- `cargo test -p onx-execution`: 34 passed, 0 failed (3 lib incl. fuzz + 12 arithmetic_safety + 1 elector + 13 execution + 5 vm_child_cells; command: `cargo test -p onx-execution`; count corrected in ADR-0028's Tests section to match).
- `cargo clippy -p onx-execution --all-targets`: zero warnings.
- `cargo fmt --check`: clean.
- `python3 reference/gen_vectors.py --check`: vectors match.
