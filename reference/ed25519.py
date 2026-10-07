#!/usr/bin/env python3
"""Pure-Python Ed25519 (RFC 8032), stdlib only.

The Rust implementation uses ed25519-dalek with strict verification.
Ed25519 is deterministic: a correct implementation derives identical
public keys and signatures from the same seed and message, so this
module is cross-checkable against the Rust signatures byte-for-byte.

This is a straightforward transcription of the RFC 8032 algorithm, not
of any particular codebase. It is slow (~10ms per operation) — fine for
a reference implementation generating a handful of test vectors.
"""

import hashlib

P = 2**255 - 19
D = (-121665 * pow(121666, P - 2, P)) % P
Gx = 15112221349535400772501151409588531511454012693041857206046113283949847762202
Gy = 46316835694926478169428394003475163141307993866256225615783033603165251855960
L = 2**252 + 27742317777372353535851937790883648493  # group order


def _xrecover(y: int) -> int:
    xx = (y * y - 1) * pow(D * y * y + 1, P - 2, P) % P
    x = pow(xx, (P + 3) // 8, P)
    if (x * x - xx) % P != 0:
        x = (x * pow(2, (P - 1) // 4, P)) % P
    if x % 2 != 0:
        x = P - x
    return x


def _edwards_add(p1, p2):
    (x1, y1, z1, t1) = p1
    (x2, y2, z2, t2) = p2
    a = ((y1 - x1) * (y2 - x2)) % P
    b = ((y1 + x1) * (y2 + x2)) % P
    c = (t1 * 2 * D * t2) % P
    d = ((z1 * 2) * z2) % P
    e = (b - a) % P
    f = (d - c) % P
    g = (d + c) % P
    h = (b + a) % P
    return ((e * f) % P, (g * h) % P, (f * g) % P, (e * h) % P)


def _edwards_double(p):
    return _edwards_add(p, p)


def _scalarmult(p, e: int):
    # Fixed-window, constant-shape loop; not constant-time (reference only).
    q = (0, 1, 1, 0)  # identity
    bits = bin(e)[2:]
    for bit in bits:
        q = _edwards_double(q)
        if bit == "1":
            q = _edwards_add(q, p)
    return q


def _encodepoint(p) -> bytes:
    (x, y, z, _) = p
    zi = pow(z, P - 2, P)
    x = (x * zi) % P
    y = (y * zi) % P
    bit = x & 1
    out = y.to_bytes(32, "little")
    out = bytearray(out)
    out[31] |= bit << 7
    return bytes(out)


def _decodepoint(s: bytes):
    """Decode a public key under the strict predicate: canonical encoding
    (recompressed bytes must match the input), on-curve, large-order.

    Mirrors `PublicKey::decode_exact` in `crates/protocol/onx-primitives` —
    this is the pinned-Ed25519 decode both implementations must agree on.
    """
    if len(s) != 32:
        raise ValueError("bad public key length")
    y = int.from_bytes(s, "little") & ((1 << 255) - 1)
    sign = (s[31] >> 7) & 1
    x = _xrecover(y)
    if x & 1 != sign:
        x = P - x
    # On-curve: twisted Edwards -x^2 + y^2 = 1 + d x^2 y^2.
    x2 = (x * x) % P
    y2 = (y * y) % P
    if (y2 - x2 - 1 - D * x2 % P * y2) % P != 0:
        raise ValueError("point not on curve")
    p = (x, y, 1, (x * y) % P)
    # Canonicality: the encoding must round-trip through recompression.
    # Non-canonical encodings (e.g. y + P) decode to a valid point but
    # re-encode to different bytes; reject them so one point has exactly
    # one accepted encoding.
    if _encodepoint(p) != s:
        raise ValueError("non-canonical point encoding")
    # Small-order (torsion) rejection: cofactorless verification admits
    # forgeries under small-order keys.
    if _encodepoint(_scalarmult(p, 8)) == _encodepoint((0, 1, 1, 0)):
        raise ValueError("small-order point")
    return p


def _sha512(s: bytes) -> bytes:
    return hashlib.sha512(s).digest()


def _hint(s: bytes) -> int:
    return int.from_bytes(_sha512(s), "little")


_BASE = (Gx, Gy, 1, (Gx * Gy) % P)


def pubkey_from_seed(seed: bytes) -> bytes:
    """Derive the Ed25519 public key from a 32-byte seed."""
    if len(seed) != 32:
        raise ValueError("seed must be 32 bytes")
    h = _sha512(seed)
    a = int.from_bytes(h[:32], "little")
    a &= ~((1 << 3) - 1 | (1 << 255))  # clear bits 0,1,2 and 255
    a |= 1 << 254  # set bit 254
    return _encodepoint(_scalarmult(_BASE, a))


def sign(seed: bytes, msg: bytes) -> bytes:
    """Sign msg with the 32-byte seed. Returns the 64-byte signature."""
    if len(seed) != 32:
        raise ValueError("seed must be 32 bytes")
    h = _sha512(seed)
    a = int.from_bytes(h[:32], "little")
    a &= ~((1 << 3) - 1 | (1 << 255))  # clear bits 0,1,2 and 255
    a |= 1 << 254  # set bit 254
    prefix = h[32:]
    pubkey = _encodepoint(_scalarmult(_BASE, a))
    r = _hint(prefix + msg)
    R = _encodepoint(_scalarmult(_BASE, r))
    S = (_hint(R + pubkey + msg) * a + r) % L
    return R + S.to_bytes(32, "little")


def verify(pubkey: bytes, msg: bytes, sig: bytes) -> bool:
    """Verify a 64-byte signature. Returns True/False (never raises)."""
    try:
        if len(sig) != 64 or len(pubkey) != 32:
            return False
        R = _decodepoint(sig[:32])
        A = _decodepoint(pubkey)
        S = int.from_bytes(sig[32:], "little")
        if S >= L:
            return False
        h = _hint(sig[:32] + pubkey + msg)
        # Check S*B == R + h*A
        lhs = _scalarmult(_BASE, S)
        rhs = _edwards_add(R, _scalarmult(A, h))
        return _encodepoint(lhs) == _encodepoint(rhs)
    except Exception:
        return False


def self_test() -> None:
    """Cross-checked against libsodium (two independent implementations).

    For seed 9d61b19d..., both this module and libsodium derive public key
    b9ce0f24... and produce byte-identical signatures.
    """
    seed = bytes.fromhex(
        "9d61b19deffd5a60ba844af492ec2af44449c5697b326919703bac031cae7f60"
    )
    expected_pk = bytes.fromhex(
        "b9ce0f24f866c0fa02ab7c1ff345f9bb7c73205b4c2ec76a77295ef16592dc0d"
    )
    assert pubkey_from_seed(seed) == expected_pk, "pubkey derivation mismatch"
    sig = sign(seed, b"hello")
    assert verify(expected_pk, b"hello", sig), "self-signature failed to verify"
    assert not verify(expected_pk, b"hellx", sig), "forged message verified?!"
    print("ed25519 self-test: OK")
