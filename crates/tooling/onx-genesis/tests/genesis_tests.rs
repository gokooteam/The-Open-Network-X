use onx_data_structures::WorkchainIdent;
use onx_genesis::{build_genesis_document, generate_genesis, parse_config, GenesisConfig};
use onx_state_model::GenesisDocument;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "onx-genesis-{}-{}-{}",
        tag,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn test_config() -> GenesisConfig {
    GenesisConfig {
        balances: vec![
            onx_genesis::Balance {
                address: "onx:alice".to_string(),
                amount: 1_000_000,
                public_key: None,
                code_hex: None,
                data_hex: None,
            },
            onx_genesis::Balance {
                address: "onx:bob".to_string(),
                amount: 2_000_000,
                public_key: None,
                code_hex: None,
                data_hex: None,
            },
        ],
        validators: vec![onx_genesis::Validator {
            public_key: "validator-01".to_string(),
            stake: 1_000,
        }],
        workchains: vec![onx_genesis::Workchain {
            id: -1,
            name: "masterchain".to_string(),
            enabled: true,
        }],
    }
}

#[test]
fn emits_canonical_genesis_and_bootstrap_files() {
    let temp = temp_dir("canonical");
    let cfg = test_config();

    generate_genesis(&cfg, temp.clone()).unwrap();
    assert!(temp.join("genesis.boc").exists());
    assert!(temp.join("node-0.toml").exists());
    assert!(temp.join("node-1.toml").exists());
    assert!(temp.join("node-2.toml").exists());
    assert!(temp.join("node-3.toml").exists());

    let node_zero = fs::read_to_string(temp.join("node-0.toml")).unwrap();
    assert!(node_zero.contains("network_bind = \"127.0.0.1:10000\""));
    assert!(node_zero.contains("peers = \"127.0.0.1:10001\""));
    // Node configs pin the chain they join.
    assert!(node_zero.contains("chain_id = \""));

    // The genesis file is a real canonical document, not text.
    let raw = fs::read(temp.join("genesis.boc")).unwrap();
    assert_eq!(&raw[0..4], b"ONXG", "genesis must open with the ONXG magic");
    let doc = GenesisDocument::from_bytes(&raw).expect("genesis must parse");
    assert_eq!(doc.workchain, WorkchainIdent::MASTERCHAIN);
    assert_eq!(doc.validators.len(), 1);
    assert_eq!(doc.accounts.len(), 2);
    assert_eq!(doc.validators[0].stake, 1_000);

    // The genesis hash is self-consistent: parse -> hash == hash of bytes.
    let doc2 = build_genesis_document(&cfg).unwrap();
    assert_eq!(doc.genesis_hash(), doc2.genesis_hash());

    // The state tree root is defined and stable.
    let root_a = doc.state_tree().state_root_hash().unwrap();
    let root_b = doc2.state_tree().state_root_hash().unwrap();
    assert_eq!(root_a, root_b);

    let _ = fs::remove_dir_all(temp);
}

#[test]
fn rejects_mislabeled_workchain() {
    // Phase 2 reconciliation: id 0 named "masterchain" contradicts the
    // protocol definition (masterchain = -1). It must fail loudly.
    let mut cfg = test_config();
    cfg.workchains[0].id = 0;
    cfg.workchains[0].name = "masterchain".to_string();
    assert!(build_genesis_document(&cfg).is_err());

    // And the reverse drift: id -1 must be named "masterchain".
    let mut cfg = test_config();
    cfg.workchains[0].name = "basic".to_string();
    assert!(build_genesis_document(&cfg).is_err());

    // The coherent basic-workchain labeling is accepted.
    let mut cfg = test_config();
    cfg.workchains[0].id = 0;
    cfg.workchains[0].name = "basic".to_string();
    let doc = build_genesis_document(&cfg).unwrap();
    assert_eq!(doc.workchain, WorkchainIdent::BASIC);
}

#[test]
fn rejects_duplicate_addresses_and_empty_sets() {
    let mut cfg = test_config();
    cfg.balances.push(onx_genesis::Balance {
        address: "onx:alice".to_string(),
        amount: 5,
        public_key: None,
        code_hex: None,
        data_hex: None,
    });
    assert!(build_genesis_document(&cfg).is_err());

    let mut cfg = test_config();
    cfg.validators.clear();
    assert!(build_genesis_document(&cfg).is_err());

    let mut cfg = test_config();
    cfg.balances.clear();
    assert!(build_genesis_document(&cfg).is_err());
}

#[test]
fn rejects_probable_hex_typo_in_keys() {
    // 64 chars, not valid hex: fail loudly rather than derive a surprise key.
    let mut cfg = test_config();
    cfg.validators[0].public_key = "zz".repeat(32);
    assert!(build_genesis_document(&cfg).is_err());
}

#[test]
fn parses_repo_default_config() {
    // The checked-in config must always build a valid genesis.
    let cfg = parse_config(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../config/genesis.toml"
    ))
    .expect("repo genesis.toml must parse");
    let doc = build_genesis_document(&cfg).expect("repo genesis.toml must build");
    assert_eq!(doc.workchain, WorkchainIdent::MASTERCHAIN);
    assert!(!doc.validators.is_empty());
    assert!(!doc.accounts.is_empty());
}

#[test]
fn rekeyed_fixture_validator_is_test_secret_key_0x11() {
    // Rekey (2026-10-07): the repo fixture's validator must be the public
    // key of test_secret_key(0x11) = SecretKey::from_seed(&[0x11; 32]) — a
    // public, deterministic test key — so fixture blocks can actually be
    // signed (ONXBLK05). The old DEV label "validator-01" derived a key
    // with no known private key and could never sign.
    let cfg = parse_config(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../config/genesis.toml"
    ))
    .expect("repo genesis.toml must parse");
    let doc = build_genesis_document(&cfg).expect("repo genesis.toml must build");

    let expected = onx_primitives::SecretKey::from_seed(&[0x11u8; 32])
        .expect("fixed test seed decodes")
        .public_key()
        .encode();
    assert_eq!(doc.validators.len(), 1);
    assert_eq!(
        doc.validators[0].pubkey, expected,
        "fixture validator is not test_secret_key(0x11)"
    );

    // Property: the rekey changes the chain ID (the genesis hash commits to
    // the validator set) but leaves the state root (accounts) untouched —
    // only the validator key moved, nothing about the funded accounts.
    let mut old_cfg = cfg.clone();
    old_cfg.validators[0].public_key = "validator-01".to_string();
    let old_doc = build_genesis_document(&old_cfg).expect("old DEV-label config must build");
    assert_ne!(
        doc.genesis_hash(),
        old_doc.genesis_hash(),
        "chain ID must change on rekey"
    );
    assert_eq!(
        doc.state_tree()
            .state_root_hash()
            .expect("state root builds"),
        old_doc
            .state_tree()
            .state_root_hash()
            .expect("state root builds"),
        "state root must be unchanged by the validator rekey"
    );
}
