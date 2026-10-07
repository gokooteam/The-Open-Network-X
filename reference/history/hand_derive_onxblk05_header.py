"""Independent re-derivation of the hand header vector for ONXBLK05 (160 bytes).

Source: spec documents only —
  - docs/specification/state-model.md §4: 160-byte header, protocol_version
    (u32be) + block_time (u64be) appended after msg_count (ADR-0032)
  - docs/specification/protocol-primitives.md §4.5: domain tags
No Rust implementation code was consulted; only the published 148-byte
layout (same vector inputs as reference/history/hand_derive_vectors_msg.py)
plus the ADR-0032 tail fields.

The script first reproduces the published 148-byte HAND_HEADER_HEX to prove
the pipeline matches the old vector, then extends it.
"""

import hashlib
import struct

def pad32(tag: str) -> bytes:
    raw = tag.encode("ascii")
    assert len(raw) <= 32
    return raw + b"\x00" * (32 - len(raw))

def domain_hash(tag: str, data: bytes) -> bytes:
    return hashlib.sha256(pad32(tag) + data).digest()

# Vector inputs (same human-chosen values as hand_derive_vectors_msg.py)
CHAIN_ID  = bytes([0xDD]) * 32
FROM      = bytes([0x11]) * 32
TO        = bytes([0x22]) * 32
NONCE     = 7
KIND      = 0
AMOUNT    = 1000
FEE       = 10
MESSAGE   = b""
PUBKEY    = bytes([0x44]) * 32
SIG_PLACEHOLDER = bytes([0x33]) * 64

def body_bytes() -> bytes:
    out = b""
    out += CHAIN_ID
    out += FROM
    out += struct.pack(">Q", NONCE)
    out += struct.pack("B", KIND)
    out += TO
    out += struct.pack(">Q", AMOUNT >> 64) + struct.pack(">Q", AMOUNT & 0xFFFFFFFFFFFFFFFF)
    out += struct.pack(">Q", FEE >> 64) + struct.pack(">Q", FEE & 0xFFFFFFFFFFFFFFFF)
    out += struct.pack(">I", len(MESSAGE))
    out += MESSAGE
    out += PUBKEY
    return out

BODY = body_bytes()
WIRE = BODY + SIG_PLACEHOLDER
MSG_HASH = domain_hash("ONX_MSG_EXT_V1", WIRE)
MSGS_ROOT = domain_hash("ONX_MSGS_ROOT_V1", MSG_HASH)

# 148-byte legacy prefix
SEQNO = 42
PREV_HASH = bytes([0xAA]) * 32
STATE_ROOT = bytes([0xBB]) * 32
LT = 1_000_000
WORKCHAIN = -1
FEE_COLLECTOR = bytes([0xCC]) * 32
MSG_COUNT = 1

HEADER148 = b""
HEADER148 += struct.pack(">I", SEQNO)
HEADER148 += PREV_HASH
HEADER148 += MSGS_ROOT
HEADER148 += STATE_ROOT
HEADER148 += struct.pack(">Q", LT)
HEADER148 += struct.pack(">i", WORKCHAIN)
HEADER148 += FEE_COLLECTOR
HEADER148 += struct.pack(">I", MSG_COUNT)
assert len(HEADER148) == 148

PUBLISHED = ("0000002aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2e63c6aa14a2b13de273f57ae0e8071fcad84badcdbf0f1e0a7e597c7c49515ebbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb00000000000f4240ffffffffcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc00000001")
assert HEADER148.hex() == PUBLISHED, "pipeline drift vs published 148-byte vector"
print("OK: 148-byte pipeline reproduces the published vector")

# ONXBLK05 tail (human-chosen vector values, documented in the test)
PROTOCOL_VERSION = 1
BLOCK_TIME = 1_700_000_000  # fixed wall-clock stand-in, arbitrary but documented

HEADER160 = HEADER148 + struct.pack(">I", PROTOCOL_VERSION) + struct.pack(">Q", BLOCK_TIME)
assert len(HEADER160) == 160
HEADER_HASH = domain_hash("ONX_BLOCK_HDR_V1", HEADER160)

print("HAND_HEADER_HEX (160) =", HEADER160.hex())
print("HAND_HEADER_HASH_HEX  =", HEADER_HASH.hex())
print("protocol_version bytes [148..152] =", HEADER160[148:152].hex())
print("block_time bytes      [152..160] =", HEADER160[152:160].hex())
