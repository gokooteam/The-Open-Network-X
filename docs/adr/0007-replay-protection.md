# ADR-0007 — Replay protection scheme

**Status:** Accepted
**Date:** 2026-10-05
**Milestone:** message-based single-shard chain

## Context

The actor model splits replay protection in two: external messages need
sender replay protection (nonces), internal messages need delivery
exactly-once (they carry no signature and no nonce — they're derived).
The whitepaper (§2.4.23) prescribes tracking recently-delivered message
hashes.

## Decision

Two independent mechanisms, one per message type:

1. **External messages: exact-match nonces.** The wallet handler requires
   `msg.nonce == account.nonce` and bumps the nonce on success. Replay of
   a consumed external fails with `NonceMismatch`; gaps fail too. Same as
   the old model — it was already correct.

2. **Internal messages: delivery IDs + per-block processed set.**
   - Every internal message has a deterministic ID:
     `domain_hash(ONX_MSG_INT_V1, canonical_bytes)`, where the canonical
     bytes include an `origin` anchor (the external hash for
     wallet-emitted messages, the bounced message's ID for bounces).
   - During phase 2, each delivery inserts its ID into a transient
     per-block `BTreeSet`; a repeat ID aborts the block with
     `DoubleDelivery`.
   - The set is transient (block scope) because cross-block replay is
     **structurally impossible**: internal messages are never submitted,
     never persisted, and never cross block boundaries (ADR-0003). The
     check is defense-in-depth within a block, not the primary mechanism.

Why the `origin` anchor matters: without it, two identical transfers
(same src/dest/value in one block) would derive the same internal ID and
falsely trip the double-delivery check. The origin makes every derived
message unique per causal external while keeping delivery deterministic.

## Alternatives considered

- **Persistent delivered-set across blocks (TON-style).** Rejected:
   unnecessary given structural impossibility (ADR-0003), and it would
   need storage tables + pruning policy for zero benefit on one shard.
- **Nonces on internal messages.** Rejected: internal messages have no
   signing sender to own a nonce sequence; per-account delivery counters
   would serialize independent senders through the receiver's state.
- **No check (rely on structural impossibility alone).** Rejected: the
   check is ~10 lines and converts a whole class of future dispatch bugs
   (queue corruption, accidental re-queue) from silent double-spends into
   loud block failures. Fail closed.

## Consequences

- `DoubleDelivery` is a consensus error: any block that triggers it is
  invalid, on every node, deterministically.
- Bounces participate identically: a bounce's ID includes its own origin,
  so a bounce can never collide with its parent or be redelivered.
- The mempool's structural dedup (files named by content hash) is a
  separate, non-consensus convenience — it collapses duplicate
  *submissions*, while this ADR covers duplicate *deliveries*.
