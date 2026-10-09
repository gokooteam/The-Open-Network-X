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
- **One milestone, one minor version** (per ADR-0040). M4 ships as `0.3.0`,
  M5 as `0.4.0`, and so on. `1.0.0` is the maintenance gate. SemVer 1.0 is
  the point where you promise compatibility, which is the same point where
  you stop developing and start maintaining.
- **Tick boxes in the same PR that earns them.** If you can't link the
  evidence, leave the box unticked. Then run `python3 scripts/site.py build`
  to update the README's milestone map (CI fails otherwise). After the
  merge, the Milestones workflow updates the GitHub milestones (the
  Milestone box in each PR's sidebar shows e.g. `M4 · 16/21 · …`). Edit
  progress here, never on GitHub; the workflow overwrites it there. New PRs
  get the milestone their title names (`M5: …`), else the current one.
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
| **M4** | **Pay down debt: a single node you'd trust** | **0.3.0** | ✅ Done 2026-10-09 |
| M5 | Networking: nodes find each other and stay in sync | 0.4.0 | ✅ Done 2026-10-09 |
| M6 | Consensus: more than one validator | 0.5.0 | ⏭ Next |
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
  automation, and the `v0.2.0` tag (#23, ADR-0040).

### Where that leaves us

| Measure | Value (2026-10-08) |
| --- | --- |
| Workspace crates | 20 |
| Specifications / ADRs | 15 / 34 (ADR-0029 and ADR-0034 still *Proposed*) _(since resolved: all accepted on 2026-10-08, ADR-0037 in part; the versioning ADR is now ADR-0040)_ |
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
  can't accept it. _(Since resolved: see the `onx-cli` criterion under M4.)_

### Where each development task stands (`docs/planning/development-tasks.md`)

| Status | Tasks |
| --- | --- |
| **Integrated and hardened** (on the replay/producer path, adversarially tested) | TASK-001 storage (realized as `onx-storage`/redb), TASK-020 genesis |
| **Integrated, partial** | TASK-002/003 TVM: a deliberately minimal opcode set; `JMPREF`/`CALLREF` work from real contracts since #32, with open control-flow bugs (see M4). TASK-012 `onxd`: single node only, no roles, no network. TASK-019 telemetry: `onxd` serves metrics on `127.0.0.1:9100`; there is no Grafana dashboard |
| **Library only** (has unit tests, not used by `onxd`) | TASK-004/005/006 ADNL/RLDP/DHT, TASK-007 consensus engine, TASK-008 block sync, TASK-009 shard pipeline, TASK-010 hypercube router, TASK-011 election/slashing, TASK-013 RPC (2 unit tests, none over HTTP), TASK-015 system contracts, TASK-016 payment-channel daemon |
| **Placeholder or not running** | TASK-014 `onx-cli` (placeholder encodings). TASK-017 simulation: a Python model, not real binaries, and not run in CI. TASK-018 fuzz targets exist, but no workflow runs them _(since resolved: see the fuzz criterion under M4)_ |

---

## Part 2 — Goals to hit

### M4 — Pay down debt: a single node you'd trust (`0.3.0`) ⏭ next

**Why first:** every later milestone stands on the single-node spine. Known
bugs, docs that say the wrong thing, and a `main` that is often red get
harder to fix once networking adds a second moving part.

**Merged:** Wave 4 (#32, 2026-10-08) hardens the VM: gas caps, 257-bit
ints, cell bit-length, storage stats, chain-bound `CHKSIGNU`, and live
`code_refs` (ADR-0034 `gas-caps` to ADR-0039, accepted 2026-10-08, ADR-0037
only in part). Review of
#32 found bugs it introduced or exposed; the ones reproduced are listed
under *Known bugs* below.

**Exit criteria**

*Known bugs*
- [x] `JMPREF`/`CALLREF` work from real contracts. PR #7 flagged that the
      STF never filled `Interpreter::code_refs`; only test code pushed to it.
      **Done (#32):** Wave 4 step 7 (ADR-0039) fills `code_refs` from the
      current code cell's children through the cell store on every code
      switch, and caps the call stack at 256 frames. Evidence:
      `stf_contract_callref_returns_right_result` in
      `crates/protocol/onx-stf/tests/callref_stf.rs`: a contract run through
      `propose_block`/`apply_block` `CALLREF`s a child that does the
      increment, and the counter goes 0 → 1 → 2 without bouncing; it fails
      on the pre-#32 `main`. `stf_contract_callref_without_callee_content_bounces`
      is the negative control. (A nested-call bug after an implicit return
      was found later and fixed; see the next item.)
- [x] Nested calls return to the wrong place after an implicit return. When
      a callee runs off the end of its code, the interpreter pops the call
      stack but leaves `c0` pointing at the frame it just restored
      (`step()` in `onx-execution/src/interpreter.rs`); an explicit `RET`
      resets it. So if A calls B, B calls C, and C falls off its end, B's
      `RET` jumps back into B and drops A's frame, and A's remaining code
      never runs. Reproduced on `main` @ `9d2b452`. Fix: reset `c0` from the
      remaining call stack on implicit return too. Evidence needed: a test
      of that A → B → C shape where A's code after the call runs.
      **Done:** the implicit return in `step()` now resets `c0` from the
      remaining call stack, exactly as `RET`/`IFRET` do. Evidence:
      `nested_implicit_return_resumes_outermost_caller` in
      `crates/protocol/onx-execution/tests/execution_tests.rs` (C falls off,
      B `RET`s, A's code after the call runs once; the test fails without
      the fix, with B's tail running twice and A's never).
- [x] Code length ignores a code cell's exact bit length. `step()` and the
      operand readers measure code as `8 × data_bytes.len()`, not
      `bit_len()`, so in a bit-granular code cell (ADR-0036) the completion
      tag and padding bits execute as instructions. Reproduced: a 1-bit code
      cell stores byte `0x40` and runs it as `NEWC`. Fix: bound reads by
      `bit_len()`, or reject code cells that aren't byte-aligned. Evidence
      needed: that 1-bit cell raises `MalformedCell` instead.
      **Done:** `step()`, `read_uint8` and the `IFELSE`/`REPEAT`/`UNTIL`
      jump bounds all measure code with `Cell::bit_len()`; trailing bits too
      few for an opcode or operand raise `MalformedCell`. Evidence:
      `one_bit_code_cell_raises_malformed_cell_instead_of_running_newc` in
      `crates/protocol/onx-execution/tests/code_bit_len.rs` (the 1-bit cell
      stores `0x40` and raises `MalformedCell` with 0 gas used), plus 9- and
      12-bit cases where only the leading `NOP` runs. Three of the four
      tests fail on the old reader.
- [x] Hitting the block gas cap drops a valid message. `propose_block`
      returns `BlockGasExceeded`, and `onxd`'s producer
      (`crates/node/onxd/src/producer.rs`) treats it like any rejection: it
      bisects to the message that tipped the block over, moves it to
      `rejected/`, and the sender's later nonces wait forever on it. Heavy
      calls can be used to get honest messages dropped. Fix: hold that
      message for a later block. Evidence needed: a producer test where an
      over-cap batch splits across two blocks with nothing rejected.
      **Done:** the bisection now returns the error of the first failing
      prefix; when it is `BlockGasExceeded`, `propose_robust` ends the block
      before that message and leaves it and the rest in `pending/`. Only a
      message that exceeds the cap by itself is rejected (it can never fit,
      and holding it would stall everything behind it). Evidence:
      `block_gas_cap_splits_batch_across_blocks_without_rejecting` in
      `producer.rs`: twelve ~9.8M-gas contract calls from six senders go
      through two real `run_tick`s; block 1 commits ten, block 2 the other
      two, every nonce reaches 2, and `rejected/` stays empty. On the old
      producer it fails: the eleventh call is rejected.
- [x] Genesis contracts whose code cell has children can't be called.
      Since ADR-0039, `run()` resolves every child of the root code cell
      before the first instruction and fails with `AbsentNode` if one is
      missing. `onx-genesis` never seeds `contract_cells`, and they are only
      written after a successful run, so such a contract bounces on every
      call. Fix: seed the code DAG at genesis, or resolve a child only when
      `JMPREF`/`CALLREF` uses it. Evidence needed: a genesis-deployed
      contract that `CALLREF`s through the STF.
      **Done (seeded at genesis, ADR-0041):** lazy resolution alone could
      not work, because the child content existed nowhere. The genesis
      document now carries the complete code/data DAGs of every contract
      whose roots have children (version 2 only then, so existing chain IDs
      are unchanged), `onx-genesis` takes them as `child_cells_hex`,
      `state_tree()` seeds `contract_cells`, and `init_genesis` persists the
      same DAGs. Evidence: `crates/tooling/onx/tests/genesis_callref.rs`
      (in memory and through storage; the first call bounces without the
      seeding).
- [x] Out-of-gas should bounce, not be fatal. ADR-0037 made `OutOfGas`
      fatal (the destination keeps the value). That rule was rejected on
      2026-10-08 (ADR-0037's status explains why: it adds no cost to an
      attack and takes the value from honest senders). The code still
      implements it: `is_fatal_exception` in `onx-stf/src/stf.rs`,
      `reference/vectors/fatal_bounce.json`, and `execution.md` §3.4. This is
      a consensus change. Fix: map `OutOfGas` to bounce, keep reporting the
      burned gas on bounce receipts, regenerate the vectors, and amend the
      spec. Evidence needed: an STF test where an out-of-gas delivery bounces
      its value back to the sender. **Done:** `is_fatal_exception` maps
      `OutOfGas` to bounce (no kind is fatal now), bounce receipts still
      report the burned gas, `fatal_bounce.json` is regenerated, and
      `execution.md` §3.4 is amended. Evidence: `tvm_out_of_gas_bounces` in
      `crates/protocol/onx-stf/tests/tvm_integration.rs` (sender gets the
      value back, keeps only the fee paid; receipt reports 1,000,000 gas).
- [x] Settle the `LDREF` disagreement. `tvm-instruction-set.md` §3.5.3/§4.4
      says `LDREF` never raises `AbsentNode`. The code fails closed
      (ADR-0029). Make one of them match the other. **Done (spec follows
      code):** the two cases were different. A *pruned* child is a cell the
      node holds, and `LDREF` passes it through; the spec was right about
      that and so was the code. An *unresolved* child has no `Cell` to push,
      and any substitute would be invented data, so `LDREF` fails closed.
      §3.5.3, the `LDREF` row, §5, §6, `execution.md` §3.4 and ADR-0024 now
      say so. Evidence: `ldref_passes_pruned_child_through_and_only_ctos_raises`
      and `ldref_on_unresolved_child_fails_closed_with_absent_node` in
      `crates/protocol/onx-execution/tests/vm_child_cells.rs`.
- [x] Measure the per-block account-map copy at 100k accounts. PR #7 noted
      27–53 ms. Then fix it, or write the budget into the spec. **Done
      (both):** measured, then fixed. The trie is now the only account store,
      so `ShardStateTree::clone()` is O(1). At 100k accounts (release, 2-core
      sandbox) the clone went from 6.7–11.1 ms to ~32 ns, and
      `propose_block`/`apply_block` for an 8-transfer block from 5.8–8.3 ms
      to ~0.75 ms each. The O(1)-copy requirement, the numbers and the
      remaining contract-cell copy are in `state-model.md` §7.1. Evidence:
      `crates/protocol/onx-stf/tests/account_map_copy.rs` (timing probe,
      `--ignored`) and the `clone_shares_trie_and_copies_on_write` /
      `trie_behaves_like_btreemap_model` tests in `onx-state-model`.

*Docs that tell the truth*
- [x] README status table: the VM row still lists the `LDREF` child-cell bug
      as open, but PR #7 fixed it. Re-grade the row. **Done:** the row is
      now ⚠️ partial: `LDREF` fixed (#7, `vm_child_cells.rs`), checked
      arithmetic (#8), `tvm_execution` fuzzed in CI, with `JMPREF`/`CALLREF`
      named as still open. _(Updated after #32: `JMPREF`/`CALLREF` are now
      wired; the row names the open implicit-return and bit-granular-code
      bugs instead.)_ Evidence: the VM row of the README status table.
- [x] ADR-0032's status line still says the Rust decoder is "not yet
      implemented", but it shipped in #13. **Done:** the status line names
      what shipped where (#12–#14). Evidence: `docs/adr/0032-onxblk05-authenticated-headers.md`.
- [x] ADR-0029 and ADR-0034: accept or reject them. Don't leave them
      *Proposed*. Since #32 there are two ADR-0034 records,
      `0034-versioning-standard.md` and `0034-gas-caps.md`, and the Wave 4
      records ADR-0035 to ADR-0039 are *Proposed* too. Renumber one of the
      ADR-0034s (and every reference to it), then accept or reject each.
      **Done:** the versioning standard is now ADR-0040
      (`docs/adr/0040-versioning-standard.md`), and every reference to it
      moved with it. Gas caps keep 0034: they sit inside the Wave 4 run, and
      code and `gas_caps.json` cite them. ADR-0029, ADR-0034 to ADR-0036,
      ADR-0038, ADR-0039 and ADR-0040 are *Accepted*. ADR-0037 is *Accepted
      in part*: its rule that out-of-gas is fatal is rejected (and now
      reverted in code, see above). Each status line names its evidence and the known bugs it
      carries. Evidence: the `Status` lines in `docs/adr/`, and `python3
      scripts/site.py check`.
- [x] `tests/simulation/README.md` says the simulation is wired into `ci.yml`,
      but it isn't. Wire it in or correct the README. **Done (corrected):**
      the README now says it is a Python model that no workflow runs, and
      that M5 replaces it. Evidence: `tests/simulation/README.md`.
- [x] `CONTRIBUTING.md` and the research logbook: the logbook's last entry is
      #12 (2026-09-10), and none of PRs #1–#23 added one. Revive the
      convention or retire it. **Done (retired as a requirement):** entries
      are optional. `CONTRIBUTING.md` says so and why, the logbook's own
      rules no longer claim CI enforces them, and the pre-commit checklist in
      `docs/planning/development-tasks.md` marks the entry as optional. The
      logbook stays, and entries #13 and #14 (added 2026-10-08) are kept.
      Evidence: `CONTRIBUTING.md` ("Research Question Logbook").
- [x] Docs and websites are checked, not trusted. **Done:** the `Docs and
      site` workflow (`.github/workflows/docs.yml`, job `site-and-docs`)
      fails a PR on a broken Markdown link (`scripts/check-doc-links.py`), a
      bad `WHITEPAPER.md §` citation, or an on-x.live / explorer page that
      disagrees with the repository (`scripts/site.py check`). Old-series ADR
      numbers in specs, crate docs and ROADMAP were renumbered. The sites are
      deployed only from commits that pass it, and `Site monitor` compares
      what is served with `main`. Evidence: `site/README.md`.
- [x] `onx-cli`: make `transfer` produce a real `ONX_MSG_EXT_V1` message, or
      remove the placeholder commands. A command that looks like it works and
      doesn't is worse than no command. **Done (both):** `transfer` signs a
      real external message (chain ID from `--genesis`/`--chain-id`, explicit
      `--nonce`, `--reveal-key` for a first spend) and writes `<hash>.msg`
      into an `onxd` pool dir; `wallet balance` and `deploy-contract` were
      removed. Evidence: `crates/tooling/onx-cli/tests/transfer_e2e.rs` —
      `faucet_to_new_wallet_and_back` runs the real binary against
      `config/genesis.toml` and applies its output through `propose_block` /
      `apply_block`; the negative cases assert `SenderHasNoKey` and
      `WrongChainId`.

*CI you can rely on*
- [x] Require CI to pass before merging (branch protection). CI runs only on
      PRs and pushes to `main`, and a run takes about 7–8 minutes. These PRs
      were merged within about a minute of being opened, so their CI could
      not have finished: #2, #5, #7, #9, #17, #18, #20, #23. #28 and #29
      were merged after their own `Fuzz` run had already failed.
      **Done (PR #34, 2026-10-08):** ruleset `main: require CI` is active —
      13 required status checks with strict policy
      (`.github/rulesets/main.json`). Verified live 2026-10-09: PR #51 sits
      merge-blocked until its checks pass.
- [x] Get `main` green and keep it green. Before `d907e2c`, 6 of the 7 most
      recent completed CI runs on `main` failed (runs #40–#55). Target: the
      last 10 merges to `main` are green on every required workflow
      _(proposed)_. **Progress:** at `29b5d1a` three workflows were red on
      `main`, all from CI configuration: `Fuzz` (never built a target),
      `OpenSSF Scorecard` (every push since #21) and `Workflow Security`
      (actionlint). #30 addresses all three; the Scorecard fix can only be
      confirmed by the first push to `main` after it merges. Tick this box
      only after 10 green merges. **Setback:** #32 was merged with its own
      `Fuzz` run (#12) red, and `Fuzz` failed on `main` @ `9d2b452` (run
      #13): the `tvm_execution` target no longer built, because ADR-0038
      made `ExecutionContext.chain_id` required and the fuzz crate sits
      outside the workspace, so the workspace build didn't catch it. Fixed
      by giving the target a fixed test chain ID; locally,
      `nightly-2026-09-20` ran it for 60 s (1.88M executions) with no
      crash. Confirmed on `main` @ `205465b` (#33): all 11 workflows that
      ran on that push are green, `Fuzz` included (run #15), and
      `OpenSSF Scorecard` has stayed green on every push since #30. Because
      #32 was red, the 10-merge count restarts at #33 (1 of 10). To keep a
      repeat out of `main`, the `CI` Rust job now type-checks the fuzz crate
      on stable (`cargo check --locked --manifest-path fuzz/Cargo.toml --bins`). With the
      pre-fix target, this check fails with the same missing `chain_id`
      error. **Count since #33** (push runs on `main`, checked 2026-10-08):
      #38 and #34 had `Code Coverage` red, from the timing-sensitive `onxd`
      `producer_loop` test that #40 then fixed; #35 was all green; #40 had
      `CI` and `Code Coverage` cancelled, because #36 was pushed 25 s later;
      #36 was all green. `Code Coverage` is not a required check in
      `.github/rulesets/main.json`. Counting every workflow, the run of
      green merges restarts at #36 (1 of 10). Counting required checks only,
      #38, #35 and #36 are green and #40 has no completed `CI` run.
      **Done 2026-10-09:** 10 consecutive merges green on all 13 required
      checks — #36, #42, #43, #44, #46, #45, #48, #47, #49, #50 (verified
      per-commit via the check-runs API; #40 merged 24 s before #36 and sits
      outside the window).
- [x] Add a finite fuzz regression run per target to CI, so all three
      targets in `fuzz/` run on every PR. **Done:** `.github/workflows/fuzz.yml`
      runs `boc_parser`, `tvm_execution` and `block_header` for 60 s each
      (one matrix job per target, pinned nightly) on every PR and push to
      `main`, and uploads crash inputs as artifacts. Correction: until #30
      the workflow never built a target in CI. cargo-fuzz was a musl build
      and defaulted to the musl target, where AddressSanitizer cannot link,
      so every run from #28 on failed in the build step. Evidence that it
      now works: Fuzz run 37754903494 on #30, where all three targets built
      and ran their full 60 s with no crash. The scheduled long-running
      job and a checked-in corpus are left for the maintenance gate's
      24-hour criterion.

*Release hygiene*
- [x] Add a `SECURITY.md` with a way to report vulnerabilities privately.
      devnet-1 is public. **Done 2026-10-09:** `SECURITY.md` exists and
      GitHub private vulnerability reporting is enabled (verified via the
      API) — the "Report a vulnerability" button is live on the Security tab.
- [x] Publish a GitHub Release for the 0.2.x line. **Done 2026-10-09 (owner
      decision):** `v0.2.0` left untouched at `9a8d72f6` — its tree doesn't
      build (the `reqwest` dep was lost in the PR #21 merge). Minted `v0.2.1`
      instead: `release/v0.2.1` = `9a8d72f6` + the dep fix (audit-approved
      slim spec from `8284e3c`) + version bump to 0.2.1. `release.yml` built,
      tested, and drafted it; the release is published:
      https://github.com/gokooteam/The-Open-Network-X/releases/tag/v0.2.1
- [x] Tag `v0.3.0` through the Version Bump workflow. **Done 2026-10-09:**
      release PR #51 (`release/v0.3.0`) merged 12:32:03Z; tag `v0.3.0`
      points at the merge commit `0ea6940a` and the draft release
      (id 407901114) was created by github-actions[bot] at merge time —
      the designed flow. One wrinkle: `versioning.yml`'s tag job skipped
      because the tag already existed when it ran; the outcome is the
      same (tag + draft at the right commit), so no re-run. Draft remains
      unpublished — publish is a maintainer call.

### M5 — Networking: nodes find each other and stay in sync (`0.4.0`)

**Goal:** a producer node and a follower node on different machines. The
follower stays in sync and verifies everything itself.

- [x] An ADR records the unfreeze. `onxd` says networking is frozen "until the
      deterministic-replay milestone passes", and that milestone has passed.
      Record the decision rather than just deleting the guard. **Done:**
      ADR-0042 (`docs/adr/0042-networking-unfreeze.md`) records the unfreeze;
      the freeze guard in `crates/node/onxd/src/lib.rs` is replaced with an
      honest not-yet-implemented refusal until the M5 network loop lands.
- [x] `onxd` with `network_enabled = true` carries blocks between nodes over
      ADNL and the overlay layer. **Done:** the M5 network loop landed —
      `run_daemon` binds an `AdnlTransportNode` under the node's identity
      key (`node_key_path`, 0600), runs the producer loop plus a `SyncServer`
      answering follower block requests (static peer list, ADR-0043), or —
      with `follower = true` — the follower loop instead of producing.
      Responses travel as RLDP transfers; the sync wire protocol is
      `docs/specification/networking-block-sync.md`. (The overlay
      *announcement* machinery stays M6+; M5 sync runs directly over ADNL
      datagrams + RLDP, polled by the follower.)
- [x] A fresh follower syncs from genesis through peers (`onx-blocks::sync`).
      It verifies the `ONXBLK05` signatures, persists the blocks, and reaches
      the same state root as the producer. It also survives a kill -9 and
      resume. **Done:** `crates/node/onxd/src/follower.rs` — fetch →
      strict decode → seqno binding → `verify_block_auth` → `commit_block`
      (STF re-execution) → atomic persist, polled from the head; kill -9
      resume via the atomic commit + startup block-file regeneration (same
      recovery as the producer). Evidence: `follower_syncs_from_genesis_and_matches_producer_root`
      (follower reaches the producer's exact root; block files
      byte-identical) and `follower_resumes_after_restart` (kill window
      between commit and file write → startup regen → resume → converge)
      in `crates/node/onxd/tests/follower_sync.rs`.
- [x] Adversarial tests: a peer that serves corrupted, forged, out-of-order,
      or wrong-chain blocks is rejected, and the follower carries on. The same
      holds for a peer that disconnects mid-transfer. **Done:**
      `follower_rejects_forged_signature`, `follower_rejects_corrupted_block_file`,
      `follower_rejects_wrong_seqno_block`, `follower_rejects_wrong_chain_block`,
      `follower_rejects_body_header_mismatch` (signed header, lying body —
      the STF's re-execution catches it), and
      `follower_survives_mid_transfer_disconnect` (partial RLDP then silence
      → clean timeout → syncs once the honest peer answers). Rogue peers are
      driven by `onx_networking::testutil::rogue`. All rejections are
      retryable — a Byzantine peer wastes time, never the chain.
- [x] Decide how peers are found for this milestone, either static peer lists
      or the DHT, and record the choice. **Done:** ADR-0043
      (`docs/adr/0043-static-peer-discovery.md`) — static peer list for M5;
      DHT stays as library code, rewiring deferred to M7.
- [x] Decide whether messages reach the producer only by direct submission or
      also by gossip, and record the choice. **Done:** ADR-0044
      (`docs/adr/0044-direct-submission-no-gossip.md`) — direct submission
      only for M5; no mempool gossip; the file-drop spool stays the
      submission path.
- [x] A multi-node test of the **real binaries** runs in CI, for example with
      docker-compose. This replaces the Python model in `tests/simulation/`.
      **Done:** `scripts/multinode-sync-test.sh` (CI job `multinode` in
      `.github/workflows/ci.yml`) — two real `onxd` binaries on localhost,
      producer + follower over ADNL; faucet transfers via `onx-cli`; the
      follower syncs block 1 and the roots are compared via two independent
      `onx replay` runs; then the follower is kill -9'd, block 2 is made
      while it's dead, it restarts, resumes, and the roots converge again.

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

## Standing directives

These are Amethyst's standing orders for how the remaining work gets done.
They are not milestone exit criteria — `scripts/site.py` does not count them —
but they bind every task the chain works. Each one is recorded in an ADR;
this section is the index.

| # | Directive | Record |
| --- | --- | --- |
| SD1 | **Milestone task chain.** The remaining exit criteria are worked one at a time, each as a GitHub issue labeled `gokoo-task`, each merged task starting the next. The issue timeline is the merge→start ledger. No new scheduler was built: the 30-minute plan driver is the scheduler, the 90-second PR event watch is the merge trigger. | ADR-0046, `docs/guides/task-chain.md` |
| SD2 | **Direction is part of the trusted record.** Work is only trustworthy if the direction behind it is documented. Every task issue carries the direction that commissioned it; her prompts and decisions are kept verbatim in `docs/conductors-score.md`. | ADR-0046 |
| SD3 | **Bring in human contributors.** The solo build got ONX this far — that is in the git log — but the best systems are not built by one person. The project is made ready for expert human contributors *before* they arrive (branch protection, review gates, contributor docs), the intent is signaled publicly, and the existing work stays protected behind the same gates that guard it now. | ADR-0047, `docs/planning/contributor-strategy.md` |

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
