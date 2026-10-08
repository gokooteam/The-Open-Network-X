//! Strict canonical BoC decoding tests.
//!
//! Pins the acceptance rules enforced by `BagOfCells::from_bytes`:
//! strictly ascending cell hashes (rejects both duplicates and unsorted
//! entries), no reserved descriptor bits, no cells unreachable from the
//! root — and the iterative, stack-safe cycle check (`verify_dag`).
//!
//! Also pins the one deliberate carve-out: references to hashes absent
//! from the map are accepted by `from_bytes`, because the in-repo Merkle
//! proof format commits sibling subtree hashes without including the
//! sibling cells and shares this decoder. `from_bytes_strict` enforces
//! the complete invariant for callers that need it.

use onx_state_model::{BagOfCells, Cell, StateModelError};
use std::collections::BTreeMap;

/// A raw BoC entry: declared hash plus the cell's encoded bytes.
type RawEntry = ([u8; 32], Vec<u8>);

/// Split canonical BoC bytes into (root_hash, entries in wire order).
fn parse_entries(bytes: &[u8]) -> ([u8; 32], Vec<RawEntry>) {
    let mut root = [0u8; 32];
    root.copy_from_slice(&bytes[0..32]);
    let count = u32::from_be_bytes(bytes[32..36].try_into().unwrap()) as usize;
    let mut offset = 36;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let mut h = [0u8; 32];
        h.copy_from_slice(&bytes[offset..offset + 32]);
        offset += 32;
        let len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        entries.push((h, bytes[offset..offset + len].to_vec()));
        offset += len;
    }
    (root, entries)
}

fn encode_raw(root: &[u8; 32], entries: &[RawEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(root);
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (h, b) in entries {
        out.extend_from_slice(h);
        out.extend_from_slice(&(b.len() as u32).to_be_bytes());
        out.extend_from_slice(b);
    }
    out
}

/// A small canonical DAG: root -> {a, b}, a -> {c}, b -> {c} (diamond, with
/// a shared grandchild so the traversal must handle converging paths).
fn diamond_boc() -> (BagOfCells, Vec<u8>) {
    let c = Cell::new(vec![3], vec![]).unwrap();
    let a = Cell::new(vec![1], vec![c.hash()]).unwrap();
    let b = Cell::new(vec![2], vec![c.hash()]).unwrap();
    let root = Cell::new(vec![0], vec![a.hash(), b.hash()]).unwrap();
    let root_hash = root.hash();
    let mut cells = BTreeMap::new();
    for cell in [&root, &a, &b, &c] {
        cells.insert(cell.hash(), cell.clone());
    }
    let boc = BagOfCells::new(root_hash, cells).unwrap();
    let bytes = boc.to_bytes();
    (boc, bytes)
}

#[test]
fn rejects_duplicate_cells() {
    let (_, bytes) = diamond_boc();
    let (root, mut entries) = parse_entries(&bytes);
    entries.insert(1, entries[0].clone());
    let bad = encode_raw(&root, &entries);
    assert!(
        matches!(
            BagOfCells::from_bytes(&bad),
            Err(StateModelError::DeserializationError(m))
                if m == "BoC cells not in strictly ascending hash order"
        ),
        "duplicate cell entry must be rejected"
    );
}

#[test]
fn rejects_unsorted_cells() {
    let (_, bytes) = diamond_boc();
    let (root, mut entries) = parse_entries(&bytes);
    assert!(entries.len() >= 2);
    entries.swap(0, 1);
    let bad = encode_raw(&root, &entries);
    assert!(
        matches!(
            BagOfCells::from_bytes(&bad),
            Err(StateModelError::DeserializationError(m))
                if m == "BoC cells not in strictly ascending hash order"
        ),
        "unsorted cell entries must be rejected"
    );
}

#[test]
fn rejects_unused_descriptor_bits() {
    // d1 carries ref_count in bits 0-2, the special flag in bit 3, and the
    // bit-granular flag in bit 4 (ADR-0036). Bits 5-7 are reserved; the
    // encoder never sets them.
    for reserved in [0x20u8, 0x40, 0x80] {
        let cell = Cell::new(vec![0xAA], vec![]).unwrap();
        let mut cell_bytes = cell.to_bytes();
        cell_bytes[0] |= reserved; // d1 is the high byte of the descriptor
        let hash = cell.hash();
        let bad = encode_raw(&hash, &[(hash, cell_bytes)]);
        assert!(
            matches!(
                BagOfCells::from_bytes(&bad),
                Err(StateModelError::InvalidDescriptor)
            ),
            "reserved descriptor bit {:#04x} must be rejected",
            reserved
        );
    }
}

#[test]
fn accepts_bit_granular_flag_in_boc() {
    // ADR-0036: d1 bit 4 is the valid bit-granular flag, not a reserved bit.
    // A properly-tagged flagged cell round-trips through the strict BoC
    // decoder with its hash intact.
    let cell = Cell::new_with_bit_len(vec![0xA0], 3, vec![]).unwrap();
    assert!(cell.is_bit_granular());
    let hash = cell.hash();
    let bytes = cell.to_bytes();
    assert_eq!(bytes[0] & 0x10, 0x10, "flag bit set in d1");
    let good = encode_raw(&hash, &[(hash, bytes)]);
    let (boc, _) = BagOfCells::from_bytes(&good).expect("flagged cell is canonical");
    let back = boc.get_cell(&hash).expect("cell present");
    assert_eq!(back, &cell);
    assert!(back.is_bit_granular());
    assert_eq!(back.bit_len(), 3);
}

#[test]
fn rejects_unreachable_cells() {
    let (_, bytes) = diamond_boc();
    let (root, mut entries) = parse_entries(&bytes);
    // Append a valid standalone cell whose hash is greater than every
    // existing entry, so the ascending-order check passes and the test
    // isolates the reachability rule.
    let max_hash = *entries.iter().map(|(h, _)| h).max().unwrap();
    let mut extra = None;
    for i in 0u64..10_000 {
        let c = Cell::new(i.to_be_bytes().to_vec(), vec![]).unwrap();
        if c.hash() > max_hash {
            extra = Some(c);
            break;
        }
    }
    let extra = extra.expect("find a cell hashing above the current maximum");
    entries.push((extra.hash(), extra.to_bytes()));
    let bad = encode_raw(&root, &entries);
    assert!(
        matches!(
            BagOfCells::from_bytes(&bad),
            Err(StateModelError::DeserializationError(m))
                if m == "BoC contains cells unreachable from the root"
        ),
        "unreachable cell must be rejected"
    );
}

#[test]
fn dangling_reference_lenient_by_default_strict_on_demand() {
    // A cell referencing a hash absent from the map.
    let missing = [0xDBu8; 32];
    let cell = Cell::new(vec![0xAA], vec![missing]).unwrap();
    let hash = cell.hash();
    let bytes = encode_raw(&hash, &[(hash, cell.to_bytes())]);

    // Shared decoder accepts: the Merkle proof format commits sibling
    // hashes without sibling cells and shares this decoder.
    let (boc, used) = BagOfCells::from_bytes(&bytes).unwrap();
    assert_eq!(used, bytes.len());
    assert_eq!(boc.cells().len(), 1);

    // Strict decoder rejects: a consensus state BoC with a missing cell
    // cannot be reconstructed.
    assert!(
        matches!(
            BagOfCells::from_bytes_strict(&bytes),
            Err(StateModelError::DeserializationError(m))
                if m == "BoC cell references a missing cell"
        ),
        "strict decode must reject the dangling reference"
    );
}

#[test]
fn encoder_emits_only_reachable_cells() {
    // from_root + add_cell can hold unreachable cells in memory; the
    // encoder must never emit them.
    let (boc, _) = diamond_boc();
    let mut with_extra = boc.clone();
    with_extra
        .add_cell(Cell::new(vec![0xFF], vec![]).unwrap())
        .unwrap();
    assert_eq!(with_extra.cells().len(), 5);
    assert_eq!(
        with_extra.to_bytes(),
        boc.to_bytes(),
        "unreachable cells must not appear on the wire"
    );
    // The filtered encoding decodes cleanly.
    let (decoded, _) = BagOfCells::from_bytes(&with_extra.to_bytes()).unwrap();
    assert_eq!(decoded, boc);
}

#[test]
fn round_trip_canonical_dag_is_stable() {
    let (boc, bytes) = diamond_boc();
    let (decoded, used) = BagOfCells::from_bytes(&bytes).unwrap();
    assert_eq!(used, bytes.len());
    assert_eq!(decoded, boc);
    assert_eq!(decoded.to_bytes(), bytes);
    // The complete DAG also passes the strict decoder.
    let (strict_decoded, strict_used) = BagOfCells::from_bytes_strict(&bytes).unwrap();
    assert_eq!(strict_used, bytes.len());
    assert_eq!(strict_decoded, boc);
}

/// Regression test for the recursive cycle check: a ~5000-cell chain
/// (~0.8 MB on the wire) must decode cleanly on a 2 MiB thread stack
/// (tokio's default). The old recursive DFS aborted the process here.
#[test]
fn deep_chain_decodes_on_small_stack() {
    const DEPTH: usize = 5000;
    let mut cells = BTreeMap::new();
    let mut child_hash: Option<[u8; 32]> = None;
    let mut tip = [0u8; 32];
    for i in 0..DEPTH {
        let refs = child_hash.map(|h| vec![h]).unwrap_or_default();
        let cell = Cell::new(vec![(i & 0xFF) as u8; 96], refs).unwrap();
        let h = cell.hash();
        cells.insert(h, cell);
        child_hash = Some(h);
        tip = h;
    }
    assert_eq!(cells.len(), DEPTH);
    let boc = BagOfCells::new(tip, cells).unwrap();
    let bytes = boc.to_bytes();
    assert!(bytes.len() > 700_000, "chain should be ~0.8 MB");

    let (decoded, used) = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || BagOfCells::from_bytes(&bytes))
        .expect("spawn small-stack thread")
        .join()
        .expect("small-stack thread must survive deep decode")
        .expect("deep chain must decode");
    assert_eq!(used, decoded.to_bytes().len());
    assert_eq!(decoded, boc);
    // Re-encoding is byte-identical.
    assert_eq!(decoded.to_bytes(), boc.to_bytes());
}

/// Deep cycle detection without recursion: `BagOfCells::new` accepts
/// caller-chosen keys, so a deep chain with a back edge is constructible
/// in memory (unlike on the wire, where content hashes make true cycles
/// unconstructible). Must report the cycle on a small stack.
#[test]
fn deep_cycle_detected_on_small_stack() {
    const DEPTH: usize = 5000;
    let key = |i: usize| {
        let mut k = [0u8; 32];
        k[0..8].copy_from_slice(&(i as u64).to_be_bytes());
        k
    };
    let mut cells = BTreeMap::new();
    for i in 0..DEPTH {
        // Cell i references cell i+1; the last cell references cell 0.
        let next = if i + 1 < DEPTH { key(i + 1) } else { key(0) };
        cells.insert(key(i), Cell::new(vec![], vec![next]).unwrap());
    }
    let root = key(0);
    let result = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || BagOfCells::new(root, cells))
        .expect("spawn small-stack thread")
        .join()
        .expect("small-stack thread must survive deep cycle check");
    assert!(
        matches!(result, Err(StateModelError::CyclicCellReference)),
        "deep cycle must be detected iteratively"
    );
}
