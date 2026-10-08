# ADR-0029 — Panic Containment and the Migration Startup Invariant

**Status:** Accepted (2026-10-08). Implemented: per-message `catch_unwind`
around the producer's dry-run (`crates/node/onxd/src/producer.rs`), no
containment on `commit_block`/`apply_block`, and the startup DAG check
`verify_contract_cell_dag_completeness` with `decode_contract_dags_persisted`
(`crates/protocol/onx-storage/src/store.rs`).
**Date:** 2026-10-06

## Context

Two findings from the 2026-10-06 working-session review of the live codebase threaten chain safety from opposite directions:

1. **The v2→v3 storage migration only bumps the schema version** (`ChainStore::migrate_v2_to_v3`). A database migrated from v2 has an EMPTY `contract_cells` table while a database replayed from genesis has full cell DAGs. The first `LDREF` into a child cell then DIVERGES: the migrated node fails closed (`AbsentNode`) and bounces the delivery, the replayed node executes it. Different receipts, different state roots, no error anywhere — a silent consensus split. This applies to the live machine now.

2. **The producer has no panic containment.** A panic during the dry-run inside `propose_block` kills `onxd`; systemd restarts it; the poison message is still in the drop dir; the process dies again — a restart loop that halts the chain. Conversely, a panic during real block application (`apply_block`) must NOT be treated as anything other than what it is: a broken node that must stop.

Both findings share one root confusion the codebase must never have again: **local node faults are not consensus verdicts.** A bounce, an "invalid block", and a node that refuses to run are three different things, and mapping one to another is how silent divergence happens.

## Reference

- `docs/specification/state-model.md` §4–§5: cell structure, canonical encoding.
- ADR-0004 (bounce semantics): a bounce is a deterministic delivery outcome — value returned minus fees — recorded in receipts. It is never an error path for broken execution.
- ADR-0014 (execution model) and ADR-0024 §Decision 6: only `CTOS` raises `AbsentNode`; `LDREF` on an absent child fails closed instead of inventing data. The STF maps that to `None` (bounce) in `try_execute_contract`.
- `crates/protocol/onx-state-model/src/boc.rs`: `BagOfCells::from_bytes` (proof profile) vs `from_bytes_strict` (persisted profile).

## Decision

### 1. Panic-containment policy: three outcomes, never confused

| Where the panic happens | Policy |
|---|---|
| **Dry-run proposal** (`propose_block` inside onxd's producer) | **Local event.** Caught per message with `catch_unwind`: the panicking message is isolated by prefix bisection, dropped loudly (message hash, sender, nonce, panic payload, stack trace via a panic hook), moved to the mempool's rejected dir, and block production continues. This breaks the systemd restart loop: the poison message is gone instead of killing every tick. |
| **Real block application** (`apply_block`, via `ChainStore::commit_block`) | **Halt the node.** Deliberately NOT wrapped in `catch_unwind`. The panic unwinds through `commit_block` — aborting the not-yet-opened write transaction, so nothing partial persists — and the process dies. |
| **Mapping** | A panic is **never** "invalid block", **never** a bounce, and **never** silent, on either path. |

Rationale for the asymmetry: a dry-run panic is *input-dependent* — one hostile message can crash the interpreter while the node itself is fine, so dropping the message is correct and keeps a one-node chain alive. An `apply_block` panic is *node-dependent* — the node's own execution is broken (the block it built, or the state it loaded, triggers it), so continuing to produce would risk silent divergence from honest nodes. Halting is the only safe outcome.

The dry-run wrapper takes `&State` and the STF clones internally, so a caught panic cannot leave the caller's state half-mutated. Panic isolation assumes the dry-run is deterministic (it is: same inputs, same result); a non-deterministic panic is an STF bug and is logged loudly rather than silently absorbed.

### 2. Migration startup invariant: refuse to start beats silent divergence

On every `ChainStore::open` (after any migration), the node verifies that **every committed code/data root present in state has a COMPLETE DAG in `contract_cells`**:

- for each `Active` account with a code or data root, a `contract_cells` entry must exist;
- it must decode under the persisted (strict) BoC profile (§3);
- it must be rooted at exactly the account's committed root hash.

Anything less is node-fatal `StorageError::IncompleteContractCellDag`, naming the account and the offending root hash and instructing the operator to replay from genesis or restore from a backup.

Why fatal instead of a warning or a lazy backfill: a warning would be ignored until the divergence already happened, and there is nothing to backfill *from* — the v2 database never had the DAG content. The only honest options are refusing to start or replaying from genesis; the invariant forces the choice up front, where it is visible, instead of at the first `LDREF`, where it is a consensus split. Silent divergence is strictly worse than refusing to start.

`init_genesis` persists single-root DAGs for genesis-installed contracts (the account record embeds the root cells), so a fresh database satisfies the invariant. A genesis code cell *with* child references has no child content anywhere — such a genesis fails the invariant loudly at first startup instead of running an unexecutable contract.

### 3. Two BoC profiles, documented in code

- **Proof profile** (`BagOfCells::from_bytes`, shared with `MerkleProof::from_bytes`): dangling references are *allowed* — a Merkle proof legitimately commits sibling subtree hashes without including the sibling cells. Unchanged by this ADR.
- **Persisted profile** (new: `decode_contract_dags_persisted` in `onx-storage`, used by `load_state` and the startup invariant): every reference must resolve to a cell in the bag. A dangling ref in persisted content is a local fault, failed loudly — because the alternative is the §2 divergence.

The profiles live in different code paths precisely so a future reader cannot "simplify" them into one.

## Consequences

### Positive

- The v2→v3 migration gap can no longer become a silent consensus split: any affected node fails at startup with a named root hash and a recovery instruction.
- A poison message can no longer halt the chain via the systemd restart loop; the producer drops it as a local event and keeps producing.
- A broken-execution panic can no longer masquerade as "invalid block" or a bounce: the node halts loudly instead of diverging.

### Negative / costs

- `ChainStore::open` walks all accounts on every startup (O(accounts)). Acceptable at current scale; revisit if account count grows large.
- A genesis that installs a contract whose code cell has child references is now unstartable (its DAG content exists nowhere). This is fail-closed by design, but it constrains future genesis tooling: such contracts need their full DAGs expressible in the genesis document first.
- `#[cfg(test)]` panic-injection hooks exist in `onxd` (dry-run) and `onx-storage` (commit path). They are test-only, minimal, and documented; they must never grow into production behavior.

## Alternatives Considered

### A. Backfill missing DAGs at startup by re-executing history

Rejected. Re-execution *is* replay from genesis — there is no cheaper source of the missing content, and a half-backfill would be a new consensus-critical code path. The invariant names replay as the recovery instead of attempting it.

### B. Contain `apply_block` panics the same way as dry-run panics

Rejected. The dry-run operates on a scratch copy the node can discard; `apply_block` operates on the node's real state. A panic there evidences broken execution, not bad input — containing it would let a broken node keep producing blocks its peers would reject or diverge from.

### C. Map a dry-run panic to "invalid block" and skip the tick

Rejected. That keeps the poison message in the drop dir and reproduces the exact systemd restart loop this ADR eliminates. The message must be removed, loudly, as a local event.
