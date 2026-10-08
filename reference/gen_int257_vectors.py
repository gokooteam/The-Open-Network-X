#!/usr/bin/env python3
"""Generate (or check) the 257-bit integer-model reference vectors.

Usage:
  python3 reference/gen_int257_vectors.py          # write reference/vectors/int257.json
  python3 reference/gen_int257_vectors.py --check  # fail if it would change (CI drift gate)

Implements the model from the specification text
(docs/specification/tvm-instruction-set.md §3.2/§3.3/§4.2 and ADR-0035),
not from the Rust code:

- Integer domain: the closed range [-2^256, 2^256 - 1] (257-bit signed).
- Every arithmetic opcode computes the TRUE mathematical result exactly,
  then applies the declared width/flavor:
    flavor 0 (unsigned): 0 <= v < 2^width, else IntegerOverflow;
    flavor 1 (signed):   -2^(width-1) <= v < 2^(width-1), else IntegerOverflow;
    flavor 2 (modulo):   v mod 2^width as the unsigned bit pattern, never
                         raises on the result.
- Range checks precede flavor selection and raise IntegerOverflow for every
  flavor: division by zero (DIV/DIVMOD), shift amount < 0 or >= width
  (LSHIFT/RSHIFT).
- DIVMOD/DIV are floored: q = floor(a/b), r = a - q*b, sign(r) == sign(b)
  or r == 0. Python's // and divmod() ARE this definition, so the language
  itself is the oracle here.

Values are encoded as canonical 33-byte big-endian hex (byte[0] & 0xFE == 0,
bit 256 is the sign), alongside the decimal string for readability.
Stdlib only.
"""

import json
import os
import sys

DOMAIN_MIN = -(1 << 256)
DOMAIN_MAX = (1 << 256) - 1

WIDTHS = [1, 2, 8, 64, 127, 128, 255, 256]
FLAVORS = [0, 1, 2]


def check_domain(v: int, where: str):
    assert DOMAIN_MIN <= v <= DOMAIN_MAX, f"{where}: {v} outside Integer domain"


def apply_width(width: int, flavor: int, v: int):
    """Returns (ok, value); ok=False means IntegerOverflow."""
    assert 1 <= width <= 256 and flavor in (0, 1, 2)
    if flavor == 0:
        if v < 0 or v >= (1 << width):
            return (False, None)
        return (True, v)
    if flavor == 1:
        lo, hi = -(1 << (width - 1)), (1 << (width - 1))
        if v < lo or v >= hi:
            return (False, None)
        return (True, v)
    return (True, v % (1 << width))


def enc(v: int):
    """A domain value as {"dec": ..., "hex33": ...}."""
    check_domain(v, "enc")
    if v >= 0:
        raw = v.to_bytes(32, "big")
        hex33 = "00" + raw.hex()
    else:
        # 257-bit two's complement: byte0 = 0x01, low 256 bits = 2^256 + v.
        low = ((1 << 256) + v).to_bytes(32, "big")
        hex33 = "01" + low.hex()
    assert len(hex33) == 66 and hex33[:2] in ("00", "01")
    return {"dec": str(v), "hex33": hex33}


def err():
    return {"error": "IntegerOverflow"}


def binop_case(op: str, a: int, b: int, width: int, flavor: int):
    """One vector case for a binary op."""
    for x in (a, b):
        check_domain(x, f"{op} operand")
    if op == "add":
        v = a + b
    elif op == "sub":
        v = a - b
    elif op == "mul":
        v = a * b
    elif op == "divmod":
        if b == 0:
            return {"op": op, "a": enc(a), "b": enc(b),
                    "width": width, "flavor": flavor, "result": err()}
        q, r = divmod(a, b)  # floored, by language definition
        ok_q, qv = apply_width(width, flavor, q)
        ok_r, rv = apply_width(width, flavor, r)
        if not ok_q or not ok_r:
            return {"op": op, "a": enc(a), "b": enc(b),
                    "width": width, "flavor": flavor, "result": err()}
        check_domain(qv, f"{op} q")
        check_domain(rv, f"{op} r")
        return {"op": op, "a": enc(a), "b": enc(b), "width": width,
                "flavor": flavor,
                "result": {"q": enc(qv), "r": enc(rv)}}
    elif op == "div":
        if b == 0:
            return {"op": op, "a": enc(a), "b": enc(b),
                    "width": width, "flavor": flavor, "result": err()}
        q = a // b  # floored, by language definition
        ok, qv = apply_width(width, flavor, q)
        if not ok:
            return {"op": op, "a": enc(a), "b": enc(b),
                    "width": width, "flavor": flavor, "result": err()}
        check_domain(qv, f"{op} q")
        return {"op": op, "a": enc(a), "b": enc(b), "width": width,
                "flavor": flavor, "result": enc(qv)}
    elif op == "lshift":
        if b < 0 or b >= width:
            return {"op": op, "a": enc(a), "b": enc(b),
                    "width": width, "flavor": flavor, "result": err()}
        v = a * (1 << b)
    elif op == "rshift":
        if b < 0 or b >= width:
            return {"op": op, "a": enc(a), "b": enc(b),
                    "width": width, "flavor": flavor, "result": err()}
        v = a // (1 << b)  # arithmetic shift right == floor division
    else:
        raise AssertionError(f"unknown op {op}")
    ok, vv = apply_width(width, flavor, v)
    if not ok:
        return {"op": op, "a": enc(a), "b": enc(b),
                "width": width, "flavor": flavor, "result": err()}
    check_domain(vv, f"{op} result")
    return {"op": op, "a": enc(a), "b": enc(b), "width": width,
            "flavor": flavor, "result": enc(vv)}


def unop_case(op: str, a: int, width: int, flavor: int):
    """One vector case for a unary op (neg, conv)."""
    check_domain(a, f"{op} operand")
    if op == "neg":
        v = -a
    elif op == "conv":
        v = a
    else:
        raise AssertionError(f"unknown op {op}")
    ok, vv = apply_width(width, flavor, v)
    if not ok:
        return {"op": op, "a": enc(a), "width": width, "flavor": flavor,
                "result": err()}
    check_domain(vv, f"{op} result")
    return {"op": op, "a": enc(a), "width": width, "flavor": flavor,
            "result": enc(vv)}


def xorshift64(state):
    x = state[0]
    x ^= (x << 13) & 0xFFFFFFFFFFFFFFFF
    x ^= x >> 7
    x ^= (x << 17) & 0xFFFFFFFFFFFFFFFF
    state[0] = x & 0xFFFFFFFFFFFFFFFF
    return state[0]


# Edge operands: small values, powers of two around every limb boundary,
# the domain extremes. All within [-2^256, 2^256 - 1].
EDGE_OPERANDS = [
    0, 1, -1, 2, -2, 3, 7, -7, 8, -8,
    255, 256, -255, -256,
    (1 << 63) - 1, 1 << 63, -(1 << 63), -(1 << 64),
    (1 << 64) - 1, 1 << 64,
    (1 << 126) - 1, 1 << 126, -(1 << 126),
    (1 << 127) - 1, 1 << 127, -(1 << 127), -(1 << 127) - 1,
    (1 << 128) - 1, 1 << 128, -(1 << 128),
    (1 << 200) - 1, 1 << 200, -(1 << 200),
    (1 << 254) - 1, 1 << 254, -(1 << 254),
    (1 << 255) - 1, 1 << 255, -(1 << 255), -(1 << 255) - 1,
    (1 << 256) - 1, -(1 << 256) + 1, -(1 << 256),
]


def rand_operand(state):
    """Deterministic pseudo-random domain value, biased to edges."""
    r = xorshift64(state)
    pick = r % 4
    if pick == 0:
        # Uniform 256-bit magnitude, random sign.
        mag = 0
        for _ in range(4):
            mag = (mag << 64) | xorshift64(state)
        v = mag if xorshift64(state) % 2 == 0 else -mag - 1
        # Clamp into the domain.
        return max(DOMAIN_MIN, min(DOMAIN_MAX, v))
    if pick == 1:
        # Small value.
        return (xorshift64(state) % 2001) - 1000
    # Edge neighborhood.
    base = EDGE_OPERANDS[xorshift64(state) % len(EDGE_OPERANDS)]
    delta = (xorshift64(state) % 5) - 2
    return max(DOMAIN_MIN, min(DOMAIN_MAX, base + delta))


def build_vectors():
    cases = []
    # 1. Exhaustive edge grid at a few (width, flavor) points per op.
    grid_points = [
        (8, 0), (8, 1), (8, 2),
        (64, 0), (64, 1), (64, 2),
        (128, 0), (128, 1), (128, 2),
        (256, 0), (256, 1), (256, 2),
    ]
    for op in ("add", "sub", "mul"):
        for width, flavor in grid_points:
            for a in EDGE_OPERANDS:
                for b in (EDGE_OPERANDS[::7]):
                    cases.append(binop_case(op, a, b, width, flavor))
    for op in ("div", "divmod"):
        for width, flavor in grid_points:
            for a in EDGE_OPERANDS:
                for b in EDGE_OPERANDS[::5]:
                    cases.append(binop_case(op, a, b, width, flavor))
    for op in ("lshift", "rshift"):
        shifts = [-1, 0, 1, 7, 8, 63, 64, 127, 128, 255, 256]
        for width, flavor in grid_points:
            for a in EDGE_OPERANDS[::3]:
                for b in shifts:
                    cases.append(binop_case(op, a, b, width, flavor))
    for op in ("neg", "conv"):
        for width, flavor in grid_points:
            for a in EDGE_OPERANDS:
                cases.append(unop_case(op, a, width, flavor))
    # 2. Deterministic pseudo-random sweep across all widths/flavors.
    state = [0x51ABC3D2E4F607]
    ops = ["add", "sub", "mul", "div", "divmod", "lshift", "rshift", "neg", "conv"]
    for _ in range(4000):
        op = ops[xorshift64(state) % len(ops)]
        width = WIDTHS[xorshift64(state) % len(WIDTHS)]
        flavor = FLAVORS[xorshift64(state) % len(FLAVORS)]
        a = rand_operand(state)
        if op in ("neg", "conv"):
            cases.append(unop_case(op, a, width, flavor))
        else:
            # Shifts need small amounts most of the time, but sometimes wild.
            if op in ("lshift", "rshift") and xorshift64(state) % 3 != 0:
                b = (xorshift64(state) % (width + 3)) - 1
            else:
                b = rand_operand(state)
            cases.append(binop_case(op, a, b, width, flavor))
    return {
        "semantics": (
            "257-bit Integer model (ADR-0035): domain [-2^256, 2^256 - 1]; "
            "exact arithmetic, then width/flavor application "
            "(0=unsigned raise, 1=signed raise, 2=modulo wrap); range checks "
            "precede flavor; DIVMOD/DIV floored (Python divmod == spec)."
        ),
        "integer_bits": 257,
        "domain_min": str(DOMAIN_MIN),
        "domain_max": str(DOMAIN_MAX),
        "cases": cases,
    }


def write_or_check(check: bool):
    vectors = build_vectors()
    out_path = os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "vectors", "int257.json"
    )
    text = json.dumps(vectors, indent=2) + "\n"
    if check:
        with open(out_path) as f:
            current = f.read()
        if current != text:
            print("int257.json is stale: regenerate with gen_int257_vectors.py")
            sys.exit(1)
        print(f"int257.json: {len(vectors['cases'])} cases, drift-clean")
    else:
        with open(out_path, "w") as f:
            f.write(text)
        print(f"wrote {out_path}: {len(vectors['cases'])} cases")


if __name__ == "__main__":
    write_or_check("--check" in sys.argv[1:])
