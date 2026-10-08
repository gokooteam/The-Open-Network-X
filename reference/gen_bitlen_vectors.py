#!/usr/bin/env python3
"""Generate (or check) the cell bit-length reference vectors.

Usage:
  python3 reference/gen_bitlen_vectors.py          # write reference/vectors/bitlen.json
  python3 reference/gen_bitlen_vectors.py --check  # fail if it would change (CI drift gate)

Implements ADR-0036 from the specification text, not from the Rust code:
  - d1 bit 4 (0x10) = bit-granular ("compatible") flag; bits 5-7 reserved.
  - finalize(data, bit_len): byte-aligned bit_len -> byte-granular cell;
    otherwise the completion tag (single 1 bit at position bit_len, zeros
    below) is written into the last byte and the flag is set. Non-zero bits
    below the tag position fail closed.
  - from_bytes: rejects flagged cells with empty data, last byte 0x00
    (no completion 1), last byte 0x80 (lone tag = zero data bits in the final
    byte), and any reserved d1 bits set.
  - bit_len(cell): 8*len(data) when byte-granular, else derived from the
    completion tag: 8*(n-1) + (7 - trailing_zeros(last)).
  - builder append_bytes: bytes appended MSB-first at the current bit
    position; bit length advances by 8*len; fails closed past 1024 bits.
  - slice remaining_bits: cell.bit_len() - bit_offset.

The headline vector is ADR-0036's motivating collision: 3-bit `101`
(flagged, data b0) and 8-bit `10100000` (byte-granular, data a0) must hash
differently. Stdlib only.
"""

import json
import os
import sys
import tempfile
import filecmp

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from reference.cell import Cell, BIT_GRANULAR_FLAG, MAX_DATA_BITS

REF = bytes([0x11] * 32)


def build_finalize_vectors():
    # (data_hex, bit_len, refs) -> expectations from the Python model.
    cases = [
        # The collision pair.
        ("a0", 3, []),
        ("a0", 8, []),
        # Tag-position edges.
        ("00", 1, []),
        ("fe", 7, []),
        ("ff00", 9, []),
        ("ffff", 16, []),
        ("", 0, []),
        # Capacity edges: 1023 flagged, 1024 byte-granular.
        ("ff" * 128, 1023, []),
        ("ff" * 128, 1024, []),
        # Multi-byte partial with refs (20 bits: top 4 of the last byte).
        ("deadb0", 20, [REF]),
        # All-zeros partial: tag is the only 1 bit.
        ("0000", 9, []),
    ]
    out = []
    for data_hex, bit_len, refs in cases:
        cell = Cell.finalize(bytes.fromhex(data_hex), bit_len, refs)
        out.append({
            "data_hex": data_hex,
            "bit_len_in": bit_len,
            "refs_hex": [r.hex() for r in refs],
            "tagged_data_hex": cell.data.hex(),
            "bit_granular": cell.bit_granular,
            "descriptor_hex": cell.descriptor().hex(),
            "bit_len_out": cell.bit_len(),
            "hash_hex": cell.hash().hex(),
        })
    return out


def build_reject_vectors():
    # (encoding_hex, rule) — from_bytes must reject each of these.
    cases = [
        ("100100", "flagged_empty_last_0x00"),
        ("100180", "flagged_lone_tag_0x80"),
        ("1000", "flagged_empty_data"),
        ("2001a0", "reserved_d1_bit5"),
        ("4001a0", "reserved_d1_bit6"),
        ("8001a0", "reserved_d1_bit7"),
        ("f001a0", "reserved_d1_bits_all"),
        ("1003a0b0", "flagged_d2_truncated"),
    ]
    out = []
    for enc_hex, rule in cases:
        try:
            Cell.from_bytes(bytes.fromhex(enc_hex))
            status = "NOT_REJECTED"
        except ValueError:
            status = "rejected"
        out.append({"encoding_hex": enc_hex, "rule": rule, "status": status})
    return out


def build_decode_vectors():
    # Canonical encodings -> decoded flag + bit length (+ hash for pinning).
    cases = [
        "1001b0",      # flagged 3-bit
        "0001a0",      # byte-granular 8-bit
        "100140",      # flagged 1-bit
        "1002ff40",    # flagged 9-bit
        "0000",        # empty byte-granular
    ]
    out = []
    for enc_hex in cases:
        cell = Cell.from_bytes(bytes.fromhex(enc_hex))
        out.append({
            "encoding_hex": enc_hex,
            "bit_granular": cell.bit_granular,
            "bit_len": cell.bit_len(),
            "hash_hex": cell.hash().hex(),
        })
    return out


def builder_append(start_data_hex, start_bit_len, append_hex):
    """Independent spec-text model of Builder::append_bytes."""
    data = bytearray(bytes.fromhex(start_data_hex))
    bit_len = start_bit_len
    append = bytes.fromhex(append_hex)
    if bit_len + 8 * len(append) > MAX_DATA_BITS:
        return {"ok": False, "error": "MalformedCell"}
    for b in append:
        for i in range(8):
            bit = (b >> (7 - i)) & 1
            byte_idx = bit_len // 8
            if byte_idx >= len(data):
                data.append(0)
            if bit:
                data[byte_idx] |= 1 << (7 - (bit_len % 8))
            bit_len += 1
    return {"ok": True, "data_hex": bytes(data).hex(), "bit_len": bit_len}


def build_builder_vectors():
    cases = [
        # (start_data, start_bit_len, append) — aligned and misaligned.
        ("", 0, "a0"),
        ("a0", 8, "ff"),
        ("a0", 3, "ff"),          # misaligned: 3 bits then a byte
        ("a0", 3, "ff00"),        # misaligned multi-byte
        ("", 0, ""),              # empty append is a no-op
        ("ff" * 128, 1024, "00"),  # at capacity: one more byte fails
        ("ff" * 127 + "fe", 1023, "00"),  # 1023 + 8 > 1024 fails
        ("ff" * 127 + "fe", 1023, ""),    # 1023 + 0 ok
    ]
    out = []
    for start_data, start_bit_len, append in cases:
        entry = {"start_data_hex": start_data, "start_bit_len": start_bit_len,
                 "append_hex": append}
        entry.update(builder_append(start_data, start_bit_len, append))
        out.append(entry)
    return out


def build_slice_vectors():
    # (cell encoding or finalize spec, bit_offset) -> remaining bits.
    cases = [
        ({"finalize": ("a0", 3)}, 0),
        ({"finalize": ("a0", 3)}, 3),
        ({"finalize": ("a0", 8)}, 0),
        ({"finalize": ("a0", 8)}, 5),
        ({"encoding": "1002ff40"}, 0),   # flagged 9-bit
        ({"encoding": "1002ff40"}, 9),
    ]
    out = []
    for spec, offset in cases:
        if "finalize" in spec:
            data_hex, bit_len = spec["finalize"]
            cell = Cell.finalize(bytes.fromhex(data_hex), bit_len, [])
            label = f"finalize({data_hex},{bit_len})"
        else:
            cell = Cell.from_bytes(bytes.fromhex(spec["encoding"]))
            label = f"decode({spec['encoding']})"
        remaining = cell.bit_len() - offset
        out.append({"cell": label, "bit_offset": offset,
                    "remaining_bits": remaining})
    return out


def build_vectors():
    return {
        "adr": "ADR-0036",
        "constants": {
            "BIT_GRANULAR_FLAG": BIT_GRANULAR_FLAG,
            "MAX_DATA_BITS": MAX_DATA_BITS,
        },
        "finalize": build_finalize_vectors(),
        "decode_reject": build_reject_vectors(),
        "decode_accept": build_decode_vectors(),
        "builder_append": build_builder_vectors(),
        "slice_remaining": build_slice_vectors(),
    }


def main():
    check = "--check" in sys.argv
    vectors = build_vectors()
    # Every reject vector must actually reject, or the file is a lie.
    bad = [v for v in vectors["decode_reject"] if v["status"] != "rejected"]
    if bad:
        print(f"MODEL FAILURE: {len(bad)} reject vectors not rejected: {bad}",
              file=sys.stderr)
        sys.exit(2)
    out_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "vectors")
    out_path = os.path.join(out_dir, "bitlen.json")
    text = json.dumps(vectors, indent=2) + "\n"

    if check:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = os.path.join(tmp, "bitlen.json")
            with open(tmp_path, "w") as f:
                f.write(text)
            if not filecmp.cmp(tmp_path, out_path, shallow=False):
                print("DRIFT: reference/vectors/bitlen.json differs from generator output",
                      file=sys.stderr)
                sys.exit(1)
        print("bitlen.json: no drift")
    else:
        with open(out_path, "w") as f:
            f.write(text)
        print(f"wrote {out_path}")


if __name__ == "__main__":
    main()
