use crate::account::AccountState;
use crate::boc::BagOfCells;
use crate::cell::Cell;
use crate::error::StateModelError;
use onx_data_structures::AccountId;
use onx_primitives::{DomainTag, Uint32};
use std::collections::BTreeMap;

pub const ONX_TRIE_NODE_TAG: DomainTag = DomainTag::from_ascii("ONX_TRIE_NODE_V1");
pub const MERKLE_PROOF_MAGIC: u32 = 0x4D505246; // "MPRF"

/// Shard State Tree mapping account IDs to account states using a binary trie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardStateTree {
    accounts: BTreeMap<AccountId, AccountState>,
}

impl ShardStateTree {
    pub fn new() -> Self {
        Self {
            accounts: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, account_id: AccountId, state: AccountState) {
        self.accounts.insert(account_id, state);
    }

    pub fn get(&self, account_id: &AccountId) -> Option<&AccountState> {
        self.accounts.get(account_id)
    }

    pub fn accounts(&self) -> &BTreeMap<AccountId, AccountState> {
        &self.accounts
    }

    /// Computes the 32-byte Merkle root hash of the shard account state tree.
    ///
    /// Fail-closed: trie-construction errors propagate to the caller as
    /// [`StateModelError::TrieConstruction`]. They are never replaced with
    /// a constant (the old `Err(_) => EMPTY_TREE` fallback would have
    /// silently collapsed distinct states onto one root — Phase 0 bug 4).
    pub fn state_root_hash(&self) -> Result<[u8; 32], StateModelError> {
        let items: Vec<([u8; 32], Vec<u8>)> = self
            .accounts
            .iter()
            .map(|(acc_id, state)| (acc_id.to_bytes(), state.to_bytes()))
            .collect();
        Self::root_hash_for_items(&items)
    }

    /// Root hash over pre-serialized `(key, value)` items. Split out so the
    /// fail-closed error path is directly testable (public account encodings
    /// are too small to trigger trie-construction failure today).
    fn root_hash_for_items(items: &[([u8; 32], Vec<u8>)]) -> Result<[u8; 32], StateModelError> {
        let mut dummy_map = BTreeMap::new();
        let root_cell = build_trie_cells(items, 0, &mut dummy_map).map_err(|e| {
            StateModelError::TrieConstruction(format!("state root computation failed: {e}"))
        })?;
        Ok(root_cell.hash())
    }

    /// Generates a Merkle proof for a given account ID.
    ///
    /// The proof contains ONLY the cells on the Merkle path from the root
    /// to the target leaf (branch cells along the path, the leaf cell, and
    /// the leaf's value-chunk cells) — O(log n) cells, not the whole tree.
    /// Returns an error if the target key is not present in the tree: a
    /// proof cannot be generated for an absent key.
    pub fn generate_proof(
        &self,
        target_account_id: AccountId,
    ) -> Result<MerkleProof, StateModelError> {
        let target_key = target_account_id.to_bytes();
        if !self.accounts.contains_key(&target_account_id) {
            return Err(StateModelError::InvalidMerkleProof(format!(
                "cannot generate proof: target key {:x?} not present in state tree",
                &target_key[..8],
            )));
        }

        let items: Vec<([u8; 32], Vec<u8>)> = self
            .accounts
            .iter()
            .map(|(acc_id, state)| (acc_id.to_bytes(), state.to_bytes()))
            .collect();

        let mut full_cells = BTreeMap::new();
        let root_cell = build_trie_cells(&items, 0, &mut full_cells)?;
        let root_hash = root_cell.hash();

        let mut proof_cells = BTreeMap::new();
        collect_proof_path(&full_cells, &root_hash, &target_key, 0, &mut proof_cells)?;
        let proof_boc = BagOfCells::new(root_hash, proof_cells)?;

        Ok(MerkleProof {
            magic_bytes: MERKLE_PROOF_MAGIC,
            target_key,
            root_hash,
            proof_boc,
        })
    }

    /// Returns every cell of the state trie, for persistence layers that
    /// store trie cells alongside account records. This is NOT a proof —
    /// it is the full cell set. (Split out of the old `generate_proof`,
    /// which used to smuggle the whole tree inside a "proof".)
    pub fn trie_cells(&self) -> Result<BTreeMap<[u8; 32], Cell>, StateModelError> {
        let items: Vec<([u8; 32], Vec<u8>)> = self
            .accounts
            .iter()
            .map(|(acc_id, state)| (acc_id.to_bytes(), state.to_bytes()))
            .collect();
        let mut cell_map = BTreeMap::new();
        build_trie_cells(&items, 0, &mut cell_map)?;
        Ok(cell_map)
    }
}

impl Default for ShardStateTree {
    fn default() -> Self {
        Self::new()
    }
}

fn build_trie_cells(
    items: &[([u8; 32], Vec<u8>)],
    bit_depth: usize,
    cell_map: &mut BTreeMap<[u8; 32], Cell>,
) -> Result<Cell, StateModelError> {
    if items.is_empty() {
        let empty_cell = Cell::new(b"EMPTY_SUBTREE".to_vec(), vec![])?;
        let hash = empty_cell.hash();
        cell_map.insert(hash, empty_cell.clone());
        return Ok(empty_cell);
    }

    if items.len() == 1 || bit_depth >= 256 {
        let (key, val) = &items[0];

        // Chunk value if > 128 bytes (for large state records)
        let mut val_chunks = Vec::new();
        for chunk in val.chunks(128) {
            let chunk_cell = Cell::new(chunk.to_vec(), vec![])?;
            let chunk_hash = chunk_cell.hash();
            cell_map.insert(chunk_hash, chunk_cell);
            val_chunks.push(chunk_hash);
        }

        let leaf_cell = Cell::new(key.to_vec(), val_chunks)?;
        let hash = leaf_cell.hash();
        cell_map.insert(hash, leaf_cell.clone());
        return Ok(leaf_cell);
    }

    let byte_idx = bit_depth / 8;
    let bit_idx = 7 - (bit_depth % 8);

    let (left, right): (Vec<_>, Vec<_>) = items
        .iter()
        .cloned()
        .partition(|(key, _)| (key[byte_idx] & (1 << bit_idx)) == 0);

    let left_cell = build_trie_cells(&left, bit_depth + 1, cell_map)?;
    let right_cell = build_trie_cells(&right, bit_depth + 1, cell_map)?;

    let branch_cell = Cell::new(vec![], vec![left_cell.hash(), right_cell.hash()])?;
    let hash = branch_cell.hash();
    cell_map.insert(hash, branch_cell.clone());
    Ok(branch_cell)
}

/// Walks the trie cell map from `node_hash` following `target_key`'s bits,
/// collecting exactly the cells on the Merkle path: branch cells along the
/// way, the leaf cell, and the leaf's value-chunk cells. Used by
/// `generate_proof` so a proof carries O(log n) cells instead of the tree.
fn collect_proof_path(
    cell_map: &BTreeMap<[u8; 32], Cell>,
    node_hash: &[u8; 32],
    target_key: &[u8; 32],
    bit_depth: usize,
    out: &mut BTreeMap<[u8; 32], Cell>,
) -> Result<(), StateModelError> {
    let cell = cell_map.get(node_hash).ok_or_else(|| {
        StateModelError::InvalidMerkleProof("proof path references missing cell".to_string())
    })?;
    out.insert(*node_hash, cell.clone());

    let data = cell.data_bytes();
    if data.len() == 32 {
        // Leaf cell: data is the key, refs are the value-chunk hashes.
        if data != target_key {
            return Err(StateModelError::InvalidMerkleProof(
                "trie walk reached a leaf for a different key".to_string(),
            ));
        }
        for chunk_hash in cell.cell_refs() {
            let chunk = cell_map.get(chunk_hash).ok_or_else(|| {
                StateModelError::InvalidMerkleProof(
                    "proof path references missing value chunk".to_string(),
                )
            })?;
            out.insert(*chunk_hash, chunk.clone());
        }
        return Ok(());
    }

    if data.is_empty() && cell.cell_refs().len() == 2 {
        if bit_depth >= 256 {
            return Err(StateModelError::InvalidMerkleProof(
                "proof path exceeds maximum trie depth".to_string(),
            ));
        }
        // Branch cell: refs are [left_hash, right_hash]; the key bit at
        // this depth selects the path child. Bit layout must match
        // `build_trie_cells`.
        let byte_idx = bit_depth / 8;
        let bit_idx = 7 - (bit_depth % 8);
        let bit = (target_key[byte_idx] >> bit_idx) & 1;
        let child_hash = cell.cell_refs()[bit as usize];
        return collect_proof_path(cell_map, &child_hash, target_key, bit_depth + 1, out);
    }

    Err(StateModelError::InvalidMerkleProof(
        "malformed cell on proof path (neither leaf nor branch)".to_string(),
    ))
}

/// Merkle proof structure per docs/specification/state-model.md §4.4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerkleProof {
    pub magic_bytes: u32,
    pub target_key: [u8; 32],
    pub root_hash: [u8; 32],
    pub proof_boc: BagOfCells,
}

impl MerkleProof {
    /// Structural validation only: magic bytes, DAG integrity, and internal
    /// consistency (the BoC root matches the declared root hash). This does
    /// NOT bind the proof to anything the verifier trusts — use `verify()`
    /// with a trusted root for that. Used at deserialization time.
    fn verify_structure(&self) -> Result<(), StateModelError> {
        if self.magic_bytes != MERKLE_PROOF_MAGIC {
            return Err(StateModelError::InvalidMerkleProof(format!(
                "Invalid magic bytes: expected {:#010x}, got {:#010x}",
                MERKLE_PROOF_MAGIC, self.magic_bytes
            )));
        }

        self.proof_boc.verify_dag()?;

        if self.proof_boc.root_hash() != &self.root_hash {
            return Err(StateModelError::InvalidMerkleProof(format!(
                "Proof root hash {:?} does not match expected root hash {:?}",
                self.proof_boc.root_hash(),
                self.root_hash
            )));
        }

        Ok(())
    }

    /// Verifies the Merkle proof against a trusted root hash.
    ///
    /// Binds the proof to the caller's trust: the declared root must equal
    /// the trusted root, and then the proof is walked down from the trusted
    /// root following the target key's bits. Every step is hash-bound, so a
    /// proof built from a different tree, or for a key not in the tree,
    /// fails. A self-consistent but untrusted proof is NOT enough.
    pub fn verify(&self, trusted_root: &[u8; 32]) -> Result<(), StateModelError> {
        self.verify_structure()?;

        if &self.root_hash != trusted_root {
            return Err(StateModelError::InvalidMerkleProof(
                "proof root does not match trusted root".to_string(),
            ));
        }

        // Walk down from the trusted root following the target key's bits.
        // Each step resolves a cell by its hash from the proof BoC, so every
        // hop is cryptographically bound to the previous one.
        let mut current = *trusted_root;
        let mut bit_depth = 0usize;
        loop {
            if bit_depth > 256 {
                return Err(StateModelError::InvalidMerkleProof(
                    "proof path exceeds maximum trie depth".to_string(),
                ));
            }
            let cell = self.proof_boc.get_cell(&current).ok_or_else(|| {
                StateModelError::InvalidMerkleProof(
                    "proof path references a cell not in the proof".to_string(),
                )
            })?;

            let data = cell.data_bytes();
            if data.len() == 32 {
                // Leaf: the cell's data must BE the target key, and every
                // value-chunk it references must be present in the proof.
                if data != self.target_key {
                    return Err(StateModelError::InvalidMerkleProof(
                        "proof path ends at a leaf for a different key".to_string(),
                    ));
                }
                for chunk_hash in cell.cell_refs() {
                    if self.proof_boc.get_cell(chunk_hash).is_none() {
                        return Err(StateModelError::InvalidMerkleProof(
                            "proof leaf references a missing value chunk".to_string(),
                        ));
                    }
                }
                return Ok(());
            }

            if data.is_empty() && cell.cell_refs().len() == 2 {
                // Branch: the key bit at this depth selects the path child.
                // Bit layout must match `build_trie_cells`.
                let byte_idx = bit_depth / 8;
                let bit_idx = 7 - (bit_depth % 8);
                let bit = (self.target_key[byte_idx] >> bit_idx) & 1;
                current = cell.cell_refs()[bit as usize];
                bit_depth += 1;
                continue;
            }

            return Err(StateModelError::InvalidMerkleProof(
                "malformed cell on proof path (neither leaf nor branch)".to_string(),
            ));
        }
    }

    /// Serializes the MerkleProof object to binary bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&Uint32(self.magic_bytes).encode());
        bytes.extend_from_slice(&self.target_key);
        bytes.extend_from_slice(&self.root_hash);
        let boc_bytes = self.proof_boc.to_bytes();
        bytes.extend_from_slice(&boc_bytes);
        bytes
    }

    /// Deserializes a MerkleProof object from binary bytes.
    pub fn from_bytes(slice: &[u8]) -> Result<(Self, usize), StateModelError> {
        if slice.len() < 4 + 32 + 32 {
            return Err(StateModelError::DeserializationError(
                "Slice too short for MerkleProof header".to_string(),
            ));
        }

        let mut cursor = slice;
        let magic_val = Uint32::read(&mut cursor)
            .map_err(|e| StateModelError::DeserializationError(e.to_string()))?;
        let magic_bytes = magic_val.0;
        let mut offset = Uint32::BYTE_LEN;

        let mut target_key = [0u8; 32];
        target_key.copy_from_slice(&cursor[..32]);
        cursor = &cursor[32..];
        offset += 32;

        let mut root_hash = [0u8; 32];
        root_hash.copy_from_slice(&cursor[..32]);
        cursor = &cursor[32..];
        offset += 32;

        let (proof_boc, consumed) = BagOfCells::from_bytes(cursor)?;
        offset += consumed;

        let proof = Self {
            magic_bytes,
            target_key,
            root_hash,
            proof_boc,
        };
        // Structural validation only: full verification requires a trusted
        // root, which deserialization cannot supply. Call `verify()` with a
        // trusted root after parsing.
        proof.verify_structure()?;

        Ok((proof, offset))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase 0 bug 4 regression: a trie-construction failure must propagate
    /// as [`StateModelError::TrieConstruction`], never collapse into a
    /// plausible-looking constant root.
    ///
    /// A 600-byte value needs 5 leaf chunks (> 4 cell references), which
    /// `build_trie_cells` rejects. Public `AccountState` encodings are far
    /// smaller today, so the failure is driven directly through the same
    /// helper `state_root_hash` uses.
    #[test]
    fn oversized_value_propagates_as_trie_construction_error() {
        let items = vec![([0x11u8; 32], vec![0xABu8; 600])];
        let err = ShardStateTree::root_hash_for_items(&items).unwrap_err();
        assert!(
            matches!(err, StateModelError::TrieConstruction(_)),
            "expected TrieConstruction, got: {err}"
        );
    }

    /// Sanity: normal-sized values still hash fine through the same path.
    #[test]
    fn normal_values_hash_without_error() {
        let items = vec![
            ([0x11u8; 32], vec![0x01u8; 64]),
            ([0x22u8; 32], vec![0x02u8; 200]),
        ];
        assert!(ShardStateTree::root_hash_for_items(&items).is_ok());
    }
}
