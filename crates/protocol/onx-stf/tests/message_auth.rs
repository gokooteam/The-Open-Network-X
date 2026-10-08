//! Message authorization regression tests: Ed25519 signatures, chain-ID
//! binding, per-account nonces, and key-reveal authorization (ADR-0005,
//! ADR-0006, ADR-0007).
//!
//! MIGRATION NOTE: this suite is the message-model successor of the old
//! `tx_auth.rs` (V1/V2/V3 transaction era). The authorization model changed
//! in three ways, and the tests changed with it:
//! - `Transaction::new_signed(from, to, amount, fee, nonce, &secret)` is now
//!   `ExternalMessage::new_signed(chain_id, kind, from, nonce, to, amount,
//!   fee, message, pubkey, &secret)` — the chain ID and the key-reveal
//!   field are now part of the signed body.
//! - A keyless sender is no longer categorically unspendable: a key-derived
//!   account (ADR-0006) can spend by revealing the pubkey that derives to
//!   its address. `SenderHasNoKey` now fires only when no key is revealed.
//! - New rejection rules get new tests: `AddressKeyMismatch` (reveal does
//!   not derive to the address), `UnexpectedPubkeyReveal` (reveal on an
//!   already-keyed account — no rotation this milestone), `WrongChainId`
//!   (chain binding checked explicitly before signature verification), and
//!   `ZeroFeeContractCall` (a call must buy gas).
//!
//! Each test builds a minimal genesis, signs a message, and asserts the
//! STF's fail-closed behavior. `propose_block` validates every message
//! internally, so authorization failures surface from the propose call
//! itself — fail-closed before any state is touched.
//!
//! Keys are deterministic test fixtures (`ONX_TEST4_KEY_V1` domain tag);
//! the preimages are public, so these keys are not secret.

use onx_data_structures::{AccountId, ShardIdent, WorkchainIdent};
use onx_primitives::{domain_hash, DomainTag, PublicKey, SecretKey};
use onx_state_model::{
    derive_account_id, AccountState, GenesisDocument, GenesisValidator, StorageStat,
};
use onx_stf::{
    apply_block, derive_address, propose_block, ExternalMessage, MsgKind, State, StfError,
};
use std::collections::BTreeMap;

/// Domain tag for this suite's deterministic signing keys (test-only).
const TEST_KEY_V1: DomainTag = DomainTag::from_ascii("ONX_TEST4_KEY_V1");

fn secret(name: &str) -> SecretKey {
    let seed = domain_hash(&TEST_KEY_V1, name.as_bytes());
    SecretKey::from_seed(&seed).expect("domain hash output is a valid seed")
}

fn active(balance: u128, pubkey: [u8; 32]) -> AccountState {
    AccountState::Active {
        balance_nanos: balance,
        last_trans_lt: 0,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 0,
            byte_count: 0,
        },
        pubkey,
        nonce: 0,
    }
}

/// Genesis with keyed Alice (funded) and keyed Bob. When `keyless_alice`,
/// Alice's address is NOT key-derived, so she can never spend — only receive.
fn genesis(alice_key: &str, keyless_alice: bool) -> (State, AccountId, AccountId, AccountId) {
    let workchain = WorkchainIdent::new(0);
    let shard = ShardIdent::root(workchain);
    let validators = vec![GenesisValidator {
        pubkey: [9u8; 32],
        stake: 1,
    }];
    let alice = derive_account_id("auth-alice");
    let bob = derive_account_id("auth-bob");
    let collector = derive_account_id("auth-collector");
    let mut accounts = BTreeMap::new();
    accounts.insert(
        alice,
        active(
            10_000_000,
            if keyless_alice {
                [0u8; 32]
            } else {
                secret(alice_key).public_key().encode()
            },
        ),
    );
    accounts.insert(bob, active(0, secret("auth-bob-key").public_key().encode()));
    let doc = GenesisDocument::new(workchain, shard, validators, accounts).unwrap();
    (State::from_genesis(&doc), alice, bob, collector)
}

/// Genesis where Alice is a key-DERIVED account (address = `derive_address`
/// of her pubkey), born keyless: her first spend must reveal the pubkey.
fn reveal_genesis() -> (State, AccountId, AccountId, AccountId) {
    let workchain = WorkchainIdent::new(0);
    let shard = ShardIdent::root(workchain);
    let validators = vec![GenesisValidator {
        pubkey: [9u8; 32],
        stake: 1,
    }];
    let alice = derive_address(&secret("reveal-alice").public_key().encode());
    let bob = derive_account_id("reveal-bob");
    let collector = derive_account_id("reveal-collector");
    let mut accounts = BTreeMap::new();
    accounts.insert(alice, active(10_000_000, [0u8; 32]));
    accounts.insert(
        bob,
        active(0, secret("reveal-bob-key").public_key().encode()),
    );
    let doc = GenesisDocument::new(workchain, shard, validators, accounts).unwrap();
    (State::from_genesis(&doc), alice, bob, collector)
}

/// Build a signed transfer (no reveal) against the state's chain ID.
fn signed_transfer(
    state: &State,
    from: AccountId,
    nonce: u64,
    to: AccountId,
    amount: u128,
    fee: u128,
    secret: &SecretKey,
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
        secret,
    )
}

/// Propose-and-apply one block of messages; returns the applied state or
/// the STF error from the failing message.
fn apply_msgs(
    state: &State,
    msgs: Vec<ExternalMessage>,
    collector: AccountId,
    lt: u64,
) -> Result<State, StfError> {
    let block = propose_block(state, msgs, lt, collector, 1, 0)?;
    Ok(apply_block(state, &block)
        .expect("a block that proposed must apply")
        .0)
}

fn alice_nonce(state: &State, alice: &AccountId) -> u64 {
    match state.tree.get(alice) {
        Some(AccountState::Active { nonce, .. }) => *nonce,
        other => panic!("alice must be active, got {other:?}"),
    }
}

#[test]
fn valid_signed_msg_applies_and_bumps_nonce() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    let msg = signed_transfer(
        &state,
        alice,
        0,
        bob,
        1_000_000,
        1_000,
        &secret("auth-alice-key"),
    );
    let state = apply_msgs(&state, vec![msg], collector, 1).expect("valid msg must apply");
    assert_eq!(alice_nonce(&state, &alice), 1);

    // The next message must use nonce 1 — the account's nonce advanced.
    let msg2 = signed_transfer(&state, alice, 1, bob, 500_000, 0, &secret("auth-alice-key"));
    let state = apply_msgs(&state, vec![msg2], collector, 2).expect("nonce-1 msg must apply");
    assert_eq!(alice_nonce(&state, &alice), 2);
}

#[test]
fn wrong_key_signature_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    // Signed by a key that does not match the account's pubkey.
    let msg = signed_transfer(
        &state,
        alice,
        0,
        bob,
        1_000_000,
        0,
        &secret("some-other-key"),
    );
    let err = apply_msgs(&state, vec![msg], collector, 1).expect_err("wrong-key msg must fail");
    assert!(
        matches!(err, StfError::InvalidSignature),
        "expected InvalidSignature, got {err}"
    );
}

#[test]
fn tampered_amount_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    let mut msg = signed_transfer(
        &state,
        alice,
        0,
        bob,
        1_000_000,
        0,
        &secret("auth-alice-key"),
    );
    // Change a signed field without re-signing: the signature no longer
    // matches the body.
    msg.amount_nanos = 2_000_000;
    let err = apply_msgs(&state, vec![msg], collector, 1).expect_err("tampered msg must fail");
    assert!(
        matches!(err, StfError::InvalidSignature),
        "expected InvalidSignature, got {err}"
    );
}

#[test]
fn tampered_nonce_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    let mut msg = signed_transfer(
        &state,
        alice,
        0,
        bob,
        1_000_000,
        0,
        &secret("auth-alice-key"),
    );
    // The nonce is part of the signed payload: bumping it invalidates the
    // signature AND breaks the nonce check. The nonce check fires first
    // (verification order: chain -> kind -> key -> nonce -> signature), so
    // the error is the specific NonceMismatch rather than InvalidSignature
    // — defense in depth, two independent checks covering the same tampering.
    msg.nonce = 1;
    let err = apply_msgs(&state, vec![msg], collector, 1).expect_err("tampered nonce must fail");
    match err {
        StfError::NonceMismatch { expected, got } => {
            assert_eq!(expected, 0);
            assert_eq!(got, 1);
        }
        other => panic!("expected NonceMismatch, got {other}"),
    }
}

#[test]
fn replayed_msg_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    let msg = signed_transfer(
        &state,
        alice,
        0,
        bob,
        1_000_000,
        0,
        &secret("auth-alice-key"),
    );
    let state =
        apply_msgs(&state, vec![msg.clone()], collector, 1).expect("first apply must succeed");
    // Same bytes again: the nonce was already consumed.
    let err = apply_msgs(&state, vec![msg], collector, 2).expect_err("replay must fail");
    match err {
        StfError::NonceMismatch { expected, got } => {
            assert_eq!(expected, 1);
            assert_eq!(got, 0);
        }
        other => panic!("expected NonceMismatch, got {other}"),
    }
}

#[test]
fn nonce_gap_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    // Skip nonce 0 and jump straight to 1: no skipping allowed.
    let msg = signed_transfer(
        &state,
        alice,
        1,
        bob,
        1_000_000,
        0,
        &secret("auth-alice-key"),
    );
    let err = apply_msgs(&state, vec![msg], collector, 1).expect_err("nonce gap must fail");
    match err {
        StfError::NonceMismatch { expected, got } => {
            assert_eq!(expected, 0);
            assert_eq!(got, 1);
        }
        other => panic!("expected NonceMismatch, got {other}"),
    }
}

#[test]
fn keyless_sender_with_no_reveal_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", true);
    // Alice's address was not derived from any key and she reveals none:
    // she can receive but never spend. A signature from *any* key must not
    // authorize her.
    let msg = signed_transfer(
        &state,
        alice,
        0,
        bob,
        1_000_000,
        0,
        &secret("auth-alice-key"),
    );
    let err = apply_msgs(&state, vec![msg], collector, 1).expect_err("keyless spend must fail");
    assert!(
        matches!(err, StfError::SenderHasNoKey(_)),
        "expected SenderHasNoKey, got {err}"
    );
}

#[test]
fn key_reveal_with_wrong_derivation_rejected() {
    let (state, alice, bob, collector) = reveal_genesis();
    // Alice IS key-derived, but the revealed pubkey hashes to a different
    // address. The derivation check fires before signature verification.
    let wrong = secret("reveal-wrong");
    let msg = ExternalMessage::new_signed(
        state.chain_id,
        MsgKind::Transfer,
        alice,
        0,
        bob,
        1_000_000,
        0,
        Vec::new(),
        wrong.public_key().encode(),
        &wrong,
    );
    let err = apply_msgs(&state, vec![msg], collector, 1).expect_err("bad reveal must fail");
    assert!(
        matches!(err, StfError::AddressKeyMismatch { .. }),
        "expected AddressKeyMismatch, got {err}"
    );
}

#[test]
fn reveal_on_already_keyed_account_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    // Alice already has a key on file; revealing one anyway is a
    // protocol fault (no key rotation this milestone).
    let key = secret("auth-alice-key");
    let msg = ExternalMessage::new_signed(
        state.chain_id,
        MsgKind::Transfer,
        alice,
        0,
        bob,
        1_000_000,
        0,
        Vec::new(),
        key.public_key().encode(),
        &key,
    );
    let err = apply_msgs(&state, vec![msg], collector, 1).expect_err("reveal must fail");
    assert!(
        matches!(err, StfError::UnexpectedPubkeyReveal(_)),
        "expected UnexpectedPubkeyReveal, got {err}"
    );
}

#[test]
fn wrong_chain_id_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    // Signed for a different chain. The explicit chain check fires before
    // signature verification — this is belt-and-braces on top of the
    // chain-bound signature, and fails fast with a clear error.
    let mut msg = signed_transfer(
        &state,
        alice,
        0,
        bob,
        1_000_000,
        0,
        &secret("auth-alice-key"),
    );
    msg.chain_id = [0xEE; 32];
    let err = apply_msgs(&state, vec![msg], collector, 1).expect_err("wrong chain must fail");
    assert!(
        matches!(err, StfError::WrongChainId { .. }),
        "expected WrongChainId, got {err}"
    );
}

#[test]
fn zero_fee_contract_call_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    // A contract call must buy gas; a zero fee is a sender-side fault,
    // rejected at the wallet handler (not bounced).
    let msg = ExternalMessage::new_signed(
        state.chain_id,
        MsgKind::ContractCall,
        alice,
        0,
        bob,
        1_000,
        0,
        b"increment".to_vec(),
        [0u8; 32],
        &secret("auth-alice-key"),
    );
    let err = apply_msgs(&state, vec![msg], collector, 1).expect_err("zero-fee call must fail");
    assert!(
        matches!(err, StfError::ZeroFeeContractCall),
        "expected ZeroFeeContractCall, got {err}"
    );
}

#[test]
fn garbage_signature_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    let mut msg = signed_transfer(
        &state,
        alice,
        0,
        bob,
        1_000_000,
        0,
        &secret("auth-alice-key"),
    );
    msg.signature = [0xFF; 64];
    let err = apply_msgs(&state, vec![msg], collector, 1).expect_err("garbage sig must fail");
    assert!(
        matches!(err, StfError::InvalidSignature),
        "expected InvalidSignature, got {err}"
    );
}

#[test]
fn signature_verifies_against_stored_pubkey() {
    // Sanity: the account's stored pubkey is what the signature is checked
    // against — verify_signature with the right key passes, with the wrong
    // key fails, before touching the STF.
    let (state, alice, _, _) = genesis("auth-alice-key", false);
    let msg = signed_transfer(&state, alice, 0, alice, 1, 0, &secret("auth-alice-key"));
    let stored = match state.tree.get(&alice) {
        Some(AccountState::Active { pubkey, .. }) => *pubkey,
        other => panic!("alice must be active, got {other:?}"),
    };
    let pubkey = PublicKey::decode_exact(&stored).expect("stored key must be a curve point");
    assert!(msg.verify_signature(&pubkey).is_ok());
    let wrong = secret("some-other-key").public_key();
    assert!(msg.verify_signature(&wrong).is_err());
}
