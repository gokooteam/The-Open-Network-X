//! Transaction authorization regression tests: Ed25519 signatures and
//! per-account nonces (the `ONX_TX_V2` upgrade).
//!
//! Each test builds a minimal genesis (one or two keyed accounts), signs a
//! transaction with `Transaction::new_signed`, and asserts the STF's
//! fail-closed behavior. The judgment calls under test:
//! - the `from` account must carry a non-zero pubkey (`SenderHasNoKey`),
//! - `tx.nonce` must equal the account's nonce exactly (`NonceMismatch`),
//! - the signature must verify over the full body including the nonce
//!   (`InvalidSignature`),
//! - a successfully applied transaction bumps the account nonce by one, so
//!   replaying the same bytes is rejected.
//!
//! Keys are deterministic test fixtures (`ONX_TEST4_KEY_V1` domain tag);
//! the preimages are public, so these keys are not secret.

use onx_data_structures::{AccountId, ShardIdent, WorkchainIdent};
use onx_primitives::{domain_hash, DomainTag, PublicKey, SecretKey};
use onx_state_model::{
    derive_account_id, AccountState, GenesisDocument, GenesisValidator, StorageStat,
};
use onx_stf::{apply_block, propose_block, State, StfError, Transaction};
use std::collections::BTreeMap;

/// Domain tag for this suite's deterministic signing keys (test-only).
const TEST_KEY_V1: DomainTag = DomainTag::from_ascii("ONX_TEST4_KEY_V1");

fn secret(name: &str) -> SecretKey {
    let seed = domain_hash(&TEST_KEY_V1, name.as_bytes());
    SecretKey::from_seed(&seed).expect("domain hash output is a valid seed")
}

fn keyed(balance: u128, name: &str) -> AccountState {
    AccountState::Active {
        balance_nanos: balance,
        last_trans_lt: 0,
        code_hash: [0u8; 32],
        data_hash: [0u8; 32],
        storage_stat: StorageStat {
            cell_count: 0,
            byte_count: 0,
        },
        pubkey: secret(name).public_key().encode(),
        nonce: 0,
    }
}

fn keyless(balance: u128) -> AccountState {
    AccountState::Active {
        balance_nanos: balance,
        last_trans_lt: 0,
        code_hash: [0u8; 32],
        data_hash: [0u8; 32],
        storage_stat: StorageStat {
            cell_count: 0,
            byte_count: 0,
        },
        pubkey: [0u8; 32],
        nonce: 0,
    }
}

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
        if keyless_alice {
            keyless(10_000_000)
        } else {
            keyed(10_000_000, alice_key)
        },
    );
    accounts.insert(bob, keyed(0, "auth-bob-key"));
    let doc = GenesisDocument::new(workchain, shard, validators, accounts).unwrap();
    (State::from_genesis(&doc), alice, bob, collector)
}

/// Propose-and-apply one block of txs; returns the applied state or the
/// STF error from the failing transaction. Note `propose_block` validates
/// the block (including every transaction) internally, so authorization
/// failures surface from the propose call itself — fail-closed before any
/// state is touched.
fn apply_txs(
    state: &State,
    txs: Vec<Transaction>,
    collector: AccountId,
    lt: u64,
) -> Result<State, StfError> {
    let block = propose_block(state, txs, lt, collector)?;
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
fn valid_signed_tx_applies_and_bumps_nonce() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    let tx = Transaction::new_signed(alice, bob, 1_000_000, 1_000, 0, &secret("auth-alice-key"));
    let state = apply_txs(&state, vec![tx], collector, 1).expect("valid tx must apply");
    assert_eq!(alice_nonce(&state, &alice), 1);

    // The next transaction must use nonce 1 — the account's nonce advanced.
    let tx2 = Transaction::new_signed(alice, bob, 500_000, 0, 1, &secret("auth-alice-key"));
    let state = apply_txs(&state, vec![tx2], collector, 2).expect("nonce-1 tx must apply");
    assert_eq!(alice_nonce(&state, &alice), 2);
}

#[test]
fn wrong_key_signature_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    // Signed by a key that does not match the account's pubkey.
    let tx = Transaction::new_signed(alice, bob, 1_000_000, 0, 0, &secret("some-other-key"));
    let err = apply_txs(&state, vec![tx], collector, 1).expect_err("wrong-key tx must fail");
    assert!(
        matches!(err, StfError::InvalidSignature),
        "expected InvalidSignature, got {err}"
    );
}

#[test]
fn tampered_amount_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    let mut tx = Transaction::new_signed(alice, bob, 1_000_000, 0, 0, &secret("auth-alice-key"));
    // Change a signed field without re-signing: the signature no longer
    // matches the body.
    tx.amount_nanos = 2_000_000;
    let err = apply_txs(&state, vec![tx], collector, 1).expect_err("tampered tx must fail");
    assert!(
        matches!(err, StfError::InvalidSignature),
        "expected InvalidSignature, got {err}"
    );
}

#[test]
fn tampered_nonce_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    let mut tx = Transaction::new_signed(alice, bob, 1_000_000, 0, 0, &secret("auth-alice-key"));
    // The nonce is part of the signed payload: bumping it invalidates the
    // signature AND breaks the nonce check. The nonce check fires first
    // (verification order: pubkey -> nonce -> signature), so the error is
    // the specific NonceMismatch rather than InvalidSignature — defense in
    // depth, two independent checks covering the same tampering.
    tx.nonce = 1;
    let err = apply_txs(&state, vec![tx], collector, 1).expect_err("tampered nonce must fail");
    match err {
        StfError::NonceMismatch { expected, got } => {
            assert_eq!(expected, 0);
            assert_eq!(got, 1);
        }
        other => panic!("expected NonceMismatch, got {other}"),
    }
}

#[test]
fn replayed_tx_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    let tx = Transaction::new_signed(alice, bob, 1_000_000, 0, 0, &secret("auth-alice-key"));
    let state = apply_txs(&state, vec![tx], collector, 1).expect("first apply must succeed");
    // Same bytes again: the nonce was already consumed.
    let err = apply_txs(&state, vec![tx], collector, 2).expect_err("replay must fail");
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
    let tx = Transaction::new_signed(alice, bob, 1_000_000, 0, 1, &secret("auth-alice-key"));
    let err = apply_txs(&state, vec![tx], collector, 1).expect_err("nonce gap must fail");
    match err {
        StfError::NonceMismatch { expected, got } => {
            assert_eq!(expected, 0);
            assert_eq!(got, 1);
        }
        other => panic!("expected NonceMismatch, got {other}"),
    }
}

#[test]
fn keyless_sender_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", true);
    // The account carries no pubkey: it can receive but never spend. A
    // signature from *any* key must not authorize it.
    let tx = Transaction::new_signed(alice, bob, 1_000_000, 0, 0, &secret("auth-alice-key"));
    let err = apply_txs(&state, vec![tx], collector, 1).expect_err("keyless spend must fail");
    assert!(
        matches!(err, StfError::SenderHasNoKey(_)),
        "expected SenderHasNoKey, got {err}"
    );
}

#[test]
fn garbage_signature_rejected() {
    let (state, alice, bob, collector) = genesis("auth-alice-key", false);
    let mut tx = Transaction::new_signed(alice, bob, 1_000_000, 0, 0, &secret("auth-alice-key"));
    tx.signature = [0xFF; 64];
    let err = apply_txs(&state, vec![tx], collector, 1).expect_err("garbage sig must fail");
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
    let tx = Transaction::new_signed(alice, alice, 1, 0, 0, &secret("auth-alice-key"));
    let stored = match state.tree.get(&alice) {
        Some(AccountState::Active { pubkey, .. }) => *pubkey,
        other => panic!("alice must be active, got {other:?}"),
    };
    let pubkey = PublicKey::decode_exact(&stored).expect("stored key must be a curve point");
    assert!(tx.verify_signature(&pubkey).is_ok());
    let wrong = secret("some-other-key").public_key();
    assert!(tx.verify_signature(&wrong).is_err());
}
