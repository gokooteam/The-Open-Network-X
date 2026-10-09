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

## [0.2.1] - 2026-10-09

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

[Unreleased]: https://github.com/gokooteam/The-Open-Network-X/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/gokooteam/The-Open-Network-X/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/gokooteam/The-Open-Network-X/releases/tag/v0.2.0
