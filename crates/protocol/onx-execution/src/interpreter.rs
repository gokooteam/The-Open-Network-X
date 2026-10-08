use crate::continuation::{Continuation, ControlRegisters};
use crate::int257::Int257;
use crate::types::{Builder, ExceptionKind, ExecutionContext, ExecutionResult, Slice, StackValue};
use onx_data_structures::Message;
use onx_primitives::{
    domain_hash,
    hash::{DomainTag, TX_BODY_V1},
    PublicKey, Signature,
};
use onx_state_model::Cell;
use std::cmp::Ordering;
use std::collections::BTreeMap;
/// `pub(crate)` so the wave-3 fuzz scaffold (`fuzz.rs`) can assert the cap
/// from a single source of truth instead of a magic number.
pub(crate) const MAX_STACK_DEPTH: usize = 1023;

pub struct Interpreter {
    pub stack: Vec<StackValue>,
    pub call_stack: Vec<(Cell, usize)>, // (code_cell, bit_offset)
    pub current_code: Cell,
    pub pc_bits: usize,
    pub data: Cell,
    pub out_messages: Vec<Message>,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub context: ExecutionContext,
    pub code_refs: Vec<Cell>,
    /// TVM continuation control registers c0 (return), c1 (alternative), c2 (exception).
    pub control_registers: ControlRegisters,
    /// The inbound message that triggered this invocation. Readable by
    /// contract code via the `0x80` message opcodes (`MSGSENDER`,
    /// `MSGVALUE`, `MSGBODY`).
    pub message: Message,
    /// Raw inbound message body bytes. `Message` only commits to
    /// `body_cell_hash`; the host holds the actual payload (e.g. the STF's
    /// `InternalMessage.payload`) and sets it here so `MSGBODY` can expose
    /// it. Empty when the host did not provide a body.
    pub message_body: Vec<u8>,
    /// Content-addressed cell store (`cell hash -> Cell`).
    ///
    /// Calling convention for cell persistence across invocations:
    /// - Before execution, the host seeds this map with every cell in the
    ///   DAGs of `code` and `data` (the constructor only seeds the two
    ///   roots; the host must provide the rest, e.g. from the persisted
    ///   `BagOfCells` of the contract's data).
    /// - `CTOS` resolves each child reference through this map; resolved
    ///   children are what `LDREF` returns.
    /// - `ENDC` registers every materialized cell here automatically, so
    ///   after execution the host can persist the full output DAG (any
    ///   entries it does not already have) and the next invocation's
    ///   `LDREF`s resolve to the actual stored children.
    /// - A child reference whose content is absent fails closed at `LDREF`
    ///   with `AbsentNode` — it is never answered with invented data.
    pub cell_store: BTreeMap<[u8; 32], Cell>,
}

impl Interpreter {
    pub fn new(code: Cell, data: Cell, message: Message, context: ExecutionContext) -> Self {
        // Seed the cell store with the two roots. The host must additionally
        // seed it with the rest of the code/data DAGs (see the `cell_store`
        // calling-convention docs) or `LDREF` on their children fails
        // closed with `AbsentNode`.
        let mut cell_store = BTreeMap::new();
        cell_store.insert(code.hash(), code.clone());
        cell_store.insert(data.hash(), data.clone());
        Self {
            stack: Vec::new(),
            call_stack: Vec::new(),
            current_code: code,
            pc_bits: 0,
            data,
            out_messages: Vec::new(),
            gas_limit: context.gas_limit,
            gas_used: 0,
            context,
            code_refs: Vec::new(),
            control_registers: ControlRegisters::default(),
            message,
            message_body: Vec::new(),
            cell_store,
        }
    }

    pub fn set_exception_handler(&mut self, continuation: Continuation) {
        self.control_registers.set_c2(continuation);
    }

    /// Installs c1, the continuation selected for an alternative return.
    pub fn set_alternative_return(&mut self, continuation: Continuation) {
        self.control_registers.set_c1(continuation);
    }

    fn jump_to(&mut self, continuation: Continuation) {
        self.current_code = continuation.code;
        self.pc_bits = continuation.pc_bits;
    }

    /// Transfers execution to c0 or c1.  This is used by embedding hosts that
    /// expose TVM's normal and alternative return paths.
    pub fn return_to_control_register(&mut self, alternative: bool) -> bool {
        if let Some(continuation) = self.control_registers.take_return(alternative) {
            self.jump_to(continuation);
            true
        } else {
            false
        }
    }

    pub fn consume_gas(&mut self, amount: u64) -> Result<(), ExceptionKind> {
        let new_gas = self.gas_used.saturating_add(amount);
        if new_gas > self.gas_limit {
            self.gas_used = self.gas_limit;
            Err(ExceptionKind::OutOfGas)
        } else {
            self.gas_used = new_gas;
            Ok(())
        }
    }

    pub fn pop(&mut self) -> Result<StackValue, ExceptionKind> {
        self.stack.pop().ok_or(ExceptionKind::MalformedCell)
    }

    pub fn pop_integer(&mut self) -> Result<Int257, ExceptionKind> {
        match self.pop()? {
            StackValue::Integer(v) => Ok(v),
            _ => Err(ExceptionKind::TypeMismatch),
        }
    }

    fn push(&mut self, value: StackValue) -> Result<(), ExceptionKind> {
        if self.stack.len() >= MAX_STACK_DEPTH {
            return Err(ExceptionKind::MalformedCell);
        }
        self.stack.push(value);
        Ok(())
    }

    pub fn pop_bytes(&mut self) -> Result<Vec<u8>, ExceptionKind> {
        match self.pop()? {
            StackValue::Bytes(bytes) => Ok(bytes),
            _ => Err(ExceptionKind::TypeMismatch),
        }
    }

    pub fn pop_cell(&mut self) -> Result<Cell, ExceptionKind> {
        match self.pop()? {
            StackValue::Cell(cell) => Ok(cell),
            _ => Err(ExceptionKind::TypeMismatch),
        }
    }

    pub fn pop_slice(&mut self) -> Result<Slice, ExceptionKind> {
        match self.pop()? {
            StackValue::Slice(slice) => Ok(slice),
            _ => Err(ExceptionKind::TypeMismatch),
        }
    }

    pub fn pop_builder(&mut self) -> Result<Builder, ExceptionKind> {
        match self.pop()? {
            StackValue::Builder(builder) => Ok(builder),
            _ => Err(ExceptionKind::TypeMismatch),
        }
    }

    /// Builds a `Slice` over `cell`, resolving each child reference through
    /// the cell store. Children the host did not provide are recorded as
    /// `None`; they fail closed at `LDREF` (`AbsentNode`) instead of being
    /// answered with invented placeholder data.
    fn resolve_slice(&self, cell: Cell) -> Slice {
        let child_cells = cell
            .cell_refs()
            .iter()
            .map(|hash| self.cell_store.get(hash).cloned())
            .collect();
        Slice::new_with_children(cell, child_cells)
    }

    pub fn read_uint8(&mut self) -> Result<u8, ExceptionKind> {
        let total_bits = self.current_code.data_bytes().len().saturating_mul(8);
        if self.pc_bits.saturating_add(8) > total_bits {
            return Err(ExceptionKind::MalformedCell);
        }
        let byte_idx = self.pc_bits / 8;
        let bit_rem = self.pc_bits % 8;
        let data = self.current_code.data_bytes();
        let val = if bit_rem == 0 {
            data[byte_idx]
        } else {
            let b1 = data[byte_idx];
            let b2 = data.get(byte_idx.saturating_add(1)).copied().unwrap_or(0);
            (b1 << bit_rem) | (b2 >> 8usize.saturating_sub(bit_rem))
        };
        self.pc_bits = self.pc_bits.saturating_add(8);
        Ok(val)
    }

    pub fn read_uint16(&mut self) -> Result<u16, ExceptionKind> {
        let high = self.read_uint8()? as u16;
        let low = self.read_uint8()? as u16;
        Ok((high << 8) | low)
    }

    pub fn read_bytes_exact(&mut self, len: usize) -> Result<Vec<u8>, ExceptionKind> {
        let mut res = Vec::with_capacity(len);
        for _ in 0..len {
            res.push(self.read_uint8()?);
        }
        Ok(res)
    }

    pub fn step(&mut self) -> Result<bool, ExceptionKind> {
        let total_bits = self.current_code.data_bytes().len().saturating_mul(8);
        if self.pc_bits >= total_bits {
            if let Some((prev_code, prev_pc)) = self.call_stack.pop() {
                self.current_code = prev_code;
                self.pc_bits = prev_pc;
                return Ok(true);
            } else {
                return Ok(false); // Execution finished successfully
            }
        }

        let opcode = self.read_uint8()?;
        match opcode {
            // 0x00: NOP
            0x00 => {
                self.consume_gas(1)?;
            }
            // 0x01: DROP
            0x01 => {
                self.consume_gas(1)?;
                self.pop()?;
            }
            // 0x02: DUP
            0x02 => {
                self.consume_gas(1)?;
                let top = self
                    .stack
                    .last()
                    .ok_or(ExceptionKind::MalformedCell)?
                    .clone();
                self.push(top)?;
            }
            // 0x03: SWAP
            0x03 => {
                self.consume_gas(1)?;
                let a = self.pop()?;
                let b = self.pop()?;
                self.push(a)?;
                self.push(b)?;
            }
            // 0x04: OVER
            0x04 => {
                self.consume_gas(1)?;
                let len = self.stack.len();
                if len < 2 {
                    return Err(ExceptionKind::MalformedCell);
                }
                let second = self.stack[len.saturating_sub(2)].clone();
                self.push(second)?;
            }
            // 0x05: ROT
            0x05 => {
                self.consume_gas(1)?;
                let c = self.pop()?;
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(b)?;
                self.push(c)?;
                self.push(a)?;
            }
            // 0x06: PICK depth
            0x06 => {
                self.consume_gas(1)?;
                let depth = self.read_uint8()? as usize;
                let len = self.stack.len();
                if depth >= len {
                    return Err(ExceptionKind::MalformedCell);
                }
                // Exact: the guard gives `depth <= len - 1`, so the chained
                // saturating subtraction computes `len - 1 - depth`.
                let item = self.stack[len.saturating_sub(1).saturating_sub(depth)].clone();
                self.push(item)?;
            }
            // 0x07: ROLL depth
            0x07 => {
                self.consume_gas(1)?;
                let depth = self.read_uint8()? as usize;
                let len = self.stack.len();
                if depth >= len {
                    return Err(ExceptionKind::MalformedCell);
                }
                // Exact by the same guard argument as PICK above.
                let item = self
                    .stack
                    .remove(len.saturating_sub(1).saturating_sub(depth));
                self.push(item)?;
            }
            // 0x08: PUSHINT signed, value[32]. The signed flag selects the
            // 32-byte operand's interpretation: nonzero = 256-bit
            // two's complement (sign-extended to 257 bits), zero = unsigned
            // 256-bit. Both fit the Integer domain (ADR-0035).
            0x08 => {
                self.consume_gas(1)?;
                let signed = self.read_uint8()?;
                let bytes = self.read_bytes_exact(32)?;
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                let v = if signed == 0 {
                    Int257::from_unsigned256(&arr)
                } else {
                    Int257::from_signed256(&arr)
                };
                self.push(StackValue::Integer(v))?;
            }
            // 0x09: PUSHBYTES len[uint16], bytes
            0x09 => {
                let len = self.read_uint16()? as usize;
                let gas_cost = len.div_ceil(32).saturating_add(1);
                self.consume_gas(gas_cost as u64)?;
                let bytes = self.read_bytes_exact(len)?;
                self.push(StackValue::Bytes(bytes))?;
            }
            // 0x0A: NIP, (a, b) -> (b)
            0x0A => {
                self.consume_gas(1)?;
                if self.stack.len() < 2 {
                    return Err(ExceptionKind::MalformedCell);
                }
                let top = self.pop()?;
                self.pop()?;
                self.push(top)?;
            }
            // 0x0B: TUCK, (a, b) -> (b, a, b)
            0x0B => {
                self.consume_gas(1)?;
                if self.stack.len() < 2 {
                    return Err(ExceptionKind::MalformedCell);
                }
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(b.clone())?;
                self.push(a)?;
                self.push(b)?;
            }
            // 0x0C: BLKSWAP left:uint8, right:uint8. Swap adjacent top blocks.
            0x0C => {
                self.consume_gas(1)?;
                let left = self.read_uint8()? as usize;
                let right = self.read_uint8()? as usize;
                let count = left
                    .checked_add(right)
                    .ok_or(ExceptionKind::MalformedCell)?;
                if left == 0 || right == 0 || self.stack.len() < count {
                    return Err(ExceptionKind::MalformedCell);
                }
                // Exact: the guard gives `count <= stack.len()`.
                let start = self.stack.len().saturating_sub(count);
                self.stack[start..].rotate_left(left);
            }
            // Arithmetic 0x10-0x15. Each opcode computes the true mathematical
            // result exactly (Int257, ADR-0035) and then applies the declared
            // width/flavor per spec §3.3: unsigned/signed flavors raise
            // IntegerOverflow when the true result does not fit, the modulo
            // flavor reduces mod 2^width.
            0x10..=0x15 => {
                self.consume_gas(if opcode == 0x13 || opcode == 0x14 {
                    8
                } else {
                    4
                })?;
                let width = self.read_uint16()?;
                let flavor = self.read_uint8()?;
                if width == 0 || width > 256 || flavor > 2 {
                    return Err(ExceptionKind::MalformedCell);
                }
                match opcode {
                    0x10 => {
                        // ADD
                        let b = self.pop_integer()?;
                        let a = self.pop_integer()?;
                        self.push(StackValue::Integer(a.add_exact(b).apply(width, flavor)?))?;
                    }
                    0x11 => {
                        // SUB
                        let b = self.pop_integer()?;
                        let a = self.pop_integer()?;
                        self.push(StackValue::Integer(a.sub_exact(b).apply(width, flavor)?))?;
                    }
                    0x12 => {
                        // NEG
                        let a = self.pop_integer()?;
                        self.push(StackValue::Integer(a.neg_exact().apply(width, flavor)?))?;
                    }
                    0x13 => {
                        // MUL
                        let b = self.pop_integer()?;
                        let a = self.pop_integer()?;
                        self.push(StackValue::Integer(a.mul_exact(b).apply(width, flavor)?))?;
                    }
                    0x14 => {
                        // DIVMOD — true floor division per spec §4.3 and
                        // ADR-0030/ADR-0035: q = floor(a/b), r = a - q*b with
                        // sign(r) == sign(b) or r == 0. (ADR-0028 pinned the
                        // Euclidean remainder by mistake; `checked_div_euclid`
                        // disagreed with the spec on negative divisors, e.g.
                        // 7 DIVMOD -2 was (-3, 1), must be (-4, -1).)
                        // Division by zero precedes flavor selection and
                        // raises for every flavor (spec §3.3).
                        let b = self.pop_integer()?;
                        let a = self.pop_integer()?;
                        let (q, r) = Int257::floored_divmod_exact(a, b)
                            .ok_or(ExceptionKind::IntegerOverflow)?;
                        self.push(StackValue::Integer(q.apply(width, flavor)?))?;
                        self.push(StackValue::Integer(r.apply(width, flavor)?))?;
                    }
                    0x15 => {
                        // CMP — total order on 257-bit integers; never raises.
                        let b = self.pop_integer()?;
                        let a = self.pop_integer()?;
                        let r = match a.cmp(&b) {
                            Ordering::Less => -1,
                            Ordering::Equal => 0,
                            Ordering::Greater => 1,
                        };
                        self.push(StackValue::Integer(Int257::from_i64(r)))?;
                    }
                    _ => unreachable!(),
                }
            }
            0x16 => {
                // ISZERO
                self.consume_gas(4)?;
                let a = self.pop_integer()?;
                let v = if a.is_zero() { 1 } else { 0 };
                self.push(StackValue::Integer(Int257::from_u64(v)))?;
            }
            // Integer division and shifts 0x17-0x19. Same width/flavor rules
            // as the baseline arithmetic family (ADR-0035): exact result,
            // then width/flavor application. Range checks (division by zero,
            // shift amount) precede flavor selection and raise
            // IntegerOverflow for every flavor (spec §3.3).
            0x17..=0x19 => {
                self.consume_gas(if opcode == 0x17 { 8 } else { 4 })?;
                let width = self.read_uint16()?;
                let flavor = self.read_uint8()?;
                if width == 0 || width > 256 || flavor > 2 {
                    return Err(ExceptionKind::MalformedCell);
                }
                let b = self.pop_integer()?;
                let a = self.pop_integer()?;
                let value = match opcode {
                    // DIV returns exactly DIVMOD's quotient (ADR-0030), with
                    // the declared width/flavor applied to it.
                    0x17 => {
                        let (q, _) = Int257::floored_divmod_exact(a, b)
                            .ok_or(ExceptionKind::IntegerOverflow)?;
                        q.apply(width, flavor)?
                    }
                    0x18 => {
                        // LSHIFT: shift amount in [0, width), else raise.
                        let shift = b.to_usize_checked().ok_or(ExceptionKind::IntegerOverflow)?;
                        if shift >= width as usize {
                            return Err(ExceptionKind::IntegerOverflow);
                        }
                        a.shl_exact(shift as u32).apply(width, flavor)?
                    }
                    0x19 => {
                        // RSHIFT: arithmetic shift right (floor division by
                        // 2^shift), exact and always in-domain; the declared
                        // width/flavor still apply to the result.
                        let shift = b.to_usize_checked().ok_or(ExceptionKind::IntegerOverflow)?;
                        if shift >= width as usize {
                            return Err(ExceptionKind::IntegerOverflow);
                        }
                        a.shr_exact(shift as u32).check_width(width, flavor)?
                    }
                    _ => unreachable!(),
                };
                self.push(StackValue::Integer(value))?;
            }
            0x20 => {
                // CONV width, signed — re-checks the value at the declared
                // width/signedness (spec §4.2, ADR-0035). Raises
                // IntegerOverflow when the value does not fit.
                self.consume_gas(4)?;
                let width = self.read_uint16()?;
                let signed = self.read_uint8()?;
                if width == 0 || width > 256 {
                    return Err(ExceptionKind::MalformedCell);
                }
                let a = self.pop_integer()?;
                let flavor = if signed == 0 { 0 } else { 1 };
                self.push(StackValue::Integer(a.check_width(width, flavor)?))?;
            }
            // Byte/bit string operations 0x30-0x33
            0x30 => {
                // BYTELEN
                self.consume_gas(1)?;
                let bytes = self.pop_bytes()?;
                self.push(StackValue::Integer(Int257::from_u64(bytes.len() as u64)))?;
            }
            0x31 => {
                // CONCAT
                let b = self.pop_bytes()?;
                let a = self.pop_bytes()?;
                let total_len = a.len().saturating_add(b.len());
                self.consume_gas(4u64.saturating_add(total_len.div_ceil(32) as u64))?;
                let mut res = a;
                res.extend(b);
                self.push(StackValue::Bytes(res))?;
            }
            0x32 => {
                // SUBBYTES
                self.consume_gas(4)?;
                let len = self
                    .pop_integer()?
                    .to_usize_checked()
                    .ok_or(ExceptionKind::MalformedCell)?;
                let offset = self
                    .pop_integer()?
                    .to_usize_checked()
                    .ok_or(ExceptionKind::MalformedCell)?;
                let bytes = self.pop_bytes()?;
                // `to_usize_checked` already rejects negatives and values
                // above `usize::MAX`; `checked_add` keeps an adversarial
                // (offset, len) pair from wrapping past the length check.
                let end = offset
                    .checked_add(len)
                    .ok_or(ExceptionKind::MalformedCell)?;
                if end > bytes.len() {
                    return Err(ExceptionKind::MalformedCell);
                }
                self.push(StackValue::Bytes(bytes[offset..end].to_vec()))?;
            }
            0x33 => {
                // BYTEEQ
                let b = self.pop_bytes()?;
                let a = self.pop_bytes()?;
                let min_len = a.len().min(b.len());
                self.consume_gas(1u64.saturating_add(min_len.div_ceil(32) as u64))?;
                self.push(StackValue::Integer(Int257::from_u64(if a == b {
                    1
                } else {
                    0
                })))?;
            }
            // Cell access 0x40-0x4C
            0x40 => {
                // NEWC
                self.consume_gas(10)?;
                self.push(StackValue::Builder(Builder::default()))?;
            }
            0x41 => {
                // ENDC — ADR-0036: the cell commits the builder's exact bit
                // length. A partial final byte sets the compatible flag and
                // gets the completion tag; byte-aligned builders stay
                // byte-granular with unchanged hashes.
                self.consume_gas(10)?;
                let builder = self.pop_builder()?;
                let cell_refs = builder.references.iter().map(|c| c.hash()).collect();
                let cell =
                    Cell::new_with_bit_len(builder.data_bytes, builder.current_bit_len, cell_refs)
                        .map_err(|_| ExceptionKind::MalformedCell)?;
                // Register the materialized cell so the host can persist
                // the full DAG after execution and so later LDREFs resolve
                // to the actual stored child.
                self.cell_store.insert(cell.hash(), cell.clone());
                self.push(StackValue::Cell(cell))?;
            }
            0x42 => {
                // STBITS width, signed. Range-checks the value at the
                // declared width/signedness (spec §4.4, ADR-0035), then
                // appends the low `width` bits of its two's-complement
                // encoding. Width 0 keeps the historical no-op acceptance.
                self.consume_gas(10)?;
                let width = self.read_uint16()?;
                let signed = self.read_uint8()?;
                if width > 256 {
                    return Err(ExceptionKind::MalformedCell);
                }
                let val = self.pop_integer()?;
                let val = if width == 0 {
                    val
                } else {
                    let flavor = if signed == 0 { 0 } else { 1 };
                    val.check_width(width, flavor)?
                };
                let mut builder = self.pop_builder()?;
                builder.append_bits(&val.to_bytes33(), width as usize)?;
                self.push(StackValue::Builder(builder))?;
            }
            0x43 => {
                // STREF
                self.consume_gas(10)?;
                let cell = self.pop_cell()?;
                let mut builder = self.pop_builder()?;
                if builder.references.len() >= 4 {
                    return Err(ExceptionKind::MalformedCell);
                }
                builder.references.push(cell);
                self.push(StackValue::Builder(builder))?;
            }
            0x44 => {
                // STBYTES — ADR-0036: bytes are appended at bit granularity
                // (each byte's 8 bits, MSB-first, at the current bit position)
                // and `current_bit_len` advances by `8 * len`, so the bit
                // length stays exact even after a partial-bit store. The old
                // code extended `data_bytes` without touching the bit length,
                // which is the bookkeeping half of the bit-length finding.
                let bytes = self.pop_bytes()?;
                self.consume_gas(10u64.saturating_add(bytes.len().div_ceil(32) as u64))?;
                let mut builder = self.pop_builder()?;
                builder.append_bytes(&bytes)?;
                self.push(StackValue::Builder(builder))?;
            }
            0x45 => {
                // CTOS
                self.consume_gas(10)?;
                let cell = self.pop_cell()?;
                if cell.is_special() {
                    return Err(ExceptionKind::AbsentNode);
                }
                let slice = self.resolve_slice(cell);
                self.push(StackValue::Slice(slice))?;
            }
            0x46 => {
                // LDU width — unsigned: zero-extended into the Integer domain.
                self.consume_gas(10)?;
                let width = self.read_uint16()? as usize;
                let mut slice = self.pop_slice()?;
                let val_bytes = slice.read_bits(width)?;
                self.push(StackValue::Slice(slice))?;
                // Canonical by construction (top bits zero); the error arm
                // documents the invariant rather than a reachable path.
                let v = Int257::from_bytes33(&val_bytes).ok_or(ExceptionKind::MalformedCell)?;
                self.push(StackValue::Integer(v))?;
            }
            0x47 => {
                // LDI width — signed: sign-extended from `width` bits into
                // the 257-bit domain (spec §4.4, ADR-0035; previously
                // byte-identical to LDU, a spec divergence).
                self.consume_gas(10)?;
                let width = self.read_uint16()? as usize;
                let mut slice = self.pop_slice()?;
                let mut val_bytes = slice.read_bits(width)?;
                self.push(StackValue::Slice(slice))?;
                if width > 0 {
                    // The sign bit is the first bit read = buffer bit
                    // (264 - width). When set, fill buffer bits
                    // [8, 264 - width) with ones and set buffer bit 7
                    // (value bit 256, the 257-bit sign); bits [0, 8) stay
                    // zero to keep the encoding canonical.
                    let sign_bit_idx = 264usize.saturating_sub(width);
                    let sign_byte = sign_bit_idx.wrapping_div(8);
                    let sign_in_byte = 7usize.saturating_sub(sign_bit_idx.wrapping_rem(8));
                    if val_bytes[sign_byte] & (1 << sign_in_byte) != 0 {
                        let mut b = 8usize;
                        while b < sign_bit_idx {
                            let byte = b.wrapping_div(8);
                            let in_byte = 7usize.saturating_sub(b.wrapping_rem(8));
                            val_bytes[byte] |= 1 << in_byte;
                            b = b.wrapping_add(1);
                        }
                        val_bytes[0] |= 1;
                    }
                }
                let v = Int257::from_bytes33(&val_bytes).ok_or(ExceptionKind::MalformedCell)?;
                self.push(StackValue::Integer(v))?;
            }
            0x48 => {
                // LDREF
                self.consume_gas(10)?;
                let mut slice = self.pop_slice()?;
                if slice.remaining_refs() == 0 {
                    return Err(ExceptionKind::MalformedCell);
                }
                // Return the actual stored child cell. A reference whose
                // content the host did not provide fails closed here
                // (AbsentNode) — it must never be answered with an invented
                // placeholder cell.
                let ref_cell = match slice.child_cells.get(slice.ref_offset) {
                    Some(Some(child)) => child.clone(),
                    _ => return Err(ExceptionKind::AbsentNode),
                };
                slice.ref_offset = slice.ref_offset.saturating_add(1);
                self.push(StackValue::Slice(slice))?;
                self.push(StackValue::Cell(ref_cell))?;
            }
            0x49 => {
                // ISEXOTIC
                self.consume_gas(10)?;
                let cell = self.pop_cell()?;
                let v = if cell.is_special() { 1 } else { 0 };
                self.push(StackValue::Integer(Int257::from_u64(v)))?;
            }
            0x4A => {
                // SEMPTY
                self.consume_gas(1)?;
                let slice = self.pop_slice()?;
                let empty = slice.remaining_bits() == 0 && slice.remaining_refs() == 0;
                let v = if empty { 1 } else { 0 };
                self.push(StackValue::Integer(Int257::from_u64(v)))?;
            }
            0x4B => {
                // SBITS
                self.consume_gas(1)?;
                let slice = self.pop_slice()?;
                self.push(StackValue::Integer(Int257::from_u64(
                    slice.remaining_bits() as u64,
                )))?;
            }
            0x4C => {
                // SREFS
                self.consume_gas(1)?;
                let slice = self.pop_slice()?;
                self.push(StackValue::Integer(Int257::from_u64(
                    slice.remaining_refs() as u64,
                )))?;
            }
            0x4D => {
                // SETDATA: pop a cell and install it as the contract's
                // persistent data (TVM c4). This is the integration hook
                // the STF uses: on successful halt, the interpreter's
                // `data` is the contract's new persistent data cell, which
                // the STF writes back to the contract account.
                self.consume_gas(10)?;
                let cell = self.pop_cell()?;
                self.data = cell;
            }
            // Cryptographic 0x60-0x62
            0x60 => {
                // HASHBYTES — the digest is an unsigned 256-bit Integer
                // (spec §4.5, ADR-0035): always in-domain, never negative.
                self.consume_gas(200)?;
                let bytes = self.pop_bytes()?;
                let tag = DomainTag::from_ascii("ONX_EXEC_HASH_V1");
                let hash = domain_hash(&tag, &bytes);
                self.push(StackValue::Integer(Int257::from_unsigned256(&hash)))?;
            }
            0x61 => {
                // HASHCELL — same unsigned-256-bit treatment as HASHBYTES.
                self.consume_gas(200)?;
                let cell = self.pop_cell()?;
                let hash = cell.hash();
                self.push(StackValue::Integer(Int257::from_unsigned256(&hash)))?;
            }
            0x62 => {
                // CHKSIGNU — the hash operand must name a 32-byte preimage,
                // i.e. lie in [0, 2^256); a negative Integer is TypeMismatch.
                self.consume_gas(4000)?;
                let hash_int = self.pop_integer()?;
                let hash32 = hash_int
                    .to_unsigned256()
                    .ok_or(ExceptionKind::TypeMismatch)?;
                let sig_bytes = self.pop_bytes()?;
                let pubkey_bytes = self.pop_bytes()?;
                if pubkey_bytes.len() != 32 || sig_bytes.len() != 64 {
                    return Err(ExceptionKind::TypeMismatch);
                }
                let pubkey = PublicKey::decode_exact(&pubkey_bytes);
                let sig = Signature::decode_exact(&sig_bytes);
                let valid = if let (Ok(pk), Ok(s)) = (pubkey, sig) {
                    pk.verify(&TX_BODY_V1, &hash32, &s).is_ok()
                } else {
                    false
                };
                self.push(StackValue::Integer(Int257::from_u64(if valid {
                    1
                } else {
                    0
                })))?;
            }
            // Control flow 0x70-0x71, 0x73-0x76
            0x70 | 0x71 | 0x73 | 0x74 | 0x75 | 0x76 => {
                self.consume_gas(4)?;
                let ref_idx = self.read_uint8()? as usize;
                // Any nonzero 257-bit Integer is true (ADR-0035: the
                // ADR-0031 fail-closed bulkhead on operands >= 2^127 is
                // gone; the model defines truthiness, not the carrier).
                let condition = match opcode {
                    0x70 | 0x71 => true,
                    0x73 | 0x75 => !self.pop_integer()?.is_zero(),
                    0x74 | 0x76 => self.pop_integer()?.is_zero(),
                    _ => unreachable!(),
                };
                if condition {
                    if ref_idx >= self.code_refs.len() {
                        return Err(ExceptionKind::MalformedCell);
                    }
                    let target_code = self.code_refs[ref_idx].clone();
                    if opcode == 0x71 || opcode == 0x75 || opcode == 0x76 {
                        // CALL variants
                        self.call_stack
                            .push((self.current_code.clone(), self.pc_bits));
                        self.control_registers
                            .set_c0(Continuation::new(self.current_code.clone(), self.pc_bits));
                    }
                    self.current_code = target_code;
                    self.pc_bits = 0;
                }
            }
            0x72 => {
                // RET
                self.consume_gas(4)?;
                if self.return_to_control_register(false) {
                    self.call_stack.pop();
                    self.control_registers.c0 = self
                        .call_stack
                        .last()
                        .map(|(code, pc)| Continuation::new(code.clone(), *pc));
                } else {
                    return Ok(false); // Execution finished successfully
                }
            }
            0x77 => {
                // THROW kind
                self.consume_gas(4)?;
                let kind_byte = self.read_uint8()?;
                let kind = match kind_byte {
                    0 => ExceptionKind::IntegerOverflow,
                    1 => ExceptionKind::AbsentNode,
                    2 => ExceptionKind::MalformedCell,
                    3 => ExceptionKind::TypeMismatch,
                    _ => return Err(ExceptionKind::MalformedCell),
                };
                return Err(kind);
            }
            // 0x78: IFELSE true_offset:int8, false_offset:int8. Offsets are relative
            // to the byte immediately after the instruction.
            0x78 => {
                self.consume_gas(4)?;
                let true_offset = self.read_uint8()? as i8;
                let false_offset = self.read_uint8()? as i8;
                let condition = !self.pop_integer()?.is_zero();
                let offset = if condition { true_offset } else { false_offset };
                self.pc_bits = self
                    .pc_bits
                    .checked_add_signed((offset as isize).saturating_mul(8))
                    .filter(|pc| *pc <= self.current_code.data_bytes().len().saturating_mul(8))
                    .ok_or(ExceptionKind::MalformedCell)?;
            }
            // 0x79: IFRET. Return from the current continuation if the condition is nonzero.
            0x79 => {
                self.consume_gas(4)?;
                if !self.pop_integer()?.is_zero() {
                    if self.return_to_control_register(false) {
                        self.call_stack.pop();
                        self.control_registers.c0 = self
                            .call_stack
                            .last()
                            .map(|(code, pc)| Continuation::new(code.clone(), *pc));
                    } else {
                        return Ok(false);
                    }
                }
            }
            // 0x7A: REPEAT count:uint8, offset:int8. Execute the preceding byte-aligned
            // block `count` times by re-entering it; a zero count is a no-op.
            0x7A => {
                self.consume_gas(4)?;
                let count = self.read_uint8()?;
                let offset = self.read_uint8()? as i8;
                if count > 0 {
                    self.pc_bits = self
                        .pc_bits
                        .checked_add_signed((offset as isize).saturating_mul(8))
                        .filter(|pc| *pc <= self.current_code.data_bytes().len().saturating_mul(8))
                        .ok_or(ExceptionKind::MalformedCell)?;
                }
            }
            // 0x7B: UNTIL offset:int8. Re-enter the preceding block while the condition is zero.
            0x7B => {
                self.consume_gas(4)?;
                let offset = self.read_uint8()? as i8;
                if self.pop_integer()?.is_zero() {
                    self.pc_bits = self
                        .pc_bits
                        .checked_add_signed((offset as isize).saturating_mul(8))
                        .filter(|pc| *pc <= self.current_code.data_bytes().len().saturating_mul(8))
                        .ok_or(ExceptionKind::MalformedCell)?;
                }
            }
            // Inbound-message access 0x80-0x82. These expose the `message`
            // the host passed to `Interpreter::new`, so contract code can
            // inspect what triggered its invocation.
            0x80 => {
                // MSGSENDER: () -> (Bytes). 36-byte sender address:
                // workchain id (i32be) || account id (32 bytes).
                self.consume_gas(4)?;
                let sender = self.message.src_address.to_bytes();
                self.push(StackValue::Bytes(sender.to_vec()))?;
            }
            0x81 => {
                // MSGVALUE: () -> (Integer). Inbound value in nanos,
                // as an unsigned 257-bit integer (always in-domain).
                self.consume_gas(4)?;
                let nanos: u128 = self.message.amount_nanos.into();
                self.push(StackValue::Integer(Int257::from_u128(nanos)))?;
            }
            0x82 => {
                // MSGBODY: () -> (Bytes). Raw inbound message body bytes.
                // Empty when the host did not provide a body: `Message`
                // only commits to `body_cell_hash`, so the host sets
                // `interpreter.message_body` from its own payload copy.
                self.consume_gas(4)?;
                self.push(StackValue::Bytes(self.message_body.clone()))?;
            }
            _ => return Err(ExceptionKind::MalformedCell),
        }

        Ok(true)
    }

    pub fn run(&mut self) -> ExecutionResult {
        loop {
            match self.step() {
                Ok(true) => continue,
                Ok(false) => {
                    return ExecutionResult::Success {
                        new_data: self.data.clone(),
                        out_messages: self.out_messages.clone(),
                        gas_used: self.gas_used,
                    };
                }
                Err(kind) => {
                    if let Some(handler) = self.control_registers.c2() {
                        // c2 receives the deterministic exception discriminator and
                        // execution continues at the handler instead of rolling back.
                        let code = match kind {
                            ExceptionKind::OutOfGas => 0,
                            ExceptionKind::IntegerOverflow => 1,
                            ExceptionKind::AbsentNode => 2,
                            ExceptionKind::MalformedCell => 3,
                            ExceptionKind::TypeMismatch => 4,
                        };
                        if self
                            .push(StackValue::Integer(Int257::from_u64(code as u64)))
                            .is_ok()
                        {
                            self.jump_to(handler);
                            continue;
                        }
                    }
                    return ExecutionResult::Exception {
                        kind,
                        gas_used: self.gas_used,
                    };
                }
            }
        }
    }
}
