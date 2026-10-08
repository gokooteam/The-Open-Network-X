# ADR-0040 — Genesis carries contract cell DAGs (Wave 4)

**Status:** Proposed (2026-10-08)
**Decider:** Gokoo (design authority between audits; flagged for Claude's Wave 4 audit)
**Amends:** ADR-0029 (genesis-time `contract_cells` seeding); the genesis document layout in `crates/protocol/onx-state-model/src/genesis.rs`

## Context

Since ADR-0039, `run()` resolves every child of the root code cell into
`code_refs` before the first instruction and fails with `AbsentNode` if one
is missing. The STF seeds the interpreter's cell store from the account's
`contract_cells` entry, and that entry was only ever written after a
successful run.

Genesis carried root cells only. `GenesisDocument::state_tree()` set no
`contract_cells`, and `ChainStore::init_genesis` wrote single-root bags. A
genesis contract whose code cell has children therefore had no child
content anywhere: every call failed with `AbsentNode` and bounced, and
because nothing ever ran successfully, no later call could repair it. The
same held for a data root with children and `LDREF`.

Two fixes were on the table: seed the code DAG at genesis, or resolve a
child only when `JMPREF`/`CALLREF` uses it. Lazy resolution alone does not
fix the bug: the child content still exists nowhere, so the first `CALLREF`
still fails. Only seeding makes such a contract callable.

## Decision

### 1. The genesis document carries complete DAGs

`GenesisDocument` gains `contract_cells: BTreeMap<AccountId, ContractCellDags>`.
The canonical rule:

- An entry exists **iff** the account is a contract (`Active`, `code` set)
  whose code root or data root (an empty cell when `data` is absent, the
  STF's calling convention) has at least one child reference.
- Each entry is rooted at exactly the account's committed roots, complete
  (every reference reachable from the root resolves), and holds no
  unreachable cell.

Anything else rejects the whole document. A genesis contract that could
never execute is refused at the trust root instead of bouncing forever.

### 2. Version 2 only when needed

The document is encoded as version 2 (a DAG section after the accounts:
`dag_count u32`, then per entry in ascending `AccountId` order
`account_id || dags_len u32 || ContractCellDags bytes`) **iff** at least one
entry exists. Otherwise it is byte-identical to version 1, so every existing
genesis (and chain ID) is unchanged. A version-2 document with an empty
DAG section is rejected as non-canonical.

### 3. One source for genesis DAGs

`state_tree()` sets `contract_cells` for every genesis contract: the
document's DAGs where present, single-root bags otherwise. `init_genesis`
persists exactly `tree.all_contract_cells()`. The in-memory STF state
(`State::from_genesis`) and the stored state now agree, and the ADR-0029
startup invariant holds on a fresh database with such a contract.

### 4. Config: `child_cells_hex`

`onx-genesis` balances gain `child_cells_hex`: canonical cell bytes of every
child cell, in any order. The builder collects each root's DAG from that
pool and rejects a reachable child missing from the list and a listed cell
neither root reaches.

## Consequences

- A genesis-deployed contract can `CALLREF` on its first call. Evidence:
  `crates/tooling/onx/tests/genesis_callref.rs` (in memory via
  `State::from_genesis`, and through `init_genesis` → reopen →
  `load_state` → `commit_block`). With the seeding removed, the same test's
  first call bounces.
- No `protocol_version` bump: block validity and execution rules are
  unchanged; only the genesis format grows, and only for documents that
  could not run before.
- Lazy child resolution in the interpreter stays out of scope. ADR-0039's
  fail-fast at `run()` is still correct once the DAG is seeded.
