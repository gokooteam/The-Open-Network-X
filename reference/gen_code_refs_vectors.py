#!/usr/bin/env python3
"""Generate (or check) the code_refs reference vectors.

Usage:
  python3 reference/gen_code_refs_vectors.py          # write reference/vectors/code_refs.json
  python3 reference/gen_code_refs_vectors.py --check  # fail if it would change (CI drift gate)

Implements ADR-0039 (Wave 4 step 7, "code_refs LAST") from the
specification text, not from the Rust code. Two halves:

A. Live code_refs (the wiring):
  - `ref_index` N addresses the Nth child reference of the CURRENT code
    cell (index order), resolved through the content-addressed cell store.
  - A child hash absent from the store fails closed with `AbsentNode`
    (same rule as LDREF/CTOS — never invented data).
  - `code_refs` is refreshed at every code switch (single choke point):
    at `run()` entry (fail fast), after every JMPREF/CALLREF jump, after
    RET restores a (code, pc) pair, and after c0/c1/c2 continuation jumps —
    so it always mirrors the current code cell, never a stale ancestor.
  - Design call (ADR-0039): refresh-on-jump, not fixed-to-root — from any
    code cell its own up-to-4 children are addressable, so a program
    larger than one cell is a navigable tree of code cells.

B. Call-stack depth limit:
  - MAX_CALL_STACK_DEPTH = 256.
  - CALL variants (0x71 CALLREF, 0x75 IFCALLREF, 0x76 IFNOTCALLREF) raise
    CallStackOverflow iff call_stack.len() >= MAX_CALL_STACK_DEPTH at CALL
    time. The check is fail-closed: no return address is pushed and the
    frame's heap clone never happens.
  - Check ordering: the opcode's 4 gas is consumed FIRST (same ordering as
    the ref_idx range check), then ref_idx bounds, then the depth check,
    then the push. A would-be overflow costs exactly the opcode's 4 gas.
  - JMP variants (0x70 JMPREF, 0x73 IFJMPREF, 0x74 IFNOTJMPREF) never touch
    the call stack, so the depth limit does not apply to them.
  - THROW (0x77) operand domain is UNCHANGED (0-3): CallStackOverflow is
    VM-raised only; a contract cannot deliberately raise it.
  - c2 exception-handler discriminator: CallStackOverflow -> 5.
    Discriminators are append-only (existing 0-4 never move).
  - Fatal-vs-bounce outcome for CallStackOverflow is "bounce" (pinned in
    reference/vectors/fatal_bounce.json, ADR-0037 + ADR-0039): it is a
    VM-raised structural failure, not gas exhaustion.

Why the limit exists (Claude's Wave 4 finding): gas bounds execution TIME,
not MEMORY. At 4 gas per CALL, the ADR-0034 per-message cap (10M gas)
admits ~2.5M call frames x ~128B/frame (~340MB of heap) from a single
paid message — an uncatchable host OOM, not a deterministic VM exception.
The depth limit caps worst-case call-stack memory at 256 x ~128B (~35KB).

Stdlib only.
"""

import json
import os
import sys
import tempfile
import filecmp

MAX_CALL_STACK_DEPTH = 256

# Opcodes that push a return address (subject to the depth limit) vs those
# that merely jump (not subject). From docs/specification/tvm-instruction-set.md §4.5.
CALL_OPCODES = {
    0x71: "CALLREF",
    0x75: "IFCALLREF",
    0x76: "IFNOTCALLREF",
}
JMP_OPCODES = {
    0x70: "JMPREF",
    0x73: "IFJMPREF",
    0x74: "IFNOTJMPREF",
}

# THROW (0x77) operand -> ExceptionKind. ADR-0039 deliberately does NOT add
# a 4 -> CallStackOverflow mapping.
THROW_OPERANDS = {
    0: "IntegerOverflow",
    1: "AbsentNode",
    2: "MalformedCell",
    3: "TypeMismatch",
}

# c2 exception-handler discriminators (append-only; ADR-0039 adds 5).
C2_DISCRIMINATORS = {
    "OutOfGas": 0,
    "IntegerOverflow": 1,
    "AbsentNode": 2,
    "MalformedCell": 3,
    "TypeMismatch": 4,
    "CallStackOverflow": 5,
}


def call_outcome(depth_before):
    """ADR-0039 CALL rule: outcome of a taken CALL variant at the given
    call-stack depth before the call."""
    if depth_before < MAX_CALL_STACK_DEPTH:
        return "ok"
    return "CallStackOverflow"


def build_vectors():
    return {
        "adr": "ADR-0039",
        # --- A. Live code_refs wiring ---
        "ref_resolution": {
            "rule": "ref_index N addresses the Nth child reference of the current code cell, resolved through cell_store in index order",
            "missing_child": "AbsentNode",
            "refresh_points": [
                "run() entry (fail fast on unseeded code DAG)",
                "after JMPREF/CALLREF (and conditional variants) switch target",
                "after RET restores a (code, pc) pair",
                "after c0/c1/c2 continuation jumps",
            ],
            "design": "refresh-on-jump, not fixed-to-root: every code cell's own children are addressable",
        },
        # --- B. Depth limit ---
        "max_call_stack_depth": MAX_CALL_STACK_DEPTH,
        # The fail-closed rule as a depth -> outcome table, pinned at the
        # boundary and at representative interior points.
        "call_rule": [
            {"depth_before": d, "outcome": call_outcome(d)}
            for d in (0, 1, 127, 255, 256, 257, 1000)
        ],
        "call_opcodes": [
            {"opcode": hex(op), "name": name} for op, name in sorted(CALL_OPCODES.items())
        ],
        "jmp_opcodes_not_subject": [
            {"opcode": hex(op), "name": name} for op, name in sorted(JMP_OPCODES.items())
        ],
        "throw_operands": [
            {"operand": op, "kind": kind} for op, kind in sorted(THROW_OPERANDS.items())
        ],
        "c2_discriminators": [
            {"kind": kind, "code": code} for kind, code in C2_DISCRIMINATORS.items()
        ],
        "check_ordering": [
            "consume 4 gas (opcode cost)",
            "validate ref_idx (MalformedCell if out of range)",
            "depth check: call_stack.len() >= MAX_CALL_STACK_DEPTH -> CallStackOverflow",
            "push return address",
        ],
    }


def main():
    check = "--check" in sys.argv
    here = os.path.dirname(os.path.abspath(__file__))
    out_path = os.path.join(here, "vectors", "code_refs.json")
    vectors = build_vectors()
    text = json.dumps(vectors, indent=2) + "\n"

    if check:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = os.path.join(tmp, "code_refs.json")
            with open(tmp_path, "w") as f:
                f.write(text)
            if not filecmp.cmp(tmp_path, out_path, shallow=False):
                print("DRIFT: reference/vectors/code_refs.json differs from generator output",
                      file=sys.stderr)
                sys.exit(1)
        print("code_refs.json: no drift")
    else:
        with open(out_path, "w") as f:
            f.write(text)
        print(f"wrote {out_path}")


if __name__ == "__main__":
    main()
