#!/usr/bin/env python3
"""Generate the ONXBLK05 authenticated-header golden vectors (ADR-0032).

Usage:
  python3 reference/gen_auth_vectors.py            # write reference/vectors/blockauth.json
  python3 reference/gen_auth_vectors.py --check   # regenerate to temp dir and
                                                  # diff against the checked-in file;
                                                  # exits non-zero on drift (CI)

Vectors use the rekeyed fixture validator: test_secret_key(0x11), i.e. the
Ed25519 key from seed [0x11; 32] (public key
d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c9778737 —
must match the Rust fixture in config/genesis.toml).

Covers:
  - 160-byte v05 header encoding (field offsets)
  - block_hash = domain_hash(ONX_BLOCK_HDR_V1, header)
  - 96-byte signing preimage
  - Ed25519 signature over the preimage
  - sig_section strict encoding
  - full ONXBLK05 block file bytes
  - verification accept path (single validator, >2/3 stake)
  - verification reject paths (bad sig, wrong chain_id, dup index,
    insufficient stake)

Stdlib only.
"""

import json
import os
import sys
import tempfile
import filecmp

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from reference import ed25519
from reference.messages import (
    TAG_BLOCK_HDR,
    block_hash,
    block_header_bytes_v05,
    block_sign_bytes,
    block_file_bytes_v05,
    decode_sig_section_strict,
    encode_sig_section,
    verify_block_auth,
)
from reference.primitives import domain_hash, pad32

# ---------------------------------------------------------------- fixture
# test_secret_key(0x11): MUST match config/genesis.toml's validator.
VALIDATOR_SEED = bytes([0x11] * 32)
VALIDATOR_PUBKEY = ed25519.pubkey_from_seed(VALIDATOR_SEED)
assert VALIDATOR_PUBKEY.hex() == \
    "d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c9778737", \
    "fixture validator key drifted from config/genesis.toml"

# Fixed chain_id for the vectors (not the fixture's real genesis hash;
# chain binding is what matters, and the value is arbitrary here).
CHAIN_ID = bytes.fromhex("aa" * 32)
WORKCHAIN = -1
FEE_COLLECTOR = bytes([0xFC] * 32)
VALIDATORS = [(VALIDATOR_PUBKEY, 1000)]  # (pubkey, stake), canonical order


def make_header(seqno=1, block_time=1_790_000_000):
    prev = bytes([seqno - 1] * 32) if seqno > 0 else bytes(32)
    return block_header_bytes_v05(
        seqno=seqno,
        prev_hash=prev,
        msgs_root_h=bytes([0x11 * seqno % 256] * 32),
        state_root=bytes([0x22 * seqno % 256] * 32),
        lt=1000 + seqno,
        workchain=WORKCHAIN,
        fee_collector=FEE_COLLECTOR,
        msg_count=0,
        protocol_version=1,
        block_time=block_time,
    )


def sign_header(header: bytes, seed: bytes = VALIDATOR_SEED):
    bh = block_hash(header)
    preimage = block_sign_bytes(CHAIN_ID, bh)
    sig = ed25519.sign(seed, preimage)
    return bh, preimage, sig


def build_vectors():
    v = {}
    v["fixture_validator_pubkey"] = VALIDATOR_PUBKEY.hex()
    v["fixture_validator_seed_hint"] = "test_secret_key(0x11) = [0x11; 32] (TEST ONLY)"

    # --- header encoding: field offsets pinned by slicing the vector
    h1 = make_header(seqno=1)
    v["header_seq1_hex"] = h1.hex()
    v["header_len"] = len(h1)
    v["header_fields"] = {
        "seqno": {"offset": 0, "len": 4, "hex": h1[0:4].hex()},
        "prev_hash": {"offset": 4, "len": 32, "hex": h1[4:36].hex()},
        "protocol_version": {"offset": 148, "len": 4, "hex": h1[148:152].hex()},
        "block_time": {"offset": 152, "len": 8, "hex": h1[152:160].hex()},
    }

    # --- hash, preimage, signature
    bh1, pre1, sig1 = sign_header(h1)
    v["block_hash_seq1"] = bh1.hex()
    v["sign_preimage_seq1_hex"] = pre1.hex()
    v["sign_preimage_len"] = len(pre1)
    v["sign_preimage_prefix_hex"] = pad32("ONX_BLOCK_SIG_V1").hex()
    v["signature_seq1_hex"] = sig1.hex()

    # --- sig section
    sec = encode_sig_section([(0, sig1)])
    v["sig_section_hex"] = sec.hex()
    assert decode_sig_section_strict(sec) == [(0, sig1)]

    # --- full block file
    fbytes = block_file_bytes_v05(h1, sec, [])
    v["block_file_magic"] = fbytes[:8].decode("ascii")
    v["block_file_seq1_hex"] = fbytes.hex()

    # --- verification: accept path
    verify_block_auth(CHAIN_ID, h1, [(0, sig1)], VALIDATORS)
    v["verify_accept"] = True

    # --- verification: reject paths (each must raise)
    rejects = {}

    # bad signature (flipped bit)
    bad_sig = bytearray(sig1)
    bad_sig[0] ^= 1
    try:
        verify_block_auth(CHAIN_ID, h1, [(0, bytes(bad_sig))], VALIDATORS)
        rejects["bad_signature"] = False
    except ValueError:
        rejects["bad_signature"] = True

    # wrong chain_id: signature binds a different chain
    try:
        verify_block_auth(bytes([0xBB] * 32), h1, [(0, sig1)], VALIDATORS)
        rejects["wrong_chain_id"] = False
    except ValueError:
        rejects["wrong_chain_id"] = True

    # tampered header: signature no longer matches the hashed bytes
    h_tampered = bytearray(h1)
    h_tampered[159] ^= 1  # flip low bit of block_time
    try:
        verify_block_auth(CHAIN_ID, bytes(h_tampered), [(0, sig1)], VALIDATORS)
        rejects["tampered_header"] = False
    except ValueError:
        rejects["tampered_header"] = True

    # duplicate validator index in the section (caught at decode)
    try:
        decode_sig_section_strict(encode_sig_section([(0, sig1), (0, sig1)]))
        rejects["duplicate_index"] = False
    except ValueError:
        rejects["duplicate_index"] = True

    # out-of-order indices (caught at decode)
    other_seed = bytes([0x22] * 32)
    other_sig = ed25519.sign(other_seed, pre1)
    try:
        decode_sig_section_strict(encode_sig_section([(1, other_sig), (0, sig1)]))
        rejects["unordered_indices"] = False
    except ValueError:
        rejects["unordered_indices"] = True

    # insufficient stake: two validators, only 1/2 signs (need >2/3)
    other_pub = ed25519.pubkey_from_seed(other_seed)
    vals2 = sorted(
        [(VALIDATOR_PUBKEY, 1000), (other_pub, 1000)],
        key=lambda t: t[0],
    )
    idx_self = next(i for i, (p, _) in enumerate(vals2) if p == VALIDATOR_PUBKEY)
    try:
        verify_block_auth(CHAIN_ID, h1, [(idx_self, sig1)], vals2)
        rejects["insufficient_stake"] = False
    except ValueError:
        rejects["insufficient_stake"] = True

    # small-order validator key rejected by the strict predicate
    small_order = bytes.fromhex(
        "0000000000000000000000000000000000000000000000000000000000000000")
    try:
        verify_block_auth(CHAIN_ID, h1, [(0, sig1)],
                          [(small_order, 1000)])
        rejects["small_order_key"] = False
    except ValueError:
        rejects["small_order_key"] = True

    v["verify_rejects"] = rejects
    assert all(rejects.values()), f"reject paths not all raising: {rejects}"

    # Sanity: the domain tags are the frozen strings.
    v["tags"] = {
        "block_hdr": TAG_BLOCK_HDR,
        "block_sig": "ONX_BLOCK_SIG_V1",
    }
    return v


def main():
    vectors = build_vectors()
    out_path = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                            "vectors", "blockauth.json")
    if "--check" in sys.argv:
        with tempfile.TemporaryDirectory() as td:
            tmp = os.path.join(td, "blockauth.json")
            with open(tmp, "w") as f:
                json.dump(vectors, f, indent=2)
                f.write("\n")
            if not filecmp.cmp(tmp, out_path, shallow=False):
                print("DRIFT: regenerated blockauth.json differs from checked-in")
                sys.exit(1)
            print("blockauth.json: no drift")
    else:
        with open(out_path, "w") as f:
            json.dump(vectors, f, indent=2)
            f.write("\n")
        print(f"wrote {out_path}")


if __name__ == "__main__":
    main()
