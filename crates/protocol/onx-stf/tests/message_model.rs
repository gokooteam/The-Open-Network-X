//! Message-model acceptance probes through `propose_block`/`apply_block`.
//!
//! End-to-end checks of the async message semantics (ADR-0001, ADR-0003,
//! ADR-0004, ADR-0005, ADR-0006):
//! - a transfer to a frozen account bounces and the sender is refunded
//!   minus fees,
//! - per-(sender, receiver)-pair FIFO delivery ordering under interleaving,
//! - chain-ID binding rejects cross-chain messages,
//! - key-derived accounts: delivery creates a keyless account, and the
//!   first spend reveals the key (ADR-0006).
//!
//! Double-delivery protection is covered by the unit test
//! `redelivered_internal_message_is_rejected` in `src/stf.rs` and is not
//! duplicated here.

use onx_data_structures::{AccountId, ShardIdent, WorkchainIdent};
use onx_primitives::{domain_hash, DomainTag, SecretKey};
use onx_state_model::{
    derive_account_id, AccountState, GenesisDocument, GenesisValidator, StorageStat,
};
use onx_stf::{
    apply_block, derive_address, propose_block, ExternalMessage, MsgKind, Receipts, State, StfError,
};
use std::collections::BTreeMap;

/// Domain tag for this suite's deterministic signing keys (test-only).
const TEST_KEY_V1: DomainTag = DomainTag::from_ascii("ONX_TEST_MSG_MODEL_V1");

fn secret(name: &str) -> SecretKey {
    let seed = domain_hash(&TEST_KEY_V1, name.as_bytes());
    SecretKey::from_seed(&seed).expect("domain hash output is a valid seed")
}

fn keyed(balance: u128, key: &SecretKey) -> AccountState {
    AccountState::Active {
        balance_nanos: balance,
        last_trans_lt: 0,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 0,
            byte_count: 0,
        },
        pubkey: key.public_key().encode(),
        nonce: 0,
    }
}

/// Genesis with three funded keyed accounts. Returns
/// (state, alice, bob, carol, collector).
fn genesis() -> (State, AccountId, AccountId, AccountId, AccountId) {
    let workchain = WorkchainIdent::new(0);
    let shard = ShardIdent::root(workchain);
    let validators = vec![GenesisValidator {
        pubkey: [9u8; 32],
        stake: 1,
    }];
    let alice = derive_account_id("model-alice");
    let bob = derive_account_id("model-bob");
    let carol = derive_account_id("model-carol");
    let collector = derive_account_id("model-collector");
    let mut accounts = BTreeMap::new();
    for (id, balance) in [
        (alice, 10_000_000u128),
        (bob, 10_000_000u128),
        (carol, 10_000_000u128),
    ] {
        accounts.insert(id, keyed(balance, &secret(&format!("{id:?}"))));
    }
    let doc = GenesisDocument::new(workchain, shard, validators, accounts).unwrap();
    (State::from_genesis(&doc), alice, bob, carol, collector)
}

fn transfer(
    state: &State,
    from: AccountId,
    nonce: u64,
    to: AccountId,
    amount: u128,
    fee: u128,
    key: &SecretKey,
) -> ExternalMessage {
    ExternalMessage::new_signed(
        state.chain_id,
        MsgKind::Transfer,
        from,
        nonce,
        to,
        amount,
        fee,
        Vec::new(),
        [0u8; 32],
        key,
    )
}

fn balance_of(state: &State, id: &AccountId) -> u128 {
    state.tree.get(id).map(|s| s.balance_nanos()).unwrap_or(0)
}

fn apply_msgs(
    state: &State,
    msgs: Vec<ExternalMessage>,
    collector: AccountId,
    lt: u64,
) -> Result<(State, Receipts), StfError> {
    let block = propose_block(state, msgs, lt, collector)?;
    apply_block(state, &block)
}

#[test]
fn bounce_to_frozen_refunds_sender_minus_fees() {
    let (mut state, alice, _bob, _carol, collector) = genesis();
    let frozen = derive_account_id("model-frozen");
    state
        .tree
        .insert(
            frozen,
            AccountState::Frozen {
                balance_nanos: 5_000,
                last_trans_lt: 0,
                storage_hash: [0u8; 32],
            },
        )
        .unwrap();

    let alice_key = secret(&format!("{alice:?}"));
    // Fee 100 -> 50 burned, 50 to the collector.
    let msg = transfer(&state, alice, 0, frozen, 1_000, 100, &alice_key);
    let (next, receipts) = apply_msgs(&state, vec![msg], collector, 1)
        .expect("a block containing a bounced delivery is still valid");

    assert_eq!(receipts.0.len(), 1);
    let r = &receipts.0[0];
    assert_eq!(r.sender, alice);
    assert_eq!(r.fee_burned_nanos, 50);
    assert_eq!(r.fee_validator_nanos, 50);
    // Two deliveries: the original (bounced off the frozen account) and
    // the bounce carrying the value back to Alice.
    assert_eq!(r.deliveries.len(), 2);
    let first = &r.deliveries[0];
    assert!(first.bounced);
    assert_eq!(first.src, alice);
    assert_eq!(first.dest, frozen);
    assert_eq!(first.value_nanos, 1_000);
    let bounce = &r.deliveries[1];
    assert!(!bounce.bounced);
    assert_eq!(bounce.src, frozen);
    assert_eq!(bounce.dest, alice);
    assert_eq!(bounce.value_nanos, 1_000);

    // Alice: debited 1_000 + 100 at the wallet, credited 1_000 by the
    // bounce. Net: initial - fee.
    assert_eq!(balance_of(&next, &alice), 10_000_000 - 100);
    // The frozen account is untouched.
    assert_eq!(balance_of(&next, &frozen), 5_000);
    assert!(matches!(
        next.tree.get(&frozen),
        Some(AccountState::Frozen { .. })
    ));
    // Collector received its half of the fee.
    assert_eq!(balance_of(&next, &collector), 50);
}

#[test]
fn per_pair_fifo_ordering_under_interleave() {
    let (state, alice, bob, carol, collector) = genesis();
    let alice_key = secret(&format!("{alice:?}"));
    let bob_key = secret(&format!("{bob:?}"));

    // Interleaved submissions: A->B, A->C, A->B, B->A.
    let msgs = vec![
        transfer(&state, alice, 0, bob, 100, 10, &alice_key),
        transfer(&state, alice, 1, carol, 200, 10, &alice_key),
        transfer(&state, alice, 2, bob, 300, 10, &alice_key),
        transfer(&state, bob, 0, alice, 400, 10, &bob_key),
    ];
    let (next, receipts) = apply_msgs(&state, msgs, collector, 1).expect("must apply");
    assert_eq!(receipts.0.len(), 4);

    // No bounces: every external produced exactly one delivery, in
    // submission order, so the per-pair sequences are FIFO trivially —
    // assert it explicitly rather than assuming it.
    let seq: Vec<(AccountId, AccountId, u128)> = receipts
        .0
        .iter()
        .flat_map(|r| {
            assert_eq!(r.deliveries.len(), 1, "no bounces expected");
            assert!(!r.deliveries[0].bounced);
            r.deliveries.iter().map(|d| (d.src, d.dest, d.value_nanos))
        })
        .collect();
    assert_eq!(
        seq,
        vec![
            (alice, bob, 100),
            (alice, carol, 200),
            (alice, bob, 300),
            (bob, alice, 400),
        ]
    );
    // Per-(sender, receiver)-pair FIFO: the (A,B) deliveries arrive in
    // submission order (100 then 300), and delivery IDs are all distinct.
    let ab: Vec<u128> = seq
        .iter()
        .filter(|(s, d, _)| *s == alice && *d == bob)
        .map(|(_, _, v)| *v)
        .collect();
    assert_eq!(ab, vec![100, 300]);
    let mut ids: Vec<[u8; 32]> = receipts
        .0
        .iter()
        .flat_map(|r| r.deliveries.iter().map(|d| d.msg_id))
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4, "delivery IDs must be unique");

    // Final balances: each account nets its sends and receives.
    // Alice: 10M - (100+10) - (200+10) - (300+10) + 400 = 9_999_770.
    assert_eq!(balance_of(&next, &alice), 10_000_000 - 630 + 400);
    assert_eq!(balance_of(&next, &bob), 10_000_000 + 100 + 300 - 410);
    assert_eq!(balance_of(&next, &carol), 10_000_000 + 200);
    // Four fees of 10: 20 burned, 20 to the collector.
    assert_eq!(balance_of(&next, &collector), 20);
}

#[test]
fn wrong_chain_id_rejected() {
    let (state, alice, bob, _carol, collector) = genesis();
    let alice_key = secret(&format!("{alice:?}"));
    // A message minted for a different chain: the chain check fires at
    // the wallet handler before signature verification, so the whole
    // block is invalid.
    let mut msg = transfer(&state, alice, 0, bob, 1_000, 10, &alice_key);
    msg.chain_id = [0xEE; 32];
    let err = apply_msgs(&state, vec![msg], collector, 1).expect_err("must fail");
    assert!(
        matches!(err, StfError::WrongChainId { .. }),
        "expected WrongChainId, got {err}"
    );
    // And signing against the wrong chain from the start is the same:
    // the signature itself binds the chain ID, so a message signed with
    // a foreign chain_id can never pass the chain check.
    let foreign = ExternalMessage::new_signed(
        [0xEE; 32],
        MsgKind::Transfer,
        alice,
        0,
        bob,
        1_000,
        10,
        Vec::new(),
        [0u8; 32],
        &alice_key,
    );
    let err = apply_msgs(&state, vec![foreign], collector, 1).expect_err("must fail");
    assert!(
        matches!(err, StfError::WrongChainId { .. }),
        "expected WrongChainId, got {err}"
    );
}

#[test]
fn key_derived_account_receives_then_spends() {
    let (state, alice, bob, _carol, collector) = genesis();
    let alice_key = secret(&format!("{alice:?}"));

    // A brand-new keypair; the account address commits to the pubkey.
    let new_key = secret("model-new-key");
    let new_pubkey = new_key.public_key().encode();
    let addr = derive_address(&new_pubkey);

    // Fund it via a plain transfer. Delivery to Uninitialized with an
    // empty payload creates a KEYLESS Active account.
    let fund = transfer(&state, alice, 0, addr, 50_000, 10, &alice_key);
    let (state, receipts) = apply_msgs(&state, vec![fund], collector, 1).expect("fund must apply");
    assert!(!receipts.0[0].deliveries[0].bounced);
    let created = state.tree.get(&addr).expect("account must exist");
    match created {
        AccountState::Active {
            balance_nanos,
            pubkey,
            nonce,
            ..
        } => {
            assert_eq!(*balance_nanos, 50_000);
            assert_eq!(*pubkey, [0u8; 32], "born keyless");
            assert_eq!(*nonce, 0);
        }
        other => panic!("expected keyless Active account, got {other:?}"),
    }

    // Spend from it, revealing the pubkey in the message. The reveal is
    // stored: the account is keyed from now on.
    let spend = ExternalMessage::new_signed(
        state.chain_id,
        MsgKind::Transfer,
        addr,
        0,
        bob,
        20_000,
        10,
        Vec::new(),
        new_pubkey,
        &new_key,
    );
    let (state, receipts) =
        apply_msgs(&state, vec![spend], collector, 2).expect("first spend with reveal must apply");
    assert!(!receipts.0[0].deliveries[0].bounced);
    match state.tree.get(&addr).expect("account must exist") {
        AccountState::Active {
            balance_nanos,
            pubkey,
            nonce,
            ..
        } => {
            assert_eq!(*pubkey, new_pubkey, "revealed key is now stored");
            assert_eq!(*nonce, 1);
            // 50_000 - 20_000 - 10 (fee).
            assert_eq!(*balance_nanos, 29_990);
        }
        other => panic!("expected keyed Active account, got {other:?}"),
    }
    assert_eq!(balance_of(&state, &bob), 10_000_000 + 20_000);

    // A second spend needs no reveal anymore — but a WRONG reveal is
    // rejected, even when it is signed by the wrong key's owner.
    let wrong_key = secret("model-wrong-key");
    let bad_spend = ExternalMessage::new_signed(
        state.chain_id,
        MsgKind::Transfer,
        addr,
        1,
        bob,
        1_000,
        10,
        Vec::new(),
        new_pubkey, // correct pubkey, but the account is already keyed: any reveal is a fault
        &wrong_key,
    );
    let err = apply_msgs(&state, vec![bad_spend], collector, 3).expect_err("must fail");
    assert!(
        matches!(err, StfError::UnexpectedPubkeyReveal(_)),
        "expected UnexpectedPubkeyReveal, got {err}"
    );

    // And a keyless key-derived account revealing a pubkey that does NOT
    // derive to its address is rejected with AddressKeyMismatch.
    let other_key = secret("model-other-key");
    let other_addr = derive_address(&other_key.public_key().encode());
    let fund2 = transfer(&state, alice, 1, other_addr, 50_000, 10, &alice_key);
    let (state, _) = apply_msgs(&state, vec![fund2], collector, 3).expect("fund must apply");
    let evil = ExternalMessage::new_signed(
        state.chain_id,
        MsgKind::Transfer,
        other_addr,
        0,
        bob,
        1_000,
        10,
        Vec::new(),
        new_pubkey, // derives to `addr`, not `other_addr`
        &new_key,
    );
    let err = apply_msgs(&state, vec![evil], collector, 4).expect_err("must fail");
    assert!(
        matches!(err, StfError::AddressKeyMismatch { .. }),
        "expected AddressKeyMismatch, got {err}"
    );
    // The failed spend left the account untouched: still keyless, balance intact.
    match state.tree.get(&other_addr).expect("account must exist") {
        AccountState::Active {
            balance_nanos,
            pubkey,
            nonce,
            ..
        } => {
            assert_eq!(*balance_nanos, 50_000);
            assert_eq!(*pubkey, [0u8; 32]);
            assert_eq!(*nonce, 0);
        }
        other => panic!("expected untouched keyless account, got {other:?}"),
    }
}
