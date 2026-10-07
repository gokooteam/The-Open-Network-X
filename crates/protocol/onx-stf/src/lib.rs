#![deny(clippy::disallowed_types)]
// Phase 1 replay plan: HashMap/HashSet banned in protocol crates (see workspace clippy.toml); use BTreeMap/BTreeSet.
// Wave-3 chain safety (ADR-0028): every integer arithmetic site in this crate
// must be overflow-explicit (`checked_*`/`saturating_*`/`wrapping_*`). A plain
// `+`/`-`/`*`/`/`/`%` on a value reachable from untrusted input (fee/balance
// math over declared fees, offsets over untrusted bytes) is a potential
// producer panic, i.e. a chain halt. `deny` keeps that property load-bearing:
// new arithmetic must name its overflow behavior.
#![deny(clippy::arithmetic_side_effects)]
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
//! Scope: Onyx value transfers and TVM contract execution, all via
//! asynchronous messages. External messages authenticate at the built-in
//! wallet handler (STF, not VM); internal messages deliver as the
//! receiver's own transaction, dispatching into `onx-execution` for
//! contract calls. The VM runs pure (no I/O, no clocks) with gas bounded
//! by the declared fee.
//!
//! Consensus rules implemented here:
//! - Blocks form a hash chain: `header.seqno == prev.seqno + 1`,
//!   `header.prev_hash == prev.header_hash`, strictly increasing `lt`.
//! - The header commits to its external messages via `msgs_root`; the STF
//!   recomputes it and rejects mismatches.
//! - The header carries the *claimed* post-state root; the STF recomputes
//!   the state root from the resulting tree and rejects mismatches. This is
//!   the property the whole replay milestone rests on.
//! - A block containing any invalid external message is itself invalid
//!   (fail-closed: the first invalid message aborts the block).
//! - Internal messages deliver at most once per block (`DoubleDelivery`
//!   fails the block); undeliverable messages bounce value to the sender.
//!
//! Fee model (documented judgment call): each external message declares
//! `fee_nanos`; the sender must cover `amount + fee`. Fees are split per
//! `onx_economics::split_transaction_fee` (50% burned, 50% validator
//! reward). The validator half is credited to an explicit `fee_collector`
//! account carried in the block header — no magic accounts, no hidden
//! minting, and no validator-set lookup required, which keeps the STF pure.
//! A future validator-set-based reward distribution can replace the
//! collector without changing transaction semantics.

pub mod block;
pub mod error;
pub mod message;
pub mod state;
pub mod stf;

pub use block::{encode_sig_section, msgs_root, Block, BlockBody, BlockHeader, SigEntry};
pub use error::StfError;
pub use message::{
    derive_address, ExternalMessage, InternalMessage, MsgKind, EXT_BODY_PREFIX_LEN,
    MAX_MESSAGE_BYTES, ONX_ADDR_V1, ONX_MSGS_ROOT_V1, ONX_MSG_EXT_SIGN_V1, ONX_MSG_EXT_V1,
    ONX_MSG_INT_V1,
};
pub use state::State;
/// Gas economics for contract execution: 1_000 gas per nano-Onyx of declared fee.
pub use stf::GAS_PER_NANO;
pub use stf::{apply_block, propose_block, AppliedMessage, DeliveryReceipt, Receipts};
