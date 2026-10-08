#!/usr/bin/env python3
"""Generate (or check) the checked-in reference vectors.

Usage:
  python3 reference/gen_vectors.py            # write reference/vectors/
  python3 reference/gen_vectors.py --check    # regenerate to temp dir and
                                              # diff against checked-in files;
                                              # exits non-zero on drift (CI)

The vector chain mirrors the Rust phase5_replay fixture so the two
implementations are comparable:
  - 4 genesis accounts: IDs [0xaa;32]..[0xad;32], balance 1e12,
    Ed25519 pubkeys from seeds [0xaa;32]..[0xad;32]
  - chain_id = the fixture's genesis hash (fixed; see below)
  - 5 blocks x 4 external messages, xorshift64 PRNG seeded 0xC10C,
    same draw order as the Rust fixture (from, to, amount, fee)

Expected roots (from the Rust implementation; the Python reference must
reproduce them independently):
  genesis state root : 079404cb2379be4802d27f77101e92c232cb94af2f93b3c8274995e85b99c04d
  after block 3      : 3fbb03b29519164464d470e1facbb62f96d0f1ea4fe2e4dce3df87661650af36
  after block 5      : 8b2f6aa6de16db8b22ac3f8fdde779ae0e292b6912a21dbf1915f98747aa6098

Stdlib only.
"""

import json
import os
import sys
import tempfile
import filecmp

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from reference import ed25519
from reference.account import Account, ZERO_HASH
from reference.chain import State, propose_block
from reference.messages import (
    ExternalMessage, KIND_TRANSFER, ZERO_PUBKEY,
    block_file_bytes, block_hash,
)
from reference.primitives import h

# ---------------------------------------------------------------- fixture
# Chain ID: the Rust fixture's genesis hash (domain_hash(ONX_GENESIS_V1,
# canonical genesis bytes)). Fixed for the vector chain; message
# signatures bind it (ADR-0005).
CHAIN_ID = bytes.fromhex(
    "58758ecb51f0f2e7fc1353896392e43e2554aa9bef02a335fc350cd804484683"
)
WORKCHAIN = -1  # masterchain
GENESIS_BALANCE = 1_000_000_000_000
N_BLOCKS = 5
MSGS_PER_BLOCK = 4
PRNG_SEED = 0xC10C

EXPECTED = {
    "genesis_root": "079404cb2379be4802d27f77101e92c232cb94af2f93b3c8274995e85b99c04d",
    "root_after_3": "3fbb03b29519164464d470e1facbb62f96d0f1ea4fe2e4dce3df87661650af36",
    "root_after_5": "8b2f6aa6de16db8b22ac3f8fdde779ae0e292b6912a21dbf1915f98747aa6098",
}

VECTORS_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "vectors")
VECTOR_FILES = ["genesis.json", "blocks.json", "expectations.json", "divmod.json"]

# ---------------------------------------------------------------- divmod
# DIVMOD (0x14) / DIV (0x17) vectors, derived from the SPEC
# (docs/specification/tvm-instruction-set.md §4.3, ADR-0030) — not from the
# Rust code. True floor division: q = floor(a/b), r = a - q*b, with
# sign(r) == sign(b) or r == 0. b = 0 and MIN/-1 raise IntegerOverflow.
# Python's divmod() is floored by language definition, so it is an
# independent implementation of the formula; the invariant is then asserted
# independently below. (TVM execution stays a deliberate non-goal of this
# reference; these are spec-derived arithmetic cases, not an interpreter.)
# Integer model: 257-bit domain [-2^256, 2^256 - 1] (ADR-0035); MIN/-1 =
# 2^256 is unrepresentable, hence IntegerOverflow.
I256_MIN = -(1 << 256)
DIVMOD_CASES = [
    (7, 2),
    (-7, 2),
    (7, -2),
    (-7, -2),
    (0, -1),
    (I256_MIN, -1),  # quotient 2^256 unrepresentable -> IntegerOverflow
    (7, 0),          # -> IntegerOverflow
]


def divmod_doc():
    cases = []
    for a, b in DIVMOD_CASES:
        if b == 0 or (a == I256_MIN and b == -1):
            cases.append({"a": str(a), "b": str(b), "error": "IntegerOverflow"})
            continue
        q, r = divmod(a, b)
        assert a == q * b + r, f"invariant a == q*b + r failed for ({a}, {b})"
        assert abs(r) < abs(b), f"invariant |r| < |b| failed for ({a}, {b})"
        assert r == 0 or (r < 0) == (b < 0), (
            f"invariant sign(r) == sign(b) failed for ({a}, {b})"
        )
        cases.append({"a": str(a), "b": str(b), "q": str(q), "r": str(r)})
    return {
        "semantics": (
            "DIVMOD (0x14): q = floor(a/b), r = a - q*b; "
            "sign(r) == sign(b) or r == 0. b = 0 and MIN/-1 raise "
            "IntegerOverflow. 257-bit Integer model (ADR-0035), MIN = -2^256. "
            "Per docs/specification/tvm-instruction-set.md §4.3 and ADR-0030."
        ),
        "integer_bits": 257,
        "cases": cases,
    }


def xorshift64(state):
    """Rust u64 wrapping xorshift64. Returns (value, new_state)."""
    x = state[0]
    x ^= (x << 13) & 0xFFFFFFFFFFFFFFFF
    x ^= x >> 7
    x ^= (x << 17) & 0xFFFFFFFFFFFFFFFF
    x &= 0xFFFFFFFFFFFFFFFF
    state[0] = x
    return x


def account_id(byte):
    return bytes([byte]) * 32


def build_genesis():
    state = State(CHAIN_ID, WORKCHAIN)
    keys = {}  # account_id -> (seed, pubkey)
    for i in range(4):
        byte = 0xAA + i
        aid = account_id(byte)
        seed = bytes([byte]) * 32
        pubkey = ed25519.pubkey_from_seed(seed)
        keys[aid] = (seed, pubkey)
        state.accounts[aid] = Account(GENESIS_BALANCE, 0, pubkey, 0)
    return state, keys


def build_messages(state, keys):
    """Generate the 5x4 deterministic message fixture (mirrors Rust)."""
    rng = [PRNG_SEED]
    nonces = {}
    all_blocks = []
    for _ in range(N_BLOCKS):
        msgs = []
        for _ in range(MSGS_PER_BLOCK):
            from_id = account_id(0xAA + xorshift64(rng) % 4)
            to_id = account_id(0xAA + xorshift64(rng) % 4)
            if to_id == from_id:
                to_id = account_id(0xDD)
            nonce = nonces.get(from_id, 0)
            nonces[from_id] = nonce + 1
            seed = keys[from_id][0]
            amount = 1_000 + xorshift64(rng) % 50_000
            fee = 10 + xorshift64(rng) % 100
            msgs.append(ExternalMessage.new_signed(
                CHAIN_ID, KIND_TRANSFER, from_id, nonce, to_id,
                amount, fee, b"", ZERO_PUBKEY, seed,
            ))
        all_blocks.append(msgs)
    return all_blocks


def generate(out_dir):
    os.makedirs(out_dir, exist_ok=True)
    state, keys = build_genesis()

    genesis_root = state.state_root()
    assert genesis_root.hex() == EXPECTED["genesis_root"], (
        f"genesis root mismatch:\n  got {genesis_root.hex()}\n"
        f"  exp {EXPECTED['genesis_root']}"
    )

    genesis_doc = {
        "chain_id": h(CHAIN_ID),
        "workchain": WORKCHAIN,
        "genesis_root": h(genesis_root),
        "accounts": [
            {
                "id": h(aid),
                "balance_nanos": str(acc.balance),
                "pubkey": h(acc.pubkey),
                "nonce": acc.nonce,
                "lt": acc.lt,
            }
            for aid, acc in sorted(state.accounts.items())
        ],
    }

    fee_collector = account_id(0xAA)
    blocks_doc = []
    expectations = {"blocks": []}
    blocks_msgs = build_messages(state, keys)

    for seqno, msgs in enumerate(blocks_msgs, start=1):
        lt = state.last_lt + 1
        header, wires, root, receipts, new_state = propose_block(
            state, msgs, lt, fee_collector
        )
        state = new_state
        bh = block_hash(header)
        blocks_doc.append({
            "seqno": seqno,
            "prev_hash": h(header[4:36]),
            "lt": lt,
            "workchain": WORKCHAIN,
            "fee_collector": h(fee_collector),
            "msgs_root": h(header[36:68]),
            "state_root": h(root),
            "msg_count": len(msgs),
            "block_hash": h(bh),
            "header_hex": h(header),
            "messages": [h(w) for w in wires],
            "block_file_hex": h(block_file_bytes(header, wires)),
        })
        expectations["blocks"].append({
            "seqno": seqno,
            "state_root": h(root),
            "receipts": receipts,
        })

    assert blocks_doc[2]["state_root"] == EXPECTED["root_after_3"], (
        f"block 3 root mismatch: got {blocks_doc[2]['state_root']}"
    )
    assert blocks_doc[4]["state_root"] == EXPECTED["root_after_5"], (
        f"block 5 root mismatch: got {blocks_doc[4]['state_root']}"
    )

    with open(os.path.join(out_dir, "genesis.json"), "w") as f:
        json.dump(genesis_doc, f, indent=2)
        f.write("\n")
    with open(os.path.join(out_dir, "blocks.json"), "w") as f:
        json.dump({"blocks": blocks_doc}, f, indent=2)
        f.write("\n")
    with open(os.path.join(out_dir, "expectations.json"), "w") as f:
        json.dump(expectations, f, indent=2)
        f.write("\n")
    with open(os.path.join(out_dir, "divmod.json"), "w") as f:
        json.dump(divmod_doc(), f, indent=2)
        f.write("\n")
    print(f"vectors written to {out_dir}")
    print(f"  genesis root: {h(genesis_root)}")
    print(f"  block 3 root: {blocks_doc[2]['state_root']}")
    print(f"  block 5 root: {blocks_doc[4]['state_root']}")


def check(vectors_dir):
    with tempfile.TemporaryDirectory() as tmp:
        generate(tmp)
        drifted = []
        for name in VECTOR_FILES:
            a = os.path.join(vectors_dir, name)
            b = os.path.join(tmp, name)
            if not os.path.exists(a) or not filecmp.cmp(a, b, shallow=False):
                drifted.append(name)
        if drifted:
            print(f"VECTOR DRIFT: {', '.join(drifted)} differ from "
                  f"reference implementation output")
            print("run: python3 reference/gen_vectors.py")
            return 1
        print("vectors match reference implementation output")
        return 0


if __name__ == "__main__":
    if "--check" in sys.argv:
        sys.exit(check(VECTORS_DIR))
    generate(VECTORS_DIR)
