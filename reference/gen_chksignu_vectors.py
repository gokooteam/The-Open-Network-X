#!/usr/bin/env python3
"""Generate (or check) the chain-bound CHKSIGNU reference vectors.

Usage:
  python3 reference/gen_chksignu_vectors.py          # write reference/vectors/chksignu.json
  python3 reference/gen_chksignu_vectors.py --check  # fail if it would change (CI drift gate)

Implements ADR-0038 from the specification text, not from the Rust code:
  - CHKSIGNU base tag: "ONX_CHKSIGNU_V1" (zero-padded to 32 bytes)
  - chain-bound tag: bind_chain(base, chain_id) = SHA256(pad32(base) || chain_id)
  - CHKSIGNU verifies the Ed25519 signature over (derived_tag || hash32),
    where hash32 is the 32-byte message hash operand.

Vectors pin:
  1. Tag derivation: chain_id -> derived tag (byte-exact).
  2. Positive signatures: (chain_id, hash32, pubkey, sig) where sig was made
     over (derived_tag || hash32) — the Rust side must verify these.
  3. Cross-chain negatives: a signature made for chain A must NOT verify
     under chain B's derived tag.
  4. Cross-protocol negatives: a signature made under the retired TX_BODY_V1
     tag (the payment-channel tag) must NOT verify under any CHKSIGNU tag.

The Python ed25519 here is the plain RFC 8032 verification (not the strict
variant); the vectors only assert tag derivation and domain separation, not
ed25519 edge semantics. Seeds are fixed test material.

Stdlib only.
"""

import hashlib
import json
import os
import sys
import tempfile
import filecmp

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from reference import ed25519

CHKSIGNU_BASE = b"ONX_CHKSIGNU_V1"
RETIRED_TX_BODY_V1 = b"ONX_TX_BODY_V1"


def pad32(s: bytes) -> bytes:
    assert len(s) <= 32
    return s + b"\x00" * (32 - len(s))


def bind_chain(base_ascii: bytes, chain_id: bytes) -> bytes:
    """ADR-0038 tag derivation: SHA256(pad32(base) || chain_id)."""
    assert len(chain_id) == 32
    return hashlib.sha256(pad32(base_ascii) + chain_id).digest()


def domain_separated_message(tag: bytes, message: bytes) -> bytes:
    assert len(tag) == 32
    return tag + message


# Fixed test material: (seed, chain_id, hash32) triples.
CHAIN_A = bytes.fromhex("aa" * 32)
CHAIN_B = bytes.fromhex("bb" * 32)
CHAIN_C = bytes.fromhex(
    "58758ecb51f0f2e7fc1353896392e43e2554aa9bef02a335fc350cd804484683"
)  # the gen_vectors.py fixture chain id

CASES = [
    (bytes([0x11] * 32), CHAIN_A, bytes.fromhex("01" * 32)),
    (bytes([0x22] * 32), CHAIN_A, bytes.fromhex("02" * 32)),
    (bytes([0x33] * 32), CHAIN_B, bytes.fromhex("03" * 32)),
    (bytes([0x44] * 32), CHAIN_C, bytes.fromhex("04" * 32)),
]


def build_vectors():
    tag_vectors = []
    for chain_id in [CHAIN_A, CHAIN_B, CHAIN_C]:
        tag_vectors.append(
            {
                "chain_id_hex": chain_id.hex(),
                "derived_tag_hex": bind_chain(CHKSIGNU_BASE, chain_id).hex(),
            }
        )

    sig_vectors = []
    for seed, chain_id, hash32 in CASES:
        tag = bind_chain(CHKSIGNU_BASE, chain_id)
        msg = domain_separated_message(tag, hash32)
        pubkey = ed25519.pubkey_from_seed(seed)
        sig = ed25519.sign(seed, msg)
        # Self-check: the oracle's own verify must accept it.
        assert ed25519.verify(pubkey, msg, sig), "oracle self-check failed"
        sig_vectors.append(
            {
                "chain_id_hex": chain_id.hex(),
                "hash32_hex": hash32.hex(),
                "pubkey_hex": pubkey.hex(),
                "sig_hex": sig.hex(),
            }
        )

    # Cross-chain negatives: signature for CHAIN_A verified under CHAIN_B's tag.
    cross_chain = []
    seed, _, hash32 = CASES[0]
    tag_a = bind_chain(CHKSIGNU_BASE, CHAIN_A)
    tag_b = bind_chain(CHKSIGNU_BASE, CHAIN_B)
    sig_a = ed25519.sign(seed, domain_separated_message(tag_a, hash32))
    pubkey = ed25519.pubkey_from_seed(seed)
    assert not ed25519.verify(
        pubkey, domain_separated_message(tag_b, hash32), sig_a
    ), "cross-chain must not verify even in the oracle"
    cross_chain.append(
        {
            "chain_id_hex": CHAIN_B.hex(),
            "hash32_hex": hash32.hex(),
            "pubkey_hex": pubkey.hex(),
            "sig_hex": sig_a.hex(),
            "note": "signed for CHAIN_A; must NOT verify under CHAIN_B",
        }
    )

    # Cross-protocol negatives: old TX_BODY_V1 signature must not verify
    # under the CHKSIGNU tag (this is the exact pre-ADR-0038 attack).
    cross_protocol = []
    old_tag = pad32(RETIRED_TX_BODY_V1)
    old_sig = ed25519.sign(seed, domain_separated_message(old_tag, hash32))
    new_tag = bind_chain(CHKSIGNU_BASE, CHAIN_A)
    assert not ed25519.verify(
        pubkey, domain_separated_message(new_tag, hash32), old_sig
    ), "cross-protocol must not verify even in the oracle"
    cross_protocol.append(
        {
            "chain_id_hex": CHAIN_A.hex(),
            "hash32_hex": hash32.hex(),
            "pubkey_hex": pubkey.hex(),
            "sig_hex": old_sig.hex(),
            "note": "signed under retired TX_BODY_V1; must NOT verify as CHKSIGNU",
        }
    )

    return {
        "adr": "ADR-0038",
        "chksignu_base": CHKSIGNU_BASE.decode(),
        "tag_vectors": tag_vectors,
        "signatures": sig_vectors,
        "cross_chain_negatives": cross_chain,
        "cross_protocol_negatives": cross_protocol,
    }


def main():
    vectors = build_vectors()
    out_path = os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "vectors", "chksignu.json"
    )
    text = json.dumps(vectors, indent=2) + "\n"
    if "--check" in sys.argv:
        with tempfile.NamedTemporaryFile(
            "w", suffix=".json", delete=False
        ) as tmp:
            tmp.write(text)
            tmp_path = tmp.name
        try:
            if not os.path.exists(out_path) or not filecmp.cmp(
                tmp_path, out_path, shallow=False
            ):
                print("chksignu.json is stale: regenerate with gen_chksignu_vectors.py")
                sys.exit(1)
        finally:
            os.unlink(tmp_path)
        print("chksignu.json drift-clean")
    else:
        with open(out_path, "w") as f:
            f.write(text)
        print(f"wrote {out_path}")


if __name__ == "__main__":
    main()
