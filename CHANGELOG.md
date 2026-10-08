# Changelog

All notable changes to the ONX workspace are recorded here.

The format follows [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/)
and the project uses [Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html)
as described in [ADR-0034](docs/adr/0034-versioning-standard.md). One version
covers every crate in the workspace. Protocol wire-format versions (block
magic, `PROTOCOL_VERSION`, storage `SCHEMA_VERSION`, domain tags) are tracked
separately and listed under each release when they change.

Add entries under **Unreleased** in the same PR as the change, using the
headings Added, Changed, Deprecated, Removed, Fixed, Security. The
`Version Bump` workflow moves them under a numbered heading at release time.
`ROADMAP.md` keeps the narrative, per-PR history.

## [Unreleased]

### Added

- `MILESTONES.md`: completed milestones M0–M3, exit criteria for M4–M7,
  and the maintenance gate that defines `1.0.0`.
- `onx-cli wallet address --wallet <file>`: print a wallet's public key and
  key-derived address (`ONX_ADDR_V1`).
- `Fuzz` CI workflow (`.github/workflows/fuzz.yml`): every PR and push to
  `main` runs each `fuzz/` target (`boc_parser`, `tvm_execution`,
  `block_header`) for 60 seconds on a pinned nightly and uploads crash
  inputs as artifacts.

### Changed

- `ShardStateTree` stores account records in its Merkle trie leaves instead
  of a separate `BTreeMap`, so `clone()` is O(1) and the STF no longer
  deep-copies every account on each `propose_block` and `apply_block`. At
  100k accounts this cut per-block STF time about 10× in local measurement
  (see `docs/specification/state-model.md` §7.1). State roots and the cell
  layout are unchanged. `ShardStateTree::accounts()` now returns an
  ascending-order iterator (`onx_state_model::Accounts`) instead of
  `&BTreeMap`; `len()` and `is_empty()` were added.
- `onx-cli transfer` now builds a real, signed `ONX_MSG_EXT_V1` external
  message that `onxd` accepts. It takes the chain ID from `--genesis` or
  `--chain-id`, the key from `--wallet` or `--seed-file` (never from argv),
  and an explicit `--nonce`; `--reveal-key` covers the first spend from a
  key-derived account. With `--out <pool dir>` it writes `<hash>.msg`
  atomically (temp file + rename); otherwise it prints the wire bytes as hex.
  Amounts and fees are in nano-Onyxi.
- `onx-cli wallet create` takes `--out`, prints the key-derived address,
  writes `wallet.json` with mode 0600 on Unix, and refuses to overwrite an
  existing wallet.
- `onx-cli --version` reports the workspace version instead of a hardcoded
  `0.1.0`.
- `tvm-instruction-set.md` §3.5.3 (and the `LDREF` row, §5 and §6),
  `execution.md` §3.4 and ADR-0024: `LDREF` raises `AbsentNode` on an
  unresolved child (no `Cell` held for its hash) and never on a pruned
  child. This matches what the VM already did; the spec previously said
  `LDREF` never raises `AbsentNode`. No behavior change.

### Removed

- `onx-cli wallet balance` and `onx-cli deploy-contract`. Both printed
  placeholder output that looked like a result; there is no RPC to answer a
  balance query and no deploy message type yet.

## [0.2.0] - 2026-10-08

First tagged release. Rolls up everything merged since the 0.1.0 protocol
library baseline (PRs #1–#21). Pre-1.0: no API, wire-format, or storage
compatibility is promised between minor versions.

**Protocol versions in this release:** block magic `ONXBLK05`,
`PROTOCOL_VERSION = 1`, storage `SCHEMA_VERSION = 4`, external message
signatures `ONX_MSG_EXT_V1`, addresses `ONX_ADDR_V1`.

### Added

- Deterministic replay: real genesis, pure state transition function, atomic
  redb storage with crash recovery, and the `onx replay` command (#1).
- Ed25519 transaction authorization with per-account nonces (#2).
- Single-node `onxd` block production loop and TVM integration with gas
  metering, `SETDATA`, and contract accounts (#4).
- Message-based (actor model) execution: external/internal messages, FIFO
  delivery, bounces, chain-ID-bound signatures, key-derived addresses (#5).
- Authenticated block headers, `ONXBLK05` (ADR-0032) (#12, #13), with
  `protocol_version` and `block_time` header fields (#14).
- Devnet faucet account in genesis (#15) and the in-repo block explorer
  pointed at devnet-1 (#9, #17).
- Opt-in Sentry crash reporting for `onxd` via `SENTRY_DSN` (#19).
- Workspace-wide versioning policy, `CHANGELOG.md`, and release automation
  (ADR-0034).

### Changed

- Currency renamed Onyx → Onyxi, ticker ONXI (ADR-0033). Docs only; no
  consensus bytes changed.
- Every crate now inherits one workspace version (`version.workspace = true`).

### Removed

- V1/V2/V3 transaction encodings, superseded by the message model (#5).

### Fixed

- Crash during first-time redb initialisation leaving a half-written
  database (#3).
- Checked VM arithmetic, migration invariant, and panic containment (#8,
  ADR-0028/0029); floored DIVMOD correction (ADR-0030); fail-closed
  arithmetic and strict validator key validation (#10, #11).
- `onxd` Sentry audit findings: panic stall, guard-drop hang, fatal exits (#19).

### Security

- rustls bumped to 0.23.45 for RUSTSEC-2026-0285 (#3).
- Mempool DoS limits and strict Bag-of-Cells decoding (#6).

## [0.1.0] - 2026-09-10

Untagged baseline: protocol library crates (primitives, data structures,
state model, transactions, blocks, execution/TVM, payment channels,
consensus, sharding, economics, networking). See `ROADMAP.md` for detail.

[Unreleased]: https://github.com/gokooteam/The-Open-Network-X/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/gokooteam/The-Open-Network-X/releases/tag/v0.2.0
