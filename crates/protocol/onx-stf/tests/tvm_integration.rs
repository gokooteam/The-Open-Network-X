//! TVM integration tests: contract-call messages executing contract
//! code through the STF.
//!
//! A minimal counter contract (increment-only) is exercised end-to-end:
//! genesis installs the contract, signed `ContractCall` external messages
//! run it on delivery, and the persistent data cell advances
//! deterministically. The bounce model applies to execution too:
//! a VM failure (bad code, exception) or a call to a codeless account
//! BOUNCES the value back to the sender (fees already taken) instead of
//! invalidating the block — the sender is refunded, the chain moves on.

use onx_data_structures::AccountId;
use onx_execution::{ExecutionContext, ExecutionResult, Interpreter, StackValue};
use onx_primitives::SecretKey;
use onx_state_model::{AccountState, Cell, ShardStateTree, StorageStat};
use onx_stf::{
    apply_block, propose_block, ExternalMessage, MsgKind, Receipts, State, StfError, GAS_PER_NANO,
};

// ---------------------------------------------------------------------------
// Counter contract fixtures.
// ---------------------------------------------------------------------------

/// TVM bytecode for the counter contract. Calling convention: the
/// contract's persistent data cell is on the stack at entry.
///
/// ```text
/// CTOS            ; cell -> slice
/// LDU 64          ; slice -> slice, counter
/// SWAP            ; -> counter, slice
/// DROP            ; -> counter
/// PUSHINT 1       ; -> counter, 1
/// ADD 128         ; -> counter+1
/// NEWC            ; -> counter+1, builder
/// SWAP            ; -> builder, counter+1
/// STBITS 64       ; -> builder
/// ENDC            ; -> new_data_cell
/// SETDATA         ; installs the new persistent data cell
/// ```
fn counter_code() -> Cell {
    let mut code = vec![
        0x45, // CTOS
        0x46, 0x00, 0x40, // LDU 64
        0x03, // SWAP
        0x01, // DROP
        0x08, 0x00, // PUSHINT (signed=0)
    ];
    let mut one = [0u8; 32];
    one[31] = 1;
    code.extend_from_slice(&one);
    code.extend_from_slice(&[
        0x10, 0x00, 0x80, 0x00, // ADD width=128 flavor=0
        0x40, // NEWC
        0x03, // SWAP
        0x42, 0x00, 0x40, 0x00, // STBITS width=64 signed=0
        0x41, // ENDC
        0x4D, // SETDATA
    ]);
    Cell::new(code, vec![]).unwrap()
}

/// Counter data cell: 8-byte big-endian u64.
fn counter_data(value: u64) -> Cell {
    Cell::new(value.to_be_bytes().to_vec(), vec![]).unwrap()
}

/// Read the counter value back out of a data cell.
fn read_counter(cell: &Cell) -> u64 {
    let bytes = cell.data_bytes();
    assert!(bytes.len() >= 8, "counter cell too short");
    u64::from_be_bytes(bytes[..8].try_into().unwrap())
}

fn test_secret() -> SecretKey {
    SecretKey::from_seed(&[0xA1; 32]).expect("fixed test seed is valid")
}

/// Genesis state with a funded keyed sender and a counter contract.
/// Returns (state, sender, contract, collector, sender_secret).
fn contract_genesis() -> (State, AccountId, AccountId, AccountId, SecretKey) {
    let sender = AccountId::from_bytes([0xA1; 32]);
    let contract = AccountId::from_bytes([0xC0; 32]);
    let collector = AccountId::from_bytes([0xCC; 32]);
    let secret = test_secret();
    let code = counter_code();
    let data = counter_data(0);

    let mut tree = ShardStateTree::new();
    tree.insert(
        sender,
        AccountState::Active {
            balance_nanos: 10_000_000,
            last_trans_lt: 0,
            code: None,
            data: None,
            storage_stat: StorageStat {
                cell_count: 0,
                byte_count: 0,
            },
            pubkey: secret.public_key().encode(),
            nonce: 0,
        },
    )
    .unwrap();
    tree.insert(
        contract,
        AccountState::Active {
            balance_nanos: 1_000_000,
            last_trans_lt: 0,
            code: Some(code.clone()),
            data: Some(data.clone()),
            storage_stat: StorageStat {
                cell_count: 2,
                byte_count: (code.to_bytes().len() + data.to_bytes().len()) as u64,
            },
            // Contracts are keyless: nobody spends FROM the contract in
            // this milestone; it only receives and executes.
            pubkey: [0u8; 32],
            nonce: 0,
        },
    )
    .unwrap();
    let state = State {
        tree,
        workchain: 0,
        chain_id: [0x99; 32],
        seqno: 0,
        last_lt: 0,
        last_hash: [0x99; 32],
    };
    (state, sender, contract, collector, secret)
}

fn contract_data_of(state: &State, contract: &AccountId) -> Cell {
    match state.tree.get(contract).expect("contract exists") {
        AccountState::Active {
            data: Some(data), ..
        } => data.clone(),
        other => panic!("contract account malformed: {other:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn call_msg(
    chain_id: [u8; 32],
    sender: AccountId,
    nonce: u64,
    contract: AccountId,
    amount: u128,
    fee: u128,
    payload: Vec<u8>,
    secret: &SecretKey,
) -> ExternalMessage {
    ExternalMessage::new_signed(
        chain_id,
        MsgKind::ContractCall,
        sender,
        nonce,
        contract,
        amount,
        fee,
        payload,
        [0u8; 32],
        secret,
    )
}

fn apply_one(
    state: &State,
    msg: ExternalMessage,
    collector: AccountId,
    lt: u64,
) -> Result<(State, Receipts), StfError> {
    let block = propose_block(state, vec![msg], lt, collector)?;
    let (next, receipts) = apply_block(state, &block)?;
    Ok((next, receipts))
}

fn balance_of(state: &State, id: &AccountId) -> u128 {
    state.tree.get(id).map(|s| s.balance_nanos()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Bytecode sanity: the counter contract in isolation.
// ---------------------------------------------------------------------------

#[test]
fn tvm_counter_bytecode_increments() {
    // Drive the VM directly with the STF's calling convention (data cell
    // on the stack at entry) to prove the bytecode is correct before the
    // STF ever sees it.
    let code = counter_code();
    let data = counter_data(41);
    let ctx = ExecutionContext {
        gen_utime: 1,
        start_lt: 1,
        end_lt: 1,
        gas_limit: 1_000_000,
    };
    let mut interp = Interpreter::new(code, data.clone(), dummy_message(), ctx);
    interp.stack.push(StackValue::Cell(data));
    match interp.run() {
        ExecutionResult::Success { new_data, .. } => {
            assert_eq!(read_counter(&new_data), 42);
        }
        ExecutionResult::Exception { kind, .. } => panic!("counter raised {kind}"),
    }
}

fn dummy_message() -> onx_data_structures::Message {
    use onx_data_structures::{FullAddress, MessageType, WorkchainIdent};
    use onx_primitives::{Uint128, Uint256, Uint64};
    let addr = FullAddress::new(WorkchainIdent::BASIC, AccountId::from_bytes([0x01; 32]));
    onx_data_structures::Message {
        msg_type: MessageType::Internal,
        src_address: addr,
        dest_address: addr,
        amount_nanos: Uint128(0),
        extra_currencies: vec![],
        created_lt: Uint64(1),
        body_cell_hash: Uint256([0u8; 32]),
    }
}

// ---------------------------------------------------------------------------
// STF integration.
// ---------------------------------------------------------------------------

#[test]
fn tvm_contract_call_increments_counter() {
    let (state, sender, contract, collector, secret) = contract_genesis();
    assert_eq!(read_counter(&contract_data_of(&state, &contract)), 0);

    let msg = call_msg(
        state.chain_id,
        sender,
        0,
        contract,
        1_000,   // amount
        100_000, // fee -> gas_limit = 100_000 * GAS_PER_NANO
        b"increment".to_vec(),
        &secret,
    );
    let (next, receipts) = apply_one(&state, msg, collector, 1).expect("contract call must apply");
    assert_eq!(read_counter(&contract_data_of(&next, &contract)), 1);

    // Balances moved exactly like a transfer: fee split 50/50.
    let sender_after = match next.tree.get(&sender).unwrap() {
        AccountState::Active {
            balance_nanos,
            nonce,
            ..
        } => (*balance_nanos, *nonce),
        _ => panic!("sender gone"),
    };
    assert_eq!(sender_after, (10_000_000 - 101_000, 1));
    // Gas was consumed and reported on the delivery receipt.
    assert_eq!(receipts.0.len(), 1);
    let d = &receipts.0[0].deliveries[0];
    assert!(!d.bounced);
    assert!(d.gas_used > 0);
    assert!(d.gas_used <= 100_000 * GAS_PER_NANO);
}

#[test]
fn tvm_contract_state_advances_across_blocks() {
    // Determinism across blocks: three calls -> counter = 3, and the
    // data cell from block N is the input to block N+1.
    let (mut state, sender, contract, collector, secret) = contract_genesis();
    for i in 0..3u64 {
        let msg = call_msg(
            state.chain_id,
            sender,
            i,
            contract,
            1_000,
            100_000,
            b"increment".to_vec(),
            &secret,
        );
        let (next, _) = apply_one(&state, msg, collector, i + 1).expect("call must apply");
        state = next;
        assert_eq!(read_counter(&contract_data_of(&state, &contract)), i + 1);
    }
}

#[test]
fn tvm_transfer_to_contract_does_not_execute() {
    // A plain Transfer to a contract account is just a value transfer:
    // no VM execution, counter untouched, gas_used == 0.
    let (state, sender, contract, collector, secret) = contract_genesis();
    let msg = ExternalMessage::new_signed(
        state.chain_id,
        MsgKind::Transfer,
        sender,
        0,
        contract,
        5_000,
        1_000,
        Vec::new(),
        [0u8; 32],
        &secret,
    );
    let (next, receipts) = apply_one(&state, msg, collector, 1).expect("transfer must apply");
    assert_eq!(read_counter(&contract_data_of(&next, &contract)), 0);
    let contract_after = match next.tree.get(&contract).unwrap() {
        AccountState::Active { balance_nanos, .. } => *balance_nanos,
        _ => panic!("contract gone"),
    };
    assert_eq!(contract_after, 1_000_000 + 5_000);
    assert_eq!(receipts.0[0].deliveries[0].gas_used, 0);
}

#[test]
fn tvm_contract_call_to_codeless_account_bounces() {
    // ContractCall to an account with no code: the block is still valid —
    // the value bounces back to the sender (fees already taken).
    let (state, sender, _contract, collector, secret) = contract_genesis();
    let plain = AccountId::from_bytes([0xB0; 32]);

    // Uninitialized + payload -> bounce (calls never create accounts).
    let msg = call_msg(
        state.chain_id,
        sender,
        0,
        plain,
        1_000,
        100_000,
        b"increment".to_vec(),
        &secret,
    );
    let (next, receipts) =
        apply_one(&state, msg, collector, 1).expect("block with a bounced call is still valid");
    let r = &receipts.0[0];
    assert_eq!(r.deliveries.len(), 2);
    assert!(r.deliveries[0].bounced, "the call itself must bounce");
    assert_eq!(r.deliveries[0].gas_used, 0);
    let bounce = &r.deliveries[1];
    assert!(!bounce.bounced);
    assert_eq!(bounce.dest, sender);
    assert_eq!(bounce.value_nanos, 1_000);
    // Sender: debited 1_000 + 100_000 at the wallet, refunded 1_000.
    assert_eq!(balance_of(&next, &sender), 10_000_000 - 100_000);

    // Fund `plain` as a codeless Active account, then call it: same bounce.
    let fund = ExternalMessage::new_signed(
        state.chain_id,
        MsgKind::Transfer,
        sender,
        0,
        plain,
        1_000,
        0,
        Vec::new(),
        [0u8; 32],
        &secret,
    );
    let (state, _) = apply_one(&state, fund, collector, 1).unwrap();
    let msg = call_msg(
        state.chain_id,
        sender,
        1,
        plain,
        1_000,
        100_000,
        b"increment".to_vec(),
        &secret,
    );
    let (next, receipts) =
        apply_one(&state, msg, collector, 2).expect("block with a bounced call is still valid");
    assert!(receipts.0[0].deliveries[0].bounced);
    // Sender: 10M - 1_000 (funding transfer, fee 0) - 101_000 (call debit)
    // + 1_000 (bounce refund) = 9_899_000.
    assert_eq!(balance_of(&next, &sender), 9_899_000);
}

#[test]
fn tvm_vm_exception_bounces() {
    // A contract whose code immediately throws: the delivery bounces and
    // the sender is refunded minus fees — revert-by-construction, the block
    // stays valid (no VmExecutionFailed error anymore).
    let (mut state, sender, _contract, collector, secret) = contract_genesis();
    let thrower = AccountId::from_bytes([0xD0; 32]);
    // Code: THROW IntegerOverflow (0x77 0x00).
    let throw_code = Cell::new(vec![0x77, 0x00], vec![]).unwrap();
    state
        .tree
        .insert(
            thrower,
            AccountState::Active {
                balance_nanos: 0,
                last_trans_lt: 0,
                code: Some(throw_code),
                data: Some(Cell::new(vec![], vec![]).unwrap()),
                storage_stat: StorageStat {
                    cell_count: 2,
                    byte_count: 0,
                },
                pubkey: [0u8; 32],
                nonce: 0,
            },
        )
        .unwrap();
    let msg = call_msg(
        state.chain_id,
        sender,
        0,
        thrower,
        1_000,
        100_000,
        b"boom".to_vec(),
        &secret,
    );
    let (next, receipts) = apply_one(&state, msg, collector, 1)
        .expect("block with a throwing contract is still valid");
    let r = &receipts.0[0];
    assert!(
        r.deliveries[0].bounced,
        "the exception delivery must bounce"
    );
    assert_eq!(r.deliveries[0].gas_used, 0);
    // Sender: debited 1_000 + 100_000 at the wallet, refunded 1_000.
    assert_eq!(balance_of(&next, &sender), 10_000_000 - 100_000);
    // The thrower's account is untouched (no value credit, no data change).
    assert_eq!(balance_of(&next, &thrower), 0);
}

#[test]
fn tvm_zero_fee_contract_call_rejected() {
    // Gas is bought with the fee: a zero-fee contract call cannot buy any
    // gas and is a sender-side fault — rejected at the wallet handler
    // (fail-closed), not bounced.
    let (state, sender, contract, collector, secret) = contract_genesis();
    let msg = call_msg(
        state.chain_id,
        sender,
        0,
        contract,
        1_000,
        0, // zero fee
        b"increment".to_vec(),
        &secret,
    );
    let err = propose_block(&state, vec![msg], 1, collector).expect_err("must fail");
    assert!(
        matches!(err, StfError::ZeroFeeContractCall),
        "unexpected: {err}"
    );
    // State untouched: propose failed, nothing was written.
    assert_eq!(balance_of(&state, &sender), 10_000_000);
}

#[test]
fn tvm_bad_msg_kind_rejected_at_parse() {
    let secret = test_secret();
    let msg = ExternalMessage::new_signed(
        [0x99; 32],
        MsgKind::Transfer,
        AccountId::from_bytes([0xA1; 32]),
        0,
        AccountId::from_bytes([0xC0; 32]),
        1_000,
        0,
        Vec::new(),
        [0u8; 32],
        &secret,
    );
    let mut bytes = msg.to_bytes();
    // Kind byte sits at offset 72 in the message-era layout (right
    // after the 8-byte nonce field at 64..72).
    bytes[72] = 0xFF;
    assert!(matches!(
        ExternalMessage::from_bytes(&bytes),
        Err(StfError::BadMsgKind(0xFF))
    ));
    // Round-trip still fine for the valid encoding.
    assert_eq!(ExternalMessage::from_bytes(&msg.to_bytes()).unwrap(), msg);
}

#[test]
fn tvm_message_too_large_rejected_at_parse() {
    // A body claiming msg_len > MAX_MESSAGE_BYTES fails closed.
    let secret = test_secret();
    let msg = ExternalMessage::new_signed(
        [0x99; 32],
        MsgKind::Transfer,
        AccountId::from_bytes([0xA1; 32]),
        0,
        AccountId::from_bytes([0xC0; 32]),
        1_000,
        0,
        Vec::new(),
        [0u8; 32],
        &secret,
    );
    let mut body = msg.body_bytes();
    // Overwrite msg_len (offset 137..141) with u32::MAX.
    body[137..141].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(matches!(
        ExternalMessage::body_from_bytes(&body),
        Err(StfError::MessageTooLarge { .. })
    ));
}

#[test]
fn tvm_contract_call_is_deterministic_across_processes() {
    // The same contract call applied twice from the same genesis yields
    // byte-identical state roots (in-process determinism; the cross-process
    // replay check lives in the tooling acceptance suite).
    let (state, sender, contract, collector, secret) = contract_genesis();
    let mk = |nonce: u64| {
        call_msg(
            state.chain_id,
            sender,
            nonce,
            contract,
            1_000,
            100_000,
            b"increment".to_vec(),
            &secret,
        )
    };
    let run_once = || {
        let mut s = state.clone();
        for i in 0..2u64 {
            let block = propose_block(&s, vec![mk(i)], i + 1, collector).unwrap();
            let (next, _) = apply_block(&s, &block).unwrap();
            s = next;
        }
        s.tree.state_root_hash().unwrap()
    };
    assert_eq!(run_once(), run_once());
    // And the tree itself (not just the root) is identical.
    let mut a = state.clone();
    let mut b = state.clone();
    for i in 0..2u64 {
        for s in [&mut a, &mut b] {
            let block = propose_block(s, vec![mk(i)], i + 1, collector).unwrap();
            let (next, _) = apply_block(s, &block).unwrap();
            *s = next;
        }
    }
    assert_eq!(a.tree.accounts(), b.tree.accounts());
}
