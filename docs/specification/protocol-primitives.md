# ONX Specification — Protocol Primitives

**Status:** Draft
**Scope:** Fundamental cryptographic primitives, integer encodings, bitstrings, byte encodings, hashing, signatures, and domain separation for Open Network X (ONX).

---

## 1. Reference

- `WHITEPAPER.md`, §2.2.8–§2.2.10: Block hashes and sha256 assumptions.
- `WHITEPAPER.md`, §2.3.1: 256-bit ECC public keys and account identifiers.
- `WHITEPAPER.md`, §11 (in `INSTRUCTIONS.md`): Cryptographic primitives requirements, domain separation, and deterministic serialization.
- `docs/specification/architecture.md`: Open question ONX-ARCH-001 regarding canonical serialization and cryptographic primitives.

---

## 2. Requirement

The protocol specification requires robust, deterministic, and independently testable cryptographic and encoding primitives. Specifically:
1. Canonical integer, bitstring, and byte string encodings across all protocol structures.
2. Standardized cryptographic hash functions providing collision resistance and preimage resistance for state trees and block hashes.
3. Standardized digital signature schemes for transaction signing, block signatures, and validator consensus.
4. Mandatory domain separation for all cryptographic hashing and signing operations to prevent cross-context replay attacks.
5. Explicit, deterministic rejection rules for all malformed inputs.

---

## 3. ONX Interpretation

1. **Integer Types:** Integers in consensus-critical data structures are fixed-width big-endian unsigned (`uint8`, `uint16`, `uint32`, `uint64`, `uint128`, `uint256`) or signed two's complement (`int8` through `int256`) integers. Variable-length integer encodings must be explicitly length-prefixed and bounded.
2. **Bitstrings and Byte Strings:** Raw data sequences are represented as canonical bitstrings or byte strings. Byte strings are bitstrings whose bit length is a multiple of 8. Bit alignment is MSB-first (big-endian bit ordering within bytes).
3. **Cryptographic Hashing:** The default 256-bit cryptographic hash function for ONX consensus, block identifiers, Merkle tree nodes, and transaction digests is **SHA-256** (FIPS PUB 180-4).
4. **Digital Signatures:** The baseline public key signature scheme for accounts, transaction authorization, and validator block signing is **Ed25519** (RFC 8032 / Edwards-curve Digital Signature Algorithm over Curve25519). Public keys are 32-byte Ed25519 public keys; signatures are 64-byte Ed25519 signatures.
5. **Domain Separation:** Every hash computation or signature digest must prepend a unique 32-byte domain separation tag (or prefixed ASCII string with explicit length and purpose identifier) to prevent cross-purpose signature or hash collision exploits between transactions, block headers, state cells, and network datagrams.

---

## 4. Serialization

### 4.1 Integer Encoding
- All `uintN` and `intN` types are serialized in **big-endian (network byte order)** binary representation occupying exactly $N/8$ bytes.
- Examples:
  - `uint32` value `0x01020304` is serialized as bytes `[0x01, 0x02, 0x03, 0x04]`.
  - `uint256` is encoded as 32 contiguous big-endian bytes.

### 4.2 Bitstrings & Byte Strings
- A byte string of length $L$ bytes is serialized directly as $L$ consecutive bytes.
- Bounded variable-length byte strings are serialized as a big-endian length prefix (`uint16` or `uint32` depending on container type) followed immediately by the payload bytes.

### 4.3 Hash Outputs
- SHA-256 digest outputs are 32 bytes (`256` bits) encoded directly as 32 big-endian bytes.

### 4.4 Cryptographic Keys and Signatures
- **Ed25519 Public Key:** 32 bytes (encoded according to RFC 8032 §5.1.5).
- **Ed25519 Private Key / Seed:** 32 bytes secret seed.
- **Ed25519 Signature:** 64 bytes ($R \parallel s$, encoded according to RFC 8032 §5.1.6).

### 4.5 Domain Separation Prefixes
All protocol hashing contexts must prepend an explicit domain separation tag before hashing. `domain_hash(tag, msg) = SHA256(pad32(tag) || msg)`, where `pad32` is the ASCII tag zero-padded to 32 bytes. **Code is the authority for tag strings**: tags are frozen in golden test vectors and MUST NOT be renamed — a renamed tag silently forks every hash it touches.

**Consensus spine** (the deterministic-replay path; frozen):

| Tag string              | Domain-separates |
|-------------------------|------------------|
| `ONX_MSG_EXT_V1`        | External message identity: `SHA256(pad32 \|\| wire)` (signature included) |
| `ONX_MSG_EXT_SIGN_V1`    | External message signature payload: `SHA256(pad32 \|\| body)` — the body includes `chain_id` |
| `ONX_MSG_INT_V1`        | Internal message delivery ID: `SHA256(pad32 \|\| canonical bytes)` |
| `ONX_MSGS_ROOT_V1`      | Ordered external-message-set commitment in the block header |
| `ONX_ADDR_V1`           | Key-derived address: `SHA256(pad32 \|\| pubkey)` |
| `ONX_BLOCK_HDR_V1`      | Block header identity: `SHA256(pad32 \|\| 160-byte header)` (ADR-0032; was 148 bytes before ONXBLK05) |
| `ONX_BLOCK_SIG_V1`      | Block signature preimage: `pad32("ONX_BLOCK_SIG_V1") \|\| chain_id \|\| block_hash` (ADR-0032) |
| `ONX_CELL_HASH_V1`      | Cell representation hash (see `state-model.md` §4.3) |
| `ONX_GENESIS_V1`        | Genesis document hash, which doubles as the chain ID |
| `ONX_GENESIS_ADDR_V1`   | `AccountId` derivation from a genesis config label |
| `ONX_GENESIS_VALKEY_V1` | Validator public-key derivation from a config label (dev-only; derived keys have no known private key) |

**Orphan crates** (pre-spine code, not exercised by `onx replay`; tags live on until those crates are integrated or retired):

| Tag string              | Used by |
|-------------------------|---------|
| `ONX_BLK_HDR_V1`        | `onx-data-structures` block hash |
| `ONX_TX_BODY_V1`        | `onx-execution` interpreter and `onx-payment-channels` signature payloads |
| `ONX_MSG_HASH_V1`       | `onx-data-structures` / `onx-transactions` message hashing |
| `ONX_VALIDATOR_SIGN_V1` | `onx-networking` validator/DHT signatures |
| `ONX_EXEC_HASH_V1`      | `onx-execution` interpreter |
| `ONX_CHANNEL_STATE_V1`  | `onx-payment-channels` channel state |
| `ONX_DHT_RECORD_V1`     | `onx-networking` DHT records |
| `ONX_ADNL_CHANNEL_V1`, `ONX_ADNL_KEY_DESC_V1`, `ONX_ADNL_PAYLOAD_V1`, `ONX_ADNL_SHARED_SECRET_V1` | `onx-networking` ADNL transport |

**Retired / dead** (kept out of new code; documented so nobody reuses the strings):

| Tag string            | Status |
|-----------------------|--------|
| `ONX_TX_V1`, `ONX_TXS_ROOT_V1` | Dropped with the V1 transaction encoding (pre-release, never shipped) |
| `ONX_TX_V2`, `ONX_TX_V2_SIGN`, `ONX_TXS_ROOT_V2` | Dropped with the V2 transaction encoding (pre-release, never shipped) |
| `ONX_TX_V3`, `ONX_TX_V3_SIGN`, `ONX_TXS_ROOT_V3` | Dropped with the V3 transaction encoding — replaced by the message model (ADR-0001/0002), pre-release, never shipped |
| `ONX_TRIE_NODE_V1`    | Defined in `onx-state-model/src/tree.rs` but never used — trie nodes hash as plain cells (`ONX_CELL_HASH_V1`) |

**Test-only** (never consensus): `ONX_TEST_KEY_V1`, `ONX_PROBE_KEY_V1`.

---

## 5. Malformed-input behavior

Any implementation parsing or verifying protocol primitives MUST fail immediately and reject the input if any of the following occur:
1. **Truncated Input:** Fewer bytes are provided than required for the integer width, fixed-length byte string, public key (32 bytes), or signature (64 bytes).
2. **Over-length / Trailing Bytes:** Unparsed extra bytes trailing a fixed-size primitive buffer.
3. **Non-canonical Public Key / Signature:** Ed25519 public key or signature component ($s$) exceeding the Curve25519 group order $L$ or non-canonical point encoding as specified in RFC 8032.
4. **Invalid Domain Prefix:** Missing, malformed, or unexpected domain separation prefix in hash/signature verification payloads.
5. **Integer Overflow / Out of Range:** Unsigned integer decoded values exceeding the specified type bit-width.

---

## 6. Test plan

1. **Unit Tests for Integer Serialization:**
   - Test big-endian conversion for zero, maximum value, minimum value, and boundary cases across `uint8` through `uint256`.
2. **SHA-256 Vector Verification:**
   - Verify NIST standard SHA-256 test vectors.
   - Verify ONX domain-separated hash outputs against expected golden test vectors.
3. **Ed25519 Signature Test Vectors:**
   - Verify RFC 8032 test vectors for key generation, signing, and verification.
   - Test non-canonical signature rejection ($s \ge L$).
   - Test signature rejection when domain separation tags differ.
4. **Negative / Adversarial Tests:**
   - Test truncated byte buffer rejection for all primitive deserializers.
   - Test unexpected trailing byte rejection.
