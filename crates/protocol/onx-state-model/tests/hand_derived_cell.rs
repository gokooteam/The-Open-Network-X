//! Hand-derived golden vector for cell hashing, straight from the spec.
//!
//! Source: `docs/specification/state-model.md` §4.3:
//!   CellHash = SHA256(pad32("ONX_CELL_HASH_V1") || d1 || d2 || data
//!                     || ref_1 || ... || ref_k)
//!
//! Vector: a cell with 0 refs (d1 = 0x00), 4 data bytes (d2 = 0x04),
//! data = DE AD BE EF. The general d1 packing of (ref_count | special_flag)
//! is ambiguous in the spec; this vector deliberately uses ref_count = 0
//! so no interpretation is needed.
//!
//! RULE: if this fails, INVESTIGATE — do not adjust the vector.

use onx_state_model::Cell;

fn h32(s: &str) -> [u8; 32] {
    assert!(s.len() == 64, "odd hex length");
    let v: Vec<u8> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("bad hex"))
        .collect();
    v.try_into().expect("expected 32 bytes")
}

const HAND_CELL_HASH_HEX: &str = "0038d5ef2f2b8e83686bf5f69da7eee472f6112676d262c67eee8c860e43196f";

#[test]
fn hand_derived_cell_hash() {
    let cell = Cell::new(vec![0xDE, 0xAD, 0xBE, 0xEF], vec![]).expect("valid cell");
    assert_eq!(cell.hash(), h32(HAND_CELL_HASH_HEX));
}
