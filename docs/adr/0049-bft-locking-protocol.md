# ADR-0049: BFT Consensus Protocol with Locking

**Status:** Draft
**Date:** 2026-10-09
**Deciders:** Gokoo (implementer), Amethyst (owner)
**Context:** M6 task (wire `onx-consensus` into `onxd`)

## Problem

The `onx-consensus` engine implements a three-phase vote counter (PreVote →
PreCommit → Commit) but no locking protocol. Review probes demonstrated
that four honest validators, with delayed messages and one timeout, can
finalize two different blocks at the same height. Both blocks pass
`verify_block_auth`. This is not a bug in the vote counting — it is a
missing protocol: nothing commits a validator to a block it already voted
to commit.

Additionally:
- Validators vote without validating the block (no seqno/prev_hash/state_root
  checks, no STF re-execution), violating `consensus.md` §3.4.
- The quorum threshold is defined two ways: the engine uses "at least 2/3"
  while `verify_block_auth` (ADR-0032) requires "more than 2/3".
- There is no record of what a validator has signed (restart safety), no
  catch-up for missed heights, and no fork-choice rule beyond "first wins".

## Decision

Adopt a Tendermint-style locking BFT protocol:

### 1. Locking

Each validator maintains:
- `locked_block`: the block hash it is locked on (or nil).
- `locked_round`: the round in which it locked.

Rules:
- A validator locks on block B at round R when it sees a quorum (>2/3 stake)
  of PreCommit votes for B at round R.
- Once locked on (B, R), the validator will only PreVote for B, or for a
  proposal at a round > R that carries a quorum certificate justifying a
  different block.
- A quorum certificate (QC) for (B, R) is >2/3 stake of PreCommit votes.
  A proposal for round R' > R must include the highest QC the proposer has
  seen; validators unlock only if the proposal's QC is from a higher round
  than their lock.

This guarantees: if two honest validators finalize different blocks at the
same height, >1/3 of stake is Byzantine (standard Tendermint safety).

### 2. Threshold: more than 2/3

Unify on **more than 2/3** (> 2/3, not ≥ 2/3) everywhere:
- Engine quorum checks.
- `verify_block_auth` (already requires this per ADR-0032).
- Quorum certificates.

Rationale: with 3 equal-stake validators, "at least 2/3" lets 2 signers
finalize a block that then fails verification. "More than 2/3" is the safe
rule. Define it once in `onx-consensus` and reference it from verifiers.

### 3. Block validation before voting

Before PreVoting, a validator MUST:
1. Check the proposal's height matches the current height.
2. Check the block's seqno = height.
3. Check the block's prev_hash matches the local head.
4. Re-execute the block's transactions via the STF dry-run (per
   `consensus.md` §3.4) and check the resulting state root matches.

A validator never votes for a block it has not validated. This is not
optional.

### 4. Signed-record (restart safety)

Each validator persists (to disk, fsync before voting):
- The highest round in which it sent each vote phase.
- The block hash it PreCommitted at each round.

On restart, it reloads this record and refuses to send a vote that would
contradict it. An honest validator never double-signs, even across restarts.

### 5. View changes

On timeout at round R:
- Increment round, clear votes (never cross rounds).
- If locked, retain the lock (locks persist across rounds within a height).
- The new leader's proposal must include the highest QC it has seen.

Timeouts use a monotonic clock, not wall-clock.

### 6. Catch-up

A validator that misses a height requests the finalized block (with its QC)
from peers via the block-sync protocol. It verifies the QC (>2/3 signatures
on the commit votes) before committing. No re-execution of consensus needed.

### 7. Fork choice

There is no fork choice rule because forks cannot occur among honest
validators: the locking protocol guarantees at most one finalized block per
height unless >1/3 of stake is Byzantine. A "conflicting proposal" is
evidence of Byzantine behavior, logged as such.

## Consequences

- The `ConsensusEngine` needs a rewrite: locks, QCs, and the unified
  threshold. The current three-phase counter is insufficient.
- The driver needs block validation before voting (STF integration).
- Validators need persistent signed-records (new storage).
- The wire protocol needs QC fields on proposals.
- This ADR supersedes the "first quorum-certified wins" note in the M6
  task log (that was a placeholder, not a protocol).

## References

- `docs/specification/consensus.md` §3.4 (validation obligations)
- ADR-0032 (block authentication, "more than 2/3")
- Tendermint consensus (Buchman et al.): locking and view changes
- Review probes: `review_probes_1_huis.rs` (finding 1: conflicting finalization)
