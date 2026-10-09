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

## Amendment (2026-10-09) — descriptors carry the public key

Implementation (the M5 follower slice) found the `abstract-address@host:port`
format insufficient: establishing the ADNL channel requires the peer's
ed25519 public key for the X25519 handshake (`AdnlTransportNode::send_datagram`
takes the key, not the address). Descriptors are now
`<ed25519-pubkey-hex>@<host:port>`; the abstract address is DERIVED from the
key (`KeyDescription::compute_abstract_address`) and pinned — a peer
presenting a different key computes a different address and its datagrams
are ignored.

This preserves and strengthens the original intent: pinning the key pins
the address deterministically, so the DNS/routing-attacker argument holds
exactly as written. The decision (static list, no DHT wiring for M5) is
unchanged; only the descriptor encoding moved to carry what the channel
needs. Parsed by `onxd::follower::parse_sync_peer`, which enforces the
strict key predicate (canonical, on-curve, large-order) at config load —
a malformed peer key is a startup refusal, not a mid-handshake surprise.

## Amendment 2 (2026-10-09) — channel-bound attribution, FullPackets skipped

Review (PR #50) found the amendment above overstated: `recv_datagram`
returned the packet's *claimed* sender address, and `FullPacket` datagrams
— which anyone holding our public key can forge with an arbitrary claimed
sender — were accepted as peer traffic. Two changes make the claim true
for the sync path:

1. `recv_datagram`'s channel (FastPacket) path now attributes the datagram
   to the channel's PINNED peer address (derived from the public key passed
   to `connect_peer`), not the packet's plaintext sender field. The
   channel's shared secret (X25519 with the pinned key) is what
   authenticates the peer; the plaintext field does not.
2. The sync layer (`AdnlSyncTransport` / `SharedAdnlTransport`) uses the new
   `AdnlTransportNode::recv_channel_datagram`, which skips `FullPacket`
   datagrams and undecryptable junk entirely — port scanners and forged
   packets never reach the sync layer, and only our own socket failing
   surfaces as an error.

A malicious configured peer therefore cannot impersonate another peer at
the sync layer, and an outsider's forged packets never reach it.
`verify_block_auth` remains the content gate; this is the authentication
floor beneath it. `recv_datagram`'s legacy behavior (claimed address,
FullPackets accepted) is unchanged for non-sync callers.
