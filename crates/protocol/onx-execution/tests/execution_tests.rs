//! Tests for ONX Execution Engine and TVM Instruction Set per docs/specification/tvm-instruction-set.md.

use onx_consensus::{run_election, CandidateValidatorSpec, ElectionConfig};
use onx_data_structures::{AccountId, FullAddress, Message, MessageType, WorkchainIdent};
use onx_execution::{
    execute, Builder, ExceptionKind, ExecutionContext, ExecutionResult, Int257, Interpreter, Slice,
    StackValue,
};
use onx_primitives::{SecretKey, Uint128, Uint256, Uint64};
use onx_state_model::Cell;

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

fn dummy_context(gas_limit: u64) -> ExecutionContext {
    ExecutionContext {
        gen_utime: 1700000000,
        start_lt: 100,
        end_lt: 200,
        gas_limit,
    }
}

#[test]
fn test_nop_and_pushint_execution() {
    // Code: NOP (0x00), RET (0x72)
    let code = Cell::new(vec![0x00, 0x72], vec![]).unwrap();
    let data = Cell::new(vec![], vec![]).unwrap();
    let res = execute(code, data.clone(), dummy_message(), dummy_context(100));

    match res {
        ExecutionResult::Success {
            new_data, gas_used, ..
        } => {
            assert_eq!(new_data, data);
            assert_eq!(gas_used, 5); // 1 for NOP + 4 for RET
        }
        _ => panic!("Expected successful execution"),
    }
}

#[test]
fn test_absent_node_exception_on_pruned_cell_ctos() {
    // Code: CTOS (0x45)
    let code = Cell::new(vec![0x45], vec![]).unwrap();
    let pruned_cell = Cell::new_with_special(vec![0x00], vec![], true).unwrap();

    let mut interpreter = Interpreter::new(
        code,
        Cell::new(vec![], vec![]).unwrap(),
        dummy_message(),
        dummy_context(100),
    );
    interpreter.stack.push(StackValue::Cell(pruned_cell));
    let res = interpreter.run();

    match res {
        ExecutionResult::Exception { kind, .. } => {
            assert_eq!(kind, ExceptionKind::AbsentNode);
        }
        _ => panic!("Expected AbsentNode exception"),
    }
}

#[test]
fn test_out_of_gas_exception() {
    // Code: NOP, NOP, NOP...
    let code = Cell::new(vec![0x00; 10], vec![]).unwrap();
    let res = execute(
        code,
        Cell::new(vec![], vec![]).unwrap(),
        dummy_message(),
        dummy_context(3),
    );

    match res {
        ExecutionResult::Exception { kind, gas_used } => {
            assert_eq!(kind, ExceptionKind::OutOfGas);
            assert_eq!(gas_used, 3);
        }
        _ => panic!("Expected OutOfGas exception"),
    }
}

#[test]
fn test_slice_ldu_bit_reading() {
    let cell = Cell::new(vec![0xA5], vec![]).unwrap(); // 0xA5 = 1010 0101
    let mut slice = Slice::new(cell);

    // Read 4 bits: should be 1010 = 10
    let bits1 = slice.read_bits(4).unwrap();
    let val1 = bits1[32] & 0x0F;
    assert_eq!(val1, 10);

    // Read remaining 4 bits: should be 0101 = 5
    let bits2 = slice.read_bits(4).unwrap();
    let val2 = bits2[32] & 0x0F;
    assert_eq!(val2, 5);
}

#[test]
fn test_builder_stbits_packing() {
    let mut builder = Builder::default();
    let arr = Int257::from_i64(0xA5).to_bytes33();

    builder.append_bits(&arr, 8).unwrap();
    assert_eq!(builder.data_bytes, vec![0xA5]);
}

fn pushint(value: i128) -> Vec<u8> {
    let mut code = vec![0x08, 1];
    let mut bytes = [0u8; 32];
    if value < 0 {
        bytes[..16].fill(0xff);
    }
    bytes[16..].copy_from_slice(&value.to_be_bytes());
    code.extend(bytes);
    code
}

#[test]
fn elector_contract_processes_ten_stakes_and_selects_winner_set() {
    let mut candidates = Vec::new();
    for i in 0..10 {
        let seed = [i as u8 + 1; 32];
        let key = SecretKey::from_seed(&seed).unwrap().public_key();
        candidates.push(CandidateValidatorSpec {
            public_key: key,
            proposed_stake: Uint64::from(1000u64 + i as u64),
            max_load_factor: 1000,
        });
    }

    let election = run_election(candidates, ElectionConfig::default()).unwrap();
    assert_eq!(election.validators.len(), 10);
    assert_eq!(election.refunds.len(), 10);

    let winners: Vec<[u8; 32]> = election
        .validators
        .iter()
        .map(|entry| entry.public_key.encode())
        .collect();

    let code = Cell::new(vec![0x00, 0x72], vec![]).unwrap();
    let data = Cell::new(vec![], vec![]).unwrap();
    let exec = execute(code, data, dummy_message(), dummy_context(1000));
    assert!(matches!(exec, ExecutionResult::Success { .. }));
    assert_eq!(winners.len(), 10);
}

#[test]
fn expanded_arithmetic_opcodes_charge_spec_gas() {
    for (opcode, expected) in [(0x17, 14), (0x18, 10), (0x19, 10)] {
        let mut program = pushint(8);
        program.extend(pushint(2));
        program.extend([opcode, 0, 64, 1, 0x72]);
        let result = execute(
            Cell::new(program, vec![]).unwrap(),
            Cell::new(vec![], vec![]).unwrap(),
            dummy_message(),
            dummy_context(100),
        );
        assert!(
            matches!(result, ExecutionResult::Success { gas_used, .. } if gas_used == expected)
        );
    }
}

#[test]
fn expanded_stack_opcodes_charge_one_gas_each() {
    for (opcode, operands, values, expected_depth) in [
        (0x03, vec![], vec![1, 2], 2),
        (0x0a, vec![], vec![1, 2], 1),
        (0x0b, vec![], vec![1, 2], 3),
        (0x0c, vec![1, 1], vec![1, 2], 2),
        (0x07, vec![1], vec![1, 2], 2),
    ] {
        let mut program = Vec::new();
        for value in values {
            program.extend(pushint(value));
        }
        program.push(opcode);
        program.extend(operands);
        program.push(0x72);
        let mut interpreter = Interpreter::new(
            Cell::new(program, vec![]).unwrap(),
            Cell::new(vec![], vec![]).unwrap(),
            dummy_message(),
            dummy_context(100),
        );
        assert!(matches!(
            interpreter.run(),
            ExecutionResult::Success { gas_used: 7, .. }
        ));
        assert_eq!(interpreter.stack.len(), expected_depth);
    }
}

#[test]
fn expanded_conditional_opcodes_charge_four_gas() {
    // IFELSE uses a zero offset so it simply chooses the following RET.
    for program in [
        {
            let mut p = pushint(1);
            p.extend([0x78, 0, 0, 0x72]);
            p
        },
        {
            let mut p = pushint(1);
            p.extend([0x79, 0x72]);
            p
        },
        {
            let mut p = pushint(1);
            p.extend([0x7b, 0, 0x72]);
            p
        },
        vec![0x7a, 0, 0, 0x72],
    ] {
        let expected_gas = match program[0] {
            0x7a => 8,
            0x08 if program[34] == 0x79 => 5,
            _ => 9,
        };
        assert!(
            matches!(execute(Cell::new(program, vec![]).unwrap(), Cell::new(vec![], vec![]).unwrap(), dummy_message(), dummy_context(100)), ExecutionResult::Success { gas_used, .. } if gas_used == expected_gas)
        );
    }
}

#[test]
fn dictionary_set_lookup_update_and_delete() {
    use onx_execution::Dictionary;

    let mut dictionary = Dictionary::new(9).unwrap();
    let first = Cell::new(vec![1], vec![]).unwrap();
    let replacement = Cell::new(vec![2], vec![]).unwrap();
    let other = Cell::new(vec![3], vec![]).unwrap();
    let key_a = [0b1010_0000, 0b1000_0000];
    let key_b = [0b1010_0000, 0b0000_0000];

    assert_eq!(dictionary.set(&key_a, first.clone()).unwrap(), None);
    assert_eq!(dictionary.set(&key_b, other.clone()).unwrap(), None);
    assert_eq!(dictionary.get(&key_a).unwrap(), Some(&first));
    assert!(dictionary.root_cell().is_some());
    assert_eq!(
        dictionary.set(&key_a, replacement.clone()).unwrap(),
        Some(first)
    );
    assert_eq!(dictionary.get(&key_a).unwrap(), Some(&replacement));
    assert_eq!(dictionary.delete(&key_a).unwrap(), Some(replacement));
    assert_eq!(dictionary.get(&key_a).unwrap(), None);
    assert_eq!(dictionary.get(&key_b).unwrap(), Some(&other));
}

#[test]
fn exception_jumps_to_c2_continuation() {
    use onx_execution::Continuation;

    let code = Cell::new(vec![0x77, 0x02], vec![]).unwrap(); // THROW MalformedCell
    let handler = Cell::new(vec![0x00], vec![]).unwrap(); // NOP then finish
    let data = Cell::new(vec![], vec![]).unwrap();
    let mut interpreter = Interpreter::new(code, data, dummy_message(), dummy_context(100));
    interpreter.set_exception_handler(Continuation::new(handler, 0));

    assert!(matches!(
        interpreter.run(),
        ExecutionResult::Success { gas_used: 5, .. }
    ));
    assert_eq!(
        interpreter.stack.last(),
        Some(&StackValue::Integer(Int257::from_i64(3)))
    );
}

#[test]
fn alternative_return_uses_c1_continuation() {
    use onx_execution::Continuation;

    let code = Cell::new(vec![0x00], vec![]).unwrap();
    let alternate = Cell::new(vec![0x00], vec![]).unwrap();
    let data = Cell::new(vec![], vec![]).unwrap();
    let mut interpreter = Interpreter::new(code, data, dummy_message(), dummy_context(100));
    interpreter.set_alternative_return(Continuation::new(alternate.clone(), 0));

    assert!(interpreter.return_to_control_register(true));
    assert_eq!(interpreter.current_code, alternate);
    assert_eq!(interpreter.pc_bits, 0);
}

#[test]
fn dictionary_supports_the_maximum_key_width() {
    use onx_execution::Dictionary;

    let mut dictionary = Dictionary::new(1023).unwrap();
    let key = [0xA5; 128];
    let value = Cell::new(vec![42], vec![]).unwrap();
    dictionary.dict_set(&key, value.clone()).unwrap();
    assert_eq!(dictionary.dict_get(&key).unwrap(), Some(&value));
    assert!(dictionary.root_cell().is_some());
    assert_eq!(dictionary.dict_del(&key).unwrap(), Some(value));
    assert!(dictionary.root_cell().is_none());
}

// ADR-0036 opcode wiring tests: ENDC commits the builder's exact bit length,
// STBYTES advances it at bit granularity, and slice readers honor it.

/// PUSHINT bytecode pushing unsigned `value` (fits in one byte here).
fn pushint_u8(value: u8) -> Vec<u8> {
    let mut code = vec![0x08, 0x00];
    code.extend_from_slice(&[0u8; 31]);
    code.push(value);
    code
}

/// STBITS bytecode: width (big-endian u16), unsigned flavor.
fn stbits(width: u16) -> Vec<u8> {
    vec![0x42, (width >> 8) as u8, (width & 0xFF) as u8, 0x00]
}

fn run_bytecode(
    code: Vec<u8>,
    stack: Vec<StackValue>,
    message_body: Vec<u8>,
) -> (ExecutionResult, Vec<StackValue>) {
    let mut interpreter = Interpreter::new(
        Cell::new(code, vec![]).unwrap(),
        Cell::new(vec![], vec![]).unwrap(),
        dummy_message(),
        dummy_context(100_000),
    );
    interpreter.stack = stack;
    interpreter.message_body = message_body;
    let res = interpreter.run();
    (res, interpreter.stack)
}

fn expect_success(res: &ExecutionResult) {
    assert!(
        matches!(res, ExecutionResult::Success { .. }),
        "expected success, got {:?}",
        res
    );
}

#[test]
fn test_endc_flags_partial_bit_cell() {
    // NEWC, PUSHINT 5, STBITS 3, ENDC — stores 3-bit `101`.
    let mut code = vec![0x40];
    code.extend(pushint_u8(5));
    code.extend(stbits(3));
    code.push(0x41); // ENDC
    let (res, stack) = run_bytecode(code, vec![], vec![]);
    expect_success(&res);
    match stack.last() {
        Some(StackValue::Cell(cell)) => {
            assert!(cell.is_bit_granular(), "partial-bit cell must be flagged");
            assert_eq!(cell.data_bytes(), &[0xB0]);
            assert_eq!(cell.bit_len(), 3);
        }
        other => panic!("expected Cell on stack, got {:?}", other),
    }
}

#[test]
fn test_endc_leaves_byte_aligned_cell_unflagged() {
    // NEWC, PUSHINT 0xA0-ish via STBITS 8, ENDC — byte-aligned stays unflagged.
    let mut code = vec![0x40];
    code.extend(pushint_u8(0xA0));
    code.extend(stbits(8));
    code.push(0x41);
    let (res, stack) = run_bytecode(code, vec![], vec![]);
    expect_success(&res);
    match stack.last() {
        Some(StackValue::Cell(cell)) => {
            assert!(!cell.is_bit_granular());
            assert_eq!(cell.data_bytes(), &[0xA0]);
            assert_eq!(cell.bit_len(), 8);
        }
        other => panic!("expected Cell on stack, got {:?}", other),
    }
}

#[test]
fn test_stbytes_advances_bit_len_at_bit_granularity() {
    // Bytecode: NEWC, PUSHINT 5, STBITS 3, MSGBODY, STBYTES, ENDC.
    // The message body byte's bits land at positions 3..11:
    // cell = [0xBF, 0xF0], 11 bits.
    let mut code = vec![0x40];
    code.extend(pushint_u8(5));
    code.extend(stbits(3));
    code.push(0x82); // MSGBODY -> Bytes([0xFF])
    code.push(0x44); // STBYTES
    code.push(0x41); // ENDC
    let (res, stack) = run_bytecode(code, vec![], vec![0xFF]);
    expect_success(&res);
    match stack.last() {
        Some(StackValue::Cell(cell)) => {
            assert!(cell.is_bit_granular());
            assert_eq!(cell.data_bytes(), &[0xBF, 0xF0]);
            assert_eq!(cell.bit_len(), 11);
        }
        other => panic!("expected Cell on stack, got {:?}", other),
    }
}

#[test]
fn test_sbits_reports_bit_len_not_byte_len() {
    // NEWC, PUSHINT 5, STBITS 3, ENDC, CTOS, SBITS -> 3, not 8.
    let mut code = vec![0x40];
    code.extend(pushint_u8(5));
    code.extend(stbits(3));
    code.push(0x41); // ENDC
    code.push(0x45); // CTOS
    code.push(0x4B); // SBITS
    let (res, stack) = run_bytecode(code, vec![], vec![]);
    expect_success(&res);
    match stack.last() {
        Some(StackValue::Integer(n)) => assert_eq!(*n, Int257::from_u64(3)),
        other => panic!("expected Integer on stack, got {:?}", other),
    }
}

#[test]
fn test_ldu_cannot_read_completion_tag_as_data() {
    // Flagged 3-bit cell: LDU 8 must fail — only 3 bits remain.
    let mut code = vec![0x40];
    code.extend(pushint_u8(5));
    code.extend(stbits(3));
    code.push(0x41); // ENDC
    code.push(0x45); // CTOS
    code.extend(vec![0x46, 0x00, 0x08]); // LDU 8
    let (res, _) = run_bytecode(code, vec![], vec![]);
    match res {
        ExecutionResult::Exception { kind, .. } => {
            assert_eq!(kind, ExceptionKind::MalformedCell)
        }
        other => panic!("expected MalformedCell, got {:?}", other),
    }
}

#[test]
fn test_partial_bits_round_trip_through_slice() {
    // NEWC, PUSHINT 5, STBITS 3, ENDC, CTOS, LDU 3 -> 5.
    let mut code = vec![0x40];
    code.extend(pushint_u8(5));
    code.extend(stbits(3));
    code.push(0x41); // ENDC
    code.push(0x45); // CTOS
    code.extend(vec![0x46, 0x00, 0x03]); // LDU 3
    let (res, stack) = run_bytecode(code, vec![], vec![]);
    expect_success(&res);
    match stack.last() {
        Some(StackValue::Integer(n)) => assert_eq!(*n, Int257::from_u64(5)),
        other => panic!("expected Integer(5) on stack, got {:?}", other),
    }
}
