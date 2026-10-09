# M6 task log — wire onx-consensus into onxd (M6-336ed3b863ff)

**Criterion:** The consensus engine (`onx-consensus`) is wired into `onxd`.
At least 4 validators come from the genesis set.
**Branch:** `gokoo/M6-336ed3b863ff`
**Kicked:** 2026-10-09 by Amethyst (direct, in chat — Issues are disabled on
the repo, so the issue-ledger kick was bypassed).
**Note:** Amethyst said "begin" — council deferred; design decisions below
are Gokoo's, documented for her review at the PR. The relay + Claude audit
on the final PR provide the review.

## Design decisions

1. **Production model:** leader rotation via the engine's round-robin
   (`round % validators.len()`). Every validator runs the engine; the
   round leader proposes, others vote, 2/3-stake quorum finalizes.
   Rationale: the engine already implements exactly this; least new
   mechanism. Revisit if the council later disagrees.
2. **Signatures:** linear N×68-byte SigEntries for M6 (4 validators).
   BLS aggregation deferred — documented as follow-up, not M6 scope.
3. **Slashing:** detection + evidence only, per D3 (enforcement deferred).
4. **Genesis:** 4 validators, deterministic test keys (0x11–0x44) for
   devnet. Real keygen documented for operators later.
5. **Fork choice:** highest quorum-certified chain wins; the follower's
   fatal halt on forks is replaced by the rule (TBD in code).
6. **Votes → SigEntries:** the engine's phase votes sign
   `vote_signing_bytes`, not block headers — but `verify_block_auth`
   needs header signatures. So commit votes carry the validator's
   header signature (new field, set at Commit phase only); on
   `FinalizedBlock` the node assembles SigEntries from them. The
   commit quorum is both the BFT evidence and the block's multi-sig.

## Phases

- [x] **P1 — Genesis & keys:** 4 validators in `config/genesis.toml`;
      `onxd` loads the N-key validator set; startup TRAP covers all keys.
- [x] **P2 — Signature budget:** `producer_sig_section_bytes` budgets
      `4 + k·68` for the quorum actually attached (ADR-0045 update).
- [ ] **P3 — Engine loop:** `onxd` drives `ConsensusEngine` — rounds,
      `receive_proposal`/`receive_vote`, `on_timeout` in the tick loop.
- [ ] **P4 — Transport:** consensus message types (proposal/vote) over
      ADNL/RLDP.
- [ ] **P5 — Fork choice + equivocation evidence:** replace fatal halt;
      double-sign detector feeding evidence records.
- [ ] **P6 — Tests:** multi-key multinode test (4 validators, 1 offline
      liveness); `scripts/multinode-sync-test.sh` extended.
- [ ] **P7 — Tick the box:** linked evidence, `site.py build`, PR
      `M6: …` labeled `gokoo-task-pr`.

## Log

- 2026-10-09: branch created on main 59cc3a9; survey + engine API read;
  plan written. Starting P1.
- 2026-10-09: P1 done — 4 validators in `config/genesis.toml` (test keys
  0x11–0x44, pubkeys verified against nacl). Per-node key loading
  unchanged: each `onxd` holds one key, TRAP 4 checks membership in the
  N-list.
- 2026-10-09: P2 done — `producer_sig_section_bytes(n)` budgets
  `4 + N·68` for the whole genesis set (D2: N fixed, fail-closed);
  devin-47 concern answered in code comment + test
  `sig_section_budget_covers_genesis_set`. BLS aggregation deferred
  past M6.
- 2026-10-09: P4 (wire) done — onx-consensus/src/wire.rs: proposal/vote
  tag-prefixed codec; commit votes carry the validator's header
  signature at the transport layer (engine untouched); 16 tests green.
  Design decision 6 recorded above. Next: P3 engine driver in onxd.
- 2026-10-09: P3 (driver) done — crates/node/onxd/src/consensus_driver.rs:
  owns ConsensusEngine per height, emits broadcast/finalize events,
  buffers out-of-order votes, assembles SigEntries from commit-vote
  header sigs. Engine gained height()/proposal()/stakes() accessors.
  Tests: 4-validator full round finalizes (quorum sigs); 3-of-4 with
  one offline still finalizes. 16/16 onxd lib tests green.
  REMAINING: tick-loop integration (driver in run_tick), RLDP transport
  for proposal/vote broadcast (P4), fork-choice rule (P5), multinode
  integration test (P6).
- 2026-10-09: Tick-loop integration done — run_tick drives the
  ConsensusDriver (timeouts, inbound, leader proposal, finalize->commit).
  run_producer_loop builds the driver from genesis + key. Single validator
  completes the full BFT round via local loopback (P4 stub). Producer
  tests ported to BFT ticks; 16/16 onxd lib green, clippy clean.
  REMAINING: P4 real network broadcast (RLDP vs ADNL datagram decision),
  P5 fork-choice rule, P6 multinode integration test.
