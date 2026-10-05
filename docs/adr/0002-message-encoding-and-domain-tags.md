# ADR-0002 — Message encodings and domain tags

**Status:** Accepted
**Date:** 2026-10-05
**Milestone:** message-based single-shard chain

## Context

Replacing transactions with messages needs canonical encodings for two new
objects (external and internal messages) and a block commitment, with domain
separation so that no hash or signature from the transaction era (V1/V2/V3
tags) can ever be confused with a message-era one.

## Decision

### External message (submitted, signed)

Body (big-endian, strict):
```text
chain_id(32) || from(32) || nonce u64be(8) || kind(1) || to(32) ||
amount_nanos u128be(16) || fee_nanos u128be(16) || msg_len u32be(4) ||
message(msg_len) || pubkey(32)
```
Wire = body || signature(64). Transfer bodies are 173 + 64 = 237 bytes.

- `kind`: 0 = Transfer (message must be empty — enforced at parse),
  1 = ContractCall (message is the contract payload).
- `pubkey`: key reveal for the first spend from a key-derived account
  (ADR-0006); all zeros otherwise. It is inside the signed body so a reveal
  cannot be swapped onto another message.
- Identity: `hash() = domain_hash(ONX_MSG_EXT_V1, wire_bytes)`.
- Signature: over `ONX_MSG_EXT_SIGN_V1 || body_bytes`. The body includes
  `chain_id`, so cross-chain replay fails signature verification (ADR-0005).

### Internal message (derived, never submitted)

Canonical bytes:
```text
src(32) || dest(32) || value_nanos u128be(16) || fee_nanos u128be(16) ||
payload_len u32be(4) || payload || is_bounce(1) || origin(32)
```
- `fee_nanos` is the gas budget carrier (`gas_limit = fee × 1000`); the fee
  itself was debited and split at the wallet handler.
- `origin`: the external message hash for wallet-emitted messages, the
  bounced message's ID for bounces. This is the uniqueness anchor —
  two different externals can never derive the same internal ID.
- Delivery ID: `id() = domain_hash(ONX_MSG_INT_V1, canonical_bytes)`.

### Block commitment

`msgs_root = domain_hash(ONX_MSGS_ROOT_V1, id₀ || id₁ || …)` over external
message hashes in block order. The 148-byte header layout is unchanged
(`txs_root`/`tx_count` renamed to `msgs_root`/`msg_count` in place);
only the commitment's domain tag changed. Block files move magic
`ONXBLK03` → `ONXBLK04`.

### Fresh domain tags (never reused from V1/V2/V3)

`ONX_MSG_EXT_V1`, `ONX_MSG_EXT_SIGN_V1`, `ONX_MSG_INT_V1`,
`ONX_MSGS_ROOT_V1`, `ONX_ADDR_V1`. The domain-tag registry
(`docs/specification`) is updated accordingly.

## Alternatives considered

- **Reuse the V3 transaction encoding with a kind byte.** Rejected: the V3
  body does not bind chain ID and has no key-reveal field; shoehorning
  fields in would fork the encoding anyway. A clean break with fresh tags
  is honest about the incompatibility.
- **Put internal messages in the block body too.** Rejected: they are
  deterministic functions of the externals — committing to them separately
  doubles block size for zero security gain and creates a replay surface
  (a resubmitted internal would look legitimate).
- **Variable-length `origin` or no origin field.** Rejected: fixed 32-byte
  origin keeps the ID preimage unambiguous and gives every internal a
  provenance chain back to its external.

## Consequences

- All golden vectors refrozen: old hashes are invalid by construction.
- `onx-transactions` (the spec-admission crate) was deliberately left
  unwired — it implements admission against `onx_data_structures::Message`,
  a different type from the STF's lean message encoding; wiring it would
  have meant two message types or a translation layer for no benefit.
- Parsers are strict: wrong lengths, trailing bytes, over-long payloads,
  and transfer-with-payload all fail closed at parse time.
