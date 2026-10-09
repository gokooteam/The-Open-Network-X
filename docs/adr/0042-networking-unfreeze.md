# ADR-0042 — Unfreeze networking (M5)

**Status:** Accepted (2026-10-09).
**Decider:** Amethyst (owner; M5 kickoff at her direction).
**Amends:** the freeze rationale in `crates/node/onxd/src/lib.rs` ("networking
is frozen until the deterministic-replay milestone passes").

## Context

Since the single-node work began, `onxd` has refused to start with
`network_enabled = true`:

> "networking is frozen until the deterministic-replay milestone passes:
> refusing to start with network_enabled=true (there is no real network
> loop yet). Set network_enabled=false to run the single-node producer."

The freeze existed for one reason: building a network on top of a
non-deterministic replay path would have cemented consensus bugs into the
wire protocol. The deterministic-replay milestone (M1) is done (2026-10-05),
and the single-node spine has since been hardened through M4 (gas caps,
257-bit ints, cell bit-length, storage stats, chain-bound `CHKSIGNU`, live
`code_refs`, authenticated headers, devnet-1). The grounds for the freeze no
longer hold.

M5's first exit criterion requires that the unfreeze be recorded as a
decision, not just enacted by deleting the guard.

## Decision

1. **Networking is unfrozen as of this ADR.** M5 ("nodes find each other and
   stay in sync", `0.4.0`) may proceed: ADNL/overlay transport, follower
   sync from genesis, adversarial peer tests, and the peer-discovery and
   gossip decisions.

2. **Unfreezing the decision is not the same as having a network.** There is
   still no real network loop, so `network_enabled = true` continues to fail
   closed — but the refusal message now says the loop is not yet implemented
   (M5 in progress) instead of citing the freeze. Claiming a network exists
   when none does would be dishonest; the metrics handle keeps reporting
   zero peers for the same reason.

3. **The mempool file-drop path stays.** It was designed as the honest
   no-network submission path (`crates/node/onxd/src/mempool.rs`): a spool
   directory with durable crash semantics, shaped like the future RPC
   endpoint's writer. When networking lands, only the writer changes.

4. **M5's exit criteria in MILESTONES.md are the definition of done** for
   this milestone. Peer discovery (static list vs DHT) and message
   propagation (direct submission only vs gossip) each get their own
   recorded decision as the work proceeds — they are not decided here.

## Consequences

- The freeze guard's rationale is retired; the fail-closed behavior remains
  with an honest message until the first real networking PR lands.
- M5 work is unblocked. Nothing about consensus changes: the single-node
  producer path is untouched, and `network_enabled = false` remains the
  only way to run a node today.
- If a future milestone needs the network frozen again (e.g. a wire-protocol
  redesign), that freeze gets its own ADR — freezes are decisions with
  reasons and expiry conditions, not comments.
