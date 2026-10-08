# ADR-0038 — Chain-bound CHKSIGNU (Wave 4, step 6)

**Status:** Proposed (2026-10-07)
**Decider:** Gokoo (design authority between audits; flagged for Claude's Wave 4 audit)
**Amends:** `docs/specification/execution.md` §3.2; `docs/specification/tvm-instruction-set.md` §4.4

## Context

Claude's 2026-10-06 review found that `CHKSIGNU` (0x62) verified signatures
under the domain tag `TX_BODY_V1` (`"ONX_TX_BODY_V1"`), the *same tag* the
payment-channel library uses for channel-state signatures, with no chain-ID
binding whatsoever. Two replay consequences:

1. **Cross-protocol:** a payment-channel state signature (signed by a channel
   party over a state hash) verifies as a contract signature under `CHKSIGNU`
   — the tags are identical, so the domain-separated bytes are identical.
2. **Cross-chain:** a contract signature valid on one chain (testnet, fork,
   rehearsal genesis) is valid on every chain, because the tag carries no
   chain identity.

Both violate the domain-separation invariant `hash.rs` exists to enforce:
identical bytes hashed (or signed) in two different protocol contexts must
never collide.

## Decision

### 1. Chain-bound tag derivation

`DomainTag` gains a `bind_chain` method; `CHKSIGNU` verification uses a
dedicated base tag:

```rust
/// `ONX:CHKSIGNU:V1` base domain tag (§4.5). Never used raw:
/// `CHKSIGNU` verification always uses `CHKSIGNU_V1.bind_chain(chain_id)`,
/// so a contract signature commits to the chain it was produced for.
pub const CHKSIGNU_V1: DomainTag = DomainTag::from_ascii("ONX_CHKSIGNU_V1");

impl DomainTag {
    /// Derives the chain-bound tag: `SHA256(pad32(base) || chain_id)`.
    pub fn bind_chain(&self, chain_id: &[u8; 32]) -> DomainTag { ... }
}
```

`CHKSIGNU` verifies the signature over `bind_chain(chain_id) || hash32`
(the existing `verify(tag, message, sig)` path — `tag || message` — is
unchanged; only the tag is now chain-derived).

Why derive the tag rather than prepend `chain_id` to the message:
the `verify(tag, message, sig)` API takes a `&DomainTag`, so the chain
binding lives entirely in tag derivation — no signature change to the
primitive, and the "tag" concept stays the single place where domain
separation is reasoned about. A contract signature is now bound to
`(chain, protocol-context)` by construction. The derived tag is a
SHA-256 digest, so it cannot collide with a registered ASCII tag except
with negligible probability.

### 2. `chain_id` as a VM input

`ExecutionContext` gains `pub chain_id: [u8; 32]`. The STF populates it
from `State.chain_id` — the chain's own identity from state, **not** from
the message (a message-carried chain_id would let the signer choose the
tag). It is fixed for the block, so execution stays deterministic, per
`execution.md` §3.2 as amended.

### 3. Payment channels use their spec'd tag

The payment-channel library migrates off `TX_BODY_V1` to
`ChannelState::DOMAIN_TAG` (`"ONX_CHANNEL_STATE_V1"`, the tag
`protocol-primitives.md` §4.5 already specifies for channel state).
This is a fixed tag, not chain-bound — sufficient because the
cross-protocol replay is closed by tag inequality: a channel-state
signature is over `"ONX_CHANNEL_STATE_V1" || state_hash`, a contract
signature over `H("ONX_CHKSIGNU_V1" || chain_id) || hash32`; the tags
can never be equal. Payment channels are off-chain bilateral
agreements (the arbiter is a local construct, not consensus), so
chain-binding them is out of scope for this step.

### 4. `TX_BODY_V1` is retired

No remaining uses after this change. The constant is **deleted** from
`hash.rs`, not left as a trap for future reuse. Any future protocol
context needing a fixed tag must define its own.

## Consequences

- **Consensus change:** signatures that verified under `CHKSIGNU` before
  (with `TX_BODY_V1`) no longer verify. No contracts are deployed on
  devnet-1 and no state exists that could contain such signatures (same
  live-chain safety argument as ADR-0036); no protocol_version bump is
  taken in this step — flagged for the Wave 4 audit alongside ADR-0036's
  identical question.
- **Test churn:** all `ExecutionContext` construction sites gain
  `chain_id`; payment-channel tests sign under `ChannelState::DOMAIN_TAG`.
- **No new opcodes, no gas changes.** `CHKSIGNU` keeps opcode 0x62,
  cost 4000, and its stack effect; only the tag derivation changes.

## Alternatives considered

- **Prepend chain_id to the signed message instead of deriving the tag:**
  rejected — splits domain-separation reasoning across two mechanisms
  and changes the `verify` call shape.
- **Chain-bind payment channels too:** rejected as out of scope — the
  finding is about CHKSIGNU; channels are off-chain and the tag
  inequality already closes the replay.
- **Take chain_id from the message instead of state:** rejected — the
  signer controls message fields; the tag's chain binding must come
  from the chain's own committed identity.
