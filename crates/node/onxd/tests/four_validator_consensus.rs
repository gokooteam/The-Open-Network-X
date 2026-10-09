//! Four-validator BFT consensus integration test (M6/P6).
//!
//! Spins up four real `onxd` producer loops on localhost with TCP gossip,
//! submits a transaction to one, and verifies all four finalize the same
//! block with a quorum of signatures.

use onx::blockfile::decode_block_file;
use onx_data_structures::AccountId;
use onx_primitives::SecretKey;
use onx_storage::ChainStore;
use onxd::mempool::Mempool;
use onxd::producer::{run_producer_loop, ProducerConfig};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn test_secret(byte: u8) -> SecretKey {
    SecretKey::from_seed(&[byte; 32]).expect("fixed test seed decodes")
}

fn write_four_validator_genesis(dir: &std::path::Path) -> PathBuf {
    let mut toml = String::from("# M6 test genesis: 4 deterministic validators\n");
    // Fund an account for the test transaction.
    let funder = test_secret(0xaa);
    toml.push_str(&format!(
        "[[balances]]\naddress = \"{}\"\namount = 1000000000000\npublic_key = \"{}\"\n\n",
        hex::encode([0xaa; 32]),
        hex::encode(funder.public_key().encode()),
    ));
    for byte in [0x11u8, 0x22, 0x33, 0x44] {
        toml.push_str(&format!(
            "[[validators]]\npublic_key = \"{}\"\nstake = 1000000\n\n",
            hex::encode(test_secret(byte).public_key().encode()),
        ));
    }
    toml.push_str("[[workchains]]\nid = -1\nname = \"masterchain\"\nenabled = true\n");
    let path = dir.join("genesis.toml");
    std::fs::write(&path, toml).unwrap();
    path
}

#[test]
fn four_validators_finalize_same_block() {
    let root = std::env::temp_dir().join(format!("onx-m6-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let genesis_path = write_four_validator_genesis(&root);
    // Init the genesis doc once; each node gets its own store.
    let config = onx_genesis::parse_config(&genesis_path).expect("genesis parses");
    let doc = onx_genesis::build_genesis_document(&config).expect("genesis builds");

    let ports: Vec<u16> = (0..4).map(|i| 19001 + i).collect();
    let mut handles = Vec::new();
    let mut shutdowns = Vec::new();
    let mut stores = Vec::new();
    let mut chain_id_opt = None;

    for i in 0..4 {
        let node_dir = root.join(format!("node{i}"));
        std::fs::create_dir_all(&node_dir).unwrap();
        let store = ChainStore::open(node_dir.join("db")).unwrap();
        store.init_genesis(&doc).unwrap();
        let chain_id = store.load_state().unwrap().unwrap().chain_id;
        if chain_id_opt.is_none() {
            chain_id_opt = Some(chain_id);
        }

        let tx_pool_dir = node_dir.join("txpool");
        std::fs::create_dir_all(tx_pool_dir.join("pending")).unwrap();
        std::fs::create_dir_all(tx_pool_dir.join("rejected")).unwrap();
        let blocks_dir = node_dir.join("blocks");
        std::fs::create_dir_all(&blocks_dir).unwrap();

        let fee_collector = AccountId::from_bytes([0xfe; 32]);
        let mempool = Mempool::new(&tx_pool_dir, 1000, chain_id, fee_collector).unwrap();

        let peers: Vec<std::net::SocketAddr> = ports
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, p)| format!("127.0.0.1:{p}").parse().unwrap())
            .collect();
        let cfg = ProducerConfig {
            fee_collector,
            poll_interval: Duration::from_millis(100),
            tx_pool_dir,
            blocks_dir,
            telemetry: None,
            signing_key: Some(test_secret([0x11, 0x22, 0x33, 0x44][i])),
            consensus_bind: Some(format!("127.0.0.1:{}", ports[i]).parse().unwrap()),
            consensus_peers: peers,
        };
        let shutdown = Arc::new(AtomicBool::new(false));
        shutdowns.push(shutdown.clone());
        stores.push(node_dir.join("db"));

        let handle = std::thread::spawn(move || {
            run_producer_loop(store, mempool, cfg, shutdown)
        });
        handles.push(handle);
    }

    // Wait for all four consensus listeners to be up before submitting.
    let deadline = Instant::now() + Duration::from_secs(30);
    for port in &ports {
        loop {
            if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                break;
            }
            if Instant::now() > deadline {
                panic!("timeout waiting for consensus listener on {port}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    // Submit the transaction to ALL nodes (the leader is validator 0 by
    // pubkey sort, which isn't node 0; simplest is to fund every mempool).
    let chain_id = chain_id_opt.unwrap();
    let from = AccountId::from_bytes([0xaa; 32]);
    let funder_secret = test_secret(0xaa);
    for i in 0..4 {
        let msg = onx_stf::ExternalMessage::new_signed(
            chain_id,
            onx_stf::MsgKind::Transfer,
            from,
            0, // nonce — same tx on all nodes; only the leader's copy commits
            AccountId::from_bytes([0xbb; 32]),
            1_000,
            10_000,
            Vec::new(),
            [0u8; 32],
            &funder_secret,
        );
        let tx_path = root.join(format!("node{i}")).join("txpool").join(format!(
            "{}.msg",
            hex::encode(msg.hash())
        ));
        std::fs::write(&tx_path, msg.to_bytes()).unwrap();
    }

    // Wait for block 1 on all four nodes (via block files on disk).
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let mut all_have = true;
        for i in 0..4 {
            let block_file = root
                .join(format!("node{i}"))
                .join("blocks")
                .join(onx::blockfile::block_file_name(1));
            if !block_file.exists() {
                all_have = false;
                break;
            }
        }
        if all_have {
            break;
        }
        if Instant::now() > deadline {
            panic!("timeout waiting for block 1 on all nodes");
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    // All four finalized the SAME block: compare hashes from the files.
    let mut hashes = Vec::new();
    for i in 0..4 {
        let block_file = std::fs::read(
            root.join(format!("node{i}"))
                .join("blocks")
                .join(onx::blockfile::block_file_name(1)),
        )
        .unwrap();
        let signed = decode_block_file(&block_file).unwrap();
        hashes.push(signed.block.header.hash());
        // The block carries a quorum of signatures (≥3 of 4).
        assert!(
            signed.sig_entries.len() >= 3,
            "node {i}: quorum signatures, got {}",
            signed.sig_entries.len()
        );
    }
    for h in &hashes[1..] {
        assert_eq!(h, &hashes[0], "all validators agree on block 1");
    }

    // Shutdown.
    for s in &shutdowns {
        s.store(true, Ordering::Relaxed);
    }
    for h in handles {
        h.join().unwrap().unwrap();
    }
    let _ = std::fs::remove_dir_all(&root);
}
