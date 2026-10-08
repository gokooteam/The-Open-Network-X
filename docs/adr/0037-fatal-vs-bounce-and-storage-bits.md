# ADR-0037 — Fatal-vs-Bounce Failure Taxonomy + Bit-Precise Storage Stats (Wave 4, step 5)

**Status:** Proposed (2026-10-07)
**Decider:** Gokoo (design authority between audits; flagged for Claude's Wave 4 audit)
**Amends:** ADR-0004 (bounce semantics); `docs/specification/execution.md` §3.4

## Context

Two Wave 4 findings converge in this step.

**1. ADR-0004 made every VM exception bounce — including out-of-gas.**
That was the deliberate break from the sync model: a failing contract
bounces its delivery instead of poisoning the block. But it prices
resource exhaustion wrong. A contract that burns its entire gas budget
and a contract that throws on its first instruction both return the
full value to the sender. The sender pays only the fee either way — so
an attacker can force the network to do `MAX_GAS_PER_MESSAGE` (10M gas,
ADR-0034) of reverted work per message at a fixed, small fee. TON's
answer to exactly this is the fatal/bounce split: out-of-gas is fatal,
the value is not returned.

**2. ADR-0036 made cells bit-granular, but `StorageStat` still measures
bytes.** `StorageStat { cell_count, byte_count }` is persisted in every
`Active` account record. After the compatible flag, a 3-bit cell and an
8-bit cell both occupy one data byte — `byte_count` cannot distinguish
them. Any future storage accounting (rent, quotas) built on bytes alone
would charge 8× too much for a 3-bit cell.

## Decision

### 1. Fatal vs bounce: the taxonomy

A contract-execution failure is **fatal** iff the message's gas budget
was exhausted. It **bounces** otherwise.

| Failure | Outcome | Rationale |
|---|---|---|
| `ExceptionKind::OutOfGas` | **fatal** | The network demonstrably spent the full paid budget on this message. Returning the value prices griefing at the fee alone. |
| `IntegerOverflow`, `AbsentNode`, `MalformedCell`, `TypeMismatch` — whether raised by the VM or deliberately by the contract via `THROW` (0x77) | **bounce** | Early, cheap failures (or deliberate contract rejections). The sender is refunded; the broken contract stays broken. |
| `Success` with non-empty `out_messages` | **bounce** (unchanged) | Message egress is still unwired this milestone; the value must not be silently dropped. |
| Structural cases (frozen/destroyed/uninitialized+payload/codeless+payload) | **bounce** (unchanged, ADR-0004) | Not execution failures at all. |

**Fatal mechanics** (mirrors TON: the account keeps the value):
- The delivery's value is credited to the destination account (after the
  usual `check_lt` fail-closed ordering).
- No contract data update, no bounce message queued.
- `gas_used` on the receipt is the consumed gas (the full limit on
  out-of-gas), so the ADR-0034 block-gas cap still accounts for it.
- A fatal delivery is still a *valid* delivery: the block is not
  invalidated. Only the value's destination changes.

**Gas accounting on failure (design call):** bounce receipts now report
the gas the VM actually burned before failing (previously pinned to 0),
exactly like fatal receipts. Rationale: the ADR-0034 block cap counts
`sum(gas_used)` as executed work — a bounce that burned 9.9M gas before
throwing is 9.9M gas of real work, and hiding it from the cap reopens
the block-fill DoS the per-block cap exists to close. State effects
still revert-by-construction; only the receipt's accounting changes.
The pre-existing `tvm_vm_exception_bounces` gas_used==0 assertion is
updated to the new rule.

**What fatal is NOT:** it is not a block-level failure, not a slash,
not a burn-to-zero. The value moves sender → destination exactly as a
plain transfer would; the only thing lost is the bounce refund.

**Deliberate non-distinctions:**
- `THROW` maps onto the existing `ExceptionKind` variants (0x77), so a
  user-thrown `IntegerOverflow` is indistinguishable from a VM-raised
  one — both bounce. No new exception kind is introduced; the taxonomy
  keys off the five closed kinds only.
- `AbsentNode` (pruned Merkle content) bounces rather than going fatal:
  the failure is in the *data the contract was given*, not in resource
  consumption. Burning the sender's value for a host-side data gap would
  punish the wrong party.

### 2. StorageStat gains `bit_count`

```rust
pub struct StorageStat {
    pub cell_count: u32,
    pub byte_count: u64,
    pub bit_count: u64,   // NEW: sum of Cell::bit_len() over code+data
}
```

- `storage_stat_for` (the single writer, called on every code/data
  change) computes `bit_count` from `Cell::bit_len()` — precise for
  flagged cells, `8 × data_bytes` for byte-granular ones.
- **The account codec does NOT change.** The 141-byte `Active` header
  layout is pinned by `reference/account.py` and the V2 golden vectors
  (`test_codeless_account_encoding_is_byte_identical_to_v2`); changing it
  would invalidate every genesis golden. `bit_count` is *derived on
  decode*: `AccountState::from_bytes` recomputes it from the decoded
  code/data cells. The persisted `(cell_count, byte_count)` plus the
  embedded cells fully determine it, so nothing is lost — the struct
  field is a cached precise measure, not new wire state.
- Scope note: `storage_stat_for` counts the two embedded root cells,
  not the full persisted cell DAG (`contract_cells`). Full-DAG
  accounting is a storage-rent design question, explicitly deferred —
  flagged for the Wave 4 audit.

## Alternatives considered

- **Keep everything bouncing (status quo).** Rejected: griefing
  asymmetry above; TON precedent.
- **Fatal = burn the value (to fee collector / zero).** Rejected: the
  value was a legitimate transfer the sender authorized; the failure
  was in execution, not in the transfer's validity. Crediting the
  destination is the minimal surprise — identical to what a plain
  transfer to that account would have done.
- **Add `bit_count` to the wire codec (149-byte header).** Rejected:
  breaks the 141-byte V2 golden invariant across the Python reference
  and every genesis vector for a field no consensus rule reads yet.
- **Drop `byte_count`, replace with bits.** Rejected: renaming the
  meaning of a persisted field is worse than adding a derived one.

## Consequences

- `DeliveryReceipt` gains `fatal: bool` (invariant: at most one of
  `bounced`/`fatal` is true; both false = processed). Receipts stay
  deterministic given `(State, Block)`; replay equivalence is
  unaffected (it compares state roots, and receipts are compared
  field-for-field where used).
- Contract developers: an out-of-gas call now loses the value, not just
  the fee. Size gas budgets accordingly (fee × `GAS_PER_NANO`, capped at
  `MAX_GAS_PER_MESSAGE`).
- `docs/specification/execution.md` §3.4 amended: the "same effect on
  state" sentence now covers state rollback only; delivery outcome is
  governed by this ADR's table. `docs/specification/state-model.md` §4.1
  notes `bit_count` as derived-on-decode.
- Reference vectors: `reference/vectors/fatal_bounce.json` (kind →
  outcome table, Rust agreement test asserts exhaustiveness over the
  closed `ExceptionKind` set) and `reference/vectors/storage_stat.json`
  (cell fixtures → `(cell_count, byte_count, bit_count)`).

## Flagged for the Wave 4 audit

1. Full-DAG vs root-cell storage accounting (see Scope note).
2. Whether `AbsentNode` should be fatal (host data gap vs sender fault).
3. Whether the fatal credit should skip accounts whose code just
   failed (it doesn't — plain-transfer equivalence is the point).
