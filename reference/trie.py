#!/usr/bin/env python3
"""Shard state trie and state-root computation.

Derived from docs/specification/state-model.md §4.5 (Shard State Trie
Node Encoding).

The trie maps 256-bit account IDs to account-record byte strings. Every
node IS a cell; node hashes are cell hashes. The tree shape is a pure
function of the key set (input order independent).

Construction (build_trie(items, bit_depth=0)):
  - empty set  -> cell(data=b"EMPTY_SUBTREE")   [13 bytes, not 12 —
                  spec §4.5 row said 12, the ASCII is 13; the code and all
                  roots use 13]
  - single item (or depth >= 256) -> leaf:
      value split into 128-byte chunks, each chunk a cell;
      leaf cell: data = 32-byte key, refs = chunk hashes in order
      (max 4 chunks -> 512-byte values)
  - otherwise -> branch:
      bit (key[depth//8] >> (7 - depth%8)) & 1 selects left (0) / right (1)
      branch cell: data = b"", refs = [left_hash, right_hash]

state_root(accounts) = hash of the root cell for the full key set.
"""

from .cell import Cell

EMPTY_SUBTREE_DATA = b"EMPTY_SUBTREE"  # 13 bytes
assert len(EMPTY_SUBTREE_DATA) == 13

CHUNK_SIZE = 128
MAX_CHUNKS = 4


def _empty_cell() -> Cell:
    return Cell(EMPTY_SUBTREE_DATA, [])


def _leaf_cell(key: bytes, value: bytes) -> Cell:
    if len(key) != 32:
        raise ValueError("trie key must be 32 bytes")
    chunks = [value[i : i + CHUNK_SIZE] for i in range(0, len(value), CHUNK_SIZE)]
    if len(chunks) > MAX_CHUNKS:
        raise ValueError(f"value too large for trie: {len(value)} bytes")
    if not chunks:
        chunks = [b""]
    chunk_hashes = [Cell(c, []).hash() for c in chunks]
    return Cell(key, chunk_hashes)


def _build(items: list, bit_depth: int) -> Cell:
    if not items:
        return _empty_cell()
    if len(items) == 1 or bit_depth >= 256:
        (key, value) = items[0]
        return _leaf_cell(key, value)
    byte_idx = bit_depth // 8
    bit_idx = 7 - (bit_depth % 8)
    left = []
    right = []
    for (key, value) in items:
        if (key[byte_idx] >> bit_idx) & 1:
            right.append((key, value))
        else:
            left.append((key, value))
    left_cell = _build(left, bit_depth + 1)
    right_cell = _build(right, bit_depth + 1)
    return Cell(b"", [left_cell.hash(), right_cell.hash()])


def build_trie(items: list) -> Cell:
    """Build the trie root cell from [(key, value)] pairs.

    Keys must be unique 32-byte strings. Order-independent.
    """
    keys = [k for (k, _) in items]
    if len(set(keys)) != len(keys):
        raise ValueError("duplicate trie keys")
    for k in keys:
        if len(k) != 32:
            raise ValueError("trie key must be 32 bytes")
    return _build(list(items), 0)


def state_root(accounts: dict) -> bytes:
    """accounts: {account_id_bytes: record_bytes} -> 32-byte state root."""
    return build_trie(list(accounts.items())).hash()
