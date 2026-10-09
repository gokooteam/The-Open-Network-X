# Open Network X

[![CI](https://github.com/gokooteam/The-Open-Network-X/actions/workflows/ci.yml/badge.svg)](https://github.com/gokooteam/The-Open-Network-X/actions/workflows/ci.yml)
[![CodeQL](https://github.com/gokooteam/The-Open-Network-X/actions/workflows/codeql.yml/badge.svg)](https://github.com/gokooteam/The-Open-Network-X/actions/workflows/codeql.yml)
[![Code Coverage](https://github.com/gokooteam/The-Open-Network-X/actions/workflows/coverage.yml/badge.svg)](https://github.com/gokooteam/The-Open-Network-X/actions/workflows/coverage.yml)
[![Cargo Deny](https://github.com/gokooteam/The-Open-Network-X/actions/workflows/deny.yml/badge.svg)](https://github.com/gokooteam/The-Open-Network-X/actions/workflows/deny.yml)

[![License: Apache-2.0](https://img.shields.io/github/license/gokooteam/The-Open-Network-X)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.98.1-orange?logo=rust&logoColor=white)](rust-toolchain.toml)
[![Last Commit](https://img.shields.io/github/last-commit/gokooteam/The-Open-Network-X)](https://github.com/gokooteam/The-Open-Network-X/commits/main)
[![Open Issues](https://img.shields.io/github/issues/gokooteam/The-Open-Network-X)](https://github.com/gokooteam/The-Open-Network-X/issues)
[![Stars](https://img.shields.io/github/stars/gokooteam/The-Open-Network-X?style=social)](https://github.com/gokooteam/The-Open-Network-X/stargazers)

[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](CONTRIBUTING.md)
[![Status](https://img.shields.io/badge/status-early--research-orange)](#project-status)

**Open Network X (ONX)** is an independent blockchain implementation project inspired by the architecture and technical vision described in the original The Open Network (TON) white paper.

ONX explores, reconstructs, and implements that vision independently and from first principles. It is not the TON blockchain, is not an official continuation of TON, and is not intended to replace the existing TON network or its community. The native currency of Open Network X is **Onyxi** (ticker: **ONXI**).

## Contents

- [What is Open Network X?](#what-is-open-network-x)
- [Independence](#independence)
- [The white paper is the starting point](#the-white-paper-is-the-starting-point)
- [Onyxi](#onyxi)
- [Project philosophy](#project-philosophy)
- [Project status](#project-status)
- [Websites](#websites)
- [Changelog and roadmap](ROADMAP.md)
- [Milestones and the maintenance gate](MILESTONES.md)
- [Development tasks](docs/planning/development-tasks.md)
- [Contributing](CONTRIBUTING.md)
- [Repository structure](#repository-structure)
- [Building and testing](#building-and-testing)
- [Long-term goal](#long-term-goal)
- [Disclaimer](#disclaimer)
- [License](#license)

## What is Open Network X?

The original TON design describes a highly scalable, decentralized blockchain architecture built around concepts including:

- Masterchains, workchains, and shardchains
- Dynamic sharding
- Asynchronous message passing
- Validator networks and Byzantine fault-tolerant consensus
- Smart contracts and a specialized virtual machine
- Distributed storage
- Scalable blockchain infrastructure

Open Network X exists to investigate what it would look like to implement that architecture independently. The guiding question is:

> *What would the Open Network look like if we went back to the original design and built an independent implementation from the specification?*

ONX is therefore not intended to be a conventional fork. We are not taking an existing implementation, changing its name, and continuing from there. Instead, the project begins with the protocol's published design and works forward toward an independent implementation.

## Independence

Open Network X is a separate project. ONX:

- Does not claim to be TON.
- Does not claim to represent the TON Foundation, TON Society, or the TON community.
- Does not attempt to replace the existing TON network.
- Does not require the existing TON network to function.
- Does not treat the current TON implementation as the authoritative specification for ONX.
- May make independent technical decisions where the original specification is ambiguous or incomplete.

Similarity between ONX and existing TON architecture is intentional where that similarity follows from the original protocol design.

## The white paper is the starting point

The original TON white paper (`WHITEPAPER.md`) is the primary historical and architectural reference for this project, and is included in this repository as a reference document. ONX does not modify the white paper to make the implementation easier — instead, the implementation adapts to the specification:

- Where the white paper is precise, ONX strives for faithful implementation.
- Where the white paper is ambiguous, ONX documents its interpretation.
- Where the white paper does not provide sufficient information, ONX explicitly identifies the missing information and documents the engineering decision that fills the gap.

## Onyxi

Onyxi is the native currency of Open Network X. The currency exists as part of the ONX protocol rather than as a separate application-layer token. The exact monetary policy, denomination system, issuance mechanism, validator economics, transaction fees, and other economic parameters will be specified as the protocol develops.

## Project philosophy

ONX follows several principles:

1. **Specification before implementation.** We begin with the protocol design, not with existing source code.
2. **Independent implementation.** Existing implementations may be studied for educational and interoperability research purposes, but ONX is intended to be independently implemented.
3. **Explicit decisions.** When the reference material does not provide an answer, the project records the decision instead of silently inventing behavior.
4. **Testable protocol behavior.** Important protocol properties should eventually have deterministic tests.
5. **No accidental compatibility.** ONX should not inherit compatibility with another network merely because doing so is convenient — compatibility must be an intentional protocol decision.
6. **Transparency.** Architectural deviations, interpretations, limitations, and known incompatibilities should be documented openly.

## Project status

<!--gen:readme_status-->
**Phase: development.** ONX is being built, not maintained. It switches to maintenance when every box in the [maintenance gate](MILESTONES.md#part-3--the-maintenance-gate-m8-100) (M8, `1.0.0`) is checked.

**Now:** M4, *Pay down debt: a single node you'd trust* (`0.3.0`): 16 of 21 exit criteria met.

```text
M0 ✅ ── M1 ✅ ── M2 ✅ ── M3 ✅ ── M4 ▶ ── M5 ○ ── M6 ○ ── M7 ○ ── M8 🏁
```

| | Milestone | Version | Progress |
| :-: | --- | --- | --- |
| ✅ | M0 · Specifications and protocol libraries | 0.1.0 baseline | done 2026-09-10 |
| ✅ | M1 · Deterministic replay | in 0.2.0 | done 2026-10-05 |
| ✅ | M2 · Single-node chain: TVM, message model, hardening | in 0.2.0 | done 2026-10-07 |
| ✅ | M3 · Authenticated blocks, devnet-1, release engineering | `v0.2.0` tag | done 2026-10-08 |
| ▶ | **M4 · Pay down debt: a single node you'd trust** | 0.3.0 | `████████░░` 16/21 |
| ○ | M5 · Networking: nodes find each other and stay in sync | 0.4.0 | `░░░░░░░░░░` 0/7 |
| ○ | M6 · Consensus: more than one validator | 0.5.0 | `░░░░░░░░░░` 0/6 |
| ○ | M7 · A public testnet other people can use | 0.6.0 – 0.9.x | `░░░░░░░░░░` 0/8 |
| 🏁 | M8 · Maintenance gate: stop developing, start maintaining | 1.0.0 | `░░░░░░░░░░` 0/14 |

Progress counts the exit-criteria checkboxes in [`MILESTONES.md`](MILESTONES.md); a box is ticked in the PR that earns it. There are no target dates. This block is generated by `scripts/site.py build`, and CI fails when it falls behind.
<!--/gen:readme_status-->

**Pre-release, `v0.2.0`.** What M0–M3 delivered: deterministic replay
(`onx replay --genesis genesis.toml --blocks ./blocks/` executes, persists,
recovers, and replays a chain to identical state roots), a single-node
producer with TVM contract calls and the message model, and authenticated
`ONXBLK05` blocks.

devnet-1 is a single-producer development chain run by the project. Its
blocks are published at `data.on-x-scan.com/devnet-1/` and shown by the
[explorer](#websites). It is not a public testnet: there is one producer,
no peers, and no consensus.

Everything below is graded honestly — no "done" claims the code hasn't
earned.

| Component | Spec | Logic | Integrated | Adversarially tested |
| --- | --- | --- | --- | --- |
| `onx replay` command | ✅ plan | ✅ | ✅ | ✅ kill -9, corruption, two-process, golden vectors |
| Canonical encodings (BoC) | ✅ | ✅ | ✅ | ✅ 50× probe, cross-process byte-identical |
| Genesis (real accounts, chain ID) | ✅ | ✅ | ✅ | ✅ cross-process determinism |
| STF — message execution (wallet handler + delivery) | ✅ | ✅ | ✅ | ✅ bounce, redelivery, ordering, kill-9, two-process |
| Merkle proofs | ✅ | ✅ | ✅ | ✅ fabricated/absent-key proofs rejected |
| Atomic storage + crash recovery | ✅ | ✅ | ✅ | ✅ 100× kill -9, full-or-nothing |
| `onxd` block-production loop | ✅ | ✅ | ✅ | ✅ spool mempool, demand blocks, replay-to-identical-roots |
| VM / TVM execution | ✅ | ✅ | ✅ | ⚠️ partial — `LDREF` child-cell bug fixed (#7, `vm_child_cells.rs`), checked arithmetic (#8), `tvm_execution` fuzzed in CI, `JMPREF`/`CALLREF` work from real contracts (#32, `callref_stf.rs`); open: nested calls after an implicit return, padding bits of bit-granular code cells (M4) |
| Consensus | ✅ | ❌ scaffold | ❌ | ❌ — frozen |
| Networking (ADNL/DHT) | ✅ | ❌ scaffold | ❌ | ❌ — frozen |
| Sharding | ✅ | ❌ | ❌ | ❌ — frozen |
| Payment channels | ✅ | ❌ | ❌ | ❌ — frozen |
| RPC / telemetry | ✅ | partial | ❌ | ❌ — frozen |

"Adversarially tested" means a probe tried to break it — torn writes,
fabricated proofs, corrupted files, killed processes — and it held. A green
test suite once coexisted with all four original bugs; the probes are the
point, not the suite.


See [`CHANGELOG.md`](CHANGELOG.md) for release notes, [`ROADMAP.md`](ROADMAP.md) for the per-PR history and roadmap of what has been built, [`MILESTONES.md`](MILESTONES.md) for the milestones still ahead and the criteria for switching from development to maintenance, and [`CONTRIBUTING.md`](CONTRIBUTING.md) for the workflow every contribution is expected to follow.

## Websites

| Site | What it is | Source in this repo |
| --- | --- | --- |
| [on-x.live](https://on-x.live/) | Project site: what ONX is, how it is built, and its current status | [`site/`](site/) |
| [on-x-scan.com](https://on-x-scan.com/) | Block explorer for devnet-1. Verifies every block in the browser against the pinned genesis hash | [`explorer/`](explorer/) |

Both sites are single static HTML files deployed from this repository.
Facts on on-x.live that come from the repository (version, counts, ADR and
specification lists, the build it describes) are generated by
`scripts/site.py`, and CI fails when they drift from the repository. A
scheduled workflow compares what each domain actually serves with `main`.
See [`site/README.md`](site/README.md).

## Repository structure

```
.
├── .github/                 # CI, dependency updates, and repository automation
├── config/
│   └── genesis.toml         # Default genesis configuration
├── contracts/system/        # Masterchain contract adapters and TVM fixtures
├── crates/
│   ├── protocol/            # Consensus-critical protocol crates
│   ├── node/                # Runtime, networking, RPC, and telemetry crates
│   └── tooling/             # CLI and genesis-generation crates
├── docs/
│   ├── adr/                 # Architecture decision records (one series, 0001–)
│   ├── decisions/           # Retired ADR location; README maps old numbers
│   ├── specification/       # ONX protocol specifications
│   ├── planning/            # Research logbook and development tasks
│   ├── guides/              # Operational and local-network guides
│   ├── generated/           # Archived log from the retired spec-tracker workflow
│   └── reference/           # One-off reference and audit notes
├── explorer/                # Block explorer served at on-x-scan.com
├── fuzz/                    # cargo-fuzz targets
├── reference/               # Independent Python reference implementation and golden vectors
├── scripts/                 # Repository checks and maintenance tools
├── site/                    # Project site served at on-x.live
├── tests/simulation/        # Python model of a multi-node network (not real binaries)
├── Cargo.toml               # Rust workspace
├── Cargo.lock               # Locked dependency versions
├── CHANGELOG.md             # Release notes per version
├── INSTRUCTIONS.md          # Development principles for this repository
├── CONTRIBUTING.md          # Contribution workflow
├── LICENSE                  # Project license
├── MILESTONES.md            # Milestones and the maintenance gate
├── ROADMAP.md               # Per-PR history
├── WHITEPAPER.md            # Reference white paper (unmodified)
├── deny.toml                # cargo-deny configuration
└── rust-toolchain.toml      # Rust toolchain pin
```

## Building and testing

ONX is implemented in Rust. With a recent stable toolchain installed:

```sh
cargo build --workspace --all-targets
cargo test --workspace --all-targets
```

## Long-term goal

The long-term goal is to develop an independent, functioning blockchain network that faithfully implements the core architecture described by the original TON design while maintaining a distinct identity, implementation, network, and ecosystem. ONX should ultimately be able to stand on its own — the project does not need to replace TON to be successful, only to demonstrate what an independent implementation of the underlying architectural vision can become.

## Disclaimer

Open Network X is an independent project. ONXI, Open Network X, and Onyxi should not be represented as official TON products, networks, or services. The use of historical TON technical material as a reference does not imply endorsement, affiliation, or control by the organizations or communities associated with the existing TON ecosystem.

## License

Open Network X is licensed under the [Apache License, Version 2.0](LICENSE), as decided in [ADR-0022](docs/adr/0022-project-license.md). Individual reference materials may have their own copyright and licensing requirements — see `WHITEPAPER.md` for the applicable source and attribution information.
