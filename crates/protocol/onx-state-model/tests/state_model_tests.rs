use onx_data_structures::AccountId;
use onx_state_model::{
    AccountState, BagOfCells, Cell, MerkleProof, ShardStateTree, StateModelError, StorageStat,
    MAX_CELL_DATA_BYTES, MAX_CELL_REFS,
};

#[test]
fn test_account_state_serialization_round_trip() {
    let uninit = AccountState::Uninitialized;
    let uninit_bytes = uninit.to_bytes();
    let (uninit_de, consumed) = AccountState::from_bytes(&uninit_bytes).unwrap();
    assert_eq!(uninit, uninit_de);
    assert_eq!(consumed, 1);

    let destroyed = AccountState::Destroyed;
    let destroyed_bytes = destroyed.to_bytes();
    let (destroyed_de, consumed) = AccountState::from_bytes(&destroyed_bytes).unwrap();
    assert_eq!(destroyed, destroyed_de);
    assert_eq!(consumed, 1);

    let active = AccountState::Active {
        balance_nanos: 1_000_000_000,
        last_trans_lt: 100,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 5,
            byte_count: 500,
        },
        pubkey: [0u8; 32],
        nonce: 0,
    };
    let active_bytes = active.to_bytes();
    let (active_de, consumed) = AccountState::from_bytes(&active_bytes).unwrap();
    assert_eq!(active, active_de);
    assert_eq!(consumed, active_bytes.len());

    let frozen = AccountState::Frozen {
        balance_nanos: 500_000,
        last_trans_lt: 200,
        storage_hash: [0x33; 32],
    };
    let frozen_bytes = frozen.to_bytes();
    let (frozen_de, consumed) = AccountState::from_bytes(&frozen_bytes).unwrap();
    assert_eq!(frozen, frozen_de);
    assert_eq!(consumed, frozen_bytes.len());
}

#[test]
fn test_account_lifecycle_transitions() {
    let uninit = AccountState::Uninitialized;
    let active_1 = AccountState::Active {
        balance_nanos: 1_000_000,
        last_trans_lt: 10,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 1,
            byte_count: 100,
        },
        pubkey: [0u8; 32],
        nonce: 0,
    };

    // Uninit -> Active (lt = 10)
    assert!(uninit.validate_transition(&active_1, 10).is_ok());

    // Active (lt=10) -> Active (lt=15)
    let active_2 = AccountState::Active {
        balance_nanos: 900_000,
        last_trans_lt: 15,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 2,
            byte_count: 150,
        },
        pubkey: [0u8; 32],
        nonce: 0,
    };
    assert!(active_1.validate_transition(&active_2, 15).is_ok());

    // Logical time regression error (lt=12 <= current 15)
    let active_regress = AccountState::Active {
        balance_nanos: 800_000,
        last_trans_lt: 12,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 2,
            byte_count: 150,
        },
        pubkey: [0u8; 32],
        nonce: 0,
    };
    assert!(matches!(
        active_2.validate_transition(&active_regress, 12),
        Err(StateModelError::LogicalTimeRegression { .. })
    ));

    // Active -> Frozen
    let frozen = AccountState::Frozen {
        balance_nanos: 100,
        last_trans_lt: 20,
        storage_hash: [9; 32],
    };
    assert!(active_2.validate_transition(&frozen, 20).is_ok());

    // Frozen -> Active (unfreeze)
    let active_unfrozen = AccountState::Active {
        balance_nanos: 1_000_000,
        last_trans_lt: 25,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 1,
            byte_count: 80,
        },
        pubkey: [0u8; 32],
        nonce: 0,
    };
    assert!(frozen.validate_transition(&active_unfrozen, 25).is_ok());

    // Active -> Destroyed
    let destroyed = AccountState::Destroyed;
    assert!(active_unfrozen.validate_transition(&destroyed, 30).is_ok());

    // Destroyed -> Active (forbidden)
    assert!(matches!(
        destroyed.validate_transition(&active_1, 35),
        Err(StateModelError::InvalidStateTransition(_))
    ));
}

#[test]
fn test_balance_underflow_and_transition_with_delta() {
    let active_init = AccountState::Active {
        balance_nanos: 1_000_000,
        last_trans_lt: 10,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 1,
            byte_count: 100,
        },
        pubkey: [0u8; 32],
        nonce: 0,
    };

    // Valid balance deduction transition (1_000_000 - 300_000 = 700_000)
    let active_next_valid = AccountState::Active {
        balance_nanos: 700_000,
        last_trans_lt: 15,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 1,
            byte_count: 100,
        },
        pubkey: [0u8; 32],
        nonce: 0,
    };
    assert!(active_init
        .validate_transition_with_delta(&active_next_valid, 15, -300_000)
        .is_ok());

    // Balance underflow rejection (1_000_000 - 1_500_000 < 0)
    let active_next_underflow = AccountState::Active {
        balance_nanos: 0,
        last_trans_lt: 15,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 1,
            byte_count: 100,
        },
        pubkey: [0u8; 32],
        nonce: 0,
    };
    assert!(matches!(
        active_init.validate_transition_with_delta(&active_next_underflow, 15, -1_500_000),
        Err(StateModelError::BalanceUnderflow)
    ));

    // Mismatched expected next balance vs actual next balance state
    assert!(matches!(
        active_init.validate_transition_with_delta(&active_next_valid, 15, -200_000),
        Err(StateModelError::InvalidStateTransition(_))
    ));
}

#[test]
fn test_cell_hashing_and_limits() {
    let child1 = Cell::new(vec![1, 2, 3], vec![]).unwrap();
    let child2 = Cell::new(vec![4, 5, 6], vec![]).unwrap();

    let parent = Cell::new(vec![7, 8, 9], vec![child1.hash(), child2.hash()]).unwrap();

    // Hashing is deterministic
    assert_eq!(parent.hash(), parent.hash());
    assert_ne!(child1.hash(), child2.hash());
    assert_ne!(parent.hash(), child1.hash());

    // Data byte limit enforcement
    let oversized_data = vec![0u8; MAX_CELL_DATA_BYTES + 1];
    assert!(matches!(
        Cell::new(oversized_data, vec![]),
        Err(StateModelError::DataTooLarge { .. })
    ));

    // Reference limit enforcement
    let too_many_refs = vec![[0u8; 32]; MAX_CELL_REFS + 1];
    assert!(matches!(
        Cell::new(vec![1], too_many_refs),
        Err(StateModelError::TooManyReferences { .. })
    ));
}

#[test]
fn test_bag_of_cells_serialization_and_cycle_detection() {
    let child = Cell::new(vec![10, 20], vec![]).unwrap();
    let root = Cell::new(vec![30, 40], vec![child.hash()]).unwrap();

    let mut boc = BagOfCells::from_root(root.clone()).unwrap();
    boc.add_cell(child.clone()).unwrap();

    let boc_bytes = boc.to_bytes();
    let (boc_de, consumed) = BagOfCells::from_bytes(&boc_bytes).unwrap();
    assert_eq!(boc, boc_de);
    assert_eq!(consumed, boc_bytes.len());

    // Test cycle detection
    let mut map = std::collections::BTreeMap::new();
    let hash_a = [0xAA; 32];
    let hash_b = [0xBB; 32];

    let cell_a = Cell::new(vec![1], vec![hash_b]).unwrap();
    let cell_b = Cell::new(vec![2], vec![hash_a]).unwrap();

    map.insert(hash_a, cell_a);
    map.insert(hash_b, cell_b);

    assert!(matches!(
        BagOfCells::new(hash_a, map),
        Err(StateModelError::CyclicCellReference)
    ));
}

#[test]
fn test_boc_rejects_impossible_cell_count_before_allocation() {
    // Root hash followed by a hostile cell count, with no cell-entry bytes.
    // The decoder must reject this before using the count to size a map collection
    // capacity, rather than attempting an attacker-controlled allocation.
    let mut bytes = vec![0u8; 32];
    bytes.extend_from_slice(&u32::MAX.to_be_bytes());

    assert!(matches!(
        BagOfCells::from_bytes(&bytes),
        Err(StateModelError::DeserializationError(message))
            if message == "BoC cell count exceeds remaining input capacity"
    ));
}

#[test]
fn test_shard_state_tree_and_merkle_proofs() {
    let mut tree = ShardStateTree::new();
    let acc1 = AccountId::from_bytes([0x10; 32]);
    let acc2 = AccountId::from_bytes([0x20; 32]);

    let state1 = AccountState::Active {
        balance_nanos: 500,
        last_trans_lt: 1,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 1,
            byte_count: 10,
        },
        pubkey: [0u8; 32],
        nonce: 0,
    };

    let state2 = AccountState::Active {
        balance_nanos: 1500,
        last_trans_lt: 2,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 2,
            byte_count: 20,
        },
        pubkey: [0u8; 32],
        nonce: 0,
    };

    tree.insert(acc1, state1).unwrap();
    tree.insert(acc2, state2).unwrap();

    let root_hash = tree.state_root_hash().unwrap();
    assert_ne!(root_hash, [0u8; 32]);

    let proof = tree.generate_proof(acc1).unwrap();
    assert_eq!(proof.root_hash, root_hash);
    assert!(proof.verify(&root_hash).is_ok());

    let proof_bytes = proof.to_bytes();
    let (proof_de, consumed) = MerkleProof::from_bytes(&proof_bytes).unwrap();
    assert_eq!(proof, proof_de);
    assert_eq!(consumed, proof_bytes.len());

    // Tampered Merkle proof root hash rejection
    let mut tampered_proof = proof;
    tampered_proof.root_hash = [0xFF; 32];
    assert!(matches!(
        tampered_proof.verify(&root_hash),
        Err(StateModelError::InvalidMerkleProof(_))
    ));
}

#[test]
fn test_codeless_account_encoding_is_byte_identical_to_v2() {
    // The contract upgrade must not change the encoding of plain accounts:
    // 141 bytes, with the code_hash/data_hash header fields all zero and
    // nothing appended. This is what keeps the V2 golden vectors valid.
    let active = AccountState::Active {
        balance_nanos: 1_000_000_000,
        last_trans_lt: 100,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 5,
            byte_count: 500,
        },
        pubkey: [0x77; 32],
        nonce: 7,
    };
    let bytes = active.to_bytes();
    assert_eq!(bytes.len(), 141, "codeless account must stay 141 bytes");
    assert_eq!(bytes[0], 1, "Active type tag");
    // code_hash (bytes 25..57) and data_hash (57..89) are all zero.
    assert!(bytes[25..57].iter().all(|&b| b == 0));
    assert!(bytes[57..89].iter().all(|&b| b == 0));
    // Round-trips with no cells.
    let (de, consumed) = AccountState::from_bytes(&bytes).unwrap();
    assert_eq!(consumed, 141);
    match de {
        AccountState::Active {
            code: None,
            data: None,
            ..
        } => {}
        other => panic!("unexpected: {other:?}"),
    }
    assert_eq!(de, active);
}

#[test]
fn test_embedded_cell_hash_mismatch_rejected() {
    // A tampered code payload (hash doesn't match the header) fails closed.
    let code = Cell::new(vec![0x45], vec![]).unwrap();
    let active = AccountState::Active {
        balance_nanos: 1,
        last_trans_lt: 1,
        code: Some(code),
        data: None,
        storage_stat: StorageStat {
            cell_count: 1,
            byte_count: 3,
        },
        pubkey: [0u8; 32],
        nonce: 0,
    };
    let mut bytes = active.to_bytes();
    // Flip a bit inside the appended code cell payload.
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    let err = AccountState::from_bytes(&bytes).unwrap_err();
    assert!(
        matches!(err, StateModelError::DeserializationError(_)),
        "unexpected: {err:?}"
    );
}
