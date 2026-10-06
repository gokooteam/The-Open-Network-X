# ONX Python Reference Implementation

An independent Python implementation of the ONX message-model block
execution, derived from the specification documents — **not** from the
Rust code.

## Why

ONX's identity is "built from the specification." A second implementation,
written only from `docs/specification/` and `docs/adr/`, is the proof:
if two independent implementations agree on every byte, the spec is
unambiguous and the code is faithful to it.

## Scope

Implemented (message-model milestone, ADR-0001–0007):
- Domain hashing and big-endian codecs (`primitives.py`)
- Ed25519 key derivation, signing, verification — pure Python, stdlib
  only, cross-checked byte-for-byte against libsodium (`ed25519.py`)
- Cells and domain-separated cell hashing, spec §4.2/§4.3 (`cell.py`)
- Shard state trie and state-root computation, spec §4.5 (`trie.py`)
- Codeless account record encoding (141 bytes), spec §4.1 (`account.py`)
- External/internal message codecs, identities, block header (148 bytes),
  block files (`ONXBLK04`), key-derived addresses — ADR-0002, ADR-0006
  (`messages.py`)
- Wallet handler (chain-ID/key/nonce/signature checks, fee split,
  debit, key-reveal storage), FIFO delivery, bounce semantics,
  double-delivery protection (`chain.py`)

Deliberate non-goals:
- **TVM execution.** Contract calls bounce in this reference (per
  ADR-0004, a delivery that would run contract code is out of scope
  here). All vector messages are plain transfers.
- **Contract accounts.** Only codeless accounts are encoded (the 141-byte
  record). Code/data cell embedding is not implemented.
- **Frozen/Destroyed accounts, persistent storage, networking,
  consensus.** The reference is single-shard, in-memory, file-replayed —
  the same discipline as the Rust single-node scope.
- **Performance.** The pure-Python Ed25519 takes ~10ms per operation.
  This is a correctness reference, not a validator.

## Where the spec was silent

The design comes from the specs; a few field-level details are confirmed
against the implementation (documented here, not hidden):
- Signature preimage is `pad32(tag) || body` signed directly (not hashed
  first) — confirmed in `onx-primitives/src/signature.rs`.
- Fee split rounding: `burn = fee * 50 // 100`, `validator = fee - burn`
  (`onx-economics`).
- Genesis accounts start at `lt = 0, nonce = 0`; wallet sets sender
  `lt` to the block lt; delivery sets receiver `lt` to the block lt.
- `EMPTY_SUBTREE` is 13 bytes (`docs/specification/state-model.md`
  §4.5 says 12 — a spec bug; the code and all roots use 13).

## Running it

Stdlib only — no install needed:

```sh
# Run the reference end-to-end (generates reference/vectors/)
python3 reference/gen_vectors.py

# Check checked-in vectors against a fresh run (CI; fails on drift)
python3 reference/gen_vectors.py --check

# Ed25519 self-test (cross-checked against libsodium)
python3 -c "import sys; sys.path.insert(0, 'reference'); import ed25519; ed25519.self_test()"
```

## Vectors

`reference/vectors/` holds the checked-in fixtures (see `vectors/README.md`):
`genesis.json`, `blocks.json`, `expectations.json`. They mirror the Rust
`phase5_replay` fixture (4 accounts, 5 blocks × 4 messages, seed `0xC10C`)
so the two implementations are directly comparable — and the Rust `onx
replay` binary accepts the Python-generated block files byte-for-byte.

## History

`reference/history/` preserves the original hand-derivation scripts that
cross-checked the Rust implementation during development. They are
superseded by this package and are not run by CI. (Note:
`hand_derive_vectors_msg.py` needs PyNaCl; the reference itself is
stdlib-only.)
