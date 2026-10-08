#!/usr/bin/env python3
"""Cells and domain-separated cell hashing.

Derived from docs/specification/state-model.md §4.2 (cell binary
serialization) and §4.3 (domain-separated cell hashing), plus ADR-0036
(bit-length commitment via the compatible flag).

A cell: up to 128 bytes of data, up to 4 references (child cell hashes).
  descriptor: d1 = ref_count | (special << 3) | (bit_granular << 4),
              d2 = data length (1 byte each)
  cell hash = SHA256(pad32("ONX_CELL_HASH_V1") || d1 || d2 || data ||
                     ref_hash_1 || ... || ref_hash_k)

Bit-granular cells (d1 bit 4 set) commit an exact bit length: the last data
byte carries a completion tag — a single 1 bit at position `bit_len`, zeros
below it. bit_len = 8*(n-1) + (7 - trailing_zeros(last_byte)).
Canonicality (fail-closed, every decoder enforces identically):
  - d1 bits 5-7 reserved, must be zero;
  - flagged cells are never empty;
  - a flagged cell's last byte is never 0x00 (no completion 1) nor 0x80
    (lone tag = zero data bits in the final byte, contradicting the flag).
"""

from .primitives import domain_hash

MAX_DATA_BYTES = 128
MAX_DATA_BITS = MAX_DATA_BYTES * 8
MAX_REFS = 4
CELL_HASH_TAG = "ONX_CELL_HASH_V1"
BIT_GRANULAR_FLAG = 0x10


class Cell:
    def __init__(self, data: bytes, refs: list, bit_granular: bool = False,
                 _skip_tag_check: bool = False):
        """refs: list of 32-byte child cell hashes.

        bit_granular=True marks a bit-granular cell: `data` must already carry
        the completion tag in its last byte (use Cell.finalize to build one
        from raw builder output). Plain Cell(data, refs) stays byte-granular.
        """
        if len(data) > MAX_DATA_BYTES:
            raise ValueError(f"cell data too long: {len(data)}")
        if len(refs) > MAX_REFS:
            raise ValueError(f"too many refs: {len(refs)}")
        for r in refs:
            if len(r) != 32:
                raise ValueError("ref hash must be 32 bytes")
        if bit_granular:
            if len(data) == 0:
                raise ValueError("bit-granular cell cannot be empty")
            if not _skip_tag_check:
                last = data[-1]
                if last == 0x00 or last == 0x80:
                    raise ValueError(
                        f"non-canonical completion tag: last byte {last:#04x}")
        self.data = bytes(data)
        self.refs = [bytes(r) for r in refs]
        self.bit_granular = bit_granular

    @classmethod
    def finalize(cls, data: bytes, bit_len: int, refs: list):
        """Build a cell from raw builder output: `data` holds the stored bytes,
        `bit_len` is the exact number of meaningful bits.

        Mirrors onx-state-model Cell::new_with_bit_len: byte-aligned bit
        lengths stay byte-granular; otherwise the completion tag is written
        into the last byte and the flag is set. Non-zero bits below the tag
        position fail closed (never silently masked).
        """
        if bit_len > MAX_DATA_BITS:
            raise ValueError(f"bit_len {bit_len} exceeds {MAX_DATA_BITS}")
        want = (bit_len + 7) // 8
        if len(data) != want:
            raise ValueError(
                f"data length {len(data)} != ceil({bit_len}/8) = {want}")
        data = bytearray(data)
        remainder = bit_len % 8
        if remainder == 0:
            return cls(bytes(data), refs, bit_granular=False)
        tag_mask = 1 << (7 - remainder)
        below_mask = tag_mask - 1
        if data[-1] & below_mask:
            raise ValueError("non-zero bits below the completion tag position")
        data[-1] |= tag_mask
        return cls(bytes(data), refs, bit_granular=True, _skip_tag_check=True)

    @classmethod
    def from_bytes(cls, buf: bytes):
        """Strict decoder: enforces the ADR-0036 canonicality rules."""
        if len(buf) < 2:
            raise ValueError("slice too short for cell descriptor")
        d1, d2 = buf[0], buf[1]
        ref_count = d1 & 0x07
        bit_granular = bool(d1 & BIT_GRANULAR_FLAG)
        if d1 & 0xE0:
            raise ValueError(f"reserved descriptor bits set: {d1:#04x}")
        if ref_count > MAX_REFS:
            raise ValueError(f"too many refs: {ref_count}")
        if d2 > MAX_DATA_BYTES:
            raise ValueError(f"cell data too long: {d2}")
        if len(buf) < 2 + d2 + 32 * ref_count:
            raise ValueError("truncated cell payload")
        data = buf[2:2 + d2]
        refs = [buf[2 + d2 + 32 * i:2 + d2 + 32 * (i + 1)]
                for i in range(ref_count)]
        return cls(data, refs, bit_granular=bit_granular)

    def descriptor(self) -> bytes:
        d1 = len(self.refs) | (BIT_GRANULAR_FLAG if self.bit_granular else 0)
        return bytes([d1, len(self.data)])

    def bit_len(self) -> int:
        """Exact meaningful data bits (completion tag excluded)."""
        if not self.bit_granular:
            return 8 * len(self.data)
        last = self.data[-1]
        # last != 0 by construction; trailing zeros of the completion tag.
        tz = (last & -last).bit_length() - 1  # index of lowest set bit
        return 8 * (len(self.data) - 1) + (7 - tz)

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
        return (f"Cell(data={len(self.data)}b, refs={len(self.refs)}, "
                f"bit_granular={self.bit_granular})")
