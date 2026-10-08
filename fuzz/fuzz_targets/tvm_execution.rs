#![no_main]

use libfuzzer_sys::fuzz_target;
use onx_data_structures::{AccountId, FullAddress, Message, MessageType, WorkchainIdent};
use onx_execution::{execute, ExecutionContext};
use onx_primitives::{Uint128, Uint256, Uint64};
use onx_state_model::{Cell, MAX_CELL_DATA_BYTES};

fn fuzz_message() -> Message {
    let address = FullAddress::new(WorkchainIdent::BASIC, AccountId::from_bytes([0; 32]));
    Message {
        msg_type: MessageType::Internal,
        src_address: address,
        dest_address: address,
        amount_nanos: Uint128::from(0u128),
        extra_currencies: Vec::new(),
        created_lt: Uint64::from(0u64),
        body_cell_hash: Uint256([0; 32]),
    }
}

// Bounded gas and cell sizes keep arbitrary opcode streams inexpensive while
// exercising decoder and stack-boundary failure paths.
fuzz_target!(|data: &[u8]| {
    let code_len = data.len().min(MAX_CELL_DATA_BYTES);
    let code = Cell::new(data[..code_len].to_vec(), Vec::new()).expect("bounded code cell");
    let data_cell = Cell::new(Vec::new(), Vec::new()).expect("empty cell is valid");
    let context = ExecutionContext {
        gen_utime: 0,
        start_lt: 0,
        end_lt: 0,
        gas_limit: 1_024,
        // Fixed test chain id (ADR-0038); CHKSIGNU only needs it to be stable.
        chain_id: [0x43; 32],
    };
    let _ = execute(code, data_cell, fuzz_message(), context);
});
