#!/usr/bin/env python3
"""External and internal message encodings.

Derived from docs/adr/0002-message-encoding-and-domain-tags.md.

External message body (big-endian, strict):
  chain_id(32) || from(32) || nonce u64be(8) || kind(1) || to(32) ||
  amount_nanos u128be(16) || fee_nanos u128be(16) || msg_len u32be(4) ||
  message(msg_len) || pubkey(32)
Wire = body || signature(64). Transfer bodies: 173 + 64 = 237 bytes.

  kind: 0 = Transfer (message must be empty), 1 = ContractCall.
  Identity:  domain_hash(ONX_MSG_EXT_V1, wire_bytes)
  Signature: Ed25519 over pad32(ONX_MSG_EXT_SIGN_V1) || body_bytes
             (the domain-separated message is signed directly, not hashed
             first).

Internal message canonical bytes:
  src(32) || dest(32) || value_nanos u128be(16) || fee_nanos u128be(16) ||
  payload_len u32be(4) || payload || is_bounce(1) || origin(32)
  Delivery ID: domain_hash(ONX_MSG_INT_V1, canonical_bytes)

Block header (148 bytes, big-endian):
  seqno u32be(4) || prev_hash(32) || msgs_root(32) || state_root(32) ||
  lt u64be(8) || workchain i32be(4) || fee_collector(32) || msg_count u32be(4)
  Header hash: domain_hash(ONX_BLOCK_HDR_V1, header_bytes).

Block commitment:
  msgs_root = domain_hash(ONX_MSGS_ROOT_V1, id_0 || id_1 || ...)
  over external message hashes in block order.

Key-derived addresses (ADR-0006):
  derive_address(pubkey) = domain_hash(ONX_ADDR_V1, pubkey)
"""

from . import ed25519
from .primitives import domain_hash, pad32, u32be, u64be, u128be, i32be

TAG_EXT = "ONX_MSG_EXT_V1"
TAG_EXT_SIGN = "ONX_MSG_EXT_SIGN_V1"
TAG_INT = "ONX_MSG_INT_V1"
TAG_MSGS_ROOT = "ONX_MSGS_ROOT_V1"
TAG_ADDR = "ONX_ADDR_V1"
TAG_BLOCK_HDR = "ONX_BLOCK_HDR_V1"

KIND_TRANSFER = 0
KIND_CONTRACT_CALL = 1

ZERO_PUBKEY = bytes(32)


def derive_address(pubkey: bytes) -> bytes:
    if len(pubkey) != 32:
        raise ValueError("pubkey must be 32 bytes")
    return domain_hash(TAG_ADDR, pubkey)


class ExternalMessage:
    def __init__(self, chain_id, from_id, nonce, kind, to_id, amount, fee,
                 message, pubkey, signature):
        for name, v in (("chain_id", chain_id), ("from", from_id),
                        ("to", to_id), ("pubkey", pubkey)):
            if len(v) != 32:
                raise ValueError(f"{name} must be 32 bytes")
        if kind not in (KIND_TRANSFER, KIND_CONTRACT_CALL):
            raise ValueError(f"bad kind: {kind}")
        if kind == KIND_TRANSFER and message:
            raise ValueError("transfer with non-empty payload")
        if len(signature) != 64:
            raise ValueError("signature must be 64 bytes")
        self.chain_id = bytes(chain_id)
        self.from_id = bytes(from_id)
        self.nonce = nonce
        self.kind = kind
        self.to_id = bytes(to_id)
        self.amount = amount
        self.fee = fee
        self.message = bytes(message)
        self.pubkey = bytes(pubkey)
        self.signature = bytes(signature)

    def body_bytes(self) -> bytes:
        out = bytearray()
        out += self.chain_id
        out += self.from_id
        out += u64be(self.nonce)
        out += bytes([self.kind])
        out += self.to_id
        out += u128be(self.amount)
        out += u128be(self.fee)
        out += u32be(len(self.message))
        out += self.message
        out += self.pubkey
        return bytes(out)

    def wire_bytes(self) -> bytes:
        return self.body_bytes() + self.signature

    def hash(self) -> bytes:
        return domain_hash(TAG_EXT, self.wire_bytes())

    def verify_signature(self, pubkey: bytes) -> bool:
        preimage = pad32(TAG_EXT_SIGN) + self.body_bytes()
        return ed25519.verify(pubkey, preimage, self.signature)

    @classmethod
    def new_signed(cls, chain_id, kind, from_id, nonce, to_id, amount, fee,
                   message, pubkey, seed):
        msg = cls(chain_id, from_id, nonce, kind, to_id, amount, fee,
                  message, pubkey, bytes(64))
        preimage = pad32(TAG_EXT_SIGN) + msg.body_bytes()
        sig = ed25519.sign(seed, preimage)
        msg.signature = sig
        return msg


class InternalMessage:
    def __init__(self, src, dest, value, fee, payload, is_bounce, origin):
        for name, v in (("src", src), ("dest", dest), ("origin", origin)):
            if len(v) != 32:
                raise ValueError(f"{name} must be 32 bytes")
        self.src = bytes(src)
        self.dest = bytes(dest)
        self.value = value
        self.fee = fee
        self.payload = bytes(payload)
        self.is_bounce = bool(is_bounce)
        self.origin = bytes(origin)

    def canonical_bytes(self) -> bytes:
        out = bytearray()
        out += self.src
        out += self.dest
        out += u128be(self.value)
        out += u128be(self.fee)
        out += u32be(len(self.payload))
        out += self.payload
        out += bytes([1 if self.is_bounce else 0])
        out += self.origin
        return bytes(out)

    def id(self) -> bytes:
        return domain_hash(TAG_INT, self.canonical_bytes())


def msgs_root(hashes: list) -> bytes:
    return domain_hash(TAG_MSGS_ROOT, b"".join(hashes))


def block_header_bytes(seqno, prev_hash, msgs_root_h, state_root, lt,
                       workchain, fee_collector, msg_count) -> bytes:
    out = bytearray()
    out += u32be(seqno)
    out += prev_hash
    out += msgs_root_h
    out += state_root
    out += u64be(lt)
    out += i32be(workchain)
    out += fee_collector
    out += u32be(msg_count)
    assert len(out) == 148, f"header must be 148 bytes, got {len(out)}"
    return bytes(out)


def block_hash(header_bytes: bytes) -> bytes:
    return domain_hash(TAG_BLOCK_HDR, header_bytes)


BLOCK_FILE_MAGIC = b"ONXBLK04"


def block_file_bytes(header_bytes: bytes, wires: list) -> bytes:
    """ONXBLK04 file: magic(8) || header(148) || u32be count ||
    [u32be len || wire]*."""
    out = bytearray(BLOCK_FILE_MAGIC)
    out += header_bytes
    out += u32be(len(wires))
    for w in wires:
        out += u32be(len(w))
        out += w
    return bytes(out)


# ---------------------------------------------------------------- ONXBLK05
# Authenticated block headers (ADR-0032). The 160-byte header appends
# protocol_version (u32be) and block_time (u64be) after msg_count; the
# signature section sits OUTSIDE the hashed header bytes.
#
#   header_v05 (160) =
#     seqno u32be(4) || prev_hash(32) || msgs_root(32) || state_root(32) ||
#     lt u64be(8) || workchain i32be(4) || fee_collector(32) ||
#     msg_count u32be(4) || protocol_version u32be(4) || block_time u64be(8)
#
#   block_hash = domain_hash(ONX_BLOCK_HDR_V1, header_v05)
#   sign_bytes = pad32("ONX_BLOCK_SIG_V1") || chain_id(32) || block_hash(32)
#   sig_section = count u32be || [validator_index u32be || sig(64)]*
#   file = magic "ONXBLK05"(8) || header(160) || sig_section || body(...)

TAG_BLOCK_SIG = "ONX_BLOCK_SIG_V1"
BLOCK_FILE_MAGIC_V05 = b"ONXBLK05"
HEADER_V05_LEN = 160
PROTOCOL_VERSION_V1 = 1


def block_header_bytes_v05(seqno, prev_hash, msgs_root_h, state_root, lt,
                           workchain, fee_collector, msg_count,
                           protocol_version=PROTOCOL_VERSION_V1,
                           block_time=0) -> bytes:
    out = bytearray()
    out += u32be(seqno)
    out += prev_hash
    out += msgs_root_h
    out += state_root
    out += u64be(lt)
    out += i32be(workchain)
    out += fee_collector
    out += u32be(msg_count)
    out += u32be(protocol_version)
    out += u64be(block_time)
    assert len(out) == HEADER_V05_LEN, \
        f"v05 header must be 160 bytes, got {len(out)}"
    return bytes(out)


def block_sign_bytes(chain_id: bytes, block_hash_v: bytes) -> bytes:
    """96-byte signing preimage: pad32(tag) || chain_id || block_hash."""
    assert len(chain_id) == 32 and len(block_hash_v) == 32
    return pad32(TAG_BLOCK_SIG) + bytes(chain_id) + bytes(block_hash_v)


def encode_sig_section(entries: list) -> bytes:
    """entries: list of (validator_index:int, sig:bytes[64])."""
    out = bytearray(u32be(len(entries)))
    for idx, sig in entries:
        assert 0 <= idx <= 0xFFFFFFFF
        assert len(sig) == 64
        out += u32be(idx)
        out += sig
    return bytes(out)


def decode_sig_section_strict(data: bytes) -> list:
    """Strict decode; returns list of (validator_index, sig). Fail-closed."""
    if len(data) < 4:
        raise ValueError("sig section truncated: no count")
    count = int.from_bytes(data[0:4], "big")
    # Each entry costs 68 bytes; bound the claim before allocating.
    if 4 + count * 68 > len(data):
        raise ValueError(
            f"sig section claims {count} entries but holds {len(data)} bytes")
    entries = []
    off = 4
    for _ in range(count):
        idx = int.from_bytes(data[off:off + 4], "big")
        sig = bytes(data[off + 4:off + 68])
        entries.append((idx, sig))
        off += 68
    if off != len(data):
        raise ValueError("sig section has trailing bytes")
    # Canonical order: strictly ascending indices, no duplicates.
    indices = [i for i, _ in entries]
    if indices != sorted(indices) or len(set(indices)) != len(indices):
        raise ValueError("sig section indices not strictly ascending")
    return entries


def verify_block_auth(chain_id: bytes, header_bytes: bytes,
                      sig_entries: list, validators: list) -> None:
    """Verify an authenticated header (ADR-0032 §5).

    validators: list of (pubkey_bytes[32], stake:int) in canonical
    (pubkey-sorted) order; validator_index addresses this list.
    Raises ValueError on any failure. Requires >2/3 of genesis stake.
    """
    if len(header_bytes) != HEADER_V05_LEN:
        raise ValueError("header must be 160 bytes")
    if not validators:
        raise ValueError("empty validator set")
    total_stake = sum(s for _, s in validators)
    if total_stake == 0:
        raise ValueError("zero total stake")

    bh = domain_hash(TAG_BLOCK_HDR, header_bytes)
    preimage = block_sign_bytes(chain_id, bh)

    seen_stake = 0
    for idx, sig in sig_entries:
        if idx >= len(validators):
            raise ValueError(f"validator index {idx} out of range")
        pubkey, stake = validators[idx]
        # Strict predicate: canonical, on-curve, large-order.
        ed25519._decodepoint(bytes(pubkey))
        if not ed25519.verify(bytes(pubkey), preimage, bytes(sig)):
            raise ValueError(f"bad signature from validator {idx}")
        seen_stake += stake

    # Strictly more than 2/3: seen*3 > total*2.
    if seen_stake * 3 <= total_stake * 2:
        raise ValueError(
            f"insufficient stake: {seen_stake}/{total_stake} (need >2/3)")


def block_file_bytes_v05(header_bytes: bytes, sig_section: bytes,
                         wires: list) -> bytes:
    """ONXBLK05 file: magic(8) || header(160) || sig_section ||
    u32be count || [u32be len || wire]*."""
    assert len(header_bytes) == HEADER_V05_LEN
    out = bytearray(BLOCK_FILE_MAGIC_V05)
    out += header_bytes
    out += sig_section
    out += u32be(len(wires))
    for w in wires:
        out += u32be(len(w))
        out += w
    return bytes(out)
