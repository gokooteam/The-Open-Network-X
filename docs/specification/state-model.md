# ONX Specification — State Model and Account Lifecycle

**Status:** Draft
**Scope:** Account states, account lifecycle transitions, authenticated state representation (Cell trees and Bag-of-Cells), Merkle-Patricia trees, state commitments, and deterministic state transitions for Open Network X (ONX).

---

## 1. Reference

- `WHITEPAPER.md`, §2.3.1–§2.3.18: Account IDs, hashmaps, smart contract persistent storage, TVM cells, and Merkle proofs.
- `WHITEPAPER.md`, §2.5.1–§2.5.15: Bag-of-Cells representation, acyclic directed graphs of cells, and global state hashing.
- `INSTRUCTIONS.md`, §10, §12, §16, §17: Consensus-critical code, serialization standards, Virtual Machine semantics, and smart contract behavior.
- `docs/specification/architecture.md`: Open question ONX-ARCH-003 regarding state representation and state transitions.
- `docs/specification/protocol-primitives.md`: SHA-256 digests, Ed25519 signatures, and integer encoding.
- `docs/specification/data-structures.md`: Workchain IDs, Account IDs, and Full Addresses.

---

## 2. Requirement

The ONX protocol requires an authenticated, deterministic state model governing account storage and state transitions across all workchains and shardchains. Specifically:
1. Formal definitions of account lifecycle states and allowable state transitions.
2. Authenticated, tree-based storage representation capable of producing compact Merkle proofs for light-node verification.
3. Canonical root hash calculation for individual account states and entire shard states (Bag-of-Cells root hashes).
4. Strictly deterministic state transitions driven by validated messages and VM execution steps.
5. Explicit rejection criteria for malformed state roots, invalid state transition requests, or unauthenticated state modifications.

---

## 3. ONX Interpretation

1. **Account Lifecycle States:**
   Every account address `(workchain_id, account_id)` exists in exactly one of four canonical states:
   - **Uninitialized (`0x00`):** The account address has zero balance, no code, no persistent storage, and has never processed a transaction.
   - **Active (`0x01`):** The account has non-zero balance, associated smart contract code, persistent cell storage, and logical time tracking. It accepts incoming and outgoing messages.
   - **Frozen (`0x02`):** The account's balance fell below storage fee requirements or was explicitly frozen. Storage is pruned and replaced with a 32-byte storage hash state commitment. No code execution is permitted until unfrozen via balance replenishment and valid unfreeze transaction.
   - **Destroyed (`0x03`):** The account was explicitly closed or permanently deleted. Zero balance remains, and all associated cell storage is deleted.

2. **Account State Record (`AccountState`):**
   An active account state consists of:
   - `balance_nanos`: 128-bit unsigned integer (`uint128`) tracking Onyxi currency balance.
   - `last_trans_lt`: 64-bit unsigned integer (`uint64`) tracking the logical time of the latest executed transaction.
   - `code_hash`: 256-bit SHA-256 digest (`uint256`) of the contract code cell tree.
   - `data_hash`: 256-bit SHA-256 digest (`uint256`) of the contract storage cell tree.
   - `storage_stat`: Account storage resource consumption parameters (cell count, byte count).

3. **Cell and Bag-of-Cells (BoC) Model:**
   - All state data, code, and storage are structured as directed acyclic graphs of **Cells**.
   - A standard Cell contains up to 128 bytes of data and up to 4 references (child links) to other Cells.
   - A **Bag-of-Cells (BoC)** is a serialized sequence of Cells forming a rooted directed acyclic graph.
   - The **State Root Hash** of a BoC is the 32-byte domain-separated SHA-256 hash of its root cell.

4. **Shard State Tree:**
   - The state of all accounts within a shard is represented as a Merkle-Patricia tree mapping `account_id` (256-bit key) to `AccountState`.
   - The overall shard state commitment is the 32-byte SHA-256 Merkle root hash of the shard account tree (`state_root_hash` in `BlockHeader`).

5. **State Transitions:**
   - State transitions are strictly deterministic functions $S' = \text{Apply}(S, M)$ where $S$ is the prior authenticated state, $M$ is a validated block or message batch, and $S'$ is the resultant authenticated state.
   - Uninitialized accounts can transition to Active upon receiving a valid transaction carrying deployment code/data and sufficient initial balance.

---

## 4. Serialization

### 4.1 Account State Record Layout
Active account state record binary layout (`AccountState`), 141 bytes total:
```
1. state_type     : uint8   (0x01 for Active)
2. balance_nanos  : uint128 (16 bytes, big-endian)
3. last_trans_lt : uint64  (8 bytes, big-endian logical time)
4. code_hash     : uint256 (32 bytes, SHA-256 code root hash)
5. data_hash     : uint256 (32 bytes, SHA-256 storage root hash)
6. cell_count    : uint32  (4 bytes, total cells used)
7. byte_count    : uint64  (8 bytes, total bytes used)
8. pubkey        : uint256 (32 bytes, Ed25519 public key authorizing spends;
                            all-zero = keyless: can receive, never spend)
9. nonce         : uint64  (8 bytes, big-endian per-account sequence number)
```
The `pubkey`/`nonce` pair is the message-authorization mechanism
(`ONX_MSG_EXT_V1`, see `docs/adr/0002-message-encoding-and-domain-tags.md`):
an external message is valid only if its signature verifies against the
sender's `pubkey`, its `nonce` equals the account's `nonce`, and the
message binds this chain's ID; successful application bumps the nonce by
one, which is what makes message replay impossible. Nonces start at 0 for
genesis accounts. Accounts created by receiving funds derive their
address from their owner's public key (`ONX_ADDR_V1`,
`docs/adr/0006-key-derived-addresses.md`), so a new account can receive
first and spend later by revealing the key that hashes to its address
(`pubkey` all-zero marks a keyless account that can receive but never
spend).

### 4.2 Cell Binary Serialization
A single Cell binary structure:
```
1. descriptor_bytes : uint16 (byte 0: d1 = ref_count | (is_special ? 0x08 : 0);
                              byte 1: d2 = data_byte_length)
2. data_bytes       : [uint8; d2] (0 to 128 raw payload bytes)
3. cell_refs        : [uint256; ref_count] (32-byte SHA-256 child cell hashes, 0 to 4 refs)
```
The special flag occupies bit 3 of `d1` (`d1 = ref_count | (special << 3)`);
bits 0–2 carry the reference count (0–4). `d2` is the exact data length in
bytes. All trie cells in §4.5 are non-special, so their `d1` equals the
reference count exactly.

### 4.3 Domain-Separated Cell Hashing
Cell representation hash $H(\text{Cell})$ is computed as:
$$\text{CellHash} = \text{SHA256}(\text{pad}_{32}(\text{"ONX_CELL_HASH_V1"}) \parallel d_1 \parallel d_2 \parallel \text{data\_bytes} \parallel \text{ref\_hash}_1 \parallel \dots \parallel \text{ref\_hash}_k)$$
Where $\text{pad}_{32}$ is the ASCII tag `ONX_CELL_HASH_V1` zero-padded to
32 bytes. The child reference hashes are concatenated in reference order.
(An earlier draft of this formula wrote the tag as `ONX:CELL:HASH:V1`;
the implementation and all golden vectors use `ONX_CELL_HASH_V1`.)

### 4.4 Merkle Proof Structure
A Merkle proof object for an account state $A$ in shard root $R$:
```
1. magic_bytes   : uint32  (0x4D505246 = "MPRF")
2. target_key    : uint256 (32 bytes, account_id)
3. root_hash     : uint256 (32 bytes, shard state_root_hash)
4. proof_boc     : BoC     (Serialized Bag-of-Cells containing target path & sibling hashes)
```

### 4.5 Shard State Trie Node Encoding

The shard state is a binary Merkle-Patricia trie mapping 256-bit account
IDs to serialized `AccountState` records (§4.1). Every trie node IS a cell
(§4.2); node hashes are cell hashes (§4.3). There is no separate trie-node
hash domain — a node is identified solely by its cell hash. (The
implementation defines an `ONX_TRIE_NODE_V1` constant, but it is unused;
all node hashing goes through the cell hash.)

**Inputs.** The trie is built from the set of `(key, value)` pairs where
`key` is the 32-byte account ID and `value` is the canonical
`AccountState` byte string. Keys are unique. The tree shape is a pure
function of the key set: input order does not affect the root.

**Construction.** `build_trie(items, bit_depth)`, starting at `bit_depth = 0`:

1. **Empty item set** → a cell with data = ASCII `"EMPTY_SUBTREE"`
   (13 bytes, `0x454D5054595F53554254524545`), zero references. Its cell
   hash is the empty-subtree hash. Cells are content-addressed, so every
   empty subtree in the trie collapses to this single cell.

2. **Exactly one item, or `bit_depth >= 256`** → leaf node:
   - Split the value into 128-byte chunks (`ceil(len / 128)` chunks; the
     last chunk may be shorter). Each chunk becomes its own cell
     (data = chunk bytes, zero references); record the chunk hashes in
     order.
   - Leaf cell: data = the 32-byte key, references = the chunk hashes in
     chunk order.
   - A cell holds at most 4 references, so a value may be at most
     $4 \times 128 = 512$ bytes. A larger value fails trie construction
     fail-closed (no fallback root is ever substituted).
   - `bit_depth >= 256` with more than one item is unreachable: keys are
     unique 256-bit IDs, so at depth 256 every partition holds at most
     one key.

3. **Otherwise** → branch node:
   - `byte_idx = bit_depth / 8`, `bit_idx = 7 - (bit_depth % 8)`.
   - Test bit `(key[byte_idx] >> bit_idx) & 1` of each key: bit `0` goes
     left, bit `1` goes right. (Most-significant bit of key byte 0 is
     tested first; traversal proceeds MSB-to-LSB, byte by byte.)
   - Recurse into `build_trie(left, bit_depth + 1)` and
     `build_trie(right, bit_depth + 1)`.
   - Branch cell: data = empty (0 bytes), references =
     `[left_child_hash, right_child_hash]` (left first).

**State root.** `state_root_hash()` is the cell hash of the cell returned
for the full account set at depth 0. For an empty state, the root is the
hash of the `"EMPTY_SUBTREE"` cell.

**Descriptor values.** Trie cells are never special, so `d1` (§4.2) equals
the reference count exactly:
| Node type     | `d1` (refs) | `d2` (data len) | data         | refs              |
|---------------|-------------|-----------------|--------------|-------------------|
| Empty subtree | 0           | 13              | `"EMPTY_SUBTREE"` | —              |
| Leaf          | 1–4         | 32              | account ID   | value-chunk hashes|
| Branch        | 2           | 0               | —            | [left, right]     |

**Proof-path walk (verifier's view).** Node types are distinguished
structurally, never by a type tag:
- 32-byte data → **leaf**: the data MUST equal the target key, and every
  referenced value-chunk cell must be present.
- empty data with exactly 2 references → **branch**: the key bit at the
  current depth (same `byte_idx`/`bit_idx` rule) selects the child.
- anything else → malformed; verification fails closed.
- paths deeper than 256 are rejected.

### 4.6 Transaction Identity and Signature Domains (`ONX_TX_V2`)

> **SUPERSEDED.** The V2 transaction format below was retired with the
> message-model milestone (PR #5) and replaced by external/internal
> messages. Do not implement from this section. The current encodings are
> `ONX_MSG_EXT_V1` / `ONX_MSG_INT_V1`; see
> `docs/adr/0001-message-based-transaction-model.md`,
> `docs/adr/0002-message-encoding-and-domain-tags.md`, and the domain-tag
> registry in `docs/specification/protocol-primitives.md` §4.5. The V2
> details are preserved here for archaeology (old test vectors, git
> history).

A V2 transaction's canonical encodings:
```
body_bytes (104 bytes, big-endian):
  from         : uint256 (32 bytes, sender account ID)
  to           : uint256 (32 bytes, recipient account ID)
  amount_nanos : uint128 (16 bytes)
  fee_nanos    : uint128 (16 bytes)
  nonce        : uint64  (8 bytes, sender's per-account sequence number)

wire encoding (168 bytes): body_bytes || signature (64 bytes, Ed25519)
```

- **Signature message:** Ed25519 signs
  `SHA256(pad32("ONX_TX_V2_SIGN") || body_bytes)` — a 136-byte preimage.
  The signature covers every transaction field except itself.
- **Transaction identity hash** `Transaction::hash()`:
  `SHA256(pad32("ONX_TX_V2") || wire_bytes)` — the **full 168-byte wire
  encoding, signature included**. The identity commits to the
  authorization itself: two different signatures over the same body are
  two different transactions and can never share an identity.
- **Transaction-set commitment** (`txs_root`, committed in the block
  header): `SHA256(pad32("ONX_TXS_ROOT_V2") || tx_hash_0 || tx_hash_1 ||
  ...)` over the ordered transaction hashes; the empty body commits to
  `SHA256(pad32("ONX_TXS_ROOT_V2") || b"")`.
- **Block header identity**: `SHA256(pad32("ONX_BLOCK_HDR_V1") || header_bytes)`
  over the 160-byte canonical header encoding (ADR-0032; was 148 bytes
  before ONXBLK05).

The complete registry of domain separation tags — spine, orphan-crate,
and retired — is maintained in `docs/specification/protocol-primitives.md`
§4.5. Code is the authority for tag strings: tags are frozen in golden
vectors and MUST NOT be renamed.

---

## 5. Malformed-input behavior

Consensus execution and state transition validation MUST reject and fail immediately if:
1. **Invalid State Transition:** Attempting to execute contract code on an `Uninitialized`, `Frozen`, or `Destroyed` account without valid initialization/unfreeze payloads.
2. **Logical Time Regression:** A state update sets `last_trans_lt` $\le$ prior account `last_trans_lt`.
3. **Balance Underflow:** A transaction or fee deduction results in `balance_nanos < 0`.
4. **Invalid Cell Representation:** A Cell specifies `data_length > 128` or `ref_count > 4`.
5. **Cyclic Cell Reference:** A Bag-of-Cells graph contains a directed cycle (violating DAG invariant).
6. **State Root Mismatch:** Recomputed state root hash after block execution does not equal the block header's declared `state_root_hash`.
7. **Malformed Merkle Proof:** A Merkle proof fails to recompute to the expected `root_hash` or contains invalid sibling hashes.

---

## 6. Test plan

1. **Account State Machine Tests:**
   - Test state transitions: `Uninitialized` $\rightarrow$ `Active` $\rightarrow$ `Frozen` $\rightarrow$ `Active` $\rightarrow$ `Destroyed`.
   - Rejection tests for invalid transitions (e.g. executing transactions on `Destroyed` accounts).
2. **Cell Hashing & Bag-of-Cells Serialization Tests:**
   - Test deterministic SHA-256 cell hash computation with `ONX_CELL_HASH_V1` domain separation.
   - Test round-trip BoC serialization and deserialization across cell graphs of varying depths.
   - Test cycle detection in malformed cell references.
3. **Shard State Tree & Merkle Proof Tests:**
   - Construct a Merkle-Patricia tree of accounts and verify root hash calculation.
   - Generate Merkle proofs for existing accounts and verify proof verification logic.
   - Negative tests for forged Merkle proofs or modified sibling hashes.
4. **State Transition Determinism Tests:**
   - Re-execute identical transaction sequences from identical initial states and verify exact bit-for-bit `state_root_hash` equivalence.
