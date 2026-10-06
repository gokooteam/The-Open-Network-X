//! Phase 3 Merkle proof regression tests.
//!
//! The old `MerkleProof::verify()` was self-referential: it checked the
//! proof's BoC root against the proof's own root_hash field, took no
//! trusted root, never checked the target key, and `generate_proof`
//! smuggled the entire tree into the "proof" (131 cells for 64 accounts).
//! These tests pin the fixed behavior:
//!   - `verify()` takes a trusted root and binds the proof to it;
//!   - proofs are O(log n) Merkle paths, not whole trees;
//!   - fabricated-tree and absent-key proofs FAIL; valid proofs PASS.

use onx_data_structures::AccountId;
use onx_state_model::{AccountState, MerkleProof, ShardStateTree, StateModelError};

fn account_id(first_byte: u8) -> AccountId {
    let mut bytes = [0u8; 32];
    bytes[0] = first_byte;
    AccountId::from_bytes(bytes)
}

fn tree_with(n: u8) -> (ShardStateTree, Vec<AccountId>) {
    let mut tree = ShardStateTree::new();
    let mut ids = Vec::new();
    for i in 0..n {
        let id = account_id(i);
        tree.insert(id, AccountState::Uninitialized).unwrap();
        ids.push(id);
    }
    (tree, ids)
}

#[test]
fn valid_key_proof_passes_against_trusted_root() {
    let (tree, ids) = tree_with(64);
    let trusted_root = tree.state_root_hash().unwrap();

    let proof = tree.generate_proof(ids[7]).unwrap();
    assert_eq!(proof.root_hash, trusted_root);
    assert!(proof.verify(&trusted_root).is_ok());

    // And after a binary round trip.
    let bytes = proof.to_bytes();
    let (decoded, consumed) = MerkleProof::from_bytes(&bytes).unwrap();
    assert_eq!(consumed, bytes.len());
    assert!(decoded.verify(&trusted_root).is_ok());
}

#[test]
fn fabricated_tree_proof_fails_against_trusted_root() {
    // Honest tree A; attacker's tree B with different accounts.
    let (tree_a, _) = tree_with(64);
    let trusted_root = tree_a.state_root_hash().unwrap();

    let mut tree_b = ShardStateTree::new();
    let evil_id = account_id(0xE0);
    tree_b.insert(evil_id, AccountState::Uninitialized).unwrap();
    for i in 100..164u8 {
        tree_b
            .insert(account_id(i), AccountState::Uninitialized)
            .unwrap();
    }
    assert_ne!(tree_b.state_root_hash().unwrap(), trusted_root);

    // The attacker's proof is perfectly valid FOR ITS OWN TREE...
    let evil_proof = tree_b.generate_proof(evil_id).unwrap();
    assert!(evil_proof
        .verify(&tree_b.state_root_hash().unwrap())
        .is_ok());

    // ...but must FAIL against the honest trusted root. This is the case
    // the old self-referential verify() got wrong (it returned Ok).
    assert!(matches!(
        evil_proof.verify(&trusted_root),
        Err(StateModelError::InvalidMerkleProof(_))
    ));
}

#[test]
fn absent_key_proof_generation_fails() {
    let (tree, _) = tree_with(64);
    let absent = account_id(0xFF);
    assert!(tree.get(&absent).is_none());

    // Generation itself must refuse: no proof exists for an absent key.
    assert!(matches!(
        tree.generate_proof(absent),
        Err(StateModelError::InvalidMerkleProof(_))
    ));
}

#[test]
fn retargeted_proof_to_absent_key_fails_verification() {
    // Even if an attacker takes a valid proof's cells and re-labels it for
    // a key that is not in the tree, verification must fail: the walk from
    // the trusted root following the absent key's bits cannot terminate at
    // a leaf carrying that key.
    let (tree, ids) = tree_with(64);
    let trusted_root = tree.state_root_hash().unwrap();

    let proof = tree.generate_proof(ids[3]).unwrap();
    let mut retargeted = proof.clone();
    retargeted.target_key = account_id(0xFE).to_bytes();

    assert!(matches!(
        retargeted.verify(&trusted_root),
        Err(StateModelError::InvalidMerkleProof(_))
    ));
}

#[test]
fn proof_for_wrong_trusted_root_fails() {
    let (tree_a, ids_a) = tree_with(64);
    let (tree_b, _) = tree_with(32);
    assert_ne!(
        tree_a.state_root_hash().unwrap(),
        tree_b.state_root_hash().unwrap()
    );

    let proof = tree_a.generate_proof(ids_a[10]).unwrap();
    assert!(matches!(
        proof.verify(&tree_b.state_root_hash().unwrap()),
        Err(StateModelError::InvalidMerkleProof(_))
    ));
}

#[test]
fn proof_is_path_sized_not_tree_sized() {
    // The old generate_proof smuggled the whole tree into the proof:
    // 131 cells for a 64-account tree. A real Merkle path is O(log n).
    let (tree, ids) = tree_with(64);
    let proof = tree.generate_proof(ids[42]).unwrap();
    let cell_count = proof.proof_boc.cells().len();
    assert!(
        cell_count < 32,
        "proof should be path-sized, got {cell_count} cells for 64 accounts"
    );
    // ...and it still verifies.
    assert!(proof.verify(&tree.state_root_hash().unwrap()).is_ok());
}

#[test]
fn single_account_tree_proof_round_trips() {
    // Degenerate case: the root IS the leaf.
    let mut tree = ShardStateTree::new();
    let id = account_id(0x01);
    tree.insert(id, AccountState::Uninitialized).unwrap();
    let trusted_root = tree.state_root_hash().unwrap();

    let proof = tree.generate_proof(id).unwrap();
    assert!(proof.verify(&trusted_root).is_ok());

    let bytes = proof.to_bytes();
    let (decoded, _) = MerkleProof::from_bytes(&bytes).unwrap();
    assert!(decoded.verify(&trusted_root).is_ok());
}
