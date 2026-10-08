# ADR-0039 — Live code_refs + call-stack depth limit (Wave 4, step 7)

**Status:** Proposed (2026-10-08)
**Decider:** Gokoo (design authority between audits; flagged for Claude's Wave 4 audit)
**Amends:** `docs/specification/tvm-instruction-set.md` §4.5; `docs/specification/execution.md` §3.4

## Context

Claude's 2026-10-06 review set this step's shape before any of Wave 4 was
built:

> JMPREF/CALLREF sequencing trap — populating code_refs makes recursion
> live, so per-message/per-block gas caps must land BEFORE or WITH that fix.
> ... code_refs adds unbounded MEMORY (call_stack heap clones ~128B/frame
> at 4 gas; ~0.001 Onyx ≈ 250M frames → OOM abort, uncatchable). code_refs
> needs per-message gas cap AND explicit call-stack depth limit (~256) IN
> THE SAME CHANGE.

The gas caps (ADR-0034, step 2) are in place: 10M gas per message. At 4 gas
per `CALLREF` that bounds recursion at 2.5M frames via gas alone — roughly
320MB of cloned `Cell`s per message. Bounded, but absurd: a single hostile
message could force a validator to allocate a third of a gigabyte of call
stack before the gas meter trips. Gas prices *time*; it does not price
*memory* at anything like its true cost here (4 gas ≈ a few integer ops,
but each frame clones a heap `Cell`).

Meanwhile `Interpreter.code_refs` was initialized empty in `new()` and never
populated in production — only tests pushed into it manually. So
`JMPREF`/`CALLREF`/`IFJMPREF`/`IFNOTJMPREF`/`IFCALLREF`/`IFNOTCALLREF` always
raised `MalformedCell` (out of range) on real code, while the spec
(`tvm-instruction-set.md` §4.5) already described them as addressing "the
code cell's up-to-4 child references as code continuations" and programs
"split across a tree of code Cells". The spec was aspirational; the wiring
is this step.

## Decision

### 1. `code_refs` mirrors the current code cell's resolved children

New method `refresh_code_refs(&mut self) -> Result<(), ExceptionKind>`:

```rust
fn refresh_code_refs(&mut self) -> Result<(), ExceptionKind> {
    let mut refs = Vec::with_capacity(self.current_code.cell_refs().len());
    for hash in self.current_code.cell_refs() {
        match self.cell_store.get(hash) {
            Some(cell) => refs.push(cell.clone()),
            None => return Err(ExceptionKind::AbsentNode),
        }
    }
    self.code_refs = refs;
    Ok(())
}
```

Resolution rules, matching the existing `LDREF`/`CTOS` contract:

- Children resolve through `cell_store` **in index order** — `ref_index`
  N addresses the Nth child reference, exactly as the spec's opcode table
  already states.
- A child hash absent from the store fails closed with `AbsentNode` —
  never invented data, same rule as `LDREF` (Wave 2). The STF seeds the
  full persisted code DAG into the store before `run()`, so in production
  a missing child means the DAG wasn't persisted: a host bug, fail fast.

Refresh points (single choke point `set_code`, used by every code switch):

- At the start of `run()` — fail fast on an unseeded code DAG instead of
  mid-execution. (Behavior change: previously such a program failed at
  the first `JMPREF` with `MalformedCell`; now it fails at entry with
  `AbsentNode`. Earlier, same fail-closed family.)
- After every `JMPREF`/`CALLREF`/conditional-jump target switch.
- After `RET` restores a `(code, pc)` pair, and after c0/c1/c2
  continuation jumps — a continuation's code cell has its own children.

**Design call — refresh-on-jump, not fixed-to-root.** An alternative would
populate `code_refs` once from the *root* code cell and never refresh.
That strands grandchildren: from a child cell you could address the
root's children (siblings) but never your own children, contradicting the
spec's "split across a tree of code Cells". Per-cell refresh makes the
full tree navigable — from any code cell, its own up-to-4 children are
addressable — which is also TON's semantics for code references.

### 2. Explicit call-stack depth limit: `MAX_CALL_STACK_DEPTH = 256`

`CALLREF`/`IFCALLREF`/`IFNOTCALLREF` (when the branch is taken) raise the
new `ExceptionKind::CallStackOverflow` if `call_stack.len() >= 256` before
pushing the return frame.

Why 256: 256 frames × ~128B ≈ 32KB worst case — three orders of magnitude
under the gas-only bound, deterministic, and far above any legitimate use
(real contract call nesting is single digits; TON's own limit is 255, so
contracts ported from TON semantics fit). The value is pinned in
`reference/vectors/code_refs.json` and asserted by the Rust agreement
test, so it cannot drift silently.

Check order inside the taken-branch path: gas charge → `ref_idx` bounds
(existing `MalformedCell` precedence preserved) → depth check → push.
The order is deterministic either way; bounds-first keeps the existing
error precedence for programs that are both out-of-range and over-depth.

### 3. The new kind bounces — it is not gas exhaustion

`is_fatal_exception` maps `CallStackOverflow` to **bounce** (explicit match
arm, no wildcard — the ADR-0037 tripwire fires at compile time as
designed). Rationale: this is a *structural* limit, sibling to the 1023
operand-stack cap (which raises `MalformedCell`, also a bouncer), not a
spent budget. A contract that recurses too deep gets its value bounced
like any other VM-raised failure; only a fully spent gas budget is fatal.

The c2 exception-handler discriminator for the new kind is `5` (next free
after `TypeMismatch = 4`).

### 4. Closed-set updates

`ExceptionKind` gains `CallStackOverflow`: `Display`, the
`is_fatal_exception` explicit match, the c2 discriminator match, the
Python oracle (`gen_fatal_bounce_vectors.py` — the fatal-vs-bounce *rule*
is unchanged, only the closed set grows by one bouncing kind), and the
Rust agreement test (`kind_name`/`all_kinds`, 5 → 6 — the "sixth kind"
its docstring anticipated).

## Consequences

- `JMPREF`/`CALLREF` work in production for the first time. No live-chain
  impact: devnet-1 has no deployed contracts, and previously these
  opcodes unconditionally failed.
- Worst-case call-stack memory per message drops from ~320MB (gas-only) to
  ~32KB (depth limit) — the OOM abort Claude identified is closed.
- The operand-stack flood test is rewritten to use real mutually-recursive
  cells instead of manually pushing `code_refs` (manual pushes are now
  moot: `run()` refreshes from the code cell).
- No `protocol_version` bump (flagged for the Wave 4 audit, same as
  ADR-0036/ADR-0038): the depth limit is a pure tightening — every
  program valid before is valid now, with identical results.

## Alternatives considered

- **Gas-only bounding (spec's old position: "no explicit call-stack depth
  limit ... resource exhaustion enforced uniformly through gas").**
  Rejected per Claude's analysis: gas prices time, not the ~128B/frame
  heap allocation; 4 gas/frame lets a hostile message allocate hundreds
  of megabytes before the meter trips. The spec prose is amended.
- **Depth limit without populating code_refs.** Meaningless — with empty
  `code_refs`, `CALLREF` can never succeed, so there is no recursion to
  bound. The two land together, as Claude required.
- **Smaller limit (e.g. 64).** 256 matches TON's 255 closely enough for
  ported contract shapes while staying tiny in absolute memory.
