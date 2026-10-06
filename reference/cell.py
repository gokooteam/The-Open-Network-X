#!/usr/bin/env python3
"""Cells and domain-separated cell hashing.

Derived from docs/specification/state-model.md §4.2 (cell binary
serialization) and §4.3 (domain-separated cell hashing).

A cell: up to 128 bytes of data, up to 4 references (child cell hashes).
  descriptor: d1 = ref_count (1 byte), d2 = data length (1 byte)
  cell hash = SHA256(pad32("ONX_CELL_HASH_V1") || d1 || d2 || data ||
                     ref_hash_1 || ... || ref_hash_k)
"""

from .primitives import domain_hash

MAX_DATA_BYTES = 128
MAX_REFS = 4
CELL_HASH_TAG = "ONX_CELL_HASH_V1"


class Cell:
    def __init__(self, data: bytes, refs: list):
        """refs: list of 32-byte child cell hashes."""
        if len(data) > MAX_DATA_BYTES:
            raise ValueError(f"cell data too long: {len(data)}")
        if len(refs) > MAX_REFS:
            raise ValueError(f"too many refs: {len(refs)}")
        for r in refs:
            if len(r) != 32:
                raise ValueError("ref hash must be 32 bytes")
        self.data = bytes(data)
        self.refs = [bytes(r) for r in refs]

    def descriptor(self) -> bytes:
        return bytes([len(self.refs), len(self.data)])

    def to_bytes(self) -> bytes:
        out = bytearray(self.descriptor())
        out += self.data
        for r in self.refs:
            out += r
        return bytes(out)

    def hash(self) -> bytes:
        return domain_hash(
            CELL_HASH_TAG, self.descriptor() + self.data + b"".join(self.refs)
        )

    def __repr__(self):
        return f"Cell(data={len(self.data)}b, refs={len(self.refs)})"
