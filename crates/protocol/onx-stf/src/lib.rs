#![deny(clippy::disallowed_types)]
// Phase 1 replay plan: HashMap/HashSet banned in protocol crates (see workspace clippy.toml); use BTreeMap/BTreeSet.
//! ONX pure state transition function (STF).
//!
//! Phase 3 of the deterministic-replay plan. This crate defines the block
//! body (a header committing to an ordered transaction set) and the state
//! transition as a **pure function**:
//!
//! ```text
//! apply_block(&State, &Block) -> Result<(State, Receipts), StfError>
//! ```
//!
//! Purity contract (enforced by construction, not just convention):
//! - no I/O of any kind,
//! - no clocks, wall-time, or randomness,
//! - no hash-ordered collections (`HashMap`/`HashSet` are banned workspace-wide
//!   in protocol crates via clippy `disallowed-types`),
//! - only `BTreeMap`/`BTreeSet`/`Vec` (insertion-ordered) and fixed-size arrays.
//!
//! Scope: Onyx transfers and fees only. No VM — the VM is wired in later,
//! after replay passes without it (plan §"Frozen until replay passes").
//!
//! Consensus rules implemented here:
//! - Blocks form a hash chain: `header.seqno == prev.seqno + 1`,
//!   `header.prev_hash == prev.header_hash`, strictly increasing `lt`.
//! - The header commits to its transactions via `txs_root`; the STF
//!   recomputes it and rejects mismatches.
//! - The header carries the *claimed* post-state root; the STF recomputes
//!   the state root from the resulting tree and rejects mismatches. This is
//!   the property the whole replay milestone rests on.
//! - A block containing any invalid transaction is itself invalid
//!   (fail-closed: the first invalid transaction aborts the block).
//!
//! Fee model (documented judgment call): each transaction declares
//! `fee_nanos`; the sender must cover `amount + fee`. Fees are split per
//! `onx_economics::split_transaction_fee` (50% burned, 50% validator
//! reward). The validator half is credited to an explicit `fee_collector`
//! account carried in the block header — no magic accounts, no hidden
//! minting, and no validator-set lookup required, which keeps the STF pure.
//! A future validator-set-based reward distribution can replace the
//! collector without changing transaction semantics.

pub mod block;
pub mod error;
pub mod state;
pub mod stf;

pub use block::{txs_root, Block, BlockBody, BlockHeader, Transaction};
pub use error::StfError;
pub use state::State;
pub use stf::{apply_block, propose_block, AppliedTx, Receipts};
