# ONX Changelog & Development Roadmap

This document is the per-PR narrative of what has been built. It follows the
build-incrementally sequence from `INSTRUCTIONS.md` §23 and the
specification sequence from `docs/specification/architecture.md`.

What comes next, in order and with checkable exit criteria, lives in
[`MILESTONES.md`](MILESTONES.md), including the gate that says when to stop
developing and start maintaining. Release notes per version live in
[`CHANGELOG.md`](CHANGELOG.md). The original breakdown of 20 development
tasks is in [`docs/planning/development-tasks.md`](docs/planning/development-tasks.md);
`MILESTONES.md` records where each of them stands today.

ADR numbers below are the current ones. Entries dated 2026-09-10 originally
used the retired `docs/decisions/` numbering; see
[`docs/decisions/README.md`](docs/decisions/README.md) for the mapping.

## Changelog

### 2026-10-08

- **PR #30 — CI red on `main`:** fixed the three workflows failing on
  `main` (`Fuzz` never built a target, `OpenSSF Scorecard` lacked
  `id-token: write`, `Workflow Security` actionlint findings).
- **PR #29 — `LDREF`/`AbsentNode` settled:** the spec now matches the VM. A
  pruned child passes through; an unresolved child fails closed.
- **PR #28 — Fuzz in CI:** every `fuzz/` target runs for 60 s on each PR and
  push to `main`.
- **PR #27 — O(1) state copy:** the trie is the only account store, so
  `ShardStateTree::clone()` is O(1) (`state-model.md` §7.1).
- **PR #26 — `onx-cli transfer` is real:** it signs an `ONX_MSG_EXT_V1`
  external message that `onxd` accepts; the placeholder `wallet balance`
  and `deploy-contract` commands were removed.
- **PRs #24, #25 — `MILESTONES.md`:** milestones M0–M3 recorded, exit
  criteria for M4–M7, and the maintenance gate.
- **PR #23 — Versioning (ADR-0040):** one workspace version, SemVer 2.0.0,
  `CHANGELOG.md`, release automation, and the `v0.2.0` tag.
- **PR #22 — Sentry follow-ups** for `onxd` crash reporting.

### 2026-10-07

- **PRs #20, #21 — CI and PR tooling:** PR audit review, TruffleHog, zizmor
  and actionlint, dependency review, cargo-machete/taplo/typos,
  cargo-audit, OpenSSF Scorecard, labelers, and Codecov.
- **PR #19 — Opt-in Sentry crash reporting for `onxd`** via `SENTRY_DSN`,
  with redaction and rate limits.
- **PR #18** merged upstream changes from the `quickerup` fork.
- **PR #17 — Explorer on devnet-1:** the explorer's testnet entry points at
  devnet-1's chain ID and `data.on-x-scan.com/devnet-1/`.
- **PR #15 — Devnet faucet account** in `config/genesis.toml`.
- **PR #14 — Header `protocol_version` and `block_time`,** plus a block-1
  acceptance test that runs the real producer loop and the real
  `onx replay` binary.
- **PRs #12, #13 — `ONXBLK05` authenticated headers (ADR-0032):** a
  signature section in every block, storage schema v4, producer signing, a
  startup check of the signing key, explorer decoding, and fixes for the
  five NO-GO findings from the audit.
- **PR #11 — Hotfix (ADR-0031):** fail-closed integer conversion and label
  fixes on top of #10.
- **Currency rename (ADR-0033, landed with #13):** Onyx → Onyxi, ticker
  ONXI. Docs only; no consensus bytes changed.

### 2026-10-06

- **PR #10 — Strict genesis validator-key validation,** and floored
  `DIVMOD` (ADR-0030).
- **PR #9 — Static block explorer** committed as `explorer/index.html`.
- **PR #8 — Wave 3 (ADR-0028, ADR-0029):** checked VM arithmetic, a startup
  invariant that every stored contract cell DAG is complete, and panic
  containment in the producer.
- **PR #7 — Wave 2:** `LDREF` returns the real stored child cells;
  `MSGSENDER`/`MSGVALUE`/`MSGBODY`; incremental state roots.
- **PR #6 — Wave 1:** mempool DoS limits, strict Bag-of-Cells decoding,
  atomic block-file writes, the two ADR series unified under `docs/adr/`,
  and the independent Python reference implementation in `reference/`.

### 2026-10-05

- **PR #5 — Message-based single-shard chain (actor model):** Replaced the
  synchronous transfer dispatch with external/internal messages per
  `docs/specification/transactions.md`. New `docs/adr/0001`–`0007` record
  the design: wallet handler (chain-ID/key/nonce/signature checks),
  FIFO per-pair delivery, bounce with refund-minus-fees, chain-ID-bound
  signatures (`ONX_MSG_EXT_V1`), key-derived addresses (`ONX_ADDR_V1`),
  and redelivery rejection. Block magic `ONXBLK04`; vectors refrozen via
  independent Python derivation. V1/V2/V3 transaction encodings retired
  (pre-release, never shipped).
- **PR #4 — TVM on the spine:** Real single-node `onxd` block-production
  loop (spool-directory mempool, demand-based blocks, no wall-clock in
  blocks) and TVM integration (determinism audit clean, gas metering,
  `SETDATA` opcode, contract accounts with code/data cells). Eight
  hand-derived golden vectors computed independently in Python; all
  matched. Trie encoding, domain-tag registry, and `Transaction::hash`
  preimage pinned in the spec.
- **PR #3 — CI green-up:** Fixed a real crash-recovery bug (kill -9 during
  first-time redb init left a half-written database); `ChainStore::open`
  now builds at a temp path and renames into place. Bumped rustls to
  0.23.45 for RUSTSEC-2026-0285.
- **PR #2 — Transaction authorization and hardening:** Stored-head pinning,
  root verification on resume, Ed25519 authorization with per-account
  nonces (`ONX_TX_V2`), schema v2, removed write-only cells table.
- **PR #1 — Deterministic replay:** Real genesis, pure STF, atomic redb
  storage, `onx replay` CLI, crash recovery, byte-identical roots across
  OS processes, fail-closed rejection of bad blocks.

### 2026-09-10

- **ADR-0023 Applied:** Updated `crates/protocol/onx-data-structures` `BlockHeader` layout to 242 bytes with `prev_ref_hash_2` and `MERGE_RESULT` bit flag (`0x0010`) consistency checks. Updated `crates/protocol/onx-blocks` with merge-block successor structural validation logic.
- **Implemented `onx-execution` crate:** Implemented `docs/specification/execution.md` + `docs/specification/tvm-instruction-set.md` (ADR-0024) in code (`crates/protocol/onx-execution`). Includes 46 TVM opcodes, stack value model, deterministic gas metering, and pruned cell `AbsentNode` exception handling.
- **Implemented `onx-payment-channels` crate:** Implemented `docs/specification/payment-channels.md` (ADR-0021) in code (`crates/protocol/onx-payment-channels`), including off-chain state updates, cooperative settlement, uncooperative dispute challenge resolution, and Merkle-proof light-client verification using `onx-execution`'s `AbsentNode` primitive.
- **Implemented `onx-consensus` crate:** Implemented `docs/specification/consensus.md` (ADR-0015) in code (`crates/protocol/onx-consensus`), including validator election, candidate actual stake calculation, 2/3 BFT quorum voting tracker, late-signature reward decay, and 2-month challenge window absolute finality evaluation.
- **Implemented `onx-networking` crate:** Implemented `networking-adnl.md`, `networking-dht.md`, and `networking-overlay.md` (ADR-0016–0018) in code (`crates/node/onx-networking`), including ADNL peer identity, key descriptions, abstract address derivation, channel ID calculation, Kademlia XOR distance metric, signed DHT records, and overlay structures.
- **Implemented `onx-sharding` crate:** Implemented `docs/specification/sharding.md` (ADR-0019) in code (`crates/protocol/onx-sharding`), including binary shard tree invariants, leaf splitting, and load-based 75% split and 20% merge trigger evaluations.
- **ADR-0025 Accepted & Applied:** Resolved **ONX-ARCH-011** (hypercube fast-path adoption triggers) and **ONX-ARCH-012** (cross-workchain exchange rates and message queue expiration bounds).
- **ADR-0026 Accepted & Implemented `onx-economics` crate:** Resolved **ONX-ARCH-008** and implemented `docs/specification/economics.md` (ADR-0020) in code (`crates/protocol/onx-economics`), including 5 billion Onyxi supply cap, storage fee accrual calculation, 50% transaction fee burn split, and annual validator inflation reward distribution.
