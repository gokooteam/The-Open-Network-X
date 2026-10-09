# ADR-0044 — Direct submission only for M5 (no gossip)

**Status:** Accepted (2026-10-09).
**Decider:** Gokoo (plan driver, Wave 4→5 push per Amethyst's 2026-10-08
authorization; M5 exit criterion: "Decide whether messages reach the
producer only by direct submission or also by gossip, and record the
choice").

## Context

M5 has exactly one block producer. User messages must reach that
producer's mempool somehow. Two designs were on the table: direct
submission (the user sends to the producer) and gossip (nodes relay
mempool transactions to each other). `onxd` already has the honest
no-network submission path: the mempool file-drop spool directory
(`crates/node/onxd/src/mempool.rs`, kept by ADR-0042 §3), shaped like the
future RPC endpoint's writer.

## Decision

**M5 uses direct submission only. There is no mempool gossip.**

## Rationale

1. **One producer, one mempool that matters.** Gossip would replicate
   pending transactions to the follower, which produces no blocks and
   has no use for them. Replicating data nobody consumes is not
   decentralization, it is traffic.
2. **Gossip is a DoS amplifier.** Every gossiped transaction is
   re-broadcast by every node that hears it. With no Sybil resistance
   yet (that is M7 territory), an attacker floods once and the network
   floods itself. Direct submission keeps the blast radius at one edge.
3. **Equivocation questions are premature.** Gossip needs answers for
   duplicate suppression, ordering across peers, and what a node does
   with a transaction that never lands in a block. M5's job is
   block sync, not mempool consensus.
4. **The submission path already exists.** File-drop today, the RPC
   `sendMessage` writer tomorrow (M7) — both are direct submission to
   the producer. Nothing new needs inventing.

## Consequences

- The follower never accepts user messages and never relays them. Its
  mempool stays empty by construction.
- `onx` CLI `transfer` output (`.msg` files into the producer's pool
  dir) remains the M5 submission path, joined by RPC `sendMessage` in
  M7.
- If a future milestone adds a second producer, this decision is
  re-opened then — gossip only earns its complexity when there is more
  than one mempool worth syncing.
