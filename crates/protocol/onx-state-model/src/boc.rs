use crate::cell::Cell;
use crate::error::StateModelError;
use onx_primitives::Uint32;
use std::collections::{BTreeMap, BTreeSet};

/// Bag-of-Cells (BoC) structure: A serialized rooted directed acyclic graph (DAG) of cells.
///
/// The cell map is a `BTreeMap` keyed by cell hash, NOT a `HashMap`.
/// `BTreeMap` iterates in ascending key order on every platform and in
/// every process, which makes `to_bytes()` a canonical encoding: the same
/// logical BoC always serializes to the same bytes. A `HashMap` would leak
/// per-process hash-seed iteration order into the encoding and break
/// cross-node / cross-run determinism (see Phase 1 of the replay plan).
/// `HashMap`/`HashSet` are banned in protocol crates via clippy
/// `disallowed-types` (workspace `clippy.toml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BagOfCells {
    root_hash: [u8; 32],
    cells: BTreeMap<[u8; 32], Cell>,
}

impl BagOfCells {
    /// Constructs a BagOfCells given a root hash and a map of all constituent cells.
    /// Rejects if root hash is missing or if the cell graph contains a cycle.
    pub fn new(
        root_hash: [u8; 32],
        cells: BTreeMap<[u8; 32], Cell>,
    ) -> Result<Self, StateModelError> {
        if !cells.contains_key(&root_hash) {
            return Err(StateModelError::DeserializationError(
                "Root cell hash not found in cell map".to_string(),
            ));
        }

        let boc = Self { root_hash, cells };
        boc.verify_dag()?;
        Ok(boc)
    }

    /// Builds a BagOfCells holding exactly the given root cell.
    ///
    /// Note: despite the name, this does NOT walk the root's references;
    /// only the root itself is inserted. Add further cells with `add_cell`.
    /// The wire encoding (`to_bytes`) always emits exactly the cells
    /// reachable from the root, so unreachable cells added this way never
    /// appear on the wire.
    pub fn from_root(root: Cell) -> Result<Self, StateModelError> {
        let mut cells = BTreeMap::new();
        let root_hash = root.hash();
        cells.insert(root_hash, root);
        let boc = Self { root_hash, cells };
        Ok(boc)
    }

    /// Adds a cell to the BoC cell set and verifies DAG consistency.
    pub fn add_cell(&mut self, cell: Cell) -> Result<[u8; 32], StateModelError> {
        let hash = cell.hash();
        self.cells.insert(hash, cell);
        self.verify_dag()?;
        Ok(hash)
    }

    pub fn root_hash(&self) -> &[u8; 32] {
        &self.root_hash
    }

    pub fn cells(&self) -> &BTreeMap<[u8; 32], Cell> {
        &self.cells
    }

    pub fn get_cell(&self, hash: &[u8; 32]) -> Option<&Cell> {
        self.cells.get(hash)
    }

    /// Verifies that the cell graph starting from root forms a valid Directed Acyclic Graph (DAG) with no cycles.
    ///
    /// Iterative depth-first search with an explicit heap-allocated stack:
    /// a hostile input shaped as a deep chain (thousands of cells) must not
    /// overflow the thread's call stack. References to hashes absent from
    /// the map terminate the path being explored; they are NOT an error
    /// here because the in-repo Merkle proof format legitimately commits
    /// sibling subtree hashes without including the sibling cells
    /// (`MerkleProof::from_bytes` shares this decoder). Callers that need
    /// the complete invariant (every reference resolves) should use
    /// `from_bytes_strict`.
    pub fn verify_dag(&self) -> Result<(), StateModelError> {
        if !self.cells.contains_key(&self.root_hash) {
            return Err(StateModelError::DeserializationError(
                "Root cell hash not found in cell map".to_string(),
            ));
        }

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum Mark {
            Visiting,
            Visited,
        }

        let mut marks: BTreeMap<[u8; 32], Mark> = BTreeMap::new();
        // Explicit stack of (node, next child index) frames. Heap-allocated,
        // so depth is bounded by memory, not by the thread stack.
        let mut stack: Vec<([u8; 32], usize)> = Vec::new();
        stack.push((self.root_hash, 0));
        marks.insert(self.root_hash, Mark::Visiting);

        while let Some((node, child_idx)) = stack.pop() {
            let cell = match self.cells.get(&node) {
                Some(cell) => cell,
                None => {
                    // Dangling reference: the path ends here. See docstring.
                    marks.insert(node, Mark::Visited);
                    continue;
                }
            };
            let refs = cell.cell_refs();
            if child_idx < refs.len() {
                // Resume this frame after the child is explored.
                stack.push((node, child_idx + 1));
                let child = refs[child_idx];
                match marks.get(&child) {
                    Some(Mark::Visiting) => {
                        // Back edge to a node on the current path: a cycle.
                        return Err(StateModelError::CyclicCellReference);
                    }
                    Some(Mark::Visited) => {
                        // Already fully explored (shared sub-DAG): skip.
                    }
                    None => {
                        marks.insert(child, Mark::Visiting);
                        stack.push((child, 0));
                    }
                }
            } else {
                marks.insert(node, Mark::Visited);
            }
        }
        Ok(())
    }

    /// The set of cell hashes reachable from the root, computed iteratively.
    ///
    /// References to hashes absent from the map terminate the path; they do
    /// not fail the traversal (see `verify_dag`).
    fn reachable_hashes(&self) -> BTreeSet<[u8; 32]> {
        let mut seen: BTreeSet<[u8; 32]> = BTreeSet::new();
        let mut stack: Vec<[u8; 32]> = vec![self.root_hash];
        while let Some(hash) = stack.pop() {
            let cell = match self.cells.get(&hash) {
                Some(cell) => cell,
                None => continue,
            };
            if !seen.insert(hash) {
                continue;
            }
            stack.extend(cell.cell_refs().iter().copied());
        }
        seen
    }

    /// Serializes the BagOfCells into binary bytes.
    ///
    /// Canonical encoding: exactly the cells reachable from the root, written
    /// in strictly ascending order of cell hash (the `BTreeSet` iteration
    /// order). Cells present in the map but unreachable from the root
    /// (possible via `from_root` + `add_cell`) are never emitted, so the
    /// encoder only ever produces canonical form. The same logical BoC
    /// therefore serializes to identical bytes in every process and on
    /// every node.
    pub fn to_bytes(&self) -> Vec<u8> {
        let reachable = self.reachable_hashes();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&self.root_hash);
        bytes.extend_from_slice(&Uint32(reachable.len() as u32).encode());

        // BTreeSet iteration order == ascending hash order: deterministic.
        for hash in &reachable {
            let cell = self
                .cells
                .get(hash)
                .expect("reachable_hashes only returns hashes present in the cell map");
            bytes.extend_from_slice(hash);
            let cell_bytes = cell.to_bytes();
            bytes.extend_from_slice(&Uint32(cell_bytes.len() as u32).encode());
            bytes.extend_from_slice(&cell_bytes);
        }
        bytes
    }

    /// Deserializes a BagOfCells from binary bytes.
    ///
    /// Strict canonical decoding. Every rejection is a clean `Err`, never a
    /// panic or abort. Rejects:
    /// - truncated input, impossible cell counts (before any allocation
    ///   sized by attacker-controlled lengths), and hash mismatches;
    /// - cells not in strictly ascending hash order — this rejects both
    ///   unsorted entries and duplicate entries;
    /// - cells whose descriptor has reserved bits set (see
    ///   `Cell::from_bytes`);
    /// - trailing bytes inside a cell entry;
    /// - a missing root cell, or a cyclic reference graph;
    /// - cells unreachable from the root: the canonical wire form contains
    ///   exactly the reachable set.
    ///
    /// References to hashes absent from the map are permitted: the in-repo
    /// Merkle proof format commits sibling subtree hashes without including
    /// the sibling cells, and `MerkleProof::from_bytes` shares this decoder.
    /// Callers that need the complete invariant should use
    /// `from_bytes_strict`.
    pub fn from_bytes(slice: &[u8]) -> Result<(Self, usize), StateModelError> {
        if slice.len() < 36 {
            return Err(StateModelError::DeserializationError(
                "Slice too short for BoC root hash and count".to_string(),
            ));
        }

        let mut root_hash = [0u8; 32];
        root_hash.copy_from_slice(&slice[0..32]);
        let mut offset = 32;

        let mut cursor = &slice[offset..];
        let count_val = Uint32::read(&mut cursor)
            .map_err(|e| StateModelError::DeserializationError(e.to_string()))?;
        offset += Uint32::BYTE_LEN;
        let count = count_val.0 as usize;

        // Every encoded cell entry has at least its 32-byte hash and 4-byte
        // length field. Reject an impossible declared count before using it
        // as a collection capacity: otherwise a tiny hostile input can ask
        // the decoder to reserve gigabytes of memory.
        const MIN_CELL_ENTRY_BYTES: usize = 32 + Uint32::BYTE_LEN;
        let remaining = slice.len().saturating_sub(offset);
        if count > remaining / MIN_CELL_ENTRY_BYTES {
            return Err(StateModelError::DeserializationError(
                "BoC cell count exceeds remaining input capacity".to_string(),
            ));
        }

        let mut cells = BTreeMap::new();
        let mut prev_hash: Option<[u8; 32]> = None;
        for _ in 0..count {
            if slice.len() < offset + 36 {
                return Err(StateModelError::DeserializationError(
                    "Truncated BoC cell entry header".to_string(),
                ));
            }
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&slice[offset..offset + 32]);
            offset += 32;

            // Canonical order: strictly ascending hashes. A duplicate entry
            // is not strictly greater than its predecessor, so this one
            // check rejects both unsorted and duplicated cells.
            if let Some(prev) = prev_hash {
                if hash <= prev {
                    return Err(StateModelError::DeserializationError(
                        "BoC cells not in strictly ascending hash order".to_string(),
                    ));
                }
            }
            prev_hash = Some(hash);

            let mut cell_cursor = &slice[offset..];
            let len_val = Uint32::read(&mut cell_cursor)
                .map_err(|e| StateModelError::DeserializationError(e.to_string()))?;
            offset += Uint32::BYTE_LEN;
            let len = len_val.0 as usize;

            if slice.len() < offset + len {
                return Err(StateModelError::DeserializationError(
                    "Truncated BoC cell content".to_string(),
                ));
            }

            let (cell, cell_consumed) = Cell::from_bytes(&slice[offset..offset + len])?;
            if cell_consumed != len {
                return Err(StateModelError::TrailingBytes {
                    remaining: len - cell_consumed,
                });
            }
            offset += len;

            if cell.hash() != hash {
                return Err(StateModelError::DeserializationError(
                    "Cell hash mismatch in BoC entry".to_string(),
                ));
            }
            cells.insert(hash, cell);
        }

        let boc = Self::new(root_hash, cells)?;
        // Canonical wire form: exactly the cells reachable from the root.
        // Anything else is non-canonical and rejected, so two honest
        // implementations never disagree on validity.
        if boc.reachable_hashes().len() != boc.cells().len() {
            return Err(StateModelError::DeserializationError(
                "BoC contains cells unreachable from the root".to_string(),
            ));
        }
        Ok((boc, offset))
    }

    /// Strict canonical decode: `from_bytes` plus the requirement that every
    /// referenced cell hash resolves to a cell in the map (no dangling
    /// references).
    ///
    /// Use for consensus state BoCs, where a missing cell means the state
    /// cannot be reconstructed. Do NOT use for Merkle proofs: the proof
    /// format intentionally commits sibling subtree hashes without including
    /// the sibling cells, so a valid proof would be rejected here.
    pub fn from_bytes_strict(slice: &[u8]) -> Result<(Self, usize), StateModelError> {
        let (boc, used) = Self::from_bytes(slice)?;
        for cell in boc.cells().values() {
            for r in cell.cell_refs() {
                if !boc.cells().contains_key(r) {
                    return Err(StateModelError::DeserializationError(
                        "BoC cell references a missing cell".to_string(),
                    ));
                }
            }
        }
        Ok((boc, used))
    }
}
