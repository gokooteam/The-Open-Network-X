use onx_state_model::{GenesisDocument, ShardStateTree};

/// The chain state as seen by the STF: the account tree plus the minimal
/// chain metadata needed to validate the next block.
///
/// This is deliberately *not* the storage layer's state: it is the pure,
/// in-memory view. Persistence (Phase 4) must reproduce exactly this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// The shard account tree. Its `state_root_hash()` is the consensus root.
    pub tree: ShardStateTree,
    /// Workchain this state belongs to. Blocks for another workchain are rejected.
    pub workchain: i32,
    /// Chain identity: the genesis hash, fixed for the chain's lifetime.
    /// External messages bind their signatures to this value — it cannot be
    /// `last_hash`, which stops being the genesis hash after block 1.
    pub chain_id: [u8; 32],
    /// Sequence number of the last applied block (genesis state: 0).
    pub seqno: u32,
    /// Logical time of the last applied block (genesis state: 0).
    pub last_lt: u64,
    /// Hash of the last applied block header. For the genesis state this is
    /// the genesis hash itself — the chain's "previous hash" at birth is its
    /// own identity (the chain ID).
    pub last_hash: [u8; 32],
}

impl State {
    /// Build the genesis state from a canonical genesis document.
    pub fn from_genesis(doc: &GenesisDocument) -> Self {
        Self {
            tree: doc.state_tree(),
            workchain: doc.workchain.0 .0,
            chain_id: doc.genesis_hash(),
            seqno: 0,
            last_lt: 0,
            last_hash: doc.genesis_hash(),
        }
    }

    /// The current consensus state root.
    ///
    /// Returns an error if trie construction fails (fail-closed — Phase 0
    /// bug 4; never a silent constant).
    pub fn state_root(&self) -> Result<[u8; 32], onx_state_model::StateModelError> {
        self.tree.state_root_hash()
    }
}
