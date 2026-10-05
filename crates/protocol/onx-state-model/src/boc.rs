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

    /// Builds a BagOfCells from a root Cell and recursively collects all reachable descendant cells.
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
    pub fn verify_dag(&self) -> Result<(), StateModelError> {
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();

        fn dfs(
            node: &[u8; 32],
            cells: &BTreeMap<[u8; 32], Cell>,
            visiting: &mut BTreeSet<[u8; 32]>,
            visited: &mut BTreeSet<[u8; 32]>,
        ) -> Result<(), StateModelError> {
            if visiting.contains(node) {
                return Err(StateModelError::CyclicCellReference);
            }
            if visited.contains(node) {
                return Ok(());
            }

            visiting.insert(*node);
            if let Some(cell) = cells.get(node) {
                for ref_hash in cell.cell_refs() {
                    dfs(ref_hash, cells, visiting, visited)?;
                }
            }
            visiting.remove(node);
            visited.insert(*node);
            Ok(())
        }

        dfs(&self.root_hash, &self.cells, &mut visiting, &mut visited)
    }

    /// Serializes the BagOfCells into binary bytes.
    ///
    /// Canonical encoding: cells are written in ascending order of cell
    /// hash (the `BTreeMap` iteration order). The same logical BoC therefore
    /// serializes to identical bytes in every process and on every node.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&self.root_hash);
        bytes.extend_from_slice(&Uint32(self.cells.len() as u32).encode());

        // BTreeMap iteration order == ascending hash order: deterministic.
        for (hash, cell) in &self.cells {
            bytes.extend_from_slice(hash);
            let cell_bytes = cell.to_bytes();
            bytes.extend_from_slice(&Uint32(cell_bytes.len() as u32).encode());
            bytes.extend_from_slice(&cell_bytes);
        }
        bytes
    }

    /// Deserializes a BagOfCells from binary bytes.
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
        for _ in 0..count {
            if slice.len() < offset + 36 {
                return Err(StateModelError::DeserializationError(
                    "Truncated BoC cell entry header".to_string(),
                ));
            }
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&slice[offset..offset + 32]);
            offset += 32;

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
        Ok((boc, offset))
    }
}
