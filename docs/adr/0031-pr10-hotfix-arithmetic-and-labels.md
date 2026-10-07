# ADR-0031 — PR #10 Hotfix: Arithmetic Fail-Closed Containment and Label Allowlist

**Status:** Accepted

**Date:** 2026-10-07

**Supersedes:** ADR-0028 Non-goals (the `to_i128` truncation statement)

## Context

PR #10 ("genesis validator key check") shipped a strict Ed25519 canonicality
predicate (verified sound: 21,076 encodings vs an independent oracle, 0
mismatches) but regressed 70 arithmetic vectors to silently wrong values and
dropped validation in two genesis paths. An external audit (2026-10-06,
verdict: KEEP but NO-GO as done) enumerated 7 hotfix items. This ADR is the
normative record of the consensus-semantic changes the hotfix makes — the
audit's blocking condition #2 was that these shipped with no ADR.

## Decision

### 1. `to_i128` fails closed (P1-minimal containment)

`StackValue::to_i128` no longer truncates to the low 128 bits. It raises
`IntegerOverflow` unless the high 128 bits are the correct sign extension
of the low 128 bits. This closes the silent-truncation class (e.g. `2^200`
as an operand, previously read as `0`) without implementing the full
257-bit integer model — that remains a Wave 4 non-goal behind the deploy
gate (ADR-0028's integer-model wave, unchanged).

ADR-0028's Non-goals line stating `to_i128` "still truncate[s] to the low
128 bits" is superseded: it is now false.

### 2. LSHIFT detects shifted-out bits; width-128 unsigned/wrap via u128

`LSHIFT` (0x18) no longer uses bare `checked_shl` (which silently discards
shifted-out bits, rejecting only amounts ≥ 128). A shift is lossless iff
`(r >> b) == a` round-trips; lost bits raise `IntegerOverflow` for flavors
0/1. At width 128:
- flavor 0 (unsigned) keeps true products in `[0, 2^128)` via a u128 path
  (`1 << 127 → 2^127`; `(2^127-1) << 1 → 2^128-2`; `2 << 127 → raise`);
- flavor 1 (signed) raises on any lost bits (true product outside
  `[-2^127, 2^127)`);
- flavor 2 (wrap) computes `(a · 2^b) mod 2^width` in u128 and never raises
  *on the result*.

The unsigned fits-check `a < 2^(w-b)` is exact only with the bound
`w-b >= 127` short-circuiting to true; the bound is pinned by regression
test (`2^126 << 2` at width 128 must raise — `u128::checked_shl` wraps
instead of reporting the overflow).

### 3. Modulo flavor result path; range checks precede flavor

`DIV` (0x17) routes `i128::MIN / -1` (true quotient `2^127`) and width-128
modulo results through a raw u128 carrier (`StackValue::from_u128`);
`-7 DIV 2` in modulo flavor at width 128 yields `2^128 - 4`, never raising.
"Never raises" describes the *result* reduction only: operand range checks —
shift amount `< 0` or `>= width`, division by zero (spec §5 test 2) —
are evaluated before flavor selection and raise `IntegerOverflow` for every
flavor. Opcodes `0x10`–`0x14` ignore the flavor operand entirely (Wave 4
non-goal). Spec §3.3 now pins "range checks precede flavor" explicitly.

### 4. Label allowlist; balance-key validation restored

Malformed key literals can no longer silently become DEV labels (F3). The
blocklist is replaced by an allowlist: labels must be non-empty ASCII
matching `[a-z0-9][a-z0-9._:-]{0,47}`; all-hex strings of length 16+ and
colon-prefixed hex runs of length 16+ (`ed25519:<32 hex>`) are rejected as
key material. Label-derived balance keys are re-validated with
`PublicKey::decode_exact`, restoring the validation main applied before
PR #10 (F4). (Genesis parsing, not runtime consensus — recorded here for
completeness; validator labels remain DEV-only by convention, unenforced.)

## Consequences

- **u128 carrier containment (auditor-confirmed by probe):** carrier results
  ≥ `2^127` are produced (e.g. `1 LSHIFT 127` unsigned at width 128) but
  cannot be consumed as operands by any opcode except `ISZERO`, `CONV`, and
  `STBITS` — every other operand load goes through `to_i128`, which raises.
  Concretely: `1 LSHIFT 127` (unsigned, width 128) followed by `CMP`
  raises `IntegerOverflow`. Unsigned-128 therefore holds `[0, 2^128)` on
  *output* but `[0, 2^127)` on *input*. This is fail-closed, but it differs
  from the spec's Integer model and must be revisited by the Wave 4
  integer-model work — it is a containment bulkhead, not the final
  semantics.
- The 70 regressed vectors are restored to spec values (independently
  verified: auditor's Python model, 77,112 vectors across widths 1–128 ×
  all flavors × edge operands, 0 mismatches; DIVMOD floor oracle 3,289/3,289
  in-range vectors).
- `DIV`/`DIVMOD` by zero still raises for every flavor (spec §5 test 2);
  the "modulo never raises" claim is narrowed to result reduction, per §3.3.

## Non-goals (explicit)

- Full 257-bit integers: still Wave 4. The fail-closed `to_i128` and the
  u128 carrier are containment, not the integer model.
- `0x10`–`0x14` width/flavor enforcement: still Wave 4.
- The `#[ignore]`d `successful_decodes_are_canonical` genesis re-sort test:
  pre-existing on main, untouched by this hotfix, still ignored.

## Implementation

- `crates/protocol/onx-execution/src/types.rs`: `to_i128` fail-closed;
  `StackValue::from_u128`.
- `crates/protocol/onx-execution/src/interpreter.rs`: LSHIFT no-bits-lost
  + u128 paths; DIV u128 carrier + MIN/−1; `Raw::I128`/`Raw::U128` result
  plumbing; "range checks precede flavor" ordering.
- `crates/protocol/onx-state-model/src/genesis.rs`: label allowlist
  (`validate_label`) incl. colon-prefixed hex-run rejection; `parse_or_derive_pubkey`
  takes a field name for error wording.
- `crates/tooling/onx-genesis/src/lib.rs`: unconditional
  `PublicKey::decode_exact` for balance keys incl. label-derived.
- `docs/specification/tvm-instruction-set.md` §3.3: "range checks precede
  flavor" pinned.

## Tests

- `crates/protocol/onx-execution/tests/arithmetic_safety.rs`: 24 tests —
  incl. 32-byte u128 representation pins (high half must be zero-padded),
  `to_i128` bad-sign-extension case (high half zero, bit 127 set),
  LSHIFT `shift >= 127` bound pin (`2^126 << 2` raises), DIV-by-zero and
  bad-shift-amount coverage in all three flavors. Mutation-checked: the
  bound widening, the sign-check relocation, and the `from_u128`
  sign-extension rewrite are each caught.
- `crates/protocol/onx-state-model/src/genesis.rs`: label boundary vectors
  (15- vs 16-hex, 48-char cap, uppercase-after-first, `ed25519:<32 hex>`).
- `cargo fmt --check` clean; `cargo clippy --workspace --all-targets`
  clean on the pinned toolchain (1.98.1).
