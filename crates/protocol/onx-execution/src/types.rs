use crate::int257::Int257;
use onx_data_structures::Message;
use onx_state_model::{Cell, StateModelError};
use std::fmt;

/// Closed exception set per docs/specification/execution.md §3.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExceptionKind {
    /// Gas limit reached.
    OutOfGas,
    /// Unsigned/signed arithmetic or conversion overflow or division by zero.
    IntegerOverflow,
    /// Access to pruned Merkle-proof cell content (CTOS).
    AbsentNode,
    /// Structural cell violation or bytecode decode/stack violation.
    MalformedCell,
    /// Operand stack type mismatch or signature length mismatch.
    TypeMismatch,
}

impl fmt::Display for ExceptionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfGas => write!(f, "OutOfGas"),
            Self::IntegerOverflow => write!(f, "IntegerOverflow"),
            Self::AbsentNode => write!(f, "AbsentNode"),
            Self::MalformedCell => write!(f, "MalformedCell"),
            Self::TypeMismatch => write!(f, "TypeMismatch"),
        }
    }
}

impl std::error::Error for ExceptionKind {}

impl From<StateModelError> for ExceptionKind {
    fn from(_err: StateModelError) -> Self {
        ExceptionKind::MalformedCell
    }
}

/// Execution environment context per docs/specification/execution.md §3.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionContext {
    pub gen_utime: u32,
    pub start_lt: u64,
    pub end_lt: u64,
    pub gas_limit: u64,
}

/// A read cursor over a Cell's data bytes and child cell references.
///
/// `child_cells` is positional: entry `i` corresponds to `cell.cell_refs()[i]`.
/// `Some(cell)` means the host provided the referenced child's content (via
/// the interpreter's cell store); `None` means the content is absent and
/// `LDREF` must fail closed (`AbsentNode`) rather than invent data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slice {
    pub cell: Cell,
    pub bit_offset: usize,
    pub ref_offset: usize,
    pub child_cells: Vec<Option<Cell>>,
}

impl Slice {
    pub fn new(cell: Cell) -> Self {
        let refs = cell.cell_refs().len();
        Self {
            cell,
            bit_offset: 0,
            ref_offset: 0,
            child_cells: vec![None; refs],
        }
    }

    pub fn new_with_children(cell: Cell, child_cells: Vec<Option<Cell>>) -> Self {
        Self {
            cell,
            bit_offset: 0,
            ref_offset: 0,
            child_cells,
        }
    }

    pub fn remaining_bits(&self) -> usize {
        let total_bits = self.cell.data_bytes().len().saturating_mul(8);
        total_bits.saturating_sub(self.bit_offset)
    }

    pub fn remaining_refs(&self) -> usize {
        self.cell.cell_refs().len().saturating_sub(self.ref_offset)
    }

    /// Reads up to 256 bits as a canonical 33-byte big-endian 257-bit
    /// value (bits right-aligned into the low 257 bits; the top 7 bits of
    /// byte 0 stay zero, so the result is always a canonical [`Int257`]
    /// encoding — the caller converts with `Int257::from_bytes33`).
    pub fn read_bits(&mut self, width_bits: usize) -> Result<[u8; 33], ExceptionKind> {
        if self.remaining_bits() < width_bits || width_bits > 256 {
            return Err(ExceptionKind::MalformedCell);
        }

        let mut res = [0u8; 33];
        let data = self.cell.data_bytes();

        for i in 0..width_bits {
            // Every index below is in-range: the guard above keeps
            // `bit_offset + i` inside the cell's data bits and
            // `dest_bit_idx` inside the 33-byte buffer, so the
            // `saturating_*`/`wrapping_*` forms below are exact, not
            // silent clamps. They exist to name the overflow behavior
            // explicitly per the crate's `arithmetic_side_effects` policy.
            let src_bit_idx = self.bit_offset.saturating_add(i);
            let src_byte_idx = src_bit_idx.wrapping_div(8);
            let src_bit_in_byte = 7usize.saturating_sub(src_bit_idx.wrapping_rem(8));
            let bit_val = (data[src_byte_idx] >> src_bit_in_byte) & 1;

            // Big-endian read: the first bit read is the most significant
            // of the `width_bits`. The value occupies buffer bits
            // [264 - width_bits, 264); bits [0, 264 - width_bits) stay zero,
            // which keeps byte 0's top 7 bits zero (canonical) for
            // width_bits <= 256.
            let dest_bit_idx = 264usize.saturating_sub(width_bits).saturating_add(i);
            let dest_byte_idx = dest_bit_idx.wrapping_div(8);
            let dest_bit_in_byte = 7usize.saturating_sub(dest_bit_idx.wrapping_rem(8));

            if bit_val == 1 {
                res[dest_byte_idx] |= 1 << dest_bit_in_byte;
            }
        }

        self.bit_offset = self.bit_offset.saturating_add(width_bits);
        Ok(res)
    }
}

/// A write accumulator for constructing new Cells.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Builder {
    pub data_bytes: Vec<u8>,
    pub current_bit_len: usize,
    pub references: Vec<Cell>,
}

impl Builder {
    /// Appends the lower `width_bits` of a canonical 33-byte big-endian
    /// 257-bit value (the low bits of its two's-complement encoding).
    pub fn append_bits(
        &mut self,
        val_bytes: &[u8; 33],
        width_bits: usize,
    ) -> Result<(), ExceptionKind> {
        if width_bits > 256 {
            return Err(ExceptionKind::MalformedCell);
        }
        let target_total_bits = self.current_bit_len.saturating_add(width_bits);
        if target_total_bits > 128usize.saturating_mul(8) {
            return Err(ExceptionKind::MalformedCell);
        }

        for i in 0..width_bits {
            // In-range by the same argument as `read_bits`: the guard above
            // keeps every index exact, so the explicit forms are not clamps.
            // The value's low `width_bits` bits are buffer bits
            // [264 - width_bits, 264).
            let src_bit_idx = 264usize.saturating_sub(width_bits).saturating_add(i);
            let src_byte_idx = src_bit_idx.wrapping_div(8);
            let src_bit_in_byte = 7usize.saturating_sub(src_bit_idx.wrapping_rem(8));
            let bit_val = (val_bytes[src_byte_idx] >> src_bit_in_byte) & 1;

            let dest_bit_idx = self.current_bit_len.saturating_add(i);
            let dest_byte_idx = dest_bit_idx.wrapping_div(8);
            let dest_bit_in_byte = 7usize.saturating_sub(dest_bit_idx.wrapping_rem(8));

            if dest_byte_idx >= self.data_bytes.len() {
                self.data_bytes.push(0);
            }

            if bit_val == 1 {
                self.data_bytes[dest_byte_idx] |= 1 << dest_bit_in_byte;
            }
        }

        self.current_bit_len = target_total_bits;
        Ok(())
    }
}

/// Stack values over the five TVM kinds per docs/specification/tvm-instruction-set.md §3.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StackValue {
    /// Canonical 257-bit signed integer (ADR-0035).
    Integer(Int257),
    Bytes(Vec<u8>),
    Cell(Cell),
    Slice(Slice),
    Builder(Builder),
}

/// Successful or exceptional execution result per docs/specification/execution.md §3.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionResult {
    Success {
        new_data: Cell,
        out_messages: Vec<Message>,
        gas_used: u64,
    },
    Exception {
        kind: ExceptionKind,
        gas_used: u64,
    },
}
