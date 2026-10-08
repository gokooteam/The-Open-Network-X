# ADR-0003 — Same-block delivery with a transient queue

**Status:** Accepted
**Date:** 2026-10-05
**Milestone:** message-based single-shard chain

## Context

In the actor model, a message sent in block N might be delivered in block
N+1 or later (TON persists output queues across blocks). The whitepaper
(§2.4.27), however, explicitly permits same-shard messages to be delivered
within the originating block. This milestone is single shard, single node,
file-replayed — there is no cross-shard routing to implement.

The design question: persist the internal queue across blocks (TON-like),
or deliver everything within the originating block with a transient queue?

## Decision

**Same-block delivery, transient in-memory queue, no persistence.**

- Phase 1 (wallet) queues one internal message per external, in order.
- Phase 2 drains the queue FIFO within the same block application.
- Bounces are appended to the same queue and delivered in the same block.
- The queue lives only in `apply_messages`'s stack frame. It is never
  persisted, never crosses a block boundary.
- Delivery rounds are defensively bounded (4× external count); exceeding
  the bound fails the block closed. Unreachable while contracts cannot
  emit messages — the bound exists so a future egress feature cannot turn
  the drain loop into a halting problem.

Per-(sender, receiver)-pair FIFO holds: externals authenticate in order,
each emits at most one internal in order, bounces append in delivery
order, and the queue is FIFO — so messages enter in generation order and
leave in delivery order.

## Alternatives considered

- **Persistent output queue across blocks (TON-like).** Rejected: it needs
  new storage tables, cross-block replay tracking, and queue-root
  commitments — all for a single shard where the whitepaper allows
  same-block delivery. The task explicitly constrained storage changes to
  what the queue needs; the queue needs nothing.
- **Deliver in a later block always (strict async).** Rejected: adds a full
  block of latency to every transfer for no protocol benefit on one shard,
  and still needs the persistent queue.
- **Interleaved wallet+delivery per external** (authenticate m₁, deliver
  m₁, authenticate m₂, …). Rejected: it changes failure semantics — a
  later external's auth failure would leave earlier deliveries committed,
  breaking the "first invalid message aborts the block" rule. Two clean
  phases keep auth atomicity.

## Consequences

- `onx-storage` table definitions are untouched by the queue (the milestone
  constraint is satisfied trivially: the queue needs no tables).
- Kill-9 resume is unaffected: a block either fully applied (committed
  atomically) or never existed; there is no half-drained queue to recover.
- **No intra-block chaining.** Because all wallets authenticate before any
  delivery, a message cannot spend funds received by an earlier message in
  the *same* block — the credit lands in the delivery phase, after the
  later message's wallet already ran. This is the honest async semantics
  (you cannot spend what you have not yet received), but it is a change
  from the old sync model where sequential `apply_tx` allowed it. The
  mempool holds such messages for the next block (its balance walk uses
  pre-block balances); liveness is preserved with a one-block delay.
  Chained payments that assumed sync settlement must submit across two
  blocks.
- Latency model: message delivery is synchronous *within* a block but the
  protocol shape is async — contracts must still be written as if delivery
  were delayed (no return values, bounce handling), so the future
  multi-block queue is a performance/robustness change, not a semantic one.
- When sharding arrives, this ADR is revisited: cross-shard messages will
  need the persistent output queue, hypercube routing, and per-block
  delivery proofs.
