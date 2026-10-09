# ADR-0043 — Static peer list for M5 (DHT deferred)

**Status:** Accepted (2026-10-09).
**Decider:** Gokoo (plan driver, Wave 4→5 push per Amethyst's 2026-10-08
authorization; M5 exit criterion: "Decide how peers are found for this
milestone, either static peer lists or the DHT, and record the choice").

## Context

M5's goal is one producer node and one follower node on different machines,
with the follower syncing from genesis and verifying everything itself.
`onxd` already parses a `peers` config key (a static list of peer
descriptors). The `onx-networking` crate also contains a Kademlia DHT
implementation (`dht_daemon.rs`) as library code, per
`docs/specification/networking-dht.md`.

## Decision

**M5 uses a static peer list. The DHT is not wired into `onxd` for this
milestone.**

## Rationale

1. **Two known nodes.** The producer and the follower run on machines whose
   addresses are known at deploy time. Discovery solves a problem M5 does
   not have.
2. **The DHT is a subsystem, not a flag.** Wiring Kademlia means bucket
   refresh, record replication and expiry, bootstrap contacts, and a new
   Sybil surface — all before the block-sync protocol itself works. That
   order is backwards: build the sync protocol first, then decide what
   discovery it needs.
3. **Static lists are auditable.** A follower that only talks to
   explicitly configured peers has a trivially inspectable trust
   boundary. For a two-node milestone, that is a feature.
4. **The DHT code stays.** `dht_daemon.rs` remains as tested library code;
   this ADR defers *wiring* it, not deleting it.

## Consequences

- `onxd` resolves its sync peers from the `peers` config key only. An
  empty peer list with `network_enabled = true` fails closed at startup
  (a node that cannot name a peer cannot sync).
- Peer descriptors are `abstract-address@host:port` pairs; the follower
  pins the producer's ADNL abstract address from config, so a DNS or
  routing attacker cannot substitute a different node without also
  holding the producer's key.
- M7 (public testnet: "someone who has never talked to you can join")
  re-opens this decision — that milestone is where discovery is actually
  needed, and this ADR will be the thing it amends.
