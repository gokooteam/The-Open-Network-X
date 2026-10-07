#!/usr/bin/env python3
"""Generate (or check) the gas-cap reference vectors.

Usage:
  python3 reference/gen_gas_vectors.py          # write reference/vectors/gas_caps.json
  python3 reference/gen_gas_vectors.py --check  # fail if it would change (CI drift gate)

Implements ADR-0034 from the specification text, not from the Rust code:
  - GAS_PER_NANO = 1_000
  - MAX_GAS_PER_MESSAGE = 10_000_000
  - MAX_GAS_PER_BLOCK = 100_000_000
  - message_gas_limit(fee_nanos) = min(fee_nanos * GAS_PER_NANO saturating at u64::MAX,
                                      MAX_GAS_PER_MESSAGE)
  - accumulate_block_gas(total, additional): new total, or BlockGasExceeded if
    the saturating sum would exceed MAX_GAS_PER_BLOCK.

Vectors pin the constants and the clamp/accumulate behavior, including the
saturation edges. Stdlib only.
"""

import json
import os
import sys
import tempfile
import filecmp

U64_MAX = (1 << 64) - 1
U128_MAX = (1 << 128) - 1

GAS_PER_NANO = 1_000
MAX_GAS_PER_MESSAGE = 10_000_000
MAX_GAS_PER_BLOCK = 100_000_000


def message_gas_limit(fee_nanos: int) -> int:
    """ADR-0034 per-message gas limit. Pure function of the fee."""
    # saturating_mul at u128, then min with u64::MAX, then the cap.
    product = fee_nanos * GAS_PER_NANO
    if product > U128_MAX:
        product = U128_MAX
    as_u64 = product if product <= U64_MAX else U64_MAX
    return as_u64 if as_u64 <= MAX_GAS_PER_MESSAGE else MAX_GAS_PER_MESSAGE


def accumulate_block_gas(current_total: int, additional: int):
    """ADR-0034 block gas accumulation.

    Returns (ok, new_total) or (err, used) where err is "BlockGasExceeded".
    """
    new_total = current_total + additional
    if new_total > U64_MAX:
        new_total = U64_MAX  # saturating_add
    if new_total > MAX_GAS_PER_BLOCK:
        return (False, new_total)
    return (True, new_total)


def build_vectors():
    # --- message_gas_limit vectors: (fee_nanos as decimal string, gas_limit) ---
    # fee_nanos is u128; JSON carries it as a decimal string to avoid
    # float-precision loss.
    fee_cases = [
        0,
        1,
        5_000,
        9_999,
        10_000,          # 10_000 * 1_000 = 10_000_000 = cap exactly
        10_001,          # first clamped value
        100_000,
        1_000_000_000,
        U64_MAX,         # saturates the u128 multiply? no: U64_MAX*1000 < 2^128
        U128_MAX,        # saturates: product would exceed u128
    ]
    message_vectors = [
        {
            "fee_nanos": str(fee),
            "gas_limit": message_gas_limit(fee),
        }
        for fee in fee_cases
    ]

    # --- accumulate_block_gas vectors: (current, additional) -> ok/new_total or err ---
    acc_cases = [
        (0, 0),
        (0, 1_000),
        (1_000, 2_000),
        (MAX_GAS_PER_BLOCK - 1, 1),   # lands exactly on the cap: ok
        (MAX_GAS_PER_BLOCK, 0),       # at cap, zero addition: ok
        (MAX_GAS_PER_BLOCK, 1),       # one over: BlockGasExceeded
        (0, MAX_GAS_PER_BLOCK + 1),   # large overshoot
        (MAX_GAS_PER_MESSAGE * 9, MAX_GAS_PER_MESSAGE),  # 90M + 10M = cap: ok
        (MAX_GAS_PER_MESSAGE * 9, MAX_GAS_PER_MESSAGE + 1),  # over by one
        (U64_MAX, U64_MAX),           # saturating_add path: still rejected
    ]
    acc_vectors = []
    for current, additional in acc_cases:
        ok, value = accumulate_block_gas(current, additional)
        entry = {
            "current_total": current,
            "additional": additional,
        }
        if ok:
            entry["ok"] = True
            entry["new_total"] = value
        else:
            entry["ok"] = False
            entry["error"] = "BlockGasExceeded"
            entry["used"] = value
            entry["cap"] = MAX_GAS_PER_BLOCK
        acc_vectors.append(entry)

    return {
        "adr": "ADR-0034",
        "constants": {
            "GAS_PER_NANO": GAS_PER_NANO,
            "MAX_GAS_PER_MESSAGE": MAX_GAS_PER_MESSAGE,
            "MAX_GAS_PER_BLOCK": MAX_GAS_PER_BLOCK,
            "BLOCK_CAP_IS_N_MESSAGE_CAPS": MAX_GAS_PER_BLOCK // MAX_GAS_PER_MESSAGE,
        },
        "message_gas_limit": message_vectors,
        "accumulate_block_gas": acc_vectors,
    }


def main():
    check = "--check" in sys.argv
    vectors = build_vectors()
    out_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "vectors")
    out_path = os.path.join(out_dir, "gas_caps.json")
    text = json.dumps(vectors, indent=2) + "\n"

    if check:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = os.path.join(tmp, "gas_caps.json")
            with open(tmp_path, "w") as f:
                f.write(text)
            if not filecmp.cmp(tmp_path, out_path, shallow=False):
                print("DRIFT: reference/vectors/gas_caps.json differs from generator output",
                      file=sys.stderr)
                sys.exit(1)
        print("gas_caps.json: no drift")
    else:
        with open(out_path, "w") as f:
            f.write(text)
        print(f"wrote {out_path}")


if __name__ == "__main__":
    main()
