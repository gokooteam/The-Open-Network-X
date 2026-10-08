#!/usr/bin/env python3
"""Generate (or check) the fatal-vs-bounce reference vectors.

Usage:
  python3 reference/gen_fatal_bounce_vectors.py          # write reference/vectors/fatal_bounce.json
  python3 reference/gen_fatal_bounce_vectors.py --check  # fail if it would change (CI drift gate)

Implements ADR-0037 from the specification text, not from the Rust code:
  - A contract-execution failure is FATAL iff the message's gas budget was
    exhausted (ExceptionKind::OutOfGas). All other exception kinds bounce,
    whether raised by the VM or deliberately by the contract via THROW
    (0x77 maps onto the existing kinds, so user-thrown and VM-raised are
    indistinguishable by design).
  - Success with non-empty out_messages bounces (egress unwired).
  - Structural delivery cases (frozen/destroyed/uninitialized+payload/
    codeless+payload) bounce per ADR-0004, unchanged.

The vectors pin the closed ExceptionKind set -> outcome mapping. The Rust
agreement test asserts exhaustiveness: every variant of the closed set
must appear here. Stdlib only.
"""

import json
import os
import sys
import tempfile
import filecmp

# The closed ExceptionKind set, docs/specification/execution.md §3.4.
# ADR-0037: only OutOfGas is fatal.
EXCEPTION_OUTCOMES = {
    "OutOfGas": "fatal",
    "IntegerOverflow": "bounce",
    "AbsentNode": "bounce",
    "MalformedCell": "bounce",
    "TypeMismatch": "bounce",
}

# Non-exception delivery situations and their outcomes.
SITUATION_OUTCOMES = {
    # Contract ran clean but tried to emit messages (egress unwired).
    "success_with_out_messages": "bounce",
    # ADR-0004 structural cases, unchanged by ADR-0037.
    "frozen_destination": "bounce",
    "destroyed_destination": "bounce",
    "uninitialized_with_payload": "bounce",
    "codeless_active_with_payload": "bounce",
}


def build_vectors():
    return {
        "adr": "ADR-0037",
        "exception_outcomes": [
            {"exception_kind": kind, "outcome": outcome}
            for kind, outcome in EXCEPTION_OUTCOMES.items()
        ],
        "situation_outcomes": [
            {"situation": sit, "outcome": outcome}
            for sit, outcome in SITUATION_OUTCOMES.items()
        ],
    }


def main():
    check = "--check" in sys.argv
    here = os.path.dirname(os.path.abspath(__file__))
    out_path = os.path.join(here, "vectors", "fatal_bounce.json")
    text = json.dumps(build_vectors(), indent=2) + "\n"
    if check:
        with open(out_path) as f:
            current = f.read()
        if current != text:
            print("fatal_bounce.json drifted: regenerate without --check", file=sys.stderr)
            return 1
        print("fatal_bounce.json drift-clean")
        return 0
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(out_path))
    with os.fdopen(fd, "w") as f:
        f.write(text)
    if os.path.exists(out_path) and filecmp.cmp(tmp, out_path, shallow=False):
        os.remove(tmp)
        print("fatal_bounce.json unchanged")
    else:
        os.replace(tmp, out_path)
        print("wrote", out_path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
