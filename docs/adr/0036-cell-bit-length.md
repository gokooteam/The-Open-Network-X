# ADR-0036 — Cell Bit-Length Commitment via Compatible Flag (Wave 4, step 4)

**Status:** Proposed (2026-10-07)
**Decider:** Gokoo (design authority between audits; flagged for Claude's Wave 4 audit)

## Context

Claude's ONX review (finding 3, `claude-onx-review-2026-10-06.md`): the cell
hash commits byte length, not bit length. A 3-bit `101` and an 8-bit
`10100000` stored in one byte hash identically — two semantically different
cells with the same hash. The builder half of the finding: `STBYTES`
extended `data_bytes` without advancing `current_bit_len`, and `ENDC`
ignored the bit length entirely, so the VM could not even express "this cell
holds 3 bits."

The Wave 4 reorder (`claude-onx-review-2026-10-06.md` §"Wave 4 reorder")
decided the design before this implementation: **compatible flag, NOT
TON-style descriptor**. A descriptor-only change buys zero TON compatibility
(TON's hash also commits child depths, level info, and exotic types) while
forcing a trie restructuring ONX does not want (ONX cells hold 128 bytes =
1024 bits; TON's scheme caps at 1023 because the completion tag consumes a
bit). Full TON representation is a months-long program; the flag keeps the
door open without paying for it now. The decision also pinned six spec
rules (flag bit, 0x00/0x80 invalid, 1024-vs-1023 capacity, ENDC sets the
flag on partial bits, every decoder enforces the rules) — this ADR is the
implementation of exactly that decision.

## Decision

### 1. The flag

Descriptor `d1` bit 4 (`0x10`) is the bit-granular ("compatible") flag.
Bits 5–7 stay reserved and must be zero. Flag clear ⇒ byte-granular cell,
bit length = `8 × d2`, hashes bit-identical to every pre-ADR-0036 cell.

### 2. The completion tag

A flagged cell's last data byte carries a TON-style completion tag: a
single 1 bit at position `bit_len` (0-indexed from the MSB), zeros below
it. The bit length is derived, never stored separately:

```
bit_len = 8 × (n − 1) + (7 − trailing_zeros(last_byte))
```

`Cell::new_with_bit_len(data, bit_len, refs)` is the single constructor
that knows this layout: byte-aligned lengths stay byte-granular; otherwise
it verifies the tag region is zero (fail-closed on caller garbage — never
silently masked), writes the tag, and sets the flag. `Cell::new` /
`Cell::new_with_special` keep their byte-granular behavior; all existing
call sites are untouched.

### 3. Decoder canonicality (fail-closed, every decoder identical)

`Cell::from_bytes` (and therefore the BoC decoder, which delegates to it)
enforces:

- d1 bits 5–7 set ⇒ `InvalidDescriptor` (unchanged, mask narrowed 0xF0→0xE0);
- flag set with empty data ⇒ `InvalidDescriptor` (a zero bit length is
  byte-granular);
- flag set with last byte `0x00` ⇒ `InvalidDescriptor` (no completion 1);
- flag set with last byte `0x80` ⇒ `InvalidDescriptor` (lone tag = zero
  data bits in the final byte ⇒ bit length would be a multiple of 8,
  contradicting the flag).

### 4. Builder bookkeeping

- `Builder::append_bytes` (new): bytes are appended MSB-first at the
  current bit position and `current_bit_len` advances by `8 × len` —
  exact at any alignment. Fail-closed past 1024 bits. `STBYTES` calls it,
  replacing the old extend-without-advancing (the bookkeeping half of the
  finding). Interleaved `STBITS`/`STBYTES` now does the obviously-intended
  bit-stream thing instead of silently dropping the bit length.
- `ENDC` calls `Cell::new_with_bit_len(builder.data_bytes,
  builder.current_bit_len, refs)`: partial-bit builders become flagged
  cells; byte-aligned builders produce byte-identical cells to before.

### 5. Slice readers honor the bit length

`Slice::remaining_bits` is `cell.bit_len() − bit_offset` (was
`8 × data.len() − bit_offset`). The completion tag is not data: `SBITS` on
a 3-bit cell reports 3, and `LDU 8` on it fails closed instead of reading
the tag as a data bit. Byte-granular cells are unaffected.

## Consequences

- The motivating collision is gone: 3-bit `101` → flagged `b0`,
  `d1 = 0x10`; 8-bit `10100000` → unflagged `a0`, `d1 = 0x00`. Different
  descriptors and different data ⇒ different hashes, pinned in
  `reference/vectors/bitlen.json`.
- Consensus surface: `Cell::from_bytes` validity, `Cell::hash` inputs,
  `ENDC`/`STBYTES` semantics, `Slice::remaining_bits`. All are covered by
  the Python reference (`reference/cell.py`, generator
  `reference/gen_bitlen_vectors.py`, `--check` drift gate) and the Rust
  agreement test (`onx-execution/tests/bitlen_vectors.rs`), plus opcode
  wiring tests in `execution_tests.rs`.
- Live-chain safety (devnet-1, chain `6b0e97…82fb`): no flagged cell can
  exist in live state — no contracts are deployed, and the old code could
  not produce the flag (d1 bit 4 was a reserved-bit rejection). Every
  byte-granular cell hashes exactly as before, `from_bytes` accepts every
  old encoding, and `remaining_bits` is unchanged for them. The new VM
  behavior only affects programs that store partial-bit cells, which
  previously produced ambiguous hashes. No protocol_version bump is taken
  in this step; whether Wave 4 as a whole needs one is flagged for the
  Wave 4 audit.
- Spec: `docs/specification/state-model.md` §4.2 carries the six normative
  rules (this ADR is the decision record); `tvm-instruction-set.md` §4.4
  documents the `STBYTES`/`ENDC`/slice semantics.

## Non-goals

- TON cell compatibility (explicitly deferred; the flag is the hook, not
  the bridge).
- `is_special` modeling in the Python reference (pre-existing gap,
  untouched).
- Changing the 128-byte / 4-ref cell limits.
