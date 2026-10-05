//! Phase 2 integration: the genesis document as an external consumer sees it —
//! parse canonical bytes, build the state tree, and confirm the root is
//! stable across independently constructed documents.

use onx_data_structures::{ShardIdent, WorkchainIdent};
use onx_state_model::{
    derive_account_id, derive_validator_pubkey, AccountState, GenesisDocument, GenesisValidator,
    StorageStat,
};
use std::collections::BTreeMap;

fn value_account(balance_nanos: u128) -> AccountState {
    AccountState::Active {
        balance_nanos,
        last_trans_lt: 0,
        code_hash: [0u8; 32],
        data_hash: [0u8; 32],
        storage_stat: StorageStat {
            cell_count: 0,
            byte_count: 0,
        },
    }
}

fn build_doc() -> GenesisDocument {
    let workchain = WorkchainIdent::MASTERCHAIN;
    let mut accounts = BTreeMap::new();
    accounts.insert(derive_account_id("onx:alice"), value_account(1_000_000));
    accounts.insert(derive_account_id("onx:bob"), value_account(2_000_000));
    GenesisDocument::new(
        workchain,
        ShardIdent::root(workchain),
        vec![GenesisValidator {
            pubkey: derive_validator_pubkey("validator-01"),
            stake: 1_000,
        }],
        accounts,
    )
    .unwrap()
}

#[test]
fn state_root_stable_across_independent_constructions() {
    let root_a = build_doc().state_tree().state_root_hash().unwrap();
    let root_b = build_doc().state_tree().state_root_hash().unwrap();
    assert_eq!(root_a, root_b);
    // And the root survives a serialize -> parse round trip.
    let bytes = build_doc().to_bytes();
    let parsed = GenesisDocument::from_bytes(&bytes).unwrap();
    assert_eq!(parsed.state_tree().state_root_hash().unwrap(), root_a);
}

#[test]
fn workchain_label_matches_protocol_definition() {
    let bytes = build_doc().to_bytes();
    // workchain_id occupies bytes 8..12 of the canonical encoding.
    let id = i32::from_be_bytes(bytes[8..12].try_into().unwrap());
    assert_eq!(id, -1, "genesis workchain must be masterchain (-1)");
}

#[test]
fn validators_come_out_sorted_by_pubkey() {
    let workchain = WorkchainIdent::MASTERCHAIN;
    let mut accounts = BTreeMap::new();
    accounts.insert(derive_account_id("x"), value_account(1));
    let doc = GenesisDocument::new(
        workchain,
        ShardIdent::root(workchain),
        vec![
            GenesisValidator {
                pubkey: [0xffu8; 32],
                stake: 1,
            },
            GenesisValidator {
                pubkey: [0x01u8; 32],
                stake: 2,
            },
        ],
        accounts,
    )
    .unwrap();
    assert_eq!(doc.validators[0].pubkey, [0x01u8; 32]);
    assert_eq!(doc.validators[1].pubkey, [0xffu8; 32]);
    // Canonical bytes preserve that order.
    let bytes = doc.to_bytes();
    let parsed = GenesisDocument::from_bytes(&bytes).unwrap();
    assert_eq!(parsed.validators[0].pubkey, [0x01u8; 32]);
}

#[test]
fn tampered_account_order_is_rejected() {
    let bytes = build_doc().to_bytes();
    let id_a = derive_account_id("onx:alice").to_bytes();
    let id_b = derive_account_id("onx:bob").to_bytes();
    // Locate both account ids in the canonical bytes and swap them,
    // breaking the required ascending order.
    let pos_a = bytes
        .windows(32)
        .position(|w| w == id_a)
        .expect("alice id must appear in genesis bytes");
    let pos_b = bytes
        .windows(32)
        .position(|w| w == id_b)
        .expect("bob id must appear in genesis bytes");
    assert_ne!(pos_a, pos_b);
    let mut tampered = bytes.clone();
    tampered[pos_a..pos_a + 32].copy_from_slice(&id_b);
    tampered[pos_b..pos_b + 32].copy_from_slice(&id_a);
    assert!(GenesisDocument::from_bytes(&tampered).is_err());
}
