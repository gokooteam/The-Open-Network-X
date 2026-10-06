//! Phase-1 regression tests: canonical BoC encodings.
//!
//! Phase 1 of the ONX deterministic-replay plan replaced the `HashMap`
//! cell store in `BagOfCells` with a `BTreeMap`, so `to_bytes()` always
//! writes cells in ascending hash order. These tests pin that invariant:
//! the same logical BoC must serialize to identical bytes regardless of
//! process, insertion order, or build path.
//!
//! (`HashMap`/`HashSet` are additionally banned in all protocol crates via
//! clippy `disallowed-types`; see the workspace `clippy.toml`.)

use onx_state_model::{BagOfCells, Cell};
use std::collections::{BTreeMap, BTreeSet};

fn build_five_cell_boc(seed: u8) -> BagOfCells {
    let root = Cell::new(vec![seed, 0], vec![]).unwrap();
    let mut boc = BagOfCells::from_root(root).unwrap();
    for j in 1..5u8 {
        boc.add_cell(Cell::new(vec![seed, j], vec![]).unwrap())
            .unwrap();
    }
    assert_eq!(boc.cells().len(), 5);
    boc
}

/// Core Phase-1 deliverable: 50 fresh builds of the same logical BoC must
/// produce exactly 1 distinct byte string.
///
/// (Phase 0 measured 50 distinct strings out of 50 here — per-process
/// `HashMap` hash seeds leaking into the encoding.)
#[test]
fn phase1_boc_serialization_is_deterministic_across_fresh_builds() {
    let mut distinct: BTreeSet<Vec<u8>> = BTreeSet::new();
    for _ in 0..50 {
        distinct.insert(build_five_cell_boc(0xAB).to_bytes());
    }
    assert_eq!(
        distinct.len(),
        1,
        "50 fresh builds of the same BoC must serialize to identical bytes"
    );
}

/// Insertion order must not affect the encoding.
#[test]
fn phase1_boc_encoding_independent_of_insertion_order() {
    let fwd = build_five_cell_boc(0xCD);

    let mut cells = BTreeMap::new();
    for j in (0..5u8).rev() {
        let c = Cell::new(vec![0xCD, j], vec![]).unwrap();
        cells.insert(c.hash(), c);
    }
    let root_hash = Cell::new(vec![0xCD, 0], vec![]).unwrap().hash();
    let rev = BagOfCells::new(root_hash, cells).unwrap();

    assert_eq!(
        fwd.to_bytes(),
        rev.to_bytes(),
        "insertion order must not affect the canonical encoding"
    );
}

/// The encoding lays cells out in strictly ascending hash order.
///
/// Note: the wire form contains exactly the cells reachable from the root,
/// so this builds a real 5-cell chain rather than five disconnected cells.
#[test]
fn phase1_boc_cells_written_in_ascending_hash_order() {
    let mut cells = BTreeMap::new();
    let mut child_hash: Option<[u8; 32]> = None;
    let mut tip = [0u8; 32];
    for j in 0..5u8 {
        let refs = child_hash.map(|h| vec![h]).unwrap_or_default();
        let cell = Cell::new(vec![0xEF, j], refs).unwrap();
        let h = cell.hash();
        cells.insert(h, cell);
        child_hash = Some(h);
        tip = h;
    }
    let boc = BagOfCells::new(tip, cells).unwrap();
    let bytes = boc.to_bytes();

    // Layout: 32-byte root hash, big-endian u32 cell count, then per cell:
    // 32-byte hash, big-endian u32 length, cell bytes.
    let count = u32::from_be_bytes(bytes[32..36].try_into().unwrap()) as usize;
    assert_eq!(count, 5);

    let mut offset = 36;
    let mut prev_hash = [0u8; 32];
    for i in 0..count {
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes[offset..offset + 32]);
        offset += 32;
        let len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4 + len;
        if i > 0 {
            assert!(
                hash > prev_hash,
                "cell hashes must appear in strictly ascending order"
            );
        }
        prev_hash = hash;
    }
}

/// Decode(encode(x)) == x, and re-encoding the decoded value is byte-identical.
///
/// The BoC under test must be canonical: every cell reachable from the root.
/// (The wire form contains exactly the reachable set, so a BoC built with
/// unreachable cells via `from_root` + `add_cell` does not round-trip
/// identically — `to_bytes` emits only the reachable cells.)
#[test]
fn phase1_boc_round_trip_is_stable() {
    // Diamond DAG: root references two children that share one grandchild.
    let grandchild = Cell::new(vec![0x12, 3], vec![]).unwrap();
    let child_a = Cell::new(vec![0x12, 1], vec![grandchild.hash()]).unwrap();
    let child_b = Cell::new(vec![0x12, 2], vec![grandchild.hash()]).unwrap();
    let root = Cell::new(vec![0x12, 0], vec![child_a.hash(), child_b.hash()]).unwrap();
    let root_hash = root.hash();

    let mut cells = BTreeMap::new();
    for cell in [&root, &child_a, &child_b, &grandchild] {
        cells.insert(cell.hash(), cell.clone());
    }
    let boc = BagOfCells::new(root_hash, cells).unwrap();

    let bytes = boc.to_bytes();
    let (decoded, used) = BagOfCells::from_bytes(&bytes).unwrap();
    assert_eq!(used, bytes.len());
    assert_eq!(decoded, boc);
    assert_eq!(decoded.to_bytes(), bytes);
}
