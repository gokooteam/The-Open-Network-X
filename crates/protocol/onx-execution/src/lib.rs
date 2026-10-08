#![deny(clippy::disallowed_types)]
// Phase 1 replay plan: HashMap/HashSet banned in protocol crates (see workspace clippy.toml); use BTreeMap/BTreeSet.
// Wave-3 chain safety (ADR-0028): every integer arithmetic site in this crate
// must be overflow-explicit (`checked_*`/`saturating_*`/`wrapping_*`). A plain
// `+`/`-`/`*`/`/`/`%` on a value reachable from contract bytecode is a
// potential producer panic, i.e. a chain halt. `deny` keeps that property
// load-bearing: new arithmetic must name its overflow behavior.
#![deny(clippy::arithmetic_side_effects)]
//! ONX Virtual Machine Execution Engine and Interpreter.
//!
//! Implements `docs/specification/execution.md` (ADR-0014) and
//! `docs/specification/tvm-instruction-set.md` (ADR-0024).

pub mod continuation;
pub mod dictionary;
#[cfg(test)]
mod fuzz;
pub mod int257;
pub mod interpreter;
pub mod types;

pub use continuation::{Continuation, ControlRegisters};
pub use dictionary::{Dictionary, DictionaryError};
pub use int257::Int257;
pub use interpreter::Interpreter;
pub use types::{Builder, ExceptionKind, ExecutionContext, ExecutionResult, Slice, StackValue};

use onx_data_structures::Message;
use onx_state_model::Cell;

/// Executes contract code against cell data, inbound message, and execution context.
pub fn execute(
    code: Cell,
    data: Cell,
    message: Message,
    context: ExecutionContext,
) -> ExecutionResult {
    let mut interpreter = Interpreter::new(code, data, message, context);
    interpreter.run()
}
