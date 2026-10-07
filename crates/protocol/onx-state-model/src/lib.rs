#![deny(clippy::disallowed_types)]
// Phase 1 replay plan: HashMap/HashSet banned in protocol crates (see workspace clippy.toml); use BTreeMap/BTreeSet.
// Wave-3 chain safety (ADR-0028): every integer arithmetic site in this crate
// must be overflow-explicit (`checked_*`/`saturating_*`/`wrapping_*`). A plain
// `+`/`-`/`*`/`/`/`%` on a value reachable from untrusted input (BoC decoder
// offsets and sizes over untrusted bytes, cell bit-length arithmetic) is a
// potential producer panic, i.e. a chain halt. `deny` keeps that property
// load-bearing: new arithmetic must name its overflow behavior.
#![deny(clippy::arithmetic_side_effects)]
//! ONX state model and account lifecycle implementation.
//!
//! Implements `docs/specification/state-model.md`: account states, lifecycle transitions,
//! Cell binary serialization, domain-separated Cell representation hashing,
//! Bag-of-Cells (BoC) graphs, and Merkle proof structures.

pub mod account;
pub mod boc;
pub mod cell;
pub mod contract_cells;
pub mod error;
pub mod genesis;
pub mod tree;

pub use account::{AccountState, AccountType, StorageStat};
pub use boc::BagOfCells;
pub use cell::{Cell, MAX_CELL_DATA_BYTES, MAX_CELL_REFS, ONX_CELL_HASH_V1_TAG};
pub use contract_cells::ContractCellDags;
pub use error::StateModelError;
pub use genesis::{
    derive_account_id, derive_validator_pubkey, is_explicit_hex_key, parse_or_derive_account_id,
    parse_or_derive_pubkey, GenesisDocument, GenesisValidator, GENESIS_MAGIC, GENESIS_VERSION,
    ONX_GENESIS_ADDR_V1, ONX_GENESIS_V1, ONX_GENESIS_VALKEY_V1,
};
pub use tree::{MerkleProof, ShardStateTree, MAX_TRIE_VALUE_BYTES, MERKLE_PROOF_MAGIC};
