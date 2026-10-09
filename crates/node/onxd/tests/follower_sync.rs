//! Follower sync integration tests (M5).
//!
//! The honest path drives the real producer loop (real signed blocks),
//! serves them over the in-memory loopback transport through the real
//! `SyncServer`, and follows them with `follow_next_block` — the same
//! step the daemon loop runs. The adversarial path uses the
//! `testutil::rogue` servers: forged signatures, corrupt files,
//! wrong-seqno and wrong-chain blocks must be rejected (never committed,
//! never skipped), a mid-transfer disconnect must time out cleanly, and a
//! fork at a committed seqno must halt the node.

use onx::blockfile::encode_block_file;
use onx_data_structures::AccountId;
use onx_networking::testutil::{rogue, LoopbackTransport};
use onx_networking::{SyncClient, SyncConfig, SyncPeer, SyncServer};
use onx_primitives::{SecretKey, Uint256};
use onx_stf::block::PROTOCOL_VERSION;
use onx_stf::{ExternalMessage, MsgKind, SigEntry, State};
use onx_storage::ChainStore;
use onxd::follower::{
    follow_next_block, parse_sync_peer, FollowError, FollowStep, FollowerConfig, FollowerContext,
};
use onxd::mempool::Mempool;
use onxd::producer::{run_producer_loop, ProducerConfig};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

// ---------- helpers ----------

fn test_secret(byte: u8) -> SecretKey {
    SecretKey::from_seed(&[byte; 32]).expect("fixed test seed decodes")
}

fn account_id(byte: u8) -> AccountId {
    AccountId::from_bytes([byte; 32])
}

fn addr_hex(byte: u8) -> String {
    hex::encode([byte; 32])
}

fn write_genesis_toml(dir: &Path, validator_byte: u8) -> PathBuf {
    let mut toml = String::from("# test genesis — fixed, deterministic\n");
    for i in 0..4u8 {
        let byte = 0xaa + i;
        let pubkey_hex = hex::encode(test_secret(byte).public_key().encode());
        toml.push_str(&format!(
            "[[balances]]\naddress = \"{}\"\namount = 1000000000000\npublic_key = \"{}\"\n\n",
            addr_hex(byte),
            pubkey_hex
        ));
    }
    toml.push_str(&format!(
        "[[validators]]\npublic_key = \"{}\"\nstake = 1000\n\n",
        hex::encode(test_secret(validator_byte).public_key().encode())
    ));
    toml.push_str("[[workchains]]\nid = -1\nname = \"masterchain\"\nenabled = true\n");
    let path = dir.join("genesis.toml");
    std::fs::write(&path, toml).unwrap();
    path
}

fn sign_msg(
    chain_id: [u8; 32],
    from_byte: u8,
    to_byte: u8,
    amount: u128,
    fee: u128,
    nonce: u64,
) -> ExternalMessage {
    ExternalMessage::new_signed(
        chain_id,
        MsgKind::Transfer,
        account_id(from_byte),
        nonce,
        account_id(to_byte),
        amount,
        fee,
        Vec::new(),
        [0u8; 32],
        &test_secret(from_byte),
    )
}

/// Tracks per-sender nonces and signs messages like a wallet.
struct TestWallet {
    chain_id: [u8; 32],
    nonces: std::collections::BTreeMap<AccountId, u64>,
}

impl TestWallet {
    fn new(chain_id: [u8; 32]) -> Self {
        Self {
            chain_id,
            nonces: std::collections::BTreeMap::new(),
        }
    }

    fn sign(&mut self, from_byte: u8, to_byte: u8, amount: u128, fee: u128) -> ExternalMessage {
        let from = account_id(from_byte);
        let nonce = self.nonces.get(&from).copied().unwrap_or(0);
        self.nonces.insert(from, nonce + 1);
        sign_msg(self.chain_id, from_byte, to_byte, amount, fee, nonce)
    }
}

/// Producer side: real store, real producer loop in a thread, real signed
/// block files.
struct ProducerHarness {
    dir: PathBuf,
    genesis_toml: PathBuf,
    store: Option<ChainStore>,
    tx_pool_dir: PathBuf,
    fee_collector: AccountId,
    chain_id: [u8; 32],
    validator_byte: u8,
}

impl ProducerHarness {
    fn new(name: &str, validator_byte: u8) -> Self {
        let dir =
            std::env::temp_dir().join(format!("onxd-follower-prod-{name}-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        let data_dir = dir.join("data");
        let tx_pool_dir = dir.join("txpool");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&tx_pool_dir).unwrap();

        let genesis_toml = write_genesis_toml(&dir, validator_byte);
        let config = onx_genesis::parse_config(&genesis_toml).unwrap();
        let doc = onx_genesis::build_genesis_document(&config).unwrap();
        let chain_id = doc.genesis_hash();
        let store = ChainStore::open(data_dir.join("chain.redb")).unwrap();
        store.init_genesis(&doc).unwrap();

        Self {
            dir,
            genesis_toml,
            store: Some(store),
            tx_pool_dir,
            fee_collector: account_id(0xaa),
            chain_id,
            validator_byte,
        }
    }

    fn blocks_dir(&self) -> PathBuf {
        self.dir.join("data").join("blocks")
    }

    fn drop_msg(&self, msg: &ExternalMessage) {
        let name = format!("{}.msg", hex::encode(msg.hash()));
        std::fs::write(self.tx_pool_dir.join(name), msg.to_bytes()).unwrap();
    }

    /// Run the real producer loop in a thread until `target_blocks` block
    /// files exist. Returns the join handle and shutdown flag.
    fn run_until(
        &mut self,
        target_blocks: usize,
        timeout: Duration,
    ) -> (
        std::thread::JoinHandle<Result<onxd::producer::ProducerStats, String>>,
        Arc<AtomicBool>,
    ) {
        let store = self.store.take().expect("store already taken");
        let cfg = ProducerConfig {
            fee_collector: self.fee_collector,
            poll_interval: Duration::from_millis(25),
            tx_pool_dir: self.tx_pool_dir.clone(),
            blocks_dir: self.blocks_dir(),
            telemetry: None,
            signing_key: Some(test_secret(self.validator_byte)),
        };
        let pool_dir = self.tx_pool_dir.clone();
        let chain_id = self.chain_id;
        let fee_collector = self.fee_collector;
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = shutdown.clone();
        let handle = std::thread::spawn(move || {
            let mempool = Mempool::new(&pool_dir, 10_000, chain_id, fee_collector).unwrap();
            run_producer_loop(store, mempool, cfg, flag)
        });
        let blocks_dir = self.blocks_dir();
        let start = Instant::now();
        loop {
            let count = std::fs::read_dir(&blocks_dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| e.path().extension().map(|x| x == "blk").unwrap_or(false))
                        .count()
                })
                .unwrap_or(0);
            if count >= target_blocks {
                break;
            }
            if start.elapsed() > timeout {
                shutdown.store(true, Ordering::Relaxed);
                let _ = handle.join();
                panic!("timed out waiting for {target_blocks} blocks");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        (handle, shutdown)
    }

    fn stop(
        handle: std::thread::JoinHandle<Result<onxd::producer::ProducerStats, String>>,
        shutdown: Arc<AtomicBool>,
    ) {
        shutdown.store(true, Ordering::Relaxed);
        handle
            .join()
            .expect("producer thread")
            .expect("producer ok");
    }

    fn reopen(&mut self) {
        assert!(self.store.is_none(), "store already open");
        self.store = Some(ChainStore::open(self.dir.join("data").join("chain.redb")).unwrap());
    }

    fn final_root_hex(&self) -> String {
        let store = self.store.as_ref().expect("store taken");
        let (seqno, _) = store.head().unwrap().expect("chain has a head");
        hex::encode(store.state_root_at(seqno).unwrap().expect("root"))
    }
}

impl Drop for ProducerHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Follower side: fresh store on the SAME genesis, its own blocks dir.
struct FollowerHarness {
    dir: PathBuf,
    store: ChainStore,
    blocks_dir: PathBuf,
}

impl FollowerHarness {
    fn new(name: &str, genesis_toml: &Path) -> Self {
        let dir =
            std::env::temp_dir().join(format!("onxd-follower-fol-{name}-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        let data_dir = dir.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let config = onx_genesis::parse_config(genesis_toml).unwrap();
        let doc = onx_genesis::build_genesis_document(&config).unwrap();
        let store = ChainStore::open(data_dir.join("chain.redb")).unwrap();
        store.init_genesis(&doc).unwrap();
        let blocks_dir = data_dir.join("blocks");
        std::fs::create_dir_all(&blocks_dir).unwrap();
        Self {
            dir,
            store,
            blocks_dir,
        }
    }

    fn head_seqno(&self) -> u32 {
        self.store.head().unwrap().map(|(s, _)| s).unwrap_or(0)
    }

    fn final_root_hex(&self) -> String {
        let (seqno, _) = self.store.head().unwrap().expect("chain has a head");
        hex::encode(self.store.state_root_at(seqno).unwrap().expect("root"))
    }
}

impl Drop for FollowerHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn test_sync_config() -> SyncConfig {
    SyncConfig {
        fetch_timeout: Duration::from_millis(800),
        ack_timeout: Duration::from_millis(50),
        ..SyncConfig::default()
    }
}

fn test_follower_config(blocks_dir: PathBuf) -> FollowerConfig {
    FollowerConfig {
        blocks_dir,
        poll_interval: Duration::from_millis(10),
        sync: test_sync_config(),
        telemetry: None,
    }
}

fn sync_peer(address: Uint256, secret: &SecretKey) -> SyncPeer {
    SyncPeer {
        public_key: secret.public_key(),
        endpoint: "127.0.0.1:0".parse().unwrap(),
        address,
    }
}

/// Build a follower client + context against a fresh follower harness.
fn follower_parts(
    fh: &FollowerHarness,
    client_transport: LoopbackTransport,
    server_secret: &SecretKey,
    server_addr: Uint256,
) -> (
    SyncClient<LoopbackTransport>,
    FollowerConfig,
    FollowerContext,
) {
    let client = SyncClient::new(
        client_transport,
        sync_peer(server_addr, server_secret),
        test_sync_config(),
    );
    let cfg = test_follower_config(fh.blocks_dir.clone());
    let ctx = FollowerContext::load(&fh.store).expect("follower context loads");
    (client, cfg, ctx)
}

/// Drive follow_next_block until the follower's head reaches `target`.
/// Panics on any rejection or fatal error — the honest path must be clean.
async fn follow_until(
    fh: &FollowerHarness,
    client: &SyncClient<LoopbackTransport>,
    cfg: &FollowerConfig,
    ctx: &FollowerContext,
    target: u32,
    timeout: Duration,
) {
    let start = Instant::now();
    loop {
        match follow_next_block(&fh.store, client, cfg, ctx).await {
            Ok(FollowStep::Synced { .. }) => {}
            Ok(FollowStep::Waiting) => {}
            Err(FollowError::Retryable(e)) => panic!("honest follow rejected: {e}"),
            Err(FollowError::Fatal(e)) => panic!("honest follow fatal: {e}"),
        }
        if fh.head_seqno() >= target {
            break;
        }
        if start.elapsed() > timeout {
            panic!("timed out waiting for follower to reach seqno {target}");
        }
        sleep(Duration::from_millis(10)).await;
    }
}

// ---------- honest path ----------

/// M5 acceptance, in-process: a fresh follower syncs two honestly produced
/// blocks from genesis through the real sync server, verifies everything
/// itself, and reaches the producer's exact state root. Its block files
/// are byte-identical to the producer's (it writes the fetched bytes).
#[tokio::test]
async fn follower_syncs_from_genesis_and_matches_producer_root() {
    let mut prod = ProducerHarness::new("honest", 0x11);
    let mut wallet = TestWallet::new(prod.chain_id);
    for _ in 0..3 {
        prod.drop_msg(&wallet.sign(0xaa, 0xab, 1000, 10));
    }
    let (handle, shutdown) = prod.run_until(1, Duration::from_secs(20));
    for _ in 0..2 {
        prod.drop_msg(&wallet.sign(0xab, 0xac, 500, 5));
    }
    // Wait for the second block, then stop the producer.
    let start = Instant::now();
    loop {
        let count = std::fs::read_dir(prod.blocks_dir())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .ok()
                    .map(|e| e.path().extension().map(|x| x == "blk").unwrap_or(false))
                    .unwrap_or(false)
            })
            .count();
        if count >= 2 || start.elapsed() > Duration::from_secs(20) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    ProducerHarness::stop(handle, shutdown);
    prod.reopen();
    let producer_root = prod.final_root_hex();

    let fol = FollowerHarness::new("honest", &prod.genesis_toml);

    let addr_server = Uint256([0xA1; 32]);
    let addr_client = Uint256([0xB2; 32]);
    let server_secret = test_secret(0x31);
    let client_secret = test_secret(0x32);
    let (server_tx, client_tx) = LoopbackTransport::pair(addr_server, addr_client);
    let server = SyncServer::new(
        server_tx,
        vec![sync_peer(addr_client, &client_secret)],
        prod.blocks_dir(),
        test_sync_config(),
    );
    let server_task = tokio::spawn(async move { server.run().await });

    let (client, cfg, ctx) = follower_parts(&fol, client_tx, &server_secret, addr_server);
    follow_until(&fol, &client, &cfg, &ctx, 2, Duration::from_secs(20)).await;
    server_task.abort();

    assert_eq!(fol.head_seqno(), 2);
    assert_eq!(
        fol.final_root_hex(),
        producer_root,
        "follower reaches the producer's exact state root"
    );
    // Byte-identical block files: the follower writes the fetched bytes.
    for seqno in 1..=2u32 {
        let name = format!("block-{seqno:08}.blk");
        let a = std::fs::read(prod.blocks_dir().join(&name)).unwrap();
        let b = std::fs::read(fol.blocks_dir.join(&name)).unwrap();
        assert_eq!(
            a, b,
            "follower's {name} is byte-identical to the producer's"
        );
    }
}

/// Kill -9 resume, in-process: follow block 1, then simulate the kill
/// window — delete the block file AFTER the atomic commit (a kill between
/// commit and file write), drop the store ("kill"), and restart through
/// the real `run_follower_loop`: startup regeneration must repair the
/// missing file from the DB, the loop must resume at block 2, and the
/// roots must converge.
#[tokio::test]
async fn follower_resumes_after_restart() {
    use onxd::follower::run_follower_loop;

    let mut prod = ProducerHarness::new("resume", 0x11);
    let mut wallet = TestWallet::new(prod.chain_id);
    prod.drop_msg(&wallet.sign(0xaa, 0xab, 1000, 10));
    let (handle, shutdown) = prod.run_until(1, Duration::from_secs(20));
    prod.drop_msg(&wallet.sign(0xab, 0xac, 500, 5));
    let start = Instant::now();
    loop {
        let count = std::fs::read_dir(prod.blocks_dir())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .ok()
                    .map(|e| e.path().extension().map(|x| x == "blk").unwrap_or(false))
                    .unwrap_or(false)
            })
            .count();
        if count >= 2 || start.elapsed() > Duration::from_secs(20) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    ProducerHarness::stop(handle, shutdown);
    prod.reopen();
    let producer_root = prod.final_root_hex();

    // Phase 1: follow block 1, then simulate the kill window — the file
    // is deleted AFTER the atomic commit, exactly as a kill between the
    // two would leave it. Then drop the store ("kill").
    let fol_dir = std::env::temp_dir().join(format!("onxd-fol-resume-{}", std::process::id()));
    if fol_dir.exists() {
        std::fs::remove_dir_all(&fol_dir).unwrap();
    }
    let data_dir = fol_dir.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = onx_genesis::parse_config(&prod.genesis_toml).unwrap();
    let doc = onx_genesis::build_genesis_document(&config).unwrap();
    let blocks_dir = data_dir.join("blocks");
    std::fs::create_dir_all(&blocks_dir).unwrap();

    let addr_server = Uint256([0xA1; 32]);
    let addr_client = Uint256([0xB2; 32]);
    let server_secret = test_secret(0x31);
    let client_secret = test_secret(0x32);

    {
        let store = ChainStore::open(data_dir.join("chain.redb")).unwrap();
        store.init_genesis(&doc).unwrap();
        let (server_tx, client_tx) = LoopbackTransport::pair(addr_server, addr_client);
        let server = SyncServer::new(
            server_tx,
            vec![sync_peer(addr_client, &client_secret)],
            prod.blocks_dir(),
            test_sync_config(),
        );
        let server_task = tokio::spawn(async move { server.run().await });
        let (client, cfg, ctx) = (
            SyncClient::new(
                client_tx,
                sync_peer(addr_server, &server_secret),
                test_sync_config(),
            ),
            test_follower_config(blocks_dir.clone()),
            FollowerContext::load(&store).unwrap(),
        );
        let step = follow_next_block(&store, &client, &cfg, &ctx)
            .await
            .expect("block 1 follows");
        assert!(
            matches!(step, FollowStep::Synced { seqno: 1 }),
            "first step syncs block 1, got {step:?}"
        );
        assert!(blocks_dir.join("block-00000001.blk").is_file());
        server_task.abort();
        // The kill window: block 1 is committed, its file is gone.
        std::fs::remove_file(blocks_dir.join("block-00000001.blk")).unwrap();
        // `store` and `client` drop here: the "kill".
    }

    // Phase 2: "restart" through the real loop. Startup regeneration must
    // repair block-00000001.blk from the DB, then block 2 syncs normally.
    let store = ChainStore::open(data_dir.join("chain.redb")).unwrap();
    assert_eq!(
        store.head().unwrap().map(|(s, _)| s),
        Some(1),
        "the kill left block 1 committed"
    );
    assert!(
        !blocks_dir.join("block-00000001.blk").is_file(),
        "test premise: the file is missing after the kill"
    );
    let (server_tx, client_tx) = LoopbackTransport::pair(addr_server, addr_client);
    let server = SyncServer::new(
        server_tx,
        vec![sync_peer(addr_client, &client_secret)],
        prod.blocks_dir(),
        test_sync_config(),
    );
    let server_task = tokio::spawn(async move { server.run().await });
    let client = SyncClient::new(
        client_tx,
        sync_peer(addr_server, &server_secret),
        test_sync_config(),
    );
    let cfg = test_follower_config(blocks_dir.clone());
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = shutdown.clone();
    let follower_task =
        tokio::spawn(async move { run_follower_loop(store, client, cfg, flag).await });
    // Wait for block 2's file (observing the blocks dir avoids the locked
    // redb file, same trick as the producer harness).
    let start = Instant::now();
    loop {
        if blocks_dir.join("block-00000002.blk").is_file()
            || start.elapsed() > Duration::from_secs(20)
        {
            break;
        }
        sleep(Duration::from_millis(25)).await;
    }
    shutdown.store(true, Ordering::Relaxed);
    let stats = follower_task
        .await
        .expect("follower task")
        .expect("follower ok");
    server_task.abort();
    assert!(
        blocks_dir.join("block-00000001.blk").is_file(),
        "startup regenerated the missing block-1 file from the DB"
    );
    assert!(
        blocks_dir.join("block-00000002.blk").is_file(),
        "block 2 synced after resume"
    );
    assert!(stats.blocks_synced >= 1);

    // Roots converge.
    let store = ChainStore::open(data_dir.join("chain.redb")).unwrap();
    let (seqno, _) = store.head().unwrap().expect("head");
    assert_eq!(seqno, 2);
    let root = hex::encode(store.state_root_at(seqno).unwrap().expect("root"));
    assert_eq!(
        root, producer_root,
        "resumed follower converges to the producer root"
    );

    let _ = std::fs::remove_dir_all(&fol_dir);
}

// ---------- adversarial ----------

/// Forged signature: the block file is well-formed but the signature
/// section is tampered with. Auth must reject it; nothing is committed;
/// the head does not move.
#[tokio::test]
async fn follower_rejects_forged_signature() {
    let mut prod = ProducerHarness::new("forged", 0x11);
    let mut wallet = TestWallet::new(prod.chain_id);
    prod.drop_msg(&wallet.sign(0xaa, 0xab, 1000, 10));
    let (handle, shutdown) = prod.run_until(1, Duration::from_secs(20));
    ProducerHarness::stop(handle, shutdown);

    let fol = FollowerHarness::new("forged", &prod.genesis_toml);

    // Tamper one signature byte: magic(8) + header(160) + count(4) +
    // index(4) = 176; the 64-byte sig follows.
    let mut bad = std::fs::read(prod.blocks_dir().join("block-00000001.blk")).unwrap();
    bad[200] ^= 0xff;

    let addr_server = Uint256([0xA1; 32]);
    let addr_client = Uint256([0xB2; 32]);
    let server_secret = test_secret(0x31);
    let client_secret = test_secret(0x32);
    let (server_tx, client_tx) = LoopbackTransport::pair(addr_server, addr_client);
    let client_peer = sync_peer(addr_client, &client_secret);
    let rogue_task = tokio::spawn(async move {
        rogue::serve_one(&server_tx, &client_peer, &bad).await;
        // Hold the transport open: a rogue peer sends bad data — it
        // doesn't close its socket (UDP has no close). Dropping the
        // transport would fail the client's ack sends, which is stricter
        // than the real network.
        std::future::pending::<()>().await;
    });

    let (client, cfg, ctx) = follower_parts(&fol, client_tx, &server_secret, addr_server);
    let err = follow_next_block(&fol.store, &client, &cfg, &ctx)
        .await
        .expect_err("forged signature must be rejected");
    assert!(
        matches!(err, FollowError::Retryable(_)),
        "forgery is retryable, not fatal: {err:?}"
    );
    assert!(
        err.to_string().contains("auth failed"),
        "rejection names the auth layer: {err}"
    );
    assert_eq!(fol.head_seqno(), 0, "nothing committed");
    rogue_task.abort();
}

/// Corrupted block file: bad magic. Strict decode rejects it before any
/// crypto runs.
#[tokio::test]
async fn follower_rejects_corrupted_block_file() {
    let mut prod = ProducerHarness::new("corrupt", 0x11);
    let mut wallet = TestWallet::new(prod.chain_id);
    prod.drop_msg(&wallet.sign(0xaa, 0xab, 1000, 10));
    let (handle, shutdown) = prod.run_until(1, Duration::from_secs(20));
    ProducerHarness::stop(handle, shutdown);

    let fol = FollowerHarness::new("corrupt", &prod.genesis_toml);

    let mut bad = std::fs::read(prod.blocks_dir().join("block-00000001.blk")).unwrap();
    bad[0] ^= 0xff; // break the ONXBLK05 magic

    let addr_server = Uint256([0xA1; 32]);
    let addr_client = Uint256([0xB2; 32]);
    let server_secret = test_secret(0x31);
    let client_secret = test_secret(0x32);
    let (server_tx, client_tx) = LoopbackTransport::pair(addr_server, addr_client);
    let client_peer = sync_peer(addr_client, &client_secret);
    let rogue_task = tokio::spawn(async move {
        rogue::serve_one(&server_tx, &client_peer, &bad).await;
        // Hold the transport open (see the forged-signature test).
        std::future::pending::<()>().await;
    });

    let (client, cfg, ctx) = follower_parts(&fol, client_tx, &server_secret, addr_server);
    let err = follow_next_block(&fol.store, &client, &cfg, &ctx)
        .await
        .expect_err("corrupt file must be rejected");
    assert!(
        matches!(err, FollowError::Retryable(_)),
        "corruption is retryable, not fatal: {err:?}"
    );
    assert_eq!(fol.head_seqno(), 0, "nothing committed");
    rogue_task.abort();
}

/// Wrong seqno: the peer serves block 2's file when asked for block 1.
/// The seqno binding rejects it — a peer serving another block's file for
/// this seqno is Byzantine, not helpful.
#[tokio::test]
async fn follower_rejects_wrong_seqno_block() {
    let mut prod = ProducerHarness::new("wrongseq", 0x11);
    let mut wallet = TestWallet::new(prod.chain_id);
    prod.drop_msg(&wallet.sign(0xaa, 0xab, 1000, 10));
    let (handle, shutdown) = prod.run_until(1, Duration::from_secs(20));
    ProducerHarness::stop(handle, shutdown);
    prod.reopen();
    // Commit block 2 directly so the file exists to be mis-served.
    let state: State = prod.store.as_ref().unwrap().load_state().unwrap().unwrap();
    let msg = wallet.sign(0xaa, 0xab, 500, 5);
    let parent_time = {
        let h1 = prod
            .store
            .as_ref()
            .unwrap()
            .block_hash_for_seqno(1)
            .unwrap()
            .expect("block 1");
        prod.store
            .as_ref()
            .unwrap()
            .get_block_header(&h1)
            .unwrap()
            .expect("header")
            .block_time
    };
    let block = onx_stf::propose_block(
        &state,
        vec![msg],
        state.last_lt + 1,
        prod.fee_collector,
        PROTOCOL_VERSION,
        parent_time,
    )
    .unwrap();
    let preimage = block.header.sign_bytes(&prod.chain_id);
    let sig = test_secret(0x11).sign_raw(&preimage).encode();
    let file = encode_block_file(
        &block,
        &[SigEntry {
            validator_index: 0,
            sig,
        }],
    );
    // Sanity: this is a REAL block 2 (it verifies).
    assert_eq!(block.header.seqno, 2);

    let fol = FollowerHarness::new("wrongseq", &prod.genesis_toml);

    let addr_server = Uint256([0xA1; 32]);
    let addr_client = Uint256([0xB2; 32]);
    let server_secret = test_secret(0x31);
    let client_secret = test_secret(0x32);
    let (server_tx, client_tx) = LoopbackTransport::pair(addr_server, addr_client);
    let client_peer = sync_peer(addr_client, &client_secret);
    let rogue_task = tokio::spawn(async move {
        // The follower asks for block 1; the rogue answers with block 2.
        rogue::serve_one(&server_tx, &client_peer, &file).await;
        // Hold the transport open (see the forged-signature test).
        std::future::pending::<()>().await;
    });

    let (client, cfg, ctx) = follower_parts(&fol, client_tx, &server_secret, addr_server);
    let err = follow_next_block(&fol.store, &client, &cfg, &ctx)
        .await
        .expect_err("wrong-seqno block must be rejected");
    assert!(
        matches!(err, FollowError::Retryable(_)),
        "wrong seqno is retryable, not fatal: {err:?}"
    );
    assert!(
        err.to_string().contains("seqno"),
        "rejection names the seqno binding: {err}"
    );
    assert_eq!(fol.head_seqno(), 0, "nothing committed");
    rogue_task.abort();
}

/// Wrong chain: a block honestly produced and signed under a DIFFERENT
/// genesis (different chain id). The signature cannot verify against this
/// chain's id — cross-chain replay is dead on arrival.
#[tokio::test]
async fn follower_rejects_wrong_chain_block() {
    // Chain B: different validator key → different genesis hash.
    let mut prod_b = ProducerHarness::new("wrongchain-b", 0x12);
    let mut wallet_b = TestWallet::new(prod_b.chain_id);
    prod_b.drop_msg(&wallet_b.sign(0xaa, 0xab, 1000, 10));
    let (handle, shutdown) = prod_b.run_until(1, Duration::from_secs(20));
    ProducerHarness::stop(handle, shutdown);
    let block_b = std::fs::read(prod_b.blocks_dir().join("block-00000001.blk")).unwrap();

    // Chain A: the follower's chain (validator 0x11).
    let prod_a = ProducerHarness::new("wrongchain-a", 0x11);
    assert_ne!(
        prod_a.chain_id, prod_b.chain_id,
        "test premise: distinct chains"
    );
    let fol = FollowerHarness::new("wrongchain", &prod_a.genesis_toml);

    let addr_server = Uint256([0xA1; 32]);
    let addr_client = Uint256([0xB2; 32]);
    let server_secret = test_secret(0x31);
    let client_secret = test_secret(0x32);
    let (server_tx, client_tx) = LoopbackTransport::pair(addr_server, addr_client);
    let client_peer = sync_peer(addr_client, &client_secret);
    let rogue_task = tokio::spawn(async move {
        rogue::serve_one(&server_tx, &client_peer, &block_b).await;
        // Hold the transport open (see the forged-signature test).
        std::future::pending::<()>().await;
    });

    let (client, cfg, ctx) = follower_parts(&fol, client_tx, &server_secret, addr_server);
    let err = follow_next_block(&fol.store, &client, &cfg, &ctx)
        .await
        .expect_err("wrong-chain block must be rejected");
    assert!(
        matches!(err, FollowError::Retryable(_)),
        "wrong chain is retryable, not fatal: {err:?}"
    );
    assert!(
        err.to_string().contains("auth failed"),
        "rejection names the auth layer: {err}"
    );
    assert_eq!(fol.head_seqno(), 0, "nothing committed");
    rogue_task.abort();
}

/// Disconnect mid-transfer: the rogue sends the first RLDP frames of a
/// multi-frame response, then goes silent. The fetch must time out
/// (Waiting — not hang, not accept a partial block), and the follower
/// must sync cleanly once the honest peer answers.
#[tokio::test]
async fn follower_survives_mid_transfer_disconnect() {
    let mut prod = ProducerHarness::new("disconnect", 0x11);
    let mut wallet = TestWallet::new(prod.chain_id);
    prod.drop_msg(&wallet.sign(0xaa, 0xab, 1000, 10));
    let (handle, shutdown) = prod.run_until(1, Duration::from_secs(20));
    ProducerHarness::stop(handle, shutdown);
    prod.reopen();
    let producer_root = prod.final_root_hex();

    let fol = FollowerHarness::new("disconnect", &prod.genesis_toml);
    let block_1 = std::fs::read(prod.blocks_dir().join("block-00000001.blk")).unwrap();

    let addr_server = Uint256([0xA1; 32]);
    let addr_client = Uint256([0xB2; 32]);
    let server_secret = test_secret(0x31);
    let client_secret = test_secret(0x32);

    // Phase 1: rogue sends 2 of many RLDP frames (the rogue uses a tiny
    // chunk size, so even a 1KB block is multi-frame), then silence.
    let (server_tx, client_tx) = LoopbackTransport::pair(addr_server, addr_client);
    let client_peer = sync_peer(addr_client, &client_secret);
    let rogue_task = tokio::spawn(async move {
        rogue::serve_partial(&server_tx, &client_peer, &block_1, 2).await;
        // Go silent WITHOUT dropping the transport: the peer disconnected
        // mid-transfer. (Dropping would close the inbox and fail the
        // client's ack sends — a real UDP peer just goes quiet.)
        std::future::pending::<()>().await;
    });
    let (client, cfg, ctx) = follower_parts(&fol, client_tx, &server_secret, addr_server);
    let step = follow_next_block(&fol.store, &client, &cfg, &ctx)
        .await
        .expect("disconnect must not error");
    assert!(
        matches!(step, FollowStep::Waiting),
        "mid-transfer disconnect surfaces as Waiting, got {step:?}"
    );
    assert_eq!(fol.head_seqno(), 0, "no partial block committed");
    rogue_task.abort();

    // Phase 2: the honest peer answers; the follower syncs cleanly.
    let (server_tx, client_tx) = LoopbackTransport::pair(addr_server, addr_client);
    let server = SyncServer::new(
        server_tx,
        vec![sync_peer(addr_client, &client_secret)],
        prod.blocks_dir(),
        test_sync_config(),
    );
    let server_task = tokio::spawn(async move { server.run().await });
    let (client, cfg, ctx) = follower_parts(&fol, client_tx, &server_secret, addr_server);
    follow_until(&fol, &client, &cfg, &ctx, 1, Duration::from_secs(20)).await;
    server_task.abort();
    assert_eq!(fol.final_root_hex(), producer_root);
}

/// Lying body: the header is honestly signed, but a message byte in the
/// body is tampered with, so the body's msgs_root no longer matches the
/// signed header. Auth passes (the header is intact); the STF's
/// re-execution catches the lie — `commit_block` rejects it, nothing is
/// committed.
#[tokio::test]
async fn follower_rejects_body_header_mismatch() {
    let mut prod = ProducerHarness::new("lyingbody", 0x11);
    let mut wallet = TestWallet::new(prod.chain_id);
    prod.drop_msg(&wallet.sign(0xaa, 0xab, 1000, 10));
    let (handle, shutdown) = prod.run_until(1, Duration::from_secs(20));
    ProducerHarness::stop(handle, shutdown);

    let fol = FollowerHarness::new("lyingbody", &prod.genesis_toml);

    // Tamper a body byte: magic(8) + header(160) + sig section(4 + 68)
    // = 240; the first message's encoding follows.
    let mut lying = std::fs::read(prod.blocks_dir().join("block-00000001.blk")).unwrap();
    lying[300] ^= 0xff;

    let addr_server = Uint256([0xA1; 32]);
    let addr_client = Uint256([0xB2; 32]);
    let server_secret = test_secret(0x31);
    let client_secret = test_secret(0x32);
    let (server_tx, client_tx) = LoopbackTransport::pair(addr_server, addr_client);
    let client_peer = sync_peer(addr_client, &client_secret);
    let rogue_task = tokio::spawn(async move {
        rogue::serve_one(&server_tx, &client_peer, &lying).await;
        // Hold the transport open (see the forged-signature test).
        std::future::pending::<()>().await;
    });

    let (client, cfg, ctx) = follower_parts(&fol, client_tx, &server_secret, addr_server);
    let err = follow_next_block(&fol.store, &client, &cfg, &ctx)
        .await
        .expect_err("body/header mismatch must be rejected");
    assert!(
        matches!(err, FollowError::Retryable(_)),
        "STF rejection is retryable, not fatal: {err:?}"
    );
    assert!(
        err.to_string().contains("STF rejected"),
        "rejection names the STF layer: {err}"
    );
    assert_eq!(fol.head_seqno(), 0, "nothing committed");
    rogue_task.abort();
}

/// No new block: the peer's blocks dir is empty, so the fetch times out.
/// The follower waits — no error, no rejection, head unmoved.
#[tokio::test]
async fn follower_waits_when_peer_has_no_new_block() {
    let prod = ProducerHarness::new("waiting", 0x11);
    // No messages dropped: the producer never makes a block.
    let fol = FollowerHarness::new("waiting", &prod.genesis_toml);

    let addr_server = Uint256([0xA1; 32]);
    let addr_client = Uint256([0xB2; 32]);
    let server_secret = test_secret(0x31);
    let client_secret = test_secret(0x32);
    let (server_tx, client_tx) = LoopbackTransport::pair(addr_server, addr_client);
    // Serve the producer's (empty) blocks dir: every fetch times out.
    let server = SyncServer::new(
        server_tx,
        vec![sync_peer(addr_client, &client_secret)],
        prod.blocks_dir(),
        test_sync_config(),
    );
    let server_task = tokio::spawn(async move { server.run().await });

    let (client, cfg, ctx) = follower_parts(&fol, client_tx, &server_secret, addr_server);
    let step = follow_next_block(&fol.store, &client, &cfg, &ctx)
        .await
        .expect("waiting must not error");
    assert!(
        matches!(step, FollowStep::Waiting),
        "empty peer surfaces as Waiting, got {step:?}"
    );
    assert_eq!(fol.head_seqno(), 0);
    server_task.abort();
}

// ---------- config ----------

#[test]
fn peer_descriptor_parsing() {
    let secret = test_secret(0x41);
    let pubkey_hex = hex::encode(secret.public_key().encode());
    let peer = parse_sync_peer(&format!("{pubkey_hex}@127.0.0.1:9001")).expect("valid descriptor");
    assert_eq!(peer.public_key, secret.public_key());
    assert_eq!(peer.endpoint.to_string(), "127.0.0.1:9001");
    // The address is derived from the key and pinned.
    let expected =
        onx_networking::KeyDescription::new_ed25519(secret.public_key()).compute_abstract_address();
    assert_eq!(peer.address, expected);

    // Hostnames resolve once at startup (localhost is in every /etc/hosts).
    let peer_host =
        parse_sync_peer(&format!("{pubkey_hex}@localhost:9001")).expect("hostname resolves");
    assert_eq!(peer_host.public_key, secret.public_key());
    assert_eq!(peer_host.endpoint.port(), 9001);
    assert!(peer_host.endpoint.ip().is_loopback());

    // Bad shapes fail closed.
    assert!(parse_sync_peer("no-at-sign").is_err());
    assert!(parse_sync_peer("zz@127.0.0.1:9001").is_err(), "non-hex key");
    assert!(
        parse_sync_peer(&format!("{}@127.0.0.1:9001", hex::encode([0u8; 31]))).is_err(),
        "short key"
    );
    assert!(
        parse_sync_peer(&format!("{pubkey_hex}@not-a-socket")).is_err(),
        "bad endpoint"
    );
    // Non-canonical ed25519 key (all zeros is not a valid Ristretto point
    // encoding... check the strict predicate rejects it).
    let zero_hex = hex::encode([0u8; 32]);
    assert!(
        parse_sync_peer(&format!("{zero_hex}@127.0.0.1:9001")).is_err(),
        "zero key rejected by the strict predicate"
    );
}

#[test]
fn follower_config_parses() {
    let cfg = onxd::parse_cli_args(&["onxd".to_string(), "--follower".to_string()]).unwrap();
    assert!(cfg.follower);
    let cfg = onxd::parse_cli_args(&["onxd".to_string()]).unwrap();
    assert!(!cfg.follower);

    let tmp = std::env::temp_dir().join(format!("onxd-follower-cfg-{}.toml", std::process::id()));
    std::fs::write(&tmp, "follower = true\nnode_key_path = \"/tmp/node.key\"\n").unwrap();
    let cfg = onxd::load_config(&tmp).unwrap();
    assert!(cfg.follower);
    assert_eq!(cfg.node_key_path.as_deref(), Some("/tmp/node.key"));
    let _ = std::fs::remove_file(tmp);
}
