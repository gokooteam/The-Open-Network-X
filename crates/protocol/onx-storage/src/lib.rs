#![deny(clippy::disallowed_types)]
// Phase 1 replay plan: HashMap/HashSet banned in protocol crates (see workspace clippy.toml); use BTreeMap/BTreeSet.
//! ONX atomic chain storage.
//!
//! Phase 4 of the deterministic-replay plan. A redb-backed embedded store
//! replaces the old hand-rolled file-per-key storage (with its torn
//! `.commit-journal` — Phase 0 bug 3). redb's copy-on-write commit protocol
//! subsumes the journal: there is no journal file to tear and no replay
//! function to get wrong.
//!
//! The critical invariant: [`ChainStore::commit_block`] persists the block
//! body, the block header, the new trie cells, the touched accounts, the
//! seqno→hash index entry, the post-state root, and the head pointer in a
//! **single atomic write transaction**. A crash can therefore only ever
//! leave the pre-commit state or the post-commit state — never a mixture.
//!
//! Crash recovery is: reopen the database, read the head pointer, resume.
//! Re-application is idempotent (same seqno + same hash → skip); a
//! conflicting hash at an existing seqno is a fatal fork error.
//!
//! Protocol-crate discipline: BTreeMap/BTreeSet only in consensus code.
//! redb's own types are not `std::collections` hash maps and are unaffected
//! by the clippy ban.

pub mod encoding;
pub mod error;
pub mod store;
pub mod support;

pub use encoding::{decode_body, encode_body, BODY_COUNT_LEN};
pub use error::StorageError;
pub use store::ChainStore;
