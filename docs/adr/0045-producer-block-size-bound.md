# ADR-0045 — Producer-side block-size bound (sync servability)

**Status:** Accepted (2026-10-09).
**Decider:** Gokoo (plan driver, Wave 4→5 push per Amethyst's 2026-10-08
authorization; fixes the review-bot findings on PR #46).

## Context

The M5 block-sync wire protocol (`docs/specification/networking-block-sync.md`,
`crates/node/onx-networking/src/block_sync.rs`) lets a follower fetch any
committed block by sequence number. The sync server serves the raw
`.blk` file for a request, bounded by `MAX_BLOCK_FILE_BYTES` (the 8 MiB
decoder cap minus the 14-byte response framing overhead — a file between
the two caps encodes to a response no follower can decode, so the server
fails closed at the tighter bound).

Review of PR #46 surfaced the deeper problem (devin 🔴): **nothing
bounded the producer's block files.** A mempool holds up to 10,000
messages of up to 65,535 bytes each; the Wave 4 gas caps bound execution
(100 M gas/block) but not bytes. A committed block over the servable
bound would be refused by every sync server — followers could never
fetch it and would stall at that height forever, while the producer
marched on. The chain would fork by reachability: one history for the
producer, a dead end for everyone else.

## Decision

**The producer never commits a block whose worst-case encoded file
exceeds `MAX_BLOCK_FILE_BYTES`.** Enforcement is producer-side, in
`propose_robust` (`crates/node/onxd/src/producer.rs`):

1. Before proposing, compute the worst-case encoded `.blk` size for the
   candidate set: magic(8) + header(160) + the full signature section
   (every canonical validator signing: 4 + n·68 bytes) +
   length-prefixed body. The encoding is deterministic, so every honest
   producer computes the same bound.
2. If the set does not fit, trim the tail — exactly like the existing
   `BlockGasExceeded` arm — and hold the excess for the next block.
   Trimming is not rejection: the messages did nothing wrong.
3. Fail closed if even a single message cannot fit (unreachable in
   practice — a max-size message is ~66 KiB against an 8 MiB budget — so
   this arm firing means the size model itself is wrong).

The size model is pinned to the real encoder by a unit test
(`size_model_matches_encode_block_file_exactly`): model and
`encode_block_file` must agree byte-for-byte, or the trim is unsound.

## Rationale

1. **The strand scenario is permanent.** A too-large committed block is
   not a transient failure — no retry, no peer rotation, no timeout
   fixes it. The only repair would be a coordinated chain halt. The
   producer must not create the situation.
2. **Trim, don't reject.** An oversize candidate set is not evidence of
   bad messages — it is evidence of too many good ones. The gas-cap arm
   already established the pattern: hold for the next block.
3. **Worst-case sig section, not actual.** The check runs before signing;
   bounding by "every validator signs" is exact on devnet (one
   validator) and conservative everywhere else. A block that fits under
   the worst case always fits on the wire.

## Consequences

- On an honest chain no committed block is ever unservable; the sync
  server's `BlockTooLarge` refusal is a backstop, unreachable in
  practice.
- A Byzantine producer could still commit an oversize block and strand
  followers. Promoting this bound to a **consensus rule** (verifiers
  reject oversize blocks in `apply_block`) is deferred to the
  multi-validator phase (queue item 5 / M6) — M5's threat model has one
  honest producer, and the rule needs the validator set inside the STF,
  which is an M6 API change.
- The follower loop (next M5 slice) inherits the guarantee: any block it
  can fetch, it can decode — `decode_envelope`'s 8 MiB bound is never the
  binding constraint on served blocks.

## Alternatives considered

- **Consensus rule now:** `apply_block` rejecting oversize blocks. More
  robust against Byzantine producers, but `apply_block` receives the
  decoded block without its sig section and without the validator set —
  enforcing it now means threading both through the STF API for a
  threat M5 does not have. Deferred, not dismissed.
- **Raise the sync cap instead:** pushing the problem to 16/32 MiB just
  moves the cliff. The invariant that matters is producer ⊆ servable,
  at whatever bound.
