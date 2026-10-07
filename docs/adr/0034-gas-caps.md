# ADR-0034: Gas caps — per-message and per-block

**Status:** Proposed (2026-10-07)
**Decider:** Gokoo (design authority between audits; flagged for Claude's Wave 4 audit)

## Context

The VM has a per-execution gas model (`gas_limit`, `gas_used`, `OutOfGas`),
but there are no protocol-level caps:

- Per-message: `gas_limit = fee_nanos * GAS_PER_NANO` (`GAS_PER_NANO = 1_000`),
  saturating at `u64::MAX`. A message with a large fee gets an effectively
  unbounded gas limit.
- Per-block: no accounting at all. `apply_messages` runs every message;
  receipts track per-delivery `gas_used` but nothing bounds the total.

Claude's Wave 4 order (`claude-onx-review-2026-10-06.md` §"Wave 4 reorder")
puts gas caps first, before 257-bit ints and before `code_refs`:

> 2. GAS CAPS FIRST (per-message + per-block) → ... → 7. code_refs LAST.
> code_refs needs per-message gas cap AND explicit call-stack depth limit
> (~256) IN THE SAME CHANGE.

And from the JMPREF/CALLREF sequencing note:

> per-message/per-block gas caps must land BEFORE or WITH that fix.

The finding: unbounded *time* is already possible via `IFELSE` backward
jumps (`0x78`) + `REPEAT` (`0x7A`), bounded only by gas; `code_refs` adds
unbounded *memory* (call-stack heap clones). The per-message cap is the
prerequisite that makes the later `code_refs` change safe.

## Decision

Two protocol constants, both consensus rules (all nodes derive them
identically from the same inputs; no configuration, no negotiation):

```rust
/// Maximum gas any single message execution may consume.
pub const MAX_GAS_PER_MESSAGE: u64 = 10_000_000; // 10M
/// Maximum total gas across all message deliveries in one block.
pub const MAX_GAS_PER_BLOCK: u64 = 100_000_000; // 100M
```

### Per-message cap

In `try_execute_contract` (`crates/protocol/onx-stf/src/stf.rs`):

```rust
let gas_limit = msg
    .fee_nanos
    .saturating_mul(GAS_PER_NANO as u128)
    .min(u64::MAX as u128) as u64;
let gas_limit = gas_limit.min(MAX_GAS_PER_MESSAGE);
```

The fee mechanics are unchanged: the sender is still debited the full
`fee_nanos` (burned/split as before). Only the *execution budget* is
clamped. A fee above 10,000 nanos buys no additional gas — the excess is
pure fee, not execution.

### Per-block cap

In `apply_messages` (shared by `apply_block` and `propose_block`, so the
producer's dry-run and the validator's replay enforce byte-identical
logic): accumulate `gas_used` across all deliveries; if the running total
would exceed `MAX_GAS_PER_BLOCK`, abort with a new fail-closed error:

```rust
pub enum StfError {
    ...
    /// Cumulative block gas exceeded MAX_GAS_PER_BLOCK. The block is
    /// invalid; the producer must select fewer/smaller messages.
    BlockGasExceeded { used: u64, cap: u64 },
}
```

The check is incremental (fail fast on exceed), but the rule is stated on
the total: a block is valid iff the sum of per-delivery `gas_used` over
all messages in the block is `<= MAX_GAS_PER_BLOCK`.

`propose_block` therefore *cannot* produce an over-cap block — it returns
`Err` instead. Producer message selection (choosing a subset that fits)
is future work; for now, over-cap message sets are simply not proposable.

### What counts toward the block cap

- `DeliveryReceipt.gas_used`: TVM gas consumed on successful contract
  execution; 0 for plain value deliveries and for bounces (unchanged).
- Bounced messages contribute 0 (they never ran the VM).
- The cap covers delivery-phase execution only, matching the existing
  `gas_used` accounting.

## Rationale for the values

- **10M per message:** At 1–10 gas per VM operation (the current pricing),
  this is ~1M–10M operations — ample headroom for complex contracts
  (the existing increment-contract test uses a few hundred gas). The fee
  required to reach the cap is 10,000 nanos at `GAS_PER_NANO = 1_000`,
  a small amount; the cap binds only on pathological or adversarial
  fees. It prevents a single message from monopolizing block execution.
- **100M per block:** 10× the per-message cap. A block can hold up to ten
  max-gas messages, or many smaller ones. It bounds total block execution
  time to a small multiple of the single-message bound.

These are devnet-sensible, not mainnet-final. They are protocol constants
today; promoting them to version-gated parameters is deferred to the
multi-validator phase if needed. Changing them later is a consensus change
(requires a `protocol_version` bump per ADR-0032's version-gating rule).

## Consequences

- New `StfError::BlockGasExceeded` variant; error type is non-exhaustive
  in practice (callers already handle `StfError` openly).
- Existing tests using fees above 10,000 nanos (e.g. `tvm_integration`
  with fee 100_000 → 100M uncapped gas_limit) now run with a 10M
  gas_limit. Assertions on `gas_used` upper bounds still hold; no test
  asserts the uncapped limit value itself.
- The Python reference (`reference/`) gains the cap rules and vectors;
  `reference/vectors/gas_caps.json` pins the constants and the
  clamp/accumulate behavior.
- Spec: `docs/specification/execution.md` §3.2/§3.4 amended with the cap
  rules (this ADR is the decision record; the spec is the normative text).
- Audit surface: the caps are consensus-critical. Claude's Wave 4 audit
  must confirm (a) the clamp is applied on every path that sets a VM
  gas_limit, (b) the block accumulator cannot be bypassed or double-counted,
  (c) `propose_block`/`apply_block` agree (shared `apply_messages`).

## Non-goals

- Fee market or gas pricing changes (`GAS_PER_NANO` untouched).
- Producer message-selection policy under the block cap.
- The `code_refs` call-stack depth limit (lands WITH `code_refs`, per
  Claude's order — this ADR only provides the gas-cap prerequisite).
