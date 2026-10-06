#!/usr/bin/env python3
"""Hand-derivation of ONX V3 golden test vectors — INDEPENDENT of the Rust code.

V3 changes vs V2 (tvm-integration): transaction gains a kind byte and a
length-prefixed message:
  body = kind(1) || from(32) || to(32) || amount u128be(16)
         || fee u128be(16) || nonce u64be(8) || msg_len u32be(4) || msg
  wire = body || signature(64)
Tags: ONX_TX_V3 (identity), ONX_TX_V3_SIGN (signature domain),
      ONX_TXS_ROOT_V3 (tx-set commitment).
V1 and V2 are dropped (never shipped).

Parameters confirmed from code (parameters only, no logic copied):
  - hash algorithm: SHA-256
  - domain_hash(tag, msg) = SHA256(32-byte-zero-padded-ASCII-tag || msg)
  - Transaction::hash() covers the full wire (body || signature)
  - signature domain message = 32-byte-padded-tag || body_bytes
"""

import hashlib
import json

import nacl.signing


def sha256(data: bytes) -> bytes:
    return hashlib.sha256(data).digest()


def tag(name: str) -> bytes:
    assert len(name) <= 32
    return name.encode("ascii") + b"\x00" * (32 - len(name))


def domain_hash(tag_name: str, msg: bytes) -> bytes:
    return sha256(tag(tag_name) + msg)


def u128be(v: int) -> bytes:
    return v.to_bytes(16, "big")


def u64be(v: int) -> bytes:
    return v.to_bytes(8, "big")


def u32be(v: int) -> bytes:
    return v.to_bytes(4, "big")


def i32be(v: int) -> bytes:
    return v.to_bytes(4, "big", signed=True)


vectors = {}

# ---------------------------------------------------------------- Vector A
# V3 transaction canonical wire bytes, fixed human-chosen fields.
# kind = 0 (Transfer), message empty.
# Layout: kind(1) || from(32) || to(32) || amount u128be(16)
# || fee u128be(16) || nonce u64be(8) || msg_len u32be(4) || sig(64).
# NOTE: signature bytes are a fixed pattern, NOT a valid signature —
# this vector tests encoding only.
kind = bytes([0x00])
frm = bytes([0x11]) * 32
to = bytes([0x22]) * 32
amount = 1000
fee = 10
nonce = 7
msg = b""
sig_pattern = bytes([0x33]) * 64

body = kind + frm + to + u128be(amount) + u128be(fee) + u64be(nonce) + u32be(len(msg)) + msg
assert len(body) == 109, len(body)
wire = body + sig_pattern
assert len(wire) == 173, len(wire)
vectors["A_tx_wire_hex"] = wire.hex()
vectors["A_tx_body_hex"] = body.hex()

# ---------------------------------------------------------------- Vector B
# Transaction identity hash: domain_hash(ONX_TX_V3, wire_bytes).
tx_hash = domain_hash("ONX_TX_V3", wire)
vectors["B_tx_hash_hex"] = tx_hash.hex()

# ---------------------------------------------------------------- Vector C
# txs_root for a single-transaction block:
# domain_hash(ONX_TXS_ROOT_V3, concat(tx hashes)).
txs_root = domain_hash("ONX_TXS_ROOT_V3", tx_hash)
vectors["C_txs_root_hex"] = txs_root.hex()

# ---------------------------------------------------------------- Vector D
# Block header canonical bytes (148), fixed fields. Layout unchanged:
# seqno u32be(4) || prev_hash(32) || txs_root(32) ||
# state_root(32) || lt u64be(8) || workchain i32be(4) ||
# fee_collector(32) || tx_count u32be(4).
seqno = 42
prev_hash = bytes([0xAA]) * 32
state_root = bytes([0xBB]) * 32
lt = 1_000_000
workchain = -1  # masterchain; i32be(-1) = 0xFFFFFFFF
fee_collector = bytes([0xCC]) * 32
tx_count = 1

header = (
    u32be(seqno)
    + prev_hash
    + txs_root
    + state_root
    + u64be(lt)
    + i32be(workchain)
    + fee_collector
    + u32be(tx_count)
)
assert len(header) == 148, len(header)
vectors["D_header_hex"] = header.hex()

# ---------------------------------------------------------------- Vector E
# Block header hash: domain_hash(ONX_BLOCK_HDR_V1, header_bytes).
header_hash = domain_hash("ONX_BLOCK_HDR_V1", header)
vectors["E_header_hash_hex"] = header_hash.hex()

# ---------------------------------------------------------------- Vector G
# Ed25519 interop: an INDEPENDENT Ed25519 implementation (PyNaCl/libsodium,
# not the Rust code) signs the domain-separated message; the Rust side
# must verify it. Signed message = pad32("ONX_TX_V3_SIGN") || body.
seed = bytes([0x42]) * 32
sk = nacl.signing.SigningKey(seed)
pubkey = bytes(sk.verify_key)
signed_msg = tag("ONX_TX_V3_SIGN") + body
assert len(signed_msg) == 141, len(signed_msg)
signature = sk.sign(signed_msg).signature
assert len(signature) == 64
vectors["G_seed_hex"] = seed.hex()
vectors["G_pubkey_hex"] = pubkey.hex()
vectors["G_signed_message_hex"] = signed_msg.hex()
vectors["G_signature_hex"] = signature.hex()
# Sanity: independent library verifies its own signature.
sk.verify_key.verify(signed_msg, signature)

# The same body with the REAL signature: tx identity hash over the wire.
wire_g = body + signature
assert len(wire_g) == 173
vectors["G_tx_hash_hex"] = domain_hash("ONX_TX_V3", wire_g).hex()

print(json.dumps(vectors, indent=2))
