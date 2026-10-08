# ONX Milestones

**Snapshot:** 2026-10-08 · workspace `v0.2.0` · `main` @ `d907e2c`

This file answers three questions:

1. **What has been done?** Part 1 covers milestones M0–M3. Everything in it
   can be traced to a PR.
2. **What comes next?** Part 2 covers milestones M4–M7, each with exit
   criteria you can check.
3. **When do we stop developing and start maintaining?** Part 3 is the
   maintenance gate (M8, `1.0.0`).

`ROADMAP.md` keeps the per-PR narrative and `CHANGELOG.md` keeps the release
notes. This file is the forward plan and the finish line.

---

## How to use this file

- **A milestone is done only when every exit criterion is checked, and each
  check links to evidence.** Evidence means a PR, a CI run, or a named test.
  "The code exists" does not count. The bar is the same as the README's
  "Adversarially tested" column: something tried to break it and it held.
- **One milestone, one minor version** (per ADR-0034). M4 ships as `0.3.0`,
  M5 as `0.4.0`, and so on. `1.0.0` is the maintenance gate. SemVer 1.0 is
  the point where you promise compatibility, which is the same point where
  you stop developing and start maintaining.
- **Tick boxes in the same PR that earns them.** If you can't link the
  evidence, leave the box unticked.
- **New ideas go to the [Parking lot](#parking-lot-post-10-or-undecided)**
  unless they block the current milestone. Most "while I'm in here" work
  belongs there.
- **Numbers marked _(proposed)_ are defaults to change, not facts.** Change
  them once, deliberately, in a PR. Don't relax them later just to pass the
  gate.

## At a glance

| # | Milestone | Version | Status |
| --- | --- | --- | --- |
| M0 | Specifications and protocol libraries | 0.1.0 baseline | ✅ Done 2026-09-10 |
| M1 | Deterministic replay | in 0.2.0 | ✅ Done 2026-10-05 |
| M2 | Single-node chain: TVM, message model, hardening | in 0.2.0 | ✅ Done 2026-10-07 |
| M3 | Authenticated blocks, devnet-1, release engineering | `v0.2.0` tag | ✅ Done 2026-10-08 |
| **M4** | **Pay down debt: a single node you'd trust** | **0.3.0** | **⏭ Next** |
| M5 | Networking: nodes find each other and stay in sync | 0.4.0 | Planned |
| M6 | Consensus: more than one validator | 0.5.0 | Planned |
| M7 | A public testnet other people can use | 0.6.0 – 0.9.x | Planned |
| M8 | **Maintenance gate: stop developing, start maintaining** | **1.0.0** | Finish line |
| — | Sharding, payment channels, multiple workchains | 2.x | Parked: [decision D1](#decisions-only-the-owner-can-make) |

---

## Part 1 — What has been done

### M0 — Specifications and protocol libraries (to 2026-09-10, `0.1.0` baseline)

- **15 protocol specifications** in `docs/specification/`. Together they
  cover primitives, data structures, state, transactions, blocks, execution,
  the TVM instruction set, consensus, ADNL/DHT/overlay networking, sharding,
  economics, and payment channels.
- **ADRs 0008–0027.** These began as `docs/decisions/ADR-0001…0020` and were
  renumbered in PR #6. They cover the multichain architecture, serialization,
  the account lifecycle, the choice of Rust, consensus, networking, sharding,
  economics (a supply of 5 billion Onyxi and a 50% fee burn), the license
  (Apache-2.0), and the TVM instruction set.
- **A library crate for each protocol layer, and library code for all 20
  tasks in `docs/planning/development-tasks.md`.** This work was merged on
  2026-09-09 and 2026-09-10, before this repository's PR #1.
- Honest caveat: most of M0 is *library* code. It compiles and has unit tests,
  but it isn't wired into a running node. The README grades consensus,
  networking, sharding, payment channels, and RPC as scaffolds. The task
  table below says exactly where each one stands.

### M1 — Deterministic replay (PRs #1–#3, 2026-10-05)

- `onx replay --genesis genesis.toml --blocks ./blocks/` executes, persists,
  recovers, and replays a chain. It reproduces byte-identical state roots
  across separate OS processes (#1).
- A real genesis, a pure state transition function, atomic redb storage,
  crash recovery, and fail-closed rejection of bad blocks (#1).
- Ed25519 transaction authorization with per-account nonces. Stored-head
  pinning and root verification on resume (#2).
- Fixed a real crash bug: a kill -9 during first-time database init left a
  half-written database. Also bumped rustls for RUSTSEC-2026-0285 (#3).

### M2 — Single-node chain: TVM, message model, hardening (PRs #4–#11, 2026-10-05 → 07)

- **TVM on the spine (#4):** a real `onxd` block-production loop with a
  spool-directory mempool, blocks produced on demand, and no wall-clock time
  inside blocks. Also gas metering, `SETDATA`, contract accounts, and eight
  hand-derived golden vectors.
- **Actor message model (#5, ADR-0001–0007):** external and internal
  messages, FIFO delivery, bounces, chain-ID-bound signatures, key-derived
  addresses, and replay protection.
- **Wave 1 (#6):** mempool DoS limits, strict Bag-of-Cells decoding, atomic
  block-file writes, and one unified ADR series. Also an independent Python
  reference implementation (`reference/`) that reproduces the golden state
  roots, with a CI job that fails on drift.
- **Wave 2 (#7):** `LDREF` now returns the real stored child cells. Added
  `MSGSENDER`/`MSGVALUE`/`MSGBODY`. Incremental state roots: computing the
  root at 100k accounts went from about 1 s to about 250 ns.
- **Wave 3 (#8, ADR-0028/0029):** checked VM arithmetic, a startup invariant
  that every stored contract cell DAG is complete, and panic containment in
  the producer.
- Static block explorer (#9). Strict validator-key validation, floored
  `DIVMOD`, and fail-closed arithmetic (#10, #11, ADR-0030/0031).

### M3 — Authenticated blocks, devnet-1, release engineering (PRs #12–#23, 2026-10-07 → 08)

- **`ONXBLK05` authenticated headers (#12, #13, ADR-0032):** a signature
  section in every block, storage schema v4, producer signing, and a startup
  check of the signing key. Also explorer decoding, and fixes for all 5
  NO-GO blockers raised in the audit.
- `protocol_version` and `block_time` header fields, plus a block-1
  acceptance test that runs the real loop and the real replay binary (#14).
- A devnet faucet account (#15). The explorer now points at devnet-1 (#17);
  that commit message says devnet-1 is live with block 1.
- The currency was renamed Onyx → Onyxi, ticker ONXI (ADR-0033).
- Opt-in Sentry crash reporting for `onxd`, with redaction and rate limits
  (#19, #22).
- CI and PR tooling (#20, #21): Claude PR audit, TruffleHog, zizmor and
  actionlint, dependency review, cargo-machete/taplo/typos, cargo-audit,
  OpenSSF Scorecard, labelers, and Codecov.
- SemVer 2.0.0 with one workspace version, `CHANGELOG.md`, release
  automation, and the `v0.2.0` tag (#23, ADR-0034).

### Where that leaves us

| Measure | Value (2026-10-08) |
| --- | --- |
| Workspace crates | 20 |
| Specifications / ADRs | 15 / 34 (ADR-0029 and ADR-0034 still *Proposed*) |
| Tests (`cargo test --workspace --all-targets`) | 426 passed, 0 failed, 6 ignored across 72 test binaries (local run, Rust 1.98.1) |
| Golden-vector files / fuzz targets | 5 / 3 |
| CI on `main` @ `d907e2c` | ✅ green (run #56), after 6 red of the previous 7 |
| Open GitHub issues | 0 |

**What someone can do today:** run one `onxd` producer, drop signed
`*.msg` files into its pool, get blocks, replay them independently to the
same roots, and look at them in the explorer.

**What nobody can do yet:**

- Run a second node or connect to a peer. `onxd` refuses to start with
  `network_enabled = true`.
- Run more than one validator.
- Query the chain over RPC.
- Create a signed transfer with a shipped tool. `onx-cli transfer` emits a
  placeholder byte string with no signature, nonce, or chain ID, so `onxd`
  can't accept it.

### Where each development task stands (`docs/planning/development-tasks.md`)

| Status | Tasks |
| --- | --- |
| **Integrated and hardened** (on the replay/producer path, adversarially tested) | TASK-001 storage (realized as `onx-storage`/redb), TASK-020 genesis |
| **Integrated, partial** | TASK-002/003 TVM: a deliberately minimal opcode set; `JMPREF`/`CALLREF` are broken from real contracts (see M4). TASK-012 `onxd`: single node only, no roles, no network. TASK-019 telemetry: `onxd` serves metrics on `127.0.0.1:9100`; there is no Grafana dashboard |
| **Library only** (has unit tests, not used by `onxd`) | TASK-004/005/006 ADNL/RLDP/DHT, TASK-007 consensus engine, TASK-008 block sync, TASK-009 shard pipeline, TASK-010 hypercube router, TASK-011 election/slashing, TASK-013 RPC (2 unit tests, none over HTTP), TASK-015 system contracts, TASK-016 payment-channel daemon |
| **Placeholder or not running** | TASK-014 `onx-cli` (placeholder encodings). TASK-017 simulation: a Python model, not real binaries, and not run in CI. TASK-018 fuzz targets exist, but no workflow runs them |

---

## Part 2 — Goals to hit

### M4 — Pay down debt: a single node you'd trust (`0.3.0`) ⏭ next

**Why first:** every later milestone stands on the single-node spine. Known
bugs, docs that say the wrong thing, and a `main` that is often red get
harder to fix once networking adds a second moving part.

**Exit criteria**

*Known bugs*
- [ ] `JMPREF`/`CALLREF` work from real contracts. PR #7 flagged this and it
      is still open: the STF never fills `Interpreter::code_refs`, and only
      test code pushes to it. Evidence: a test where a contract executed
      through the STF calls `CALLREF` and gets the right result.
- [ ] Settle the `LDREF` disagreement. `tvm-instruction-set.md` §3.5.3/§4.4
      says `LDREF` never raises `AbsentNode`. The code fails closed
      (ADR-0029). Make one of them match the other.
- [ ] Measure the per-block account-map copy at 100k accounts. PR #7 noted
      27–53 ms. Then fix it, or write the budget into the spec.

*Docs that tell the truth*
- [ ] README status table: the VM row still lists the `LDREF` child-cell bug
      as open, but PR #7 fixed it. Re-grade the row.
- [ ] ADR-0032's status line still says the Rust decoder is "not yet
      implemented", but it shipped in #13.
- [ ] ADR-0029 and ADR-0034: accept or reject them. Don't leave them
      *Proposed*.
- [ ] `tests/simulation/README.md` says the simulation is wired into `ci.yml`,
      but it isn't. Wire it in or correct the README.
- [ ] `CONTRIBUTING.md` and the research logbook: the logbook's last entry is
      #12 (2026-09-10), and none of PRs #1–#23 added one. Revive the
      convention or retire it.
- [ ] `onx-cli`: make `transfer` produce a real `ONX_MSG_EXT_V1` message, or
      remove the placeholder commands. A command that looks like it works and
      doesn't is worse than no command.

*CI you can rely on*
- [ ] Require CI to pass before merging (branch protection). CI runs only on
      PRs and pushes to `main`, and a run takes about 7–8 minutes. These PRs
      were merged within about a minute of being opened, so their CI could
      not have finished: #2, #5, #7, #9, #17, #18, #20, #23.
- [ ] Get `main` green and keep it green. Before `d907e2c`, 6 of the 7 most
      recent completed CI runs on `main` failed (runs #40–#55). Target: the
      last 10 merges to `main` are green on every required workflow
      _(proposed)_.
- [ ] Add a finite fuzz regression run per target to CI, so all three
      targets in `fuzz/` run on every PR.

*Release hygiene*
- [ ] Add a `SECURITY.md` with a way to report vulnerabilities privately.
      devnet-1 is public.
- [ ] Publish the GitHub Release for `v0.2.0`. The tag exists and
      `release.yml` creates a *draft*, but no published release exists yet.
- [ ] Tag `v0.3.0` through the Version Bump workflow.

### M5 — Networking: nodes find each other and stay in sync (`0.4.0`)

**Goal:** a producer node and a follower node on different machines. The
follower stays in sync and verifies everything itself.

- [ ] An ADR records the unfreeze. `onxd` says networking is frozen "until the
      deterministic-replay milestone passes", and that milestone has passed.
      Record the decision rather than just deleting the guard.
- [ ] `onxd` with `network_enabled = true` carries blocks between nodes over
      ADNL and the overlay layer.
- [ ] A fresh follower syncs from genesis through peers (`onx-blocks::sync`).
      It verifies the `ONXBLK05` signatures, persists the blocks, and reaches
      the same state root as the producer. It also survives a kill -9 and
      resume.
- [ ] Adversarial tests: a peer that serves corrupted, forged, out-of-order,
      or wrong-chain blocks is rejected, and the follower carries on. The same
      holds for a peer that disconnects mid-transfer.
- [ ] Decide how peers are found for this milestone, either static peer lists
      or the DHT, and record the choice.
- [ ] Decide whether messages reach the producer only by direct submission or
      also by gossip, and record the choice.
- [ ] A multi-node test of the **real binaries** runs in CI, for example with
      docker-compose. This replaces the Python model in `tests/simulation/`.

### M6 — Consensus: more than one validator (`0.5.0`)

**Goal:** blocks are finalized by a set of validators, not by one trusted
producer.

- [ ] The consensus engine (`onx-consensus`) is wired into `onxd`. At least 4
      validators come from the genesis set.
- [ ] A block is final only with signatures from ≥ 2/3 of stake.
      `verify_block_auth` already checks the stake threshold; extend it to
      multiple real signers.
- [ ] Liveness: the chain keeps finalizing with 1 of 4 validators offline, and
      the offline validator catches up when it returns.
- [ ] Safety: a validator that double-signs is detected and evidence is
      produced. With fewer than 1/3 faulty validators, tests never produce
      conflicting finalized blocks, including across a network partition and
      its heal.
- [ ] Soak test: 4 validators run 7 days _(proposed)_ with no state-root
      divergence and no unplanned halt.
- [ ] Decisions recorded for D2 (elected or fixed validator set) and D3
      (on-chain economics); see
      [Decisions](#decisions-only-the-owner-can-make).

### M7 — A public testnet other people can use (`0.6.0` – `0.9.x`)

**Goal:** someone who has never talked to you can join, transact, and run a
node using only the docs.

- [ ] Wallet: create a key, show the address, sign and submit a transfer, and
      check the balance, all with shipped tooling.
- [ ] RPC: `onx-rpc` is wired into `onxd` and serves `getAccountState` (with
      Merkle proof), `sendMessage`, `getLatestBlock`, and `estimateFee`, with
      rate limiting. It has HTTP integration tests; today it has 2 unit
      tests and none over HTTP.
- [ ] The explorer **verifies** block signatures and state proofs. Today it
      decodes and displays signatures but doesn't verify them.
- [ ] A faucet service. The genesis faucet uses a known test key, and must
      never reach a mainnet genesis.
- [ ] An upgrade path, tested: start from the previous release's database,
      upgrade, and get the same roots. Bumping `PROTOCOL_VERSION` has a
      written procedure.
- [ ] Operator docs: running a validator, managing keys, backups and restore,
      upgrades, and monitoring with alerts.
- [ ] At least 5 validators _(proposed)_ that **you don't run** stay on the
      testnet for 4 weeks _(proposed)_.
- [ ] An independent security review of the consensus-critical code. Every
      critical or high finding is fixed.

---

## Part 3 — The maintenance gate (M8, `1.0.0`)

This is the answer to "when do I stop developing?" **Stop when every box
below is checked.** Until then, keep going. Once they are all checked, you
are maintaining.

**1. The scope is finished, and it was decided on purpose**
- [ ] Every box in M4–M7 is checked.
- [ ] Every component in the 1.0 scope is ✅ in all four README columns:
      Spec, Logic, Integrated, and Adversarially tested.
- [ ] Every component *outside* the scope has an ADR that labels it as a
      deliberate simplification (INSTRUCTIONS §15). Nothing is just quietly
      missing.

**2. The protocol has stopped moving**
- [ ] For the last 8 weeks _(proposed)_, nothing on the consensus spine has
      changed: block magic, `PROTOCOL_VERSION`, storage `SCHEMA_VERSION`,
      domain tags, message encodings, and the golden vectors.
- [ ] The specs match the code. No known spec/code disagreements remain.

**3. The chain is stable under real use**
- [ ] The public testnet has run 30 days _(proposed)_ with no consensus
      failure, no unplanned halt, and no state-root divergence.
- [ ] No new critical or high bug has turned up in the last 6 weeks
      _(proposed)_. The bug-discovery rate has flattened, not just slowed for
      a week.

**4. Quality holds without heroics**
- [ ] CI checks are required and have been green on the last 20 merges to
      `main` _(proposed)_.
- [ ] Each fuzz target has run for 24 hours _(proposed)_ with no new crash.
- [ ] Zero open issues labeled critical or high. Zero *Proposed* ADRs. Every
      known limitation is written down.

**5. It works without you**
- [ ] Someone other than the author has set up a node from the docs alone,
      with no help.
- [ ] Restore from backup has been rehearsed.

**6. The promise is written down**
- [ ] A compatibility promise for 1.x: what stays stable (wire formats,
      storage, RPC, CLI) and what may change.
- [ ] `1.0.0` is tagged through the release workflow, with a `CHANGELOG.md`
      entry.

### Signs you're close, and signs you're not

These don't replace the checklist, but they help you read the trend between
checks.

| Signs you're close | Signs you're not |
| --- | --- |
| Most recent PRs are fixes, docs, and tests, not features | Wire formats or domain tags changed in the last month |
| Audits find should-fix items, not NO-GO blockers | An audit still finds NO-GO blockers (#13 had five) |
| New ideas go to the parking lot without regret | You're adding features because the white paper has them, not because a user or the 1.0 scope needs them |
| `main` is boring: green, with small diffs | `main` goes red after merges |

---

## After the gate: what "maintaining" means

**Allowed in maintenance**, as `1.0.x` patches or compatible `1.x` minors:

- Security fixes and bug fixes.
- Dependency updates (dependabot, `cargo audit`, Scorecard).
- Performance work that changes no wire bytes.
- Docs, tests, tooling, and explorer and operator UX.

**Not allowed without reopening development:** any change to consensus rules,
wire formats, storage layout, or the 1.x compatibility promise. Reopening
takes an ADR plus a new milestone in this file, and it targets the next
major version or a planned network upgrade.

**Cadence** _(proposed)_:

- Weekly: triage dependabot, cargo-audit, and Scorecard results.
- Monthly: a patch release, if anything changed.
- Quarterly: re-read this file and the parking lot.

**When to start developing again:**

- A security finding can only be fixed by changing the protocol.
- A real user need can't be met within 1.x.
- You decide to start the 2.0 cycle (sharding; see D1).

---

## Decisions only the owner can make

Each of these changes how far away the finish line is. Record each answer in
an ADR, and update this file to match.

| # | Decision | Recommendation |
| --- | --- | --- |
| D1 | **Is sharding (split/merge, hypercube routing, multiple workchains) in 1.0?** | **No.** Make 1.0 a stable single-shard network and label the simplification per INSTRUCTIONS §15. Run sharding as the 2.0 development cycle. Including it would push the gate out by the largest single amount of work left. The libraries (`onx-sharding`, the router) don't go away; they wait. The trade-off is that 1.0 won't yet be the full architecture the README names as the long-term goal. |
| D2 | Is the validator set elected on-chain (elector contract) in 1.0, or fixed in genesis? | Fixed in genesis for 1.0, with on-chain elections as a 1.x or 2.0 milestone. This keeps M6 focused on BFT safety. |
| D3 | Are on-chain economics (fee burn, inflation rewards, slashing debits) live in 1.0? | Fees and the fee burn, yes, since fees already exist. Inflation and slashing ride with D2. |
| D4 | The _(proposed)_ thresholds in M4–M8 | Accept them or change them once, now. Don't change them later just to pass the gate. |

## Parking lot (post-1.0 or undecided)

These items have specs, and often library code, but no milestone. Moving one
into a milestone is a scope change, so it needs an ADR.

- Dynamic sharding: shard split/merge pipeline and hypercube cross-shard
  routing (TASK-009, TASK-010). Depends on D1.
- Masterchain/workchain separation and multiple workchains. Depends on D1.
- Payment-channel hub daemon and arbiter (TASK-016).
- On-chain elector and config system contracts (TASK-015). Depends on D2.
- Lite-client protocol and binary state-proof endpoints beyond JSON-RPC.
- TVM opcodes beyond the current minimal set (ADR-0024 calls 46 opcodes
  "deliberately minimal").
- A Grafana dashboard and OpenTelemetry tracing export (the rest of
  TASK-019).
