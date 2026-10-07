# ADR-0028 — VM Arithmetic Safety (Wave 3: Chain Safety)

**Status:** Accepted

**Date:** 2026-10-06

## Context

A review of the wave-2 VM found that `DIVMOD` (`crates/protocol/onx-execution/src/interpreter.rs`) computed `a / b` and `a % b` on `i128` with only a divide-by-zero guard. The pair `(i128::MIN, -1)` overflows `i128` and **panics** — in both debug and release builds. The producer (`onxd`) has no `catch_unwind` around execution, so a single crafted message containing that pair halts block production and wedges the node into a systemd restart loop: a one-message chain halt, triggerable by anyone who can send a message to a contract.

The audit also had to answer two questions before any fix: which `ExceptionKind` an arithmetic fault maps to, and whether that mapping bounces the message or aborts the block.

## Decision

1. **All VM arithmetic is overflow-explicit.** `DIVMOD` (0x14) now uses `checked_div_euclid` / `checked_rem_euclid`; every overflow — including `MIN / -1` and `MIN % -1` — maps to `ExceptionKind::IntegerOverflow`. The sibling opcodes (`ADD`/`SUB`/`NEG`/`MUL` at 0x10–0x13, `DIV`/`LSHIFT` at 0x17–0x18) already used `checked_*` and are unchanged in their overflow mapping. `SUBBYTES` (0x32) now uses `checked_add` for `offset + len`: adversarial `(offset, len)` pairs (e.g. `i128::MAX` wrapping to `usize::MAX`) previously panicked on `usize` overflow in debug builds; they now fail closed with `MalformedCell`. Gas-accounting additions that are unreachable in practice use `saturating_add` so the accounting can never be the panic site.
2. **Floored division, pinned.** Per `docs/specification/tvm-instruction-set.md` §4.2, `DIVMOD` is floored: `(a, b) -> (a div b, a mod b)` with `a = q*b + r` and `0 <= r < |b|` — e.g. `-7 DIVMOD 2 → (-4, 1)`. The spec's "floored" label is underspecified for *negative* divisors (strict Knuth-floor would give `7 DIVMOD -2 → (-4, -1)`), so this ADR pins the non-negative-remainder invariant `0 <= r < |b|` for all divisors (`7 / -2 → (-3, 1)`, `-7 / -2 → (4, 1)`), which coincides with floored division for every positive divisor. `DIV` (0x17) uses the same operator so the two opcodes agree on the quotient (the old truncating `checked_div` did not). Division by zero keeps its existing mapping to `IntegerOverflow` (ADR-0024 §3.3). No new exception kinds were added; the closed set is untouched.
3. **Overflow bounces; it never aborts the block.** Verified by reading (not changing) `crates/protocol/onx-stf/src/stf.rs`: `ExecutionResult::Exception { .. }` maps to `must_bounce = true`, which queues a bounce message returning the value to the sender and continues draining the queue. The only block-fatal path is `BounceUndeliverable` (a bounce that itself cannot be delivered), which is unreachable in honest operation. So `IntegerOverflow` from a crafted message bounces that message — the chain keeps producing.
4. **Lint policy: `clippy::arithmetic_side_effects` is `deny` for `onx-execution`** (set in `src/lib.rs`, alongside the existing `disallowed_types` deny). Rationale: a plain `+`/`-`/`*`/`/`/`%` on any value reachable from contract bytecode is a potential producer panic, i.e. a potential chain halt. Every arithmetic site in the crate now names its overflow behavior (`checked_*` → exception, `saturating_*` → gas/index bookkeeping, `wrapping_*` → provably in-range bit manipulation), so the property is load-bearing for future code, not just this wave's audit. `indexing_slicing`, `unwrap_used`, and `expect_used` were evaluated and deliberately **not** enabled: ~38 indexing sites in `src/` alone and 61 unwrap/expect sites across `src/`+`tests/` exceed the churn budget; they remain future work.
5. **Fuzz scaffold, zero new dependencies.** `src/fuzz.rs` (gated `#[cfg(test)]`, no proptest/cargo-fuzz — verified absent from the workspace dependency closure) drives the interpreter with a hand-rolled deterministic XorShift64 PRNG over two corpora: biased-random bytecode (valid instruction shapes mixed with noise, capped at the 128-byte cell limit) and exhaustive `(a, b)` edge-value pairs × the full arithmetic opcode family × all width/flavor combinations. Every run asserts no-panic (via `catch_unwind` in the harness only), the 1023 stack-depth cap, gas-limit adherence, and run-twice determinism. The scaffold was verified to catch the original bug: reintroducing `a / b` fails `fuzz_arithmetic_edge_values_never_panic` on the `(MIN, -1)` DIVMOD program.

## Consequences

- The one-message chain-halt class is closed: no `ExceptionKind`-mappable arithmetic fault can panic the producer anymore. What remains panic-reachable is outside the interpreter's arithmetic (allocation failure on absurd `Bytes` sizes, which gas pricing already bounds — noted, not fixed here).
- `DIV` (0x17) changes observable semantics for negative dividends (`-7/2`: `-3` → `-4`). This is a deliberate consensus-semantic alignment with the spec's floored `div` operator, not just a panic fix; any downstream consumer of truncated quotients must be aware.
- The negative-divisor remainder convention (`0 <= r < |b|`) is now pinned code, but the *spec text* still says only "floored". A follow-up spec edit should state the invariant explicitly; this ADR is the normative record until then.

## Non-goals (explicit)

- **The 257-bit integer-model correction is a LATER wave behind the deploy gate.** The interpreter still computes on `i128` while the spec's `Integer` kind is `[-2^256, 2^256 - 1]`; `StackValue::from_i128`/`to_i128` still truncate to the low 128 bits; the baseline arithmetic family's `width`/`flavor` operands are still accepted-but-unenforced (e.g. `ADD` does not check the declared width, `CONV`/`STBITS` do not range-check the value). None of that is changed here — those are consensus-semantic changes owned by the integer-model wave, not safety fixes.
- No Python reference changes (`reference/` untouched — later wave).

## Implementation

- `crates/protocol/onx-execution/src/interpreter.rs`: floored checked `DIVMOD`/`DIV`; `checked_add` in `SUBBYTES`; `saturating_*` gas/index arithmetic; `MAX_STACK_DEPTH` made `pub(crate)` for the fuzz scaffold.
- `crates/protocol/onx-execution/src/types.rs`: `read_bits`/`append_bits`/`remaining_bits` rewritten with explicit `saturating_*`/`wrapping_*` forms (provably exact under the existing guards).
- `crates/protocol/onx-execution/src/dictionary.rs`: same explicit-arithmetic treatment (required by the crate-wide deny).
- `crates/protocol/onx-execution/src/lib.rs`: `#![deny(clippy::arithmetic_side_effects)]` with rationale comment; `#[cfg(test)] mod fuzz`.
- `crates/protocol/onx-execution/src/fuzz.rs`: new, test-gated fuzz scaffold (no new dependencies).
- `crates/protocol/onx-execution/tests/arithmetic_safety.rs`: new, 12 regression tests.

## Tests

- `cargo test -p onx-execution`: 34 passed, 0 failed — 12 `arithmetic_safety` tests (MIN/-1 div and rem → `IntegerOverflow`; true-floor negatives `-7/2 → (-4, 1)`, `7/-2 → (-4, -1)`, `-7/-2 → (3, -1)` with the `a = q*b + r`, `|r| < |b|`, `sign(r) = sign(b)` invariant asserted independently — corrected from ADR-0028's Euclidean pin by ADR-0030; div-by-zero mapping unchanged; MIN/MAX edges; ADD/SUB/MUL/NEG overflow kinds; LSHIFT overflow/negative/`>= width` shift; SUBBYTES adversarial offset/len → `MalformedCell` plus happy path; recursive PUSHINT flood respects the 1023 stack cap), 3 fuzz tests, 19 pre-existing tests unbroken. (Count verified 2026-10-06 by running `cargo test -p onx-execution` at the ADR-0030 commit: 3 lib + 12 arithmetic_safety + 1 elector + 13 execution + 5 vm_child_cells = 34.)
- `cargo clippy -p onx-execution --all-targets`: zero warnings (42 `arithmetic_side_effects` sites fixed).
- `cargo fmt`: clean on all touched files.
