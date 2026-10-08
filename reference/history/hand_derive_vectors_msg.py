#!/usr/bin/env python3
"""Hand-derive golden vectors for the ONX message-model encodings.

INDEPENDENT derivation, straight from `docs/adr/0002-message-encoding-and-domain-tags.md`
— the Rust implementation was consulted only for parameters (hash primitive,
tag padding, which fields cover the signature), never for encoding logic.

Spec summary (ADR-0002):
  External message body (big-endian, strict):
      chain_id(32) || from(32) || nonce u64be(8) || kind(1) || to(32) ||
      amount_nanos u128be(16) || fee_nanos u128be(16) || msg_len u32be(4) ||
      message(msg_len) || pubkey(32)
  Wire = body || signature(64). Transfer bodies are 173 + 64 = 237 bytes.
  kind: 0 = Transfer (message must be empty), 1 = ContractCall.
  Message hash:  SHA256(pad32("ONX_MSG_EXT_V1")  || wire_bytes).
  Signature is over: pad32("ONX_MSG_EXT_SIGN_V1") || body_bytes.
  msgs_root:    SHA256(pad32("ONX_MSGS_ROOT_V1") || h0 || h1 || ...).
  Block header (148 bytes, big-endian):
      seqno u32be(4) || prev_hash(32) || msgs_root(32) || state_root(32) ||
      lt u64be(8) || workchain i32be(4) || fee_collector(32) || msg_count u32be(4)
  Header hash:  SHA256(pad32("ONX_BLOCK_HDR_V1")  || header_bytes).
  Address:      SHA256(pad32("ONX_ADDR_V1")       || pubkey).

Ed25519 signatures use PyNaCl/libsodium (NOT the Rust ed25519-dalek code) —
the Rust test then cross-checks the independent signature against its own
`verify_signature`, so both implementations must agree.

Human-chosen vector inputs (fixed below, arbitrary but distinguishable).
"""

import hashlib
import struct
from nacl.signing import SigningKey

# ---------------------------------------------------------------- parameters
def pad32(tag: str) -> bytes:
    raw = tag.encode("ascii")
    assert len(raw) <= 32
    return raw + b"\x00" * (32 - len(raw))

def domain_hash(tag: str, data: bytes) -> bytes:
    return hashlib.sha256(pad32(tag) + data).digest()

# ---------------------------------------------------------------- vector inputs
CHAIN_ID  = bytes([0xDD]) * 32   # chain identity (genesis hash stand-in)
FROM      = bytes([0x11]) * 32
TO        = bytes([0x22]) * 32
NONCE     = 7
KIND      = 0                    # Transfer
AMOUNT    = 1000
FEE       = 10
MESSAGE   = b""                  # Transfer: must be empty
PUBKEY    = bytes([0x44]) * 32   # nonzero reveal field (encoding vector)
SIG_PLACEHOLDER = bytes([0x33]) * 64  # pattern only; NOT a valid signature

# ---------------------------------------------------------------- body / wire
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
assert len(BODY) == 141 + 0 + 32 == 173, f"body len {len(BODY)}"
WIRE = BODY + SIG_PLACEHOLDER
assert len(WIRE) == 237, f"wire len {len(WIRE)}"

MSG_HASH = domain_hash("ONX_MSG_EXT_V1", WIRE)
MSGS_ROOT = domain_hash("ONX_MSGS_ROOT_V1", MSG_HASH)   # single-message body

# ---------------------------------------------------------------- block header
SEQNO = 42
PREV_HASH = bytes([0xAA]) * 32
STATE_ROOT = bytes([0xBB]) * 32
LT = 1_000_000
WORKCHAIN = -1                       # i32be(-1) = 0xFFFFFFFF
FEE_COLLECTOR = bytes([0xCC]) * 32
MSG_COUNT = 1

HEADER = b""
HEADER += struct.pack(">I", SEQNO)
HEADER += PREV_HASH
HEADER += MSGS_ROOT
HEADER += STATE_ROOT
HEADER += struct.pack(">Q", LT)
HEADER += struct.pack(">i", WORKCHAIN)
HEADER += FEE_COLLECTOR
HEADER += struct.pack(">I", MSG_COUNT)
assert len(HEADER) == 148, f"header len {len(HEADER)}"
HEADER_HASH = domain_hash("ONX_BLOCK_HDR_V1", HEADER)

# ------------------------------------------------- independent Ed25519 vector
SEED = bytes([0x42]) * 32
sk = SigningKey(SEED)
G_PUBKEY = bytes(sk.verify_key)
SIGNING_INPUT = pad32("ONX_MSG_EXT_SIGN_V1") + BODY
G_SIGNATURE = sk.sign(SIGNING_INPUT).signature
G_WIRE = BODY + G_SIGNATURE
G_MSG_HASH = domain_hash("ONX_MSG_EXT_V1", G_WIRE)
G_ADDRESS = domain_hash("ONX_ADDR_V1", G_PUBKEY)

# sanity: PyNaCl must verify its own signature over the same input
sk.verify_key.verify(SIGNING_INPUT, G_SIGNATURE)

def emit(name, b: bytes):
    print(f'{name}_HEX = "{b.hex()}"')

print("# ---- external message encoding vectors (pattern signature 0x33*64)")
emit("HAND_MSG_WIRE", WIRE)
emit("HAND_MSG_BODY", BODY)
emit("HAND_MSG_HASH", MSG_HASH)
emit("HAND_MSGS_ROOT", MSGS_ROOT)
print("# ---- block header vectors")
emit("HAND_HEADER", HEADER)
emit("HAND_HEADER_HASH", HEADER_HASH)
print("# ---- independent Ed25519 vector (seed 0x42*32, PyNaCl/libsodium)")
emit("HAND_G_PUBKEY", G_PUBKEY)
emit("HAND_G_SIGNATURE", G_SIGNATURE)
emit("HAND_G_MSG_HASH", G_WIRE and G_MSG_HASH)
emit("HAND_G_ADDRESS", G_ADDRESS)
print(f"# body_len={len(BODY)} wire_len={len(WIRE)} header_len={len(HEADER)}")
