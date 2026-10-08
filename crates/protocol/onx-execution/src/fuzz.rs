//! Property-style fuzz scaffold for the interpreter (wave-3, ADR-0028).
//!
//! Deliberately dependency-free: a tiny hand-rolled XorShift64 PRNG generates
//! bytecode streams that are fed to the interpreter. Every generated program
//! asserts the two load-bearing production invariants:
//!
//! 1. **No panic.** A panic inside the producer halts block production and
//!    wedges the node into a systemd restart loop. The interpreter must map
//!    every adversarial input to an [`ExceptionKind`], never unwind.
//!    (`catch_unwind` is used here in the *test harness only* to turn a
//!    would-be halt into a test failure.)
//! 2. **Stack-depth caps.** The operand stack never exceeds `MAX_STACK_DEPTH`
//!    (`push` fails closed with `MalformedCell` past the cap), and the call
//!    stack never exceeds `MAX_CALL_STACK_DEPTH` (ADR-0039: `CALLREF`
//!    fails closed with `CallStackOverflow` past the cap).
//!
//! A third property is checked opportunistically: the same program executed
//! twice yields the same [`ExecutionResult`] (deterministic replay is a
//! consensus requirement; a nondeterministic interpreter would fork the
//! chain).

use crate::interpreter::{Interpreter, MAX_CALL_STACK_DEPTH, MAX_STACK_DEPTH};
use crate::types::{ExceptionKind, ExecutionContext, ExecutionResult};
use onx_data_structures::{AccountId, FullAddress, Message, MessageType, WorkchainIdent};
use onx_primitives::{Uint128, Uint256, Uint64};
use onx_state_model::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Minimal xorshift64* PRNG. Deterministic across runs and platforms for a
/// fixed seed, so every fuzz corpus here is reproducible.
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_u8(&mut self) -> u8 {
        self.next_u64() as u8
    }

    /// Value in `0..n`. `n` must be positive. Uses multiply-high (Lemire)
    /// range reduction instead of `%`: division-free, so there is no
    /// divisor at all to be zero (and no `arithmetic_side_effects` lint
    /// to trip on). Slight modulo bias is irrelevant for a fuzz PRNG.
    fn below(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        ((self.next_u64() as u128).wrapping_mul(n as u128) >> 64) as usize
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    fn next_i128(&mut self) -> i128 {
        i128::from_be_bytes([
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
            self.next_u8(),
        ])
    }
}

fn dummy_message() -> Message {
    let addr = FullAddress::new(WorkchainIdent::BASIC, AccountId::from_bytes([0x01; 32]));
    Message {
        msg_type: MessageType::Internal,
        src_address: addr,
        dest_address: addr,
        amount_nanos: Uint128::from(1000u128),
        extra_currencies: vec![],
        created_lt: Uint64::from(100u64),
        body_cell_hash: Uint256([0xAA; 32]),
    }
}

fn dummy_context() -> ExecutionContext {
    ExecutionContext {
        gen_utime: 1700000000,
        start_lt: 100,
        end_lt: 200,
        gas_limit: 5_000,
        chain_id: [0x43; 32], // test chain id (ADR-0038)
    }
}

fn push_int_bytes(prog: &mut Vec<u8>, v: i128) {
    prog.push(0x08);
    prog.push(1); // signed flag (ignored by the interpreter)
    let mut bytes = [0u8; 32];
    if v < 0 {
        bytes[..16].fill(0xFF);
    }
    bytes[16..32].copy_from_slice(&v.to_be_bytes());
    prog.extend_from_slice(&bytes);
}

/// Edge magnitudes the wave-3 audit cares about, plus room for randoms.
const EDGE_I128: [i128; 21] = [
    0,
    1,
    -1,
    2,
    -2,
    127,
    128,
    -128,
    255,
    256,
    -256,
    32767,
    -32768,
    2147483647,
    -2147483648,
    9223372036854775807,
    -9223372036854775808,
    i128::MAX,
    i128::MIN,
    i128::MAX - 1,
    i128::MIN + 1,
];

const WIDTHS: [u16; 9] = [0, 1, 8, 63, 64, 65, 127, 128, 256];
const FLAVORS: [u8; 5] = [0, 1, 2, 3, 255];
/// 0x10..=0x19 arithmetic family; 0x16 ISZERO takes one operand.
const ARITH_TWO_OPERAND: [u8; 9] = [0x10, 0x11, 0x13, 0x14, 0x15, 0x17, 0x18, 0x19, 0x12];
const STACK_OPS: [u8; 8] = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x0A, 0x0B];
const DEPTH_OPS: [u8; 7] = [0x06, 0x07, 0x0C, 0x78, 0x79, 0x7A, 0x7B];

/// Random bytecode biased toward decodable instruction shapes: PUSHINTs with
/// edge/random immediates, arithmetic ops with (in)valid width/flavor
/// operands, stack/byte-string ops, and raw noise (bad opcodes, truncated
/// operands). Capped at the 128-byte cell data limit.
fn gen_program(rng: &mut XorShift64) -> Vec<u8> {
    let mut prog = Vec::new();
    let target_len = rng.below(129);
    while prog.len() < target_len {
        match rng.below(10) {
            0..=2 => {
                let v = if rng.below(2) == 0 {
                    *rng.pick(&EDGE_I128)
                } else {
                    rng.next_i128()
                };
                push_int_bytes(&mut prog, v);
            }
            3 => {
                let op = *rng.pick(&ARITH_TWO_OPERAND);
                prog.push(op);
                let w = *rng.pick(&WIDTHS);
                prog.push((w >> 8) as u8);
                prog.push((w & 0xFF) as u8);
                prog.push(*rng.pick(&FLAVORS));
            }
            4 => {
                prog.push(0x16); // ISZERO, one operand
            }
            5 => {
                prog.push(*rng.pick(&STACK_OPS));
            }
            6 => {
                prog.push(*rng.pick(&DEPTH_OPS));
                prog.push(rng.next_u8());
                prog.push(rng.next_u8());
            }
            7 => {
                prog.push(0x09); // PUSHBYTES
                let len = rng.below(65);
                prog.push((len >> 8) as u8);
                prog.push((len & 0xFF) as u8);
                for _ in 0..len {
                    prog.push(rng.next_u8());
                }
            }
            _ => {
                prog.push(rng.next_u8());
            }
        }
    }
    prog.truncate(128);
    prog
}

/// Runs `program` to completion, asserting the no-panic and stack-cap
/// invariants. Returns the result for further property checks.
fn run_checked(program: &[u8]) -> ExecutionResult {
    let code = Cell::new(program.to_vec(), vec![]).expect("fuzz program fits a cell");
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let mut interp = Interpreter::new(
            code,
            Cell::new(vec![], vec![]).expect("empty cell"),
            dummy_message(),
            dummy_context(),
        );
        let result = interp.run();
        (
            result,
            interp.stack.len(),
            interp.call_stack.len(),
            interp.gas_used,
        )
    }));
    match outcome {
        Ok((result, depth, call_depth, gas_used)) => {
            assert!(
                depth <= MAX_STACK_DEPTH,
                "stack depth {depth} exceeded cap on program {program:02x?}"
            );
            assert!(
                call_depth <= MAX_CALL_STACK_DEPTH,
                "call-stack depth {call_depth} exceeded cap on program {program:02x?}"
            );
            assert!(
                gas_used <= dummy_context().gas_limit,
                "gas_used {gas_used} exceeded limit on program {program:02x?}"
            );
            result
        }
        Err(_) => panic!("interpreter PANICKED on fuzzed program {program:02x?}"),
    }
}

#[test]
fn fuzz_biased_bytecode_never_panics() {
    for seed in 1..=4u64 {
        let mut rng = XorShift64::new(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        for _ in 0..200 {
            let program = gen_program(&mut rng);
            // Determinism: the same bytecode must produce the same result
            // twice in a row (consensus-critical).
            let first = run_checked(&program);
            let second = run_checked(&program);
            assert_eq!(
                first, second,
                "nondeterministic execution on program {program:02x?}"
            );
            // Any exception kind is acceptable; only panics are not.
            let _ = first;
        }
    }
}

#[test]
fn fuzz_arithmetic_edge_values_never_panic() {
    // Exhaustive (a, b) edge pairs across the arithmetic family, including
    // every width/flavor combination the interpreter accepts or rejects.
    // This is the directed version of the MIN/-1 regression: no pair may
    // panic, whatever exception kind results.
    for &op in ARITH_TWO_OPERAND.iter().chain(std::iter::once(&0x16)) {
        for &a in &EDGE_I128 {
            for &b in &EDGE_I128 {
                for &w in &WIDTHS {
                    for &f in &FLAVORS {
                        let mut prog = Vec::new();
                        push_int_bytes(&mut prog, a);
                        if op != 0x16 {
                            push_int_bytes(&mut prog, b);
                        }
                        prog.push(op);
                        if op != 0x16 {
                            prog.push((w >> 8) as u8);
                            prog.push((w & 0xFF) as u8);
                            prog.push(f);
                        }
                        let result = run_checked(&prog);
                        // Exception kinds are all legitimate outcomes here;
                        // the property under test is "no panic".
                        match result {
                            ExecutionResult::Success { .. } | ExecutionResult::Exception { .. } => {
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn fuzz_result_kinds_stay_in_closed_set() {
    // The exception surface must remain exactly the spec's closed set —
    // `ExceptionKind` is a closed enum so this is structural, but the test
    // pins the Display mapping the STF/log pipeline relies on.
    for kind in [
        ExceptionKind::OutOfGas,
        ExceptionKind::IntegerOverflow,
        ExceptionKind::AbsentNode,
        ExceptionKind::MalformedCell,
        ExceptionKind::TypeMismatch,
    ] {
        assert!(!kind.to_string().is_empty());
    }
}
