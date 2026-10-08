#!/usr/bin/env python3
"""Generate (or check) the storage-stat reference vectors.

Usage:
  python3 reference/gen_storage_vectors.py          # write reference/vectors/storage_stat.json
  python3 reference/gen_storage_vectors.py --check  # fail if it would change (CI drift gate)

Implements ADR-0037 from the specification text, not from the Rust code:
  - StorageStat { cell_count, byte_count, bit_count } over the account's
    embedded (code, data) root cells.
  - cell_count: number of cells.
  - byte_count: sum of canonical cell bytes (Cell.to_bytes().len()).
  - bit_count: sum of Cell.bit_len() — precise for bit-granular cells
    (ADR-0036), 8 * data_bytes for byte-granular ones.

Fixtures cover: empty stat, byte-granular cells, bit-granular cells
(3-bit `101` vs 8-bit `10100000` — the ADR-0036 collision pair), cells
with references, and mixed code+data pairs. Stdlib only.
"""

import json
import os
import sys
import tempfile
import filecmp

from reference.cell import Cell


def storage_stat(cells):
    """Reference StorageStat over a list of (role, fixture)."""
    objs = [f["obj"] for _, f in cells]
    cell_count = len(objs)
    byte_count = sum(c.to_bytes().__len__() for c in objs)
    bit_count = sum(c.bit_len() for c in objs)
    return {
        "cell_count": cell_count,
        "byte_count": byte_count,
        "bit_count": bit_count,
    }


def main():
    check = "--check" in sys.argv
    here = os.path.dirname(os.path.abspath(__file__))
    out_path = os.path.join(here, "vectors", "storage_stat.json")

    # Fixture cells, built through the reference model. Each fixture records
    # the inputs needed to rebuild the cell independently (data_hex,
    # bit_len, refs_hex), so the Rust agreement test reconstructs them via
    # Cell::new_with_bit_len rather than trusting this script's outputs.
    def fixture(cell, data_hex, bit_len, refs_hex):
        return {
            "cell": {"data_hex": data_hex, "bit_len": bit_len, "refs_hex": refs_hex},
            "obj": cell,
        }

    code_plain = fixture(Cell(b"\x01\x02\x03\x04", []), "01020304", 32, [])
    data_plain = fixture(Cell(b"\x00" * 8, []), "00" * 8, 64, [])
    tiny_flagged = fixture(Cell.finalize(b"\xb0", 3, []), "b0", 3, [])       # 3-bit `101`, flagged (ADR-0036 pair)
    eight_bits = fixture(Cell(b"\xa0", []), "a0", 8, [])                     # 8-bit `10100000`, unflagged (ADR-0036 pair)
    partial = fixture(Cell.finalize(b"\xab\xc8", 12, []), "abc8", 12, [])   # 12 bits over 2 bytes
    with_refs = fixture(
        Cell(b"\xff", [bytes([0x11] * 32), bytes([0x22] * 32)]),
        "ff", 8, ["11" * 32, "22" * 32],
    )

    cases = [
        {"name": "empty", "cells": []},
        {"name": "single_byte_granular", "cells": [("code", code_plain)]},
        {"name": "code_and_data", "cells": [("code", code_plain), ("data", data_plain)]},
        {"name": "flagged_3bit", "cells": [("data", tiny_flagged)]},
        {"name": "byte_8bit_collision_pair", "cells": [("data", eight_bits)]},
        {"name": "flagged_12bit", "cells": [("data", partial)]},
        {"name": "cell_with_refs", "cells": [("code", with_refs)]},
        {
            "name": "mixed",
            "cells": [("code", with_refs), ("data", tiny_flagged)],
        },
    ]

    vectors = {
        "adr": "ADR-0037",
        "note": (
            "bit_count is sum(Cell.bit_len()); byte_count is sum of canonical "
            "cell bytes. The 3-bit flagged cell and the 8-bit byte-granular "
            "cell share byte_count but differ in bit_count — that is the "
            "ADR-0036 precision this step adds."
        ),
        "cases": [
            {
                "name": c["name"],
                "cells": [
                    {"role": role, **f["cell"]} for role, f in c["cells"]
                ],
                "stat": storage_stat(c["cells"]),
            }
            for c in cases
        ],
    }

    text = json.dumps(vectors, indent=2) + "\n"
    if check:
        with open(out_path) as f:
            current = f.read()
        if current != text:
            print("storage_stat.json drifted: regenerate without --check", file=sys.stderr)
            return 1
        print("storage_stat.json drift-clean")
        return 0
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(out_path))
    with os.fdopen(fd, "w") as f:
        f.write(text)
    if os.path.exists(out_path) and filecmp.cmp(tmp, out_path, shallow=False):
        os.remove(tmp)
        print("storage_stat.json unchanged")
    else:
        os.replace(tmp, out_path)
        print("wrote", out_path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
