//! Code length is the code cell's exact bit length (ADR-0036).
//!
//! `step()` and the operand readers used to measure code as
//! `8 × data_bytes.len()`, so in a bit-granular code cell the completion tag
//! and padding bits executed as instructions: a 1-bit code cell stores byte
//! `0x40` and ran it as `NEWC`. They are now bounded by `Cell::bit_len()`,
//! and trailing bits too few for a whole opcode raise `MalformedCell`.

use onx_data_structures::{AccountId, FullAddress, Message, MessageType, WorkchainIdent};
use onx_execution::{execute, ExceptionKind, ExecutionContext, ExecutionResult};
use onx_primitives::{Uint128, Uint256, Uint64};
use onx_state_model::Cell;

fn message() -> Message {
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

fn context() -> ExecutionContext {
    ExecutionContext {
        gen_utime: 1700000000,
        start_lt: 100,
        end_lt: 200,
        gas_limit: 1_000,
        chain_id: [0x43; 32],
    }
}

fn run(code: Cell) -> ExecutionResult {
    execute(
        code,
        Cell::new(vec![], vec![]).unwrap(),
        message(),
        context(),
    )
}

#[test]
fn one_bit_code_cell_raises_malformed_cell_instead_of_running_newc() {
    // The single data bit is 0; the completion tag makes the stored byte
    // 0x40, which is NEWC's opcode.
    let code = Cell::new_with_bit_len(vec![0x00], 1, vec![]).unwrap();
    assert_eq!(code.data_bytes(), &[0x40]);
    assert_eq!(code.bit_len(), 1);

    match run(code) {
        ExecutionResult::Exception { kind, gas_used } => {
            assert_eq!(kind, ExceptionKind::MalformedCell);
            assert_eq!(gas_used, 0, "no instruction ran");
        }
        other => panic!("expected MalformedCell, got {other:?}"),
    }
}

#[test]
fn trailing_partial_byte_after_whole_opcodes_raises_malformed_cell() {
    // 9 bits: NOP (0x00), then one 0 bit. Stored as [0x00, 0x40]; the old
    // reader ran NOP then NEWC from the tag byte and succeeded.
    let code = Cell::new_with_bit_len(vec![0x00, 0x00], 9, vec![]).unwrap();
    assert_eq!(code.data_bytes(), &[0x00, 0x40]);

    match run(code) {
        ExecutionResult::Exception { kind, gas_used } => {
            assert_eq!(kind, ExceptionKind::MalformedCell);
            assert_eq!(gas_used, 1, "only the NOP ran");
        }
        other => panic!("expected MalformedCell, got {other:?}"),
    }
}

#[test]
fn byte_aligned_code_cell_is_unaffected() {
    // NOP, RET: byte-granular, runs to completion as before.
    let code = Cell::new(vec![0x00, 0x72], vec![]).unwrap();
    match run(code) {
        ExecutionResult::Success { gas_used, .. } => assert_eq!(gas_used, 5),
        other => panic!("expected success, got {other:?}"),
    }
}

#[test]
fn bit_granular_cell_with_whole_bytes_of_code_and_bits_raises_on_the_remainder() {
    // 12 bits: NOP, then 4 zero bits. The four bits can't hold an opcode.
    let code = Cell::new_with_bit_len(vec![0x00, 0x00], 12, vec![]).unwrap();
    assert_eq!(code.bit_len(), 12);
    match run(code) {
        ExecutionResult::Exception { kind, gas_used } => {
            assert_eq!(kind, ExceptionKind::MalformedCell);
            assert_eq!(gas_used, 1);
        }
        other => panic!("expected MalformedCell, got {other:?}"),
    }
}
