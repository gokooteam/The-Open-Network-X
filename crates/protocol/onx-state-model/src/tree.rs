use crate::account::AccountState;
use crate::boc::BagOfCells;
use crate::cell::{Cell, MAX_CELL_REFS};
use crate::contract_cells::ContractCellDags;
use crate::error::StateModelError;
use onx_data_structures::AccountId;
use onx_primitives::{DomainTag, Uint32};
use std::collections::BTreeMap;
use std::rc::Rc;

pub const ONX_TRIE_NODE_TAG: DomainTag = DomainTag::from_ascii("ONX_TRIE_NODE_V1");
pub const MERKLE_PROOF_MAGIC: u32 = 0x4D505246; // "MPRF"

/// Canonical data bytes of the empty-subtree cell: every empty trie branch
/// collapses to this single cell (spec §4.5).
const EMPTY_SUBTREE_DATA: &[u8] = b"EMPTY_SUBTREE";

/// Maximum serialized account bytes committable to the state trie: values
/// are chunked into 128-byte cells and a leaf holds at most
/// `MAX_CELL_REFS` chunk references (spec §4.5 documents the 512-byte
/// limit). Accounts exceeding this cannot be trie-committed; genesis
/// construction and [`ShardStateTree::insert`] reject them fail-closed.
pub const MAX_TRIE_VALUE_BYTES: usize = 128 * MAX_CELL_REFS;

/// Bit `depth` of a 256-bit trie key, matching `build_trie_cells`' layout:
/// byte `depth/8`, bit `7 - (depth%8)` (big-endian within the byte).
fn key_bit(key: &[u8; 32], depth: usize) -> usize {
    debug_assert!(depth < 256, "trie depth exceeds 256-bit key");
    // `depth % 8 <= 7`, so this never saturates; explicit per the arithmetic lint.
    ((key[depth / 8] >> 7usize.saturating_sub(depth % 8)) & 1) as usize
}

/// One node of the incremental Merkle trie.
///
/// The trie is *persistent*: nodes are reference-counted (`Rc`) and never
/// mutated in place. `insert` rebuilds only the O(log n) nodes on the key's
/// path and shares every other subtree with the previous version.
/// Consequences:
/// - `ShardStateTree::clone()` is O(1) (a single `Rc` bump for the trie;
///   see the note on [`ShardStateTree`] for the contract-cell map). Leaves
///   hold the account records themselves, so there is no second,
///   separately copied accounts map.
/// - Replaced nodes are freed automatically when the old root is dropped:
///   no refcounts, no orphan sweeps, no leaks.
/// - The node set is exactly what `build_trie_cells` produces for the same
///   account set (proven by the differential unit test below), so consensus
///   roots are byte-identical to the batch construction.
#[derive(Debug, Clone)]
struct TrieNode {
    /// The canonical cell at this node. Its hash IS the node's Merkle hash.
    cell: Cell,
    /// Children for branch nodes; `None` for leaves and the empty node.
    left: Option<Rc<TrieNode>>,
    right: Option<Rc<TrieNode>>,
    /// Value-chunk cells for leaf nodes, so the full cell set is
    /// recoverable by traversal. Empty for branches and the empty node.
    /// Chunks are stored per-leaf (not refcounted): each account owns its
    /// chunks, so replacing a leaf drops the old chunks automatically.
    chunks: Vec<Cell>,
    /// For leaves: the account this leaf commits to. `cell` and `chunks`
    /// are a pure function of it (`id.to_bytes()`, `state.to_bytes()`).
    /// `None` for branches and the empty node.
    entry: Option<(AccountId, AccountState)>,
}

impl TrieNode {
    /// The shared empty-subtree node.
    fn empty() -> Rc<Self> {
        let cell =
            Cell::new(EMPTY_SUBTREE_DATA.to_vec(), vec![]).expect("empty subtree cell is valid");
        Rc::new(Self {
            cell,
            left: None,
            right: None,
            chunks: vec![],
            entry: None,
        })
    }

    /// Build a leaf node for `(key, value)`, mirroring `build_trie_cells`'
    /// leaf case exactly: the value is chunked into 128-byte cells and the
    /// leaf commits to the key plus the chunk hashes. Pure: constructs
    /// cells without touching any cache. `value` must be
    /// `state.to_bytes()` and `key` must be `id.to_bytes()`; the caller
    /// has both already (it checks the value size first).
    fn build_leaf(
        id: AccountId,
        state: &AccountState,
        key: &[u8; 32],
        value: &[u8],
    ) -> Result<Self, StateModelError> {
        debug_assert_eq!(&id.to_bytes(), key);
        let mut chunks = Vec::new();
        for chunk in value.chunks(128) {
            chunks.push(Cell::new(chunk.to_vec(), vec![])?);
        }
        let chunk_hashes: Vec<[u8; 32]> = chunks.iter().map(|c| c.hash()).collect();
        let cell = Cell::new(key.to_vec(), chunk_hashes)?;
        Ok(Self {
            cell,
            left: None,
            right: None,
            chunks,
            entry: Some((id, state.clone())),
        })
    }

    /// Is this the empty-subtree node?
    fn is_empty_node(&self) -> bool {
        self.cell.data_bytes() == EMPTY_SUBTREE_DATA
    }

    /// If this is a leaf, its 32-byte key.
    fn leaf_key(&self) -> Option<[u8; 32]> {
        if self.cell.data_bytes().len() == 32 && !self.is_empty_node() {
            Some(
                self.cell
                    .data_bytes()
                    .try_into()
                    .expect("length checked above"),
            )
        } else {
            None
        }
    }
}

/// The account an `insert` is writing, with its canonical encodings
/// computed once up front (the size check needs `value` before any trie
/// work starts).
struct Leaf<'a> {
    id: AccountId,
    state: &'a AccountState,
    key: &'a [u8; 32],
    value: &'a [u8],
}

impl Leaf<'_> {
    fn build(&self) -> Result<TrieNode, StateModelError> {
        TrieNode::build_leaf(self.id, self.state, self.key, self.value)
    }
}

/// Shard State Tree mapping account IDs to account states using a binary trie.
///
/// The Merkle trie over `(account_id, state.to_bytes())` is maintained
/// *incrementally*: every `insert` updates only the O(log n) nodes on the
/// key's path, so `state_root_hash()` is an O(1) cache read instead of an
/// O(n) full recomputation. Block validation and replay therefore scale
/// with the accounts a block touches, not with total state.
///
/// The trie is also the account map: each leaf holds its account record,
/// so lookups walk the key's path and `clone()` shares the whole trie.
/// The STF clones the tree once per `propose_block` and once per
/// `apply_block` (so a failed or panicking block can never touch the
/// caller's state); before this, that clone deep-copied a separate
/// `BTreeMap` of every account, O(n) per block (MILESTONES.md M4,
/// `docs/specification/state-model.md` §7.1).
///
/// `contract_cells` is still a plain map and is still copied by `clone()`.
/// That cost scales with the number of *contract* accounts and their DAG
/// sizes, not with all accounts; see the same spec section.
#[derive(Clone)]
pub struct ShardStateTree {
    /// Number of accounts (leaves) in the trie.
    len: usize,
    /// Root of the persistent Merkle trie. Leaves hold the account records.
    trie_root: Rc<TrieNode>,
    /// Trie cells constructed by the incremental path. Instrumentation for
    /// regression tests — proves updates cost O(log n) cells, not O(n).
    /// Not consensus state (hence excluded from `PartialEq`).
    trie_cells_built: u64,
    /// Persisted contract cell DAGs, by account: the full code and data
    /// DAGs (not just root hashes) the TVM needs for `LDREF` across
    /// invocations. Auxiliary state — it does NOT enter the state root
    /// (the root commits to the account record, which commits to the cell
    /// hashes). The STF seeds the interpreter's cell store from these on
    /// contract load and rebuilds them from the drained store after
    /// execution; the storage layer persists them to the `contract_cells`
    /// table. Excluded from `PartialEq` (like the trie cache, it is a
    /// deterministic function of execution history, not consensus state).
    contract_cells: BTreeMap<AccountId, ContractCellDags>,
}

// Consensus equality is over the account set alone: the same
// `(AccountId, AccountState)` pairs, compared value by value in key order.
// The trie cells are a deterministic function of that set; the build
// counter and the contract cell DAGs are auxiliary execution state, not
// consensus state. Two trees that share a root node trivially hold the same
// accounts, so that case skips the walk (equality is still exact: it never
// compares hashes in place of values).
impl PartialEq for ShardStateTree {
    fn eq(&self, other: &Self) -> bool {
        if Rc::ptr_eq(&self.trie_root, &other.trie_root) {
            return true;
        }
        self.len == other.len && self.accounts().eq(other.accounts())
    }
}
impl Eq for ShardStateTree {}

// Hand-written so the output stays what it was with the old `BTreeMap`
// field (the accounts, in key order) instead of dumping trie internals.
impl std::fmt::Debug for ShardStateTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShardStateTree")
            .field("accounts", &DebugAccounts(self))
            .field("trie_cells_built", &self.trie_cells_built)
            .field("contract_cells", &self.contract_cells)
            .finish()
    }
}

struct DebugAccounts<'a>(&'a ShardStateTree);

impl std::fmt::Debug for DebugAccounts<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.0.accounts()).finish()
    }
}

/// In-order (ascending `AccountId`) iterator over a trie's leaves.
///
/// Ascending key order falls out of the layout: the trie branches on key
/// bits most-significant first, left = 0, and `AccountId` orders as its
/// big-endian 32 bytes, so a left-first walk visits keys in exactly the
/// order the old `BTreeMap<AccountId, _>` iterated them.
pub struct Accounts<'a> {
    stack: Vec<&'a TrieNode>,
    remaining: usize,
}

impl<'a> Iterator for Accounts<'a> {
    type Item = (&'a AccountId, &'a AccountState);

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(node) = self.stack.pop() {
            if let Some((id, state)) = &node.entry {
                self.remaining = self.remaining.saturating_sub(1);
                return Some((id, state));
            }
            // Right first so left is popped (visited) first.
            if let Some(right) = &node.right {
                self.stack.push(right);
            }
            if let Some(left) = &node.left {
                self.stack.push(left);
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for Accounts<'_> {}

impl ShardStateTree {
    pub fn new() -> Self {
        Self {
            len: 0,
            trie_root: TrieNode::empty(),
            // The empty-subtree cell itself.
            trie_cells_built: 1,
            contract_cells: BTreeMap::new(),
        }
    }

    /// Insert or replace an account, updating the Merkle trie incrementally.
    ///
    /// Fail-closed (Phase 0 bug 4): a value that cannot be trie-committed
    /// (over 512 bytes → more than 4 value-chunk references, exactly the
    /// case `build_trie_cells` rejects) is an error here, *before* the
    /// accounts map is mutated — the tree is left untouched. The old batch
    /// path surfaced this at `state_root_hash()` time; surfacing it at the
    /// write is strictly earlier and equally fail-closed.
    pub fn insert(
        &mut self,
        account_id: AccountId,
        state: AccountState,
    ) -> Result<(), StateModelError> {
        let key = account_id.to_bytes();
        let value = state.to_bytes();
        if value.len() > MAX_TRIE_VALUE_BYTES {
            return Err(StateModelError::TrieConstruction(format!(
                "account value too large for trie: {} bytes (max {MAX_TRIE_VALUE_BYTES})",
                value.len(),
            )));
        }
        let is_new = self.get(&account_id).is_none();
        let mut built = 0u64;
        let leaf = Leaf {
            id: account_id,
            state: &state,
            key: &key,
            value: &value,
        };
        let new_root = Self::insert_at(&self.trie_root, &leaf, 0, &mut built)?;
        // Diagnostic counter: saturating is behavior-identical in practice
        // (u64 counts cells; overflow would need exabytes of trie).
        self.trie_cells_built = self.trie_cells_built.saturating_add(built);
        self.trie_root = new_root;
        if is_new {
            // At most 2^256 distinct keys exist, but `usize` counts leaves
            // that each occupy memory, so this cannot overflow in practice.
            self.len = self.len.saturating_add(1);
        }
        Ok(())
    }

    /// Recursive incremental insert. Returns the new subtree root, sharing
    /// every unchanged child via `Rc`. `built` counts constructed cells.
    fn insert_at(
        node: &Rc<TrieNode>,
        leaf: &Leaf<'_>,
        depth: usize,
        built: &mut u64,
    ) -> Result<Rc<TrieNode>, StateModelError> {
        let key = leaf.key;
        if node.is_empty_node() {
            let leaf = leaf.build()?;
            *built = (*built)
                .saturating_add(1)
                .saturating_add(leaf.chunks.len() as u64);
            return Ok(Rc::new(leaf));
        }
        if let Some(node_key) = node.leaf_key() {
            if node_key == *key {
                // Same key: replace the value. If the record is identical
                // the leaf (and its hash) are identical — share the node.
                // Compare the records, not just the hashes, so a lookup
                // always returns exactly the value last inserted.
                let candidate = leaf.build()?;
                let same_record = node
                    .entry
                    .as_ref()
                    .is_some_and(|(_, old)| old == leaf.state);
                if same_record && candidate.cell.hash() == node.cell.hash() {
                    return Ok(Rc::clone(node));
                }
                *built = (*built)
                    .saturating_add(1)
                    .saturating_add(candidate.chunks.len() as u64);
                return Ok(Rc::new(candidate));
            }
            // Different key: expand the leaf into a branch chain down to
            // the first differing bit — exactly what `build_trie_cells`
            // produces for the two-item set at this depth.
            return Self::expand_leaf(node, leaf, depth, built);
        }
        // Branch node: recurse into the child selected by the key bit,
        // rebuild the path. Unchanged subtrees are shared.
        let (left, right) = match (node.left.as_ref(), node.right.as_ref()) {
            (Some(l), Some(r)) => (l, r),
            _ => {
                return Err(StateModelError::TrieConstruction(
                    "branch node in trie cache is missing a child".to_string(),
                ))
            }
        };
        let bit = key_bit(key, depth);
        let old_child = if bit == 0 { left } else { right };
        let new_child = Self::insert_at(old_child, leaf, depth.saturating_add(1), built)?;
        if Rc::ptr_eq(&new_child, old_child) {
            return Ok(Rc::clone(node));
        }
        let (new_left, new_right) = if bit == 0 {
            (new_child, Rc::clone(right))
        } else {
            (Rc::clone(left), new_child)
        };
        let cell = Cell::new(vec![], vec![new_left.cell.hash(), new_right.cell.hash()])
            .expect("branch cell: empty data with 2 refs is valid");
        *built = (*built).saturating_add(1);
        Ok(Rc::new(TrieNode {
            cell,
            left: Some(new_left),
            right: Some(new_right),
            chunks: vec![],
            entry: None,
        }))
    }

    /// Expand a leaf holding `old_key` into a branch chain that also holds
    /// `key`, splitting at the first differing bit at or after `depth`.
    fn expand_leaf(
        old_node: &Rc<TrieNode>,
        leaf: &Leaf<'_>,
        depth: usize,
        built: &mut u64,
    ) -> Result<Rc<TrieNode>, StateModelError> {
        let key = leaf.key;
        let old_key: [u8; 32] = old_node
            .leaf_key()
            .expect("expand_leaf called on a leaf node");
        let mut split = depth;
        while split < 256 && key_bit(&old_key, split) == key_bit(key, split) {
            split = split.saturating_add(1);
        }
        if split >= 256 {
            // Unreachable: the keys are distinct (the caller checked), so
            // they differ in some bit below 256.
            return Err(StateModelError::TrieConstruction(
                "duplicate key in trie expansion".to_string(),
            ));
        }
        let new_leaf = leaf.build()?;
        *built = (*built)
            .saturating_add(1)
            .saturating_add(new_leaf.chunks.len() as u64);
        let new_leaf = Rc::new(new_leaf);

        // Branch at the split bit, leaves on their bit-sides.
        let old_bit = key_bit(&old_key, split);
        let (left, right) = if old_bit == 0 {
            (Rc::clone(old_node), new_leaf)
        } else {
            (new_leaf, Rc::clone(old_node))
        };
        let mut child = Self::make_branch(left, right, built);
        // Chain back up to `depth`; the shared empty subtree hangs on the
        // far side at each level (mirrors the batch builder's empty
        // partitions).
        for d in (depth..split).rev() {
            let bit = key_bit(&old_key, d);
            let (l, r) = if bit == 0 {
                (child, TrieNode::empty())
            } else {
                (TrieNode::empty(), child)
            };
            child = Self::make_branch(l, r, built);
        }
        Ok(child)
    }

    /// Build a branch node over two children, counting the cell.
    fn make_branch(left: Rc<TrieNode>, right: Rc<TrieNode>, built: &mut u64) -> Rc<TrieNode> {
        let cell = Cell::new(vec![], vec![left.cell.hash(), right.cell.hash()])
            .expect("branch cell: empty data with 2 refs is valid");
        *built = (*built).saturating_add(1);
        Rc::new(TrieNode {
            cell,
            left: Some(left),
            right: Some(right),
            chunks: vec![],
            entry: None,
        })
    }

    /// Look up an account by walking its key's path: one step per branch
    /// level, O(log n) for uniformly distributed (hash-derived) IDs and at
    /// most 256 steps for any key set.
    pub fn get(&self, account_id: &AccountId) -> Option<&AccountState> {
        let key = account_id.to_bytes();
        let mut node: &TrieNode = &self.trie_root;
        let mut depth = 0usize;
        loop {
            if let Some((id, state)) = &node.entry {
                return (id == account_id).then_some(state);
            }
            let next = if depth < 256 && key_bit(&key, depth) == 0 {
                node.left.as_deref()
            } else if depth < 256 {
                node.right.as_deref()
            } else {
                None
            };
            // `None` here is the empty node (or a malformed branch, which
            // `insert_at` would already have refused): the key is absent.
            node = next?;
            depth = depth.saturating_add(1);
        }
    }

    /// Every account, in ascending `AccountId` order (the same order the
    /// previous `BTreeMap` field iterated in). O(n) to exhaust; not used on
    /// the block path.
    pub fn accounts(&self) -> Accounts<'_> {
        Accounts {
            stack: vec![&self.trie_root],
            remaining: self.len,
        }
    }

    /// Number of accounts in the tree.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True if the tree holds no accounts.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Trie cells constructed by the incremental path since this tree was
    /// created (including clones, which share the count). Instrumentation
    /// for regression tests: proves block application costs O(k log n)
    /// trie cells for k touched accounts, not O(n). Not consensus state.
    pub fn trie_cells_built(&self) -> u64 {
        self.trie_cells_built
    }

    /// Persisted contract cell DAGs for an account, if any. The STF
    /// maintains this: seeded from the storage layer on load, rebuilt from
    /// the interpreter's drained cell store after each contract execution.
    pub fn contract_cells(&self, account_id: &AccountId) -> Option<&ContractCellDags> {
        self.contract_cells.get(account_id)
    }

    /// All persisted contract cell DAGs. The storage layer writes these to
    /// the `contract_cells` table on commit and loads them on startup.
    pub fn all_contract_cells(&self) -> &BTreeMap<AccountId, ContractCellDags> {
        &self.contract_cells
    }

    /// Store (or replace) an account's contract cell DAGs. Called by the
    /// STF after a contract execution rebuilds them from the drained
    /// interpreter cell store, and by the storage layer when loading.
    pub fn set_contract_cells(&mut self, account_id: AccountId, dags: ContractCellDags) {
        self.contract_cells.insert(account_id, dags);
    }

    /// Mutable access to the contract cell DAG map, for the STF's
    /// contract-execution path (seed the interpreter, persist the drained
    /// store).
    pub fn contract_cells_mut(&mut self) -> &mut BTreeMap<AccountId, ContractCellDags> {
        &mut self.contract_cells
    }

    /// The 32-byte Merkle root hash of the shard account state tree.
    ///
    /// O(1): the root is maintained incrementally by [`Self::insert`].
    /// Fail-closed construction errors surface at `insert` time (Phase 0
    /// bug 4); they are never replaced with a constant here.
    pub fn state_root_hash(&self) -> Result<[u8; 32], StateModelError> {
        Ok(self.trie_root.cell.hash())
    }

    /// Root hash over pre-serialized `(key, value)` items. Split out so the
    /// fail-closed error path is directly testable (public account encodings
    /// are too small to trigger trie-construction failure today).
    /// Test-only: the production path is the incremental trie.
    #[cfg(test)]
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
        if self.get(&target_account_id).is_none() {
            return Err(StateModelError::InvalidMerkleProof(format!(
                "cannot generate proof: target key {:x?} not present in state tree",
                &target_key[..8],
            )));
        }

        let items: Vec<([u8; 32], Vec<u8>)> = self
            .accounts()
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
    ///
    /// Collected by traversing the incremental trie (O(n) traversal, but
    /// not in the hot path — `state_root_hash()` never calls this).
    pub fn trie_cells(&self) -> Result<BTreeMap<[u8; 32], Cell>, StateModelError> {
        let mut out = BTreeMap::new();
        let mut stack = vec![Rc::clone(&self.trie_root)];
        while let Some(node) = stack.pop() {
            let hash = node.cell.hash();
            if out.contains_key(&hash) {
                continue;
            }
            out.insert(hash, node.cell.clone());
            for chunk in &node.chunks {
                out.insert(chunk.hash(), chunk.clone());
            }
            if let Some(left) = &node.left {
                stack.push(Rc::clone(left));
            }
            if let Some(right) = &node.right {
                stack.push(Rc::clone(right));
            }
        }
        Ok(out)
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
    let bit_idx = 7usize.saturating_sub(bit_depth % 8); // `% 8 <= 7`: never saturates

    let (left, right): (Vec<_>, Vec<_>) = items
        .iter()
        .cloned()
        .partition(|(key, _)| (key[byte_idx] & (1 << bit_idx)) == 0);

    let left_cell = build_trie_cells(&left, bit_depth.saturating_add(1), cell_map)?;
    let right_cell = build_trie_cells(&right, bit_depth.saturating_add(1), cell_map)?;

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
        let bit_idx = 7usize.saturating_sub(bit_depth % 8); // `% 8 <= 7`: never saturates
        let bit = (target_key[byte_idx] >> bit_idx) & 1;
        let child_hash = cell.cell_refs()[bit as usize];
        return collect_proof_path(
            cell_map,
            &child_hash,
            target_key,
            bit_depth.saturating_add(1),
            out,
        );
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
                let bit_idx = 7usize.saturating_sub(bit_depth % 8); // `% 8 <= 7`: never saturates
                let bit = (self.target_key[byte_idx] >> bit_idx) & 1;
                current = cell.cell_refs()[bit as usize];
                bit_depth = bit_depth.saturating_add(1);
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
        offset = offset.saturating_add(32);

        let mut root_hash = [0u8; 32];
        root_hash.copy_from_slice(&cursor[..32]);
        cursor = &cursor[32..];
        offset = offset.saturating_add(32);

        let (proof_boc, consumed) = BagOfCells::from_bytes(cursor)?;
        offset = offset.saturating_add(consumed); // `consumed <= cursor.len()`: never saturates

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

    /// The incremental `insert` rejects an uncommittable value fail-closed,
    /// *before* mutating the accounts map: the tree is left exactly as it
    /// was (same accounts, same root).
    #[test]
    fn insert_rejects_oversized_value_fail_closed() {
        use crate::account::StorageStat;

        let mut tree = ShardStateTree::new();
        let id = AccountId::from_bytes([0x01; 32]);
        tree.insert(
            id,
            AccountState::Active {
                balance_nanos: 1,
                last_trans_lt: 0,
                code: None,
                data: None,
                storage_stat: StorageStat {
                    cell_count: 0,
                    byte_count: 0,
                    bit_count: 0,
                },
                pubkey: [0; 32],
                nonce: 0,
            },
        )
        .unwrap();
        let root_before = tree.state_root_hash().unwrap();

        // A max-size cell is 2 + 128 + 4*32 = 258 bytes on the wire; code +
        // data + the 141-byte header exceeds the 512-byte trie limit.
        let big_cell = Cell::new(vec![0xAA; 128], vec![[0x11; 32]; 4]).unwrap();
        let oversized = AccountState::Active {
            balance_nanos: 1,
            last_trans_lt: 0,
            code: Some(big_cell.clone()),
            data: Some(big_cell),
            storage_stat: StorageStat {
                cell_count: 0,
                byte_count: 0,
                bit_count: 0,
            },
            pubkey: [0; 32],
            nonce: 0,
        };
        assert!(oversized.to_bytes().len() > 512);

        let err = tree.insert(id, oversized).unwrap_err();
        assert!(
            matches!(err, StateModelError::TrieConstruction(_)),
            "expected TrieConstruction, got: {err}"
        );
        // Untouched: same account, same root.
        assert_eq!(tree.accounts().len(), 1);
        assert_eq!(tree.state_root_hash().unwrap(), root_before);
    }

    /// Consensus-critical differential test: the incremental trie must
    /// produce byte-identical roots to the batch `build_trie_cells`
    /// construction for the same account set, at many sizes (including
    /// sizes that force deep splits and single-item subtrees).
    #[test]
    fn incremental_matches_batch_construction() {
        use crate::account::StorageStat;

        fn active(balance: u128, nonce: u64) -> AccountState {
            AccountState::Active {
                balance_nanos: balance,
                last_trans_lt: 0,
                code: None,
                data: None,
                storage_stat: StorageStat {
                    cell_count: 0,
                    byte_count: 0,
                    bit_count: 0,
                },
                pubkey: [0x22; 32],
                nonce,
            }
        }

        for n in [1usize, 2, 3, 7, 8, 9, 15, 16, 17, 64, 100, 257] {
            let mut tree = ShardStateTree::new();
            let mut items = Vec::new();
            for i in 0..n {
                // Unique keys spread across the key space (not just low
                // bytes) to exercise splits at many depths.
                let mut kb = [0u8; 32];
                kb[..8].copy_from_slice(&(i as u64).to_be_bytes());
                kb[8] = (i as u64).wrapping_mul(37) as u8;
                kb[16] = (i as u64).wrapping_mul(101) as u8;
                kb[24] = (i as u64).wrapping_mul(7) as u8;
                let id = AccountId::from_bytes(kb);
                let st = active(i as u128 * 1000 + 7, i as u64);
                tree.insert(id, st.clone()).unwrap();
                items.push((kb, st.to_bytes()));
            }
            let batch_root = {
                let mut m = BTreeMap::new();
                build_trie_cells(&items, 0, &mut m).unwrap().hash()
            };
            assert_eq!(
                tree.state_root_hash().unwrap(),
                batch_root,
                "incremental root diverged from batch at n={n}"
            );
        }
    }

    /// Insertion order must not affect the root: the trie is a pure
    /// function of the account set.
    #[test]
    fn root_is_insertion_order_independent() {
        use crate::account::StorageStat;

        fn active(balance: u128) -> AccountState {
            AccountState::Active {
                balance_nanos: balance,
                last_trans_lt: 0,
                code: None,
                data: None,
                storage_stat: StorageStat {
                    cell_count: 0,
                    byte_count: 0,
                    bit_count: 0,
                },
                pubkey: [0x33; 32],
                nonce: 0,
            }
        }

        let ids: Vec<AccountId> = (0..50)
            .map(|i| {
                let mut kb = [0u8; 32];
                kb[0] = i as u8;
                kb[16] = (i as u8).wrapping_mul(13);
                AccountId::from_bytes(kb)
            })
            .collect();

        let mut a = ShardStateTree::new();
        let mut b = ShardStateTree::new();
        for id in &ids {
            a.insert(*id, active(999)).unwrap();
        }
        for id in ids.iter().rev() {
            b.insert(*id, active(999)).unwrap();
        }
        assert_eq!(
            a.state_root_hash().unwrap(),
            b.state_root_hash().unwrap(),
            "root must not depend on insertion order"
        );
        // And the tries themselves compare equal (same accounts).
        assert_eq!(a, b);
    }

    // --- The trie is the account map (MILESTONES.md M4) -------------------

    fn model_state(balance: u128, nonce: u64) -> AccountState {
        AccountState::Active {
            balance_nanos: balance,
            last_trans_lt: 0,
            code: None,
            data: None,
            storage_stat: crate::account::StorageStat {
                cell_count: 0,
                byte_count: 0,
                bit_count: 0,
            },
            pubkey: [0x33; 32],
            nonce,
        }
    }

    /// Check every observable view of `tree` against a `BTreeMap` model:
    /// lookups (present and absent), length, iteration order, and the root
    /// (against the independent batch builder).
    fn assert_matches_model(tree: &ShardStateTree, model: &BTreeMap<AccountId, AccountState>) {
        assert_eq!(tree.len(), model.len());
        assert_eq!(tree.is_empty(), model.is_empty());
        assert_eq!(tree.accounts().len(), model.len());
        let walked: Vec<(&AccountId, &AccountState)> = tree.accounts().collect();
        let expected: Vec<(&AccountId, &AccountState)> = model.iter().collect();
        assert_eq!(walked, expected, "iteration must be ascending AccountId");
        for (id, st) in model {
            assert_eq!(tree.get(id), Some(st));
        }
        let mut absent = [0xA5u8; 32];
        while model.contains_key(&AccountId::from_bytes(absent)) {
            absent[31] = absent[31].wrapping_add(1);
        }
        assert_eq!(tree.get(&AccountId::from_bytes(absent)), None);
        let items: Vec<([u8; 32], Vec<u8>)> = model
            .iter()
            .map(|(id, st)| (id.to_bytes(), st.to_bytes()))
            .collect();
        let mut cells = BTreeMap::new();
        let batch_root = build_trie_cells(&items, 0, &mut cells).unwrap().hash();
        assert_eq!(tree.state_root_hash().unwrap(), batch_root);
    }

    proptest::proptest! {
        /// Random inserts and overwrites (small key space so overwrites are
        /// common; keys that share long prefixes so splits happen deep)
        /// behave exactly like the `BTreeMap` the tree used to carry.
        #[test]
        fn trie_behaves_like_btreemap_model(
            ops in proptest::collection::vec((0u8..48, 0u8..4, 0u64..5), 0..120)
        ) {
            let mut tree = ShardStateTree::new();
            let mut model = BTreeMap::new();
            for (k, prefix, v) in ops {
                let mut kb = [0u8; 32];
                kb[0] = prefix;
                kb[31] = k;
                let id = AccountId::from_bytes(kb);
                let st = model_state(u128::from(v) * 10, v);
                tree.insert(id, st.clone()).unwrap();
                model.insert(id, st);
            }
            assert_matches_model(&tree, &model);
        }
    }

    #[test]
    fn empty_tree_views() {
        let tree = ShardStateTree::new();
        assert_matches_model(&tree, &BTreeMap::new());
        assert_eq!(tree.accounts().next(), None);
    }

    /// Keys that differ only in the last bit force a 255-level branch chain;
    /// lookups must still terminate and find both.
    #[test]
    fn get_handles_maximally_deep_split() {
        let a = AccountId::from_bytes([0u8; 32]);
        let mut kb = [0u8; 32];
        kb[31] = 1;
        let b = AccountId::from_bytes(kb);
        let mut tree = ShardStateTree::new();
        let mut model = BTreeMap::new();
        for (id, v) in [(a, 1u64), (b, 2)] {
            tree.insert(id, model_state(1, v)).unwrap();
            model.insert(id, model_state(1, v));
        }
        assert_matches_model(&tree, &model);
    }

    /// `clone()` shares the trie instead of copying it, and the clone and
    /// original then evolve independently.
    #[test]
    fn clone_shares_trie_and_copies_on_write() {
        let mut tree = ShardStateTree::new();
        for i in 0..200u64 {
            tree.insert(
                AccountId::from_bytes(domain_key(i)),
                model_state(u128::from(i), 0),
            )
            .unwrap();
        }
        let before = tree.state_root_hash().unwrap();
        let mut copy = tree.clone();
        assert!(Rc::ptr_eq(&tree.trie_root, &copy.trie_root));

        let id = AccountId::from_bytes(domain_key(5));
        copy.insert(id, model_state(999, 1)).unwrap();
        assert!(!Rc::ptr_eq(&tree.trie_root, &copy.trie_root));
        assert_eq!(tree.state_root_hash().unwrap(), before);
        assert_eq!(tree.get(&id).unwrap().balance_nanos(), 5);
        assert_eq!(copy.get(&id).unwrap().balance_nanos(), 999);
        assert_eq!(tree.len(), copy.len());
        assert_ne!(tree, copy);
    }

    /// Re-inserting an identical record shares the leaf (no cells built),
    /// and equality is by value even for trees built in different orders
    /// (different `Rc` allocations, same accounts).
    #[test]
    fn identical_reinsert_is_free_and_equality_is_by_value() {
        let ids: Vec<AccountId> = (0..50u64)
            .map(|i| AccountId::from_bytes(domain_key(i)))
            .collect();
        let mut fwd = ShardStateTree::new();
        let mut rev = ShardStateTree::new();
        for id in &ids {
            fwd.insert(*id, model_state(7, 0)).unwrap();
        }
        for id in ids.iter().rev() {
            rev.insert(*id, model_state(7, 0)).unwrap();
        }
        assert!(!Rc::ptr_eq(&fwd.trie_root, &rev.trie_root));
        assert_eq!(fwd, rev);

        let built = fwd.trie_cells_built();
        fwd.insert(ids[3], model_state(7, 0)).unwrap();
        assert_eq!(fwd.trie_cells_built(), built);
        assert_eq!(fwd.len(), 50);

        rev.insert(ids[3], model_state(8, 0)).unwrap();
        assert_ne!(fwd, rev);
    }

    /// The `Debug` output still lists accounts by ID (what the old derived
    /// impl printed), rather than trie internals.
    #[test]
    fn debug_lists_accounts() {
        let mut tree = ShardStateTree::new();
        tree.insert(AccountId::from_bytes([1u8; 32]), model_state(42, 0))
            .unwrap();
        let s = format!("{tree:?}");
        assert!(s.contains("accounts"));
        assert!(s.contains("balance_nanos: 42"));
        assert!(!s.contains("chunks"));
    }

    fn domain_key(i: u64) -> [u8; 32] {
        onx_primitives::domain_hash(&ONX_TRIE_NODE_TAG, &i.to_be_bytes())
    }
}
