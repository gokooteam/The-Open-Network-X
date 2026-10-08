# ADR-0032 — ONXBLK05: Authenticated Block Headers (Producer Signatures)

**Status:** Accepted (design; Rust decoder/verifier is ONXBLK05 step 3, not
yet implemented)

**Date:** 2026-10-07

## Context

ONX blocks are currently unsigned: `onx replay` verifies state transitions
but nothing proves a block was produced by an authorized validator. This
enables undetectable block withholding and forks — a producer can publish
two different blocks at the same sequence number and no verifier can tell
which (if either) is legitimate. The chain needs producer authentication
before block 1.

Claude's ONXBLK05 design review (guidance check-in #3, 2026-10-06) settled
the design; this ADR is its normative record. The implementation order is:
1. fixture rekey PR (done: PR #12),
2. spec + golden vectors + Python reference (this ADR),
3. decoder + verifier in the block-acceptance layer (shared by replay and node),
4. storage v4 (signature in the DB, atomic with the block commit),
5. producer signing + startup key/genesis check,
6. explorer decode.

## Decision

### 1. Header grows to 160 bytes: `protocol_version` + `block_time`

The 148-byte header gains two fields, appended at the end (all existing
field offsets unchanged):

| Offset | Size | Field | Encoding |
|--------|------|-------|----------|
| 0 | 4 | `seqno` | u32be |
| 4 | 32 | `prev_hash` | bytes |
| 36 | 32 | `msgs_root` | bytes |
| 68 | 32 | `state_root` | bytes |
| 100 | 8 | `lt` | u64be |
| 108 | 4 | `workchain` | i32be |
| 112 | 32 | `fee_collector` | bytes |
| 144 | 4 | `msg_count` | u32be |
| 148 | 4 | `protocol_version` | u32be |
| 152 | 8 | `block_time` | u64be |

- `protocol_version`: the protocol version the block was produced under.
  Genesis declares v1. Validity is version-gated: a node rejects blocks
  whose `protocol_version` it does not understand. The version field IS the
  upgrade mechanism — launching without it means the first format change
  can only be a chain reset.
- `block_time`: unix seconds, wall-clock time at production. Monotonic
  (non-decreasing across sequence numbers) and deterministic
  (replay-checked against the committed value). The STF stays pure: time is
  a header input, not execution state. Producer policy: never stamp ahead
  of its own clock.

### 2. Signatures live OUTSIDE the header

The block hash commits to the header only:

```
block_hash = domain_hash(ONX_BLOCK_HDR_V1, header_bytes)   # 160-byte header
```

(`ONX_BLOCK_HDR_V1`'s spec-table description is amended from "148-byte
header" to "160-byte header"; the tag string is unchanged — tags are
frozen.)

The signature section is appended after the header in the block file:

```
sig_section = count(u32be) || [validator_index(u32be) || sig(64)]*
```

Block file layout (magic bumped ONXBLK04 → ONXBLK05):

```
magic "ONXBLK05"(8) || header(160) || sig_section || body(u32be count || [u32be len || wire]*)
```

Rationale: signatures authenticate the header; including them *in* the
hashed bytes would make the hash self-referential. Keeping them outside
also means the STF never sees signatures — verification lives in the
block-acceptance layer (see §5).

### 3. Signing preimage

```
sign_bytes = pad32("ONX_BLOCK_SIG_V1") || chain_id || block_hash   # 96 bytes
```

- `pad32`: the ASCII tag zero-padded to 32 bytes (domain-separation style,
  consistent with §4.5).
- `chain_id`: 32 bytes (the genesis hash).
- `block_hash`: 32 bytes (from §2).

The signature is a standard Ed25519 signature over `sign_bytes`. Binding
the chain ID makes a signature valid on exactly one chain (ADR-0005);
binding the block hash makes it valid for exactly one block.

### 4. Validator set and signing rule

- `validator_index` is the validator's position in the genesis validator
  list **sorted by public-key bytes** (canonical order). Sorting removes
  any dependence on config-file order.
- Signing rule from genesis: a block is authenticated iff it carries valid
  signatures from genesis validators holding **more than 2/3 of total
  genesis stake**. With a single-validator genesis (all current fixtures),
  that means exactly one signature from the sole validator. The rule's
  shape survives into BFT; only the validator set becomes dynamic.
- Canonical signature-section rules (fail closed):
  - entries sorted strictly ascending by `validator_index`;
  - no duplicate indices;
  - every index in range of the genesis validator list;
  - until multi-validator consensus exists, multi-validator geneses are
    rejected at startup (single validator only).
- The >2/3 rule is evaluated against **genesis stake**, not current stake:
  no staking transactions exist yet, so the genesis set is the authority.

### 5. Verification lives outside the STF

Signature verification happens in the block-acceptance layer, **not** in
the STF. The STF stays a pure state-transition function (header in,
state out); authentication is a separate concern. `onx replay` and the
node share one verifier implementation — two verifiers would be a
consensus-split waiting to happen.

Verification steps for a candidate block:
1. Parse header (strict 160-byte decode) and signature section (strict).
2. Recompute `block_hash`; reject on mismatch with `prev_hash` linkage.
3. For each signature entry: look up the validator pubkey by index,
   check the Ed25519 predicate (§6), verify the signature over
   `sign_bytes`.
4. Sum the stake of valid signers; require > 2/3 of genesis total.
5. Check `block_time` monotonicity and `protocol_version` support.

### 6. Ed25519 predicate pinned (moved into ONXBLK05 from Wave 4)

Every validator public key — in genesis configs, in the acceptance layer,
in the explorer, in the Python reference — must pass the strict predicate:
**canonical encoding** (recompress-compare: `decompress(buf).compress() ==
buf`), **on-curve**, **large-order** (reject the 8 small-order points and
their `y+p` aliases). This closes the small-order-validator-key forgery
class: a small-order key would let an attacker forge "valid" headers the
explorer accepts. `onx-genesis` already rejects such keys (PR #10); this
ADR makes the predicate a consensus rule, not just a config-time check.

### 7. What ONXBLK05 does NOT do

- **Freshness**: signatures prove *authenticity* (who produced the block)
  and give *fork evidence* (two signed headers at one seqno = provable
  equivocation). They do not prove freshness — a stale but validly signed
  chain is indistinguishable from a live one. Freshness comes from the
  signed `head.json` (chain_id, seqno, block_hash, wall_time) with
  explorer staleness warnings — separate work, not this ADR.
- **BFT**: the >2/3 rule is evaluated against a static genesis set. Dynamic
  validator sets, voting rounds, and finality are the multi-validator
  phase, not here.
- **Issuance**: `block_time` exists, but issuance is explicitly NOT
  implemented on devnet-1 (no consensus time in single-producer; operator
  discretion over timestamps is visible but unconstrained).

## Consequences

- Block file magic ONXBLK04 → ONXBLK05; v4 files are rejected at the magic
  check, never silently misparsed (same policy as the V3→V4 bump).
- `ONX_BLOCK_HDR_V1` now hashes 160 bytes; every implementation must move
  together (spec + Rust + Python + explorer move in the same milestone —
  step 3/6).
- The fixture rekey (PR #12) was the prerequisite: the old DEV-label
  validator key had no known private key and could never sign. Test
  vectors sign with `test_secret_key(0x11)`.
- New domain tag `ONX_BLOCK_SIG_V1` is frozen on first vector publication.
- Storage v4 (step 4) must persist the signature section in the DB and
  commit block + signature atomically; TRAP 2 (signature only in the file)
  is a consensus hazard on crash recovery.
