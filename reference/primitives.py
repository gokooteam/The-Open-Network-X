#!/usr/bin/env python3
"""Primitive codecs for the ONX reference implementation.

Derived from:
  - docs/specification/state-model.md §4.2 (cell serialization),
    §4.3 (domain-separated hashing)
  - docs/adr/0002 (message encodings; domain tags are zero-padded to 32
    bytes before hashing or signing)

All integers are big-endian, strict width, no trailing bytes.
"""

import hashlib


def pad32(tag: str) -> bytes:
    """Zero-pad an ASCII domain tag to 32 bytes."""
    raw = tag.encode("ascii")
    if len(raw) > 32:
        raise ValueError(f"domain tag too long: {tag!r}")
    return raw + b"\x00" * (32 - len(raw))


def domain_hash(tag: str, data: bytes) -> bytes:
    """SHA256(pad32(tag) || data)."""
    return hashlib.sha256(pad32(tag) + data).digest()


def u32be(v: int) -> bytes:
    if not 0 <= v < 2**32:
        raise ValueError(f"u32 out of range: {v}")
    return v.to_bytes(4, "big")


def u64be(v: int) -> bytes:
    if not 0 <= v < 2**64:
        raise ValueError(f"u64 out of range: {v}")
    return v.to_bytes(8, "big")


def u128be(v: int) -> bytes:
    if not 0 <= v < 2**128:
        raise ValueError(f"u128 out of range: {v}")
    return v.to_bytes(16, "big")


def i32be(v: int) -> bytes:
    if not -(2**31) <= v < 2**31:
        raise ValueError(f"i32 out of range: {v}")
    return v.to_bytes(4, "big", signed=True)


def h(b: bytes) -> str:
    """Hex-encode bytes (for vector files)."""
    return b.hex()


def uh(s: str) -> bytes:
    """Hex-decode to bytes."""
    return bytes.fromhex(s)
