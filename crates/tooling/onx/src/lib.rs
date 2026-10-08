//! `onx` — the deterministic-replay command crate.
//!
//! Phase 5 of the deterministic-replay plan. The milestone command:
//!
//! ```text
//! onx replay --genesis genesis.toml --blocks ./blocks/
//! ```
//!
//! The [`blockfile`] module defines the on-disk block file format; the
//! binary wires genesis (Phase 2) → pure STF (Phase 3) → atomic store
//! (Phase 4) together.

pub mod auth;
pub mod blockfile;
