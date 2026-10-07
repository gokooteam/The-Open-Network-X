//! Producer loop integration tests: mempool behavior, block production,
//! crash-safety, and end-to-end replay equivalence through the real
//! `onx replay` binary.
//!
//! All tests drive the real loop and the real store; no mocks.
//! Submissions are external messages dropped as `*.msg` files.

use onx_data_structures::AccountId;
use onx_primitives::SecretKey;
use onx_stf::{derive_address, propose_block, ExternalMessage, MsgKind, State};
use onx_storage::ChainStore;
use onxd::mempool::Mempool;
use onxd::producer::{run_producer_loop, ProducerConfig};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

fn write_genesis_toml(dir: &Path) -> PathBuf {
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
        // Real deterministic key: explicit hex validator keys must clear the
        // strict predicate (canonical, on-curve, large-order).
        hex::encode(test_secret(0x11).public_key().encode())
    ));
    toml.push_str("[[workchains]]\nid = -1\nname = \"masterchain\"\nenabled = true\n");
    let path = dir.join("genesis.toml");
    std::fs::write(&path, toml).unwrap();
    path
}

/// Sign one transfer message with an explicit nonce (the building block
/// the wallet uses).
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

/// Sign from an arbitrary address with an explicit revealed pubkey —
/// the shape of a first spend from a keyless (key-derived) account.
#[allow(clippy::too_many_arguments)] // test helper mirroring new_signed's shape
fn sign_msg_from(
    chain_id: [u8; 32],
    from: AccountId,
    to: AccountId,
    amount: u128,
    fee: u128,
    nonce: u64,
    pubkey: [u8; 32],
    secret: &SecretKey,
) -> ExternalMessage {
    ExternalMessage::new_signed(
        chain_id,
        MsgKind::Transfer,
        from,
        nonce,
        to,
        amount,
        fee,
        Vec::new(),
        pubkey,
        secret,
    )
}

/// Tracks per-sender nonces and signs messages like a wallet.
struct TestWallet {
    chain_id: [u8; 32],
    nonces: BTreeMap<AccountId, u64>,
}

impl TestWallet {
    fn new(chain_id: [u8; 32]) -> Self {
        Self {
            chain_id,
            nonces: BTreeMap::new(),
        }
    }

    fn sign(&mut self, from_byte: u8, to_byte: u8, amount: u128, fee: u128) -> ExternalMessage {
        let from = account_id(from_byte);
        let nonce = self.nonces.get(&from).copied().unwrap_or(0);
        self.nonces.insert(from, nonce + 1);
        sign_msg(self.chain_id, from_byte, to_byte, amount, fee, nonce)
    }
}

struct Harness {
    dir: PathBuf,
    genesis_toml: PathBuf,
    // Option so tests can move the store into the producer thread: redb
    // takes an exclusive file lock, so only one opener may exist at a time.
    store: Option<ChainStore>,
    tx_pool_dir: PathBuf,
    fee_collector: AccountId,
    chain_id: [u8; 32],
}

impl Harness {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("onxd-loop-test-{}-{}", name, std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        let data_dir = dir.join("data");
        let tx_pool_dir = dir.join("txpool");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&tx_pool_dir).unwrap();

        let genesis_toml = write_genesis_toml(&dir);
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
        }
    }

    fn store(&self) -> &ChainStore {
        self.store.as_ref().expect("store taken by producer thread")
    }

    /// Reopen the store after the producer thread has joined.
    fn reopen(&mut self) {
        assert!(self.store.is_none(), "store already open");
        self.store = Some(ChainStore::open(self.dir.join("data").join("chain.redb")).unwrap());
    }

    fn blocks_dir(&self) -> PathBuf {
        self.dir.join("data").join("blocks")
    }

    fn block_files(&self) -> usize {
        std::fs::read_dir(self.blocks_dir())
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().map(|x| x == "blk").unwrap_or(false))
                    .count()
            })
            .unwrap_or(0)
    }

    fn drop_msg(&self, msg: &ExternalMessage) {
        let name = format!("{}.msg", hex::encode(msg.hash()));
        std::fs::write(self.tx_pool_dir.join(name), msg.to_bytes()).unwrap();
    }

    fn head_seqno(&self) -> u32 {
        self.store().head().unwrap().map(|(s, _)| s).unwrap_or(0)
    }

    fn final_root_hex(&self) -> String {
        let (seqno, _) = self.store().head().unwrap().expect("chain has a head");
        hex::encode(self.store().state_root_at(seqno).unwrap().expect("root"))
    }

    fn producer_config(&self) -> ProducerConfig {
        ProducerConfig {
            fee_collector: self.fee_collector,
            poll_interval: Duration::from_millis(25),
            tx_pool_dir: self.tx_pool_dir.clone(),
            blocks_dir: self.dir.join("data").join("blocks"),
            telemetry: None,
        }
    }

    /// Move the store into the producer thread and run the loop until
    /// `target_blocks` block files exist (observing block files avoids
    /// opening the locked redb file from the test thread). Returns the
    /// join handle and shutdown flag; call `reopen()` after joining to
    /// inspect the store again.
    fn run_until(
        &mut self,
        mempool_max: usize,
        target_blocks: usize,
        timeout: Duration,
    ) -> (
        std::thread::JoinHandle<Result<onxd::producer::ProducerStats, String>>,
        Arc<AtomicBool>,
    ) {
        let store = self.store.take().expect("store already taken");
        let cfg = self.producer_config();
        let pool_dir = self.tx_pool_dir.clone();
        let blocks_dir = self.blocks_dir();
        let chain_id = self.chain_id;
        let fee_collector = self.fee_collector;
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = shutdown.clone();
        let handle = std::thread::spawn(move || {
            let mempool = Mempool::new(&pool_dir, mempool_max, chain_id, fee_collector).unwrap();
            run_producer_loop(store, mempool, cfg, flag)
        });
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

    fn wait_for_blocks(&self, target_blocks: usize, timeout: Duration) {
        let start = Instant::now();
        while self.block_files() < target_blocks {
            if start.elapsed() > timeout {
                panic!("timed out waiting for {target_blocks} blocks");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn account_nonce(store: &ChainStore, byte: u8) -> u64 {
    use onx_state_model::AccountState;
    match store.get_account(&account_id(byte)).unwrap() {
        Some(AccountState::Active { nonce, .. }) => nonce,
        other => panic!("expected active account, got {other:?}"),
    }
}

// ---------- mempool tests ----------

#[test]
fn mempool_accepts_valid_rejects_bad_signature() {
    let h = Harness::new("intake");
    let mut wallet = TestWallet::new(h.chain_id);
    let mut mempool = Mempool::new(&h.tx_pool_dir, 10_000, h.chain_id, h.fee_collector).unwrap();

    // Valid signed message → pending/.
    let good = wallet.sign(0xaa, 0xab, 1000, 10);
    h.drop_msg(&good);
    let stats = mempool.scan_drop_dir(&h.tx_pool_dir, h.store()).unwrap();
    assert_eq!(stats.accepted, 1);
    assert_eq!(mempool.len(), 1);
    assert!(h.tx_pool_dir.join("pending").read_dir().unwrap().count() == 1);

    // Tampered signature → rejected/.
    let mut evil = wallet.sign(0xaa, 0xab, 1000, 10);
    evil.signature[0] ^= 0xff;
    // Use a distinct filename so it isn't treated as a different message
    // hash colliding with pending (it has a different hash now).
    std::fs::write(h.tx_pool_dir.join("evil.msg"), evil.to_bytes()).unwrap();
    let stats = mempool.scan_drop_dir(&h.tx_pool_dir, h.store()).unwrap();
    assert_eq!(stats.rejected, 1);
    assert_eq!(mempool.len(), 1);
    assert_eq!(
        h.tx_pool_dir.join("rejected").read_dir().unwrap().count(),
        1
    );
}

#[test]
fn mempool_rejects_stale_nonce_and_dedupes() {
    let h = Harness::new("stale");
    let mut wallet = TestWallet::new(h.chain_id);
    let mut mempool = Mempool::new(&h.tx_pool_dir, 10_000, h.chain_id, h.fee_collector).unwrap();

    let msg0 = wallet.sign(0xaa, 0xab, 1000, 10);
    h.drop_msg(&msg0);
    // Exact duplicate bytes, different filename.
    std::fs::write(h.tx_pool_dir.join("copy.msg"), msg0.to_bytes()).unwrap();
    let stats = mempool.scan_drop_dir(&h.tx_pool_dir, h.store()).unwrap();
    assert_eq!(stats.accepted, 1);
    assert_eq!(stats.duplicates, 1);
    assert_eq!(mempool.len(), 1);

    // Stale nonce: hand-sign nonce 0 again after the wallet moved on.
    // (Simulate by signing directly with nonce 0.)
    // First advance the chain: commit a block spending nonce 0 via the store
    // directly, so the mempool message's nonce is stale at intake.
    // Hand-sign the filler (the wallet already handed out nonce 0 for msg0).
    let state: State = h.store().load_state().unwrap().unwrap();
    let filler = sign_msg(h.chain_id, 0xaa, 0xac, 100, 1, 0);
    let block = propose_block(
        &state,
        vec![filler],
        state.last_lt + 1,
        h.fee_collector,
        1,
        0,
    )
    .unwrap();
    h.store().commit_block(&state, &block).unwrap();
    assert_eq!(account_nonce(h.store(), 0xaa), 1);

    // Now drop a stale nonce-0 message (the wallet has moved on).
    let stale2 = sign_msg(h.chain_id, 0xaa, 0xab, 500, 5, 0);
    std::fs::write(h.tx_pool_dir.join("stale.msg"), stale2.to_bytes()).unwrap();
    let stats = mempool.scan_drop_dir(&h.tx_pool_dir, h.store()).unwrap();
    assert_eq!(stats.rejected, 1, "stale nonce must be rejected at intake");
    assert_eq!(mempool.len(), 1, "only the original message remains");
}

#[test]
fn mempool_holds_future_nonce_until_gap_fills() {
    let h = Harness::new("gapfill");
    let mut mempool = Mempool::new(&h.tx_pool_dir, 10_000, h.chain_id, h.fee_collector).unwrap();

    // Submit nonce 1 before nonce 0: held, not rejected.
    let msg1 = sign_msg(h.chain_id, 0xaa, 0xab, 1000, 10, 1);
    std::fs::write(h.tx_pool_dir.join("future.msg"), msg1.to_bytes()).unwrap();
    let stats = mempool.scan_drop_dir(&h.tx_pool_dir, h.store()).unwrap();
    assert_eq!(stats.accepted, 1);
    assert_eq!(mempool.len(), 1);

    // Selection with the gap unfilled yields nothing (no block).
    let state: State = h.store().load_state().unwrap().unwrap();
    let candidates = mempool.select_candidates(&state).unwrap();
    assert!(
        candidates.is_empty(),
        "gap must block the chain, not skip it"
    );

    // Now fill the gap: nonce 0 arrives.
    let msg0 = sign_msg(h.chain_id, 0xaa, 0xab, 500, 5, 0);
    std::fs::write(h.tx_pool_dir.join("gap.msg"), msg0.to_bytes()).unwrap();
    mempool.scan_drop_dir(&h.tx_pool_dir, h.store()).unwrap();
    let candidates = mempool.select_candidates(&state).unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].nonce, 0);
    assert_eq!(candidates[1].nonce, 1);
}

#[test]
fn mempool_rejects_when_full() {
    let h = Harness::new("full");
    let mut wallet = TestWallet::new(h.chain_id);
    let mut mempool = Mempool::new(&h.tx_pool_dir, 2, h.chain_id, h.fee_collector).unwrap();

    for _ in 0..2 {
        let msg = wallet.sign(0xaa, 0xab, 10, 1);
        h.drop_msg(&msg);
    }
    let stats = mempool.scan_drop_dir(&h.tx_pool_dir, h.store()).unwrap();
    assert_eq!(stats.accepted, 2);

    let extra = wallet.sign(0xaa, 0xab, 10, 1);
    h.drop_msg(&extra);
    let stats = mempool.scan_drop_dir(&h.tx_pool_dir, h.store()).unwrap();
    assert_eq!(stats.rejected, 1, "full mempool rejects new submissions");
    assert_eq!(mempool.len(), 2, "valid pending messages are never evicted");
}

#[test]
fn mempool_rejects_zero_amount_transfer_and_zero_fee_call() {
    // The two message shapes from the audit: correctly signed, so the old
    // signature-only filter accepted them, but `propose_block` rejects
    // them. Intake must now reject them too — and the honest message queued
    // alongside must survive.
    let h = Harness::new("zeroval");
    let mut wallet = TestWallet::new(h.chain_id);
    let mut mempool = Mempool::new(&h.tx_pool_dir, 10_000, h.chain_id, h.fee_collector).unwrap();

    let good = wallet.sign(0xaa, 0xab, 1000, 10);
    h.drop_msg(&good);

    // Zero-amount transfer (nonce 1: the wallet already handed out 0).
    let zero_amt = sign_msg(h.chain_id, 0xaa, 0xab, 0, 10, 1);
    std::fs::write(h.tx_pool_dir.join("zero_amt.msg"), zero_amt.to_bytes()).unwrap();

    // Zero-fee contract call (nonce 2).
    let zero_fee_call = ExternalMessage::new_signed(
        h.chain_id,
        MsgKind::ContractCall,
        account_id(0xaa),
        2,
        account_id(0xab),
        0,
        0,
        Vec::new(),
        [0u8; 32],
        &test_secret(0xaa),
    );
    std::fs::write(h.tx_pool_dir.join("zero_fee.msg"), zero_fee_call.to_bytes()).unwrap();

    let stats = mempool.scan_drop_dir(&h.tx_pool_dir, h.store()).unwrap();
    assert_eq!(stats.accepted, 1, "only the honest message is accepted");
    assert_eq!(
        stats.rejected, 2,
        "both defective messages rejected at intake"
    );
    assert_eq!(mempool.len(), 1, "honest message survives in pending/");

    // The survivor is still a valid block candidate.
    let state: State = h.store().load_state().unwrap().unwrap();
    let candidates = mempool.select_candidates(&state).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].hash(), good.hash());
}

// ---------- loop tests ----------

#[test]
fn loop_produces_blocks_advancing_head() {
    let mut h = Harness::new("produce");
    let mut wallet = TestWallet::new(h.chain_id);
    for _ in 0..3 {
        h.drop_msg(&wallet.sign(0xaa, 0xab, 1000, 10));
    }
    for _ in 0..2 {
        h.drop_msg(&wallet.sign(0xab, 0xac, 500, 5));
    }

    let (handle, shutdown) = h.run_until(10_000, 1, Duration::from_secs(20));
    shutdown.store(true, Ordering::Relaxed);
    let stats = handle
        .join()
        .expect("producer thread")
        .expect("producer ok");
    assert!(stats.blocks_produced >= 1);
    assert_eq!(stats.msgs_committed, 5, "all submitted messages committed");

    h.reopen();
    // Nonces advanced on-chain exactly as signed.
    assert_eq!(account_nonce(h.store(), 0xaa), 3);
    assert_eq!(account_nonce(h.store(), 0xab), 2);
    assert!(h.head_seqno() >= 1);

    // Block files were emitted for replay.
    assert!(h.blocks_dir().join("block-00000001.blk").is_file());
}

#[test]
fn loop_drops_stale_msg_without_crashing() {
    let mut h = Harness::new("staledrop");
    let mut wallet = TestWallet::new(h.chain_id);

    // Advance sender 0xaa's nonce out-of-band (direct store commit),
    // simulating state moving under the mempool.
    let state: State = h.store().load_state().unwrap().unwrap();
    let filler = wallet.sign(0xaa, 0xac, 100, 1); // nonce 0
    let block = propose_block(
        &state,
        vec![filler],
        state.last_lt + 1,
        h.fee_collector,
        1,
        0,
    )
    .unwrap();
    h.store().commit_block(&state, &block).unwrap();

    // Now drop a message that was valid when written but is stale now,
    // plus a good one.
    let stale = sign_msg(
        h.chain_id, 0xaa, 0xab, 500, 5, 0, // stale: chain is at nonce 1
    );
    std::fs::write(h.tx_pool_dir.join("stale.msg"), stale.to_bytes()).unwrap();
    let good = wallet.sign(0xaa, 0xab, 200, 2); // nonce 1
    h.drop_msg(&good);

    // Head is at seqno 1 from the direct commit; wait for block 2.
    let (handle, shutdown) = h.run_until(10_000, 1, Duration::from_secs(20));
    shutdown.store(true, Ordering::Relaxed);
    let stats = handle
        .join()
        .expect("producer thread")
        .expect("producer ok");

    // The stale message was dropped to rejected/, the good one committed,
    // and the loop never crashed.
    assert_eq!(
        h.tx_pool_dir.join("rejected").read_dir().unwrap().count(),
        1
    );
    h.reopen();
    assert_eq!(account_nonce(h.store(), 0xaa), 2);
    assert!(stats.blocks_produced >= 1);
}

#[test]
fn loop_shutdown_leaves_clean_state() {
    let mut h = Harness::new("shutdown");
    let mut wallet = TestWallet::new(h.chain_id);
    for _ in 0..10 {
        h.drop_msg(&wallet.sign(0xaa, 0xab, 100, 1));
    }

    let (handle, shutdown) = h.run_until(10_000, 1, Duration::from_secs(20));
    // Signal shutdown mid-stream (more messages may still be pending).
    shutdown.store(true, Ordering::Relaxed);
    let stats = handle
        .join()
        .expect("producer thread")
        .expect("producer ok");
    assert!(stats.blocks_produced >= 1);

    // The store must load cleanly with full integrity: load_state runs the
    // gap-2 root verification, so a torn write would fail here.
    h.reopen();
    let state = h
        .store()
        .load_state()
        .unwrap()
        .expect("state loads after shutdown");
    let root = state.state_root().unwrap();
    let (seqno, _) = h.store().head().unwrap().unwrap();
    assert_eq!(
        root,
        h.store().state_root_at(seqno).unwrap().unwrap(),
        "rebuilt root matches stored root: no half-written block"
    );
}

#[test]
fn select_candidates_drops_became_stale_without_crashing() {
    // Direct unit-level proof of the proposal-time path: a message valid at
    // intake becomes stale before selection (out-of-band commit), and
    // select_candidates moves it to rejected/ instead of crashing.
    let h = Harness::new("selectstale");
    let mut wallet = TestWallet::new(h.chain_id);
    let mut mempool = Mempool::new(&h.tx_pool_dir, 10_000, h.chain_id, h.fee_collector).unwrap();

    let msg = wallet.sign(0xaa, 0xab, 1000, 10); // nonce 0, valid now
    h.drop_msg(&msg);
    mempool.scan_drop_dir(&h.tx_pool_dir, h.store()).unwrap();
    assert_eq!(mempool.len(), 1);

    // Out-of-band commit consumes nonce 0.
    let state: State = h.store().load_state().unwrap().unwrap();
    let direct = sign_msg(h.chain_id, 0xaa, 0xac, 100, 1, 0);
    let block = propose_block(
        &state,
        vec![direct],
        state.last_lt + 1,
        h.fee_collector,
        1,
        0,
    )
    .unwrap();
    h.store().commit_block(&state, &block).unwrap();

    // Selection against the fresh head: the stale message is rejected, no panic.
    let fresh: State = h.store().load_state().unwrap().unwrap();
    let candidates = mempool.select_candidates(&fresh).unwrap();
    assert!(candidates.is_empty());
    assert_eq!(mempool.len(), 0);
    assert_eq!(
        h.tx_pool_dir.join("rejected").read_dir().unwrap().count(),
        1
    );
}

#[test]
fn loop_double_key_reveal_drops_only_second_reveal() {
    // The third audit shape: a fresh key-derived account reveals its key in
    // TWO queued messages. The first reveal is valid; the second is
    // permanently invalid (the account is keyed after the first). The old
    // code fed both to `propose_block`, failed the whole block three times,
    // then quarantined the entire mempool. Now only the second reveal is
    // dropped and blocks keep flowing.
    use onx_state_model::AccountState;

    let mut h = Harness::new("reveal");
    let mut wallet = TestWallet::new(h.chain_id);

    // Fresh keypair → address that has never appeared on chain.
    let secret = test_secret(0x99);
    let pubkey = secret.public_key().encode();
    let addr = derive_address(&pubkey);

    // Fund it out-of-band: a transfer landing on a fresh address creates a
    // keyless Active account (this is the only way to become keyless).
    let state: State = h.store().load_state().unwrap().unwrap();
    let fund = sign_msg_from(
        h.chain_id,
        account_id(0xaa),
        addr,
        1_000_000,
        10,
        0,
        [0u8; 32],
        &test_secret(0xaa),
    );
    let block =
        propose_block(&state, vec![fund], state.last_lt + 1, h.fee_collector, 1, 0).unwrap();
    h.store().commit_block(&state, &block).unwrap();

    // Confirm the test's premise: funded but keyless.
    match h.store().get_account(&addr).unwrap() {
        Some(AccountState::Active { pubkey: k, .. }) => {
            assert_eq!(k, [0u8; 32], "freshly funded account is keyless")
        }
        other => panic!("expected keyless active account, got {other:?}"),
    }

    // Two reveals from the fresh account + one honest message from 0xab.
    let reveal1 = sign_msg_from(
        h.chain_id,
        addr,
        account_id(0xab),
        500,
        5,
        0,
        pubkey,
        &secret,
    );
    let reveal2 = sign_msg_from(
        h.chain_id,
        addr,
        account_id(0xab),
        500,
        5,
        1,
        pubkey,
        &secret,
    );
    let honest = wallet.sign(0xab, 0xac, 100, 1);
    h.drop_msg(&reveal1);
    h.drop_msg(&reveal2);
    h.drop_msg(&honest);

    let (handle, shutdown) = h.run_until(10_000, 1, Duration::from_secs(20));
    shutdown.store(true, Ordering::Relaxed);
    let stats = handle
        .join()
        .expect("producer thread")
        .expect("producer ok");
    assert!(stats.blocks_produced >= 1);

    // Only the second reveal was dropped; the first reveal and the honest
    // message committed, and the loop did not quarantine anything.
    assert_eq!(
        h.tx_pool_dir.join("rejected").read_dir().unwrap().count(),
        1,
        "exactly the second reveal lands in rejected/"
    );
    assert_eq!(
        h.tx_pool_dir.join("pending").read_dir().unwrap().count(),
        0,
        "nothing left stranded in pending/"
    );
    h.reopen();
    let state: State = h.store().load_state().unwrap().unwrap();
    match h.store().get_account(&addr).unwrap() {
        Some(AccountState::Active {
            nonce, pubkey: k, ..
        }) => {
            assert_eq!(nonce, 1, "only the first reveal committed");
            assert_eq!(k, pubkey, "account is keyed after the first reveal");
        }
        other => panic!("expected active account, got {other:?}"),
    }
    assert_eq!(
        account_nonce(h.store(), 0xab),
        1,
        "honest message committed"
    );
    drop(state);

    // Blocks keep flowing afterwards: one more honest message → block 3.
    // (0xaa's on-chain nonce is 1 after the funding commit. Note the target
    // is 3 files: block 2 from phase 1 plus the regenerated block 1 already
    // exist.)
    let more = sign_msg(h.chain_id, 0xaa, 0xac, 50, 1, 1);
    h.drop_msg(&more);
    let (handle, shutdown) = h.run_until(10_000, 3, Duration::from_secs(20));
    shutdown.store(true, Ordering::Relaxed);
    handle
        .join()
        .expect("producer thread")
        .expect("producer ok");
    h.reopen();
    assert_eq!(account_nonce(h.store(), 0xaa), 2);
}

#[test]
fn startup_regenerates_block_file_missing_after_crash() {
    // The gap the audit found by reading: the producer used to write the
    // block file AFTER the DB commit, non-atomically. A kill in between
    // left the block committed but `blocks/` missing its file, forever.
    // Simulate exactly that, restart the producer, and require the file to
    // come back byte-correct.
    use onx_stf::block::BLOCK_HEADER_BYTE_LEN;
    use onx_stf::BlockHeader;

    const BLK_MAGIC: &[u8; 8] = b"ONXBLK04";

    let mut h = Harness::new("regen");
    let mut wallet = TestWallet::new(h.chain_id);
    h.drop_msg(&wallet.sign(0xaa, 0xab, 1000, 10));

    // One block through the normal path.
    let (handle, shutdown) = h.run_until(10_000, 1, Duration::from_secs(20));
    shutdown.store(true, Ordering::Relaxed);
    handle
        .join()
        .expect("producer thread")
        .expect("producer ok");
    let blk1 = h.blocks_dir().join("block-00000001.blk");
    assert!(blk1.is_file());

    // The crash: commit block 2 to the DB directly, never writing its file.
    h.reopen();
    let state: State = h.store().load_state().unwrap().unwrap();
    let msg = wallet.sign(0xaa, 0xab, 500, 5);
    let block = propose_block(&state, vec![msg], state.last_lt + 1, h.fee_collector, 1, 0).unwrap();
    assert_eq!(block.header.seqno, 2);
    h.store().commit_block(&state, &block).unwrap();
    let blk2 = h.blocks_dir().join("block-00000002.blk");
    assert!(
        !blk2.is_file(),
        "crash simulation: no file for committed block 2"
    );

    // Restart the producer (run_until takes the reopened store).
    // Startup regeneration must fill the gap before the loop even starts
    // producing.
    let (handle, shutdown) = h.run_until(10_000, 2, Duration::from_secs(20));
    shutdown.store(true, Ordering::Relaxed);
    handle
        .join()
        .expect("producer thread")
        .expect("producer ok");

    // The missing file is back, with no gap in the sequence.
    assert!(blk1.is_file());
    assert!(blk2.is_file(), "startup regenerated the missing block file");

    // Byte-correct: the regenerated file's header matches the committed
    // block in the DB exactly.
    let bytes = std::fs::read(&blk2).unwrap();
    assert_eq!(&bytes[..8], BLK_MAGIC, "magic intact");
    let file_header =
        BlockHeader::from_bytes(&bytes[8..8 + BLOCK_HEADER_BYTE_LEN]).expect("header parses");
    h.reopen();
    let block_hash = h
        .store()
        .block_hash_for_seqno(2)
        .unwrap()
        .expect("block 2 hash in DB");
    let db_header = h
        .store()
        .get_block_header(&block_hash)
        .unwrap()
        .expect("block 2 in DB");
    assert_eq!(file_header.seqno, db_header.seqno);
    assert_eq!(file_header.prev_hash, db_header.prev_hash);
    assert_eq!(file_header.msgs_root, db_header.msgs_root);
    assert_eq!(file_header.state_root, db_header.state_root);

    // And it replays: the regenerated file is a first-class block file.
    let daemon_root = h.final_root_hex();
    let replay_data = h.dir.join("replay-data");
    let out = Command::new(onx_binary())
        .args([
            "replay",
            "--genesis",
            h.genesis_toml.to_str().unwrap(),
            "--blocks",
            h.blocks_dir().to_str().unwrap(),
            "--data-dir",
            replay_data.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run onx replay");
    assert!(
        out.status.success(),
        "onx replay failed on regenerated file: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let replay_root = stdout
        .lines()
        .find_map(|l| l.strip_prefix("final_state_root="))
        .expect("replay printed final_state_root")
        .trim();
    assert_eq!(
        replay_root, daemon_root,
        "replay through the regenerated file reproduces the daemon root"
    );
}

// ---------- end-to-end: the loop's output through the real replay binary ----------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
}

fn cargo_bin() -> String {
    // `cargo test` sets CARGO to the running cargo binary; fall back to
    // PATH otherwise.
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

fn onx_binary() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("target"));
    for profile in ["debug", "release"] {
        let bin = target_dir.join(profile).join("onx");
        if bin.is_file() {
            return bin;
        }
    }
    // Build it: cargo test builds test harnesses, not the binary itself.
    let status = Command::new(cargo_bin())
        .args(["build", "-p", "onx", "--bin", "onx"])
        .current_dir(workspace_root())
        .status()
        .expect("cargo build -p onx failed to spawn");
    assert!(status.success(), "cargo build -p onx --bin onx failed");
    let bin = target_dir.join("debug").join("onx");
    assert!(bin.is_file(), "onx binary still missing after build");
    bin
}

#[test]
fn e2e_producer_output_replays_through_onx_binary() {
    let mut h = Harness::new("e2e");
    let mut wallet = TestWallet::new(h.chain_id);

    // Two waves → at least two blocks, exercising multi-block replay.
    for _ in 0..3 {
        h.drop_msg(&wallet.sign(0xaa, 0xab, 1000, 10));
    }
    let (handle, shutdown) = h.run_until(10_000, 1, Duration::from_secs(20));
    // Keep the loop alive for wave two.
    for _ in 0..3 {
        h.drop_msg(&wallet.sign(0xab, 0xac, 400, 5));
    }
    h.wait_for_blocks(2, Duration::from_secs(20));
    shutdown.store(true, Ordering::Relaxed);
    let stats = handle
        .join()
        .expect("producer thread")
        .expect("producer ok");
    assert!(stats.blocks_produced >= 2);

    h.reopen();
    let daemon_root = h.final_root_hex();

    // The daemon emitted canonical block files. Replay them from genesis on
    // a FRESH data dir through the real `onx replay` binary.
    let blocks_dir = h.blocks_dir();
    assert!(blocks_dir.join("block-00000001.blk").is_file());
    assert!(blocks_dir.join("block-00000002.blk").is_file());
    let replay_data = h.dir.join("replay-data");

    let out = Command::new(onx_binary())
        .args([
            "replay",
            "--genesis",
            h.genesis_toml.to_str().unwrap(),
            "--blocks",
            blocks_dir.to_str().unwrap(),
            "--data-dir",
            replay_data.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run onx replay");
    assert!(
        out.status.success(),
        "onx replay failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let replay_root = stdout
        .lines()
        .find_map(|l| l.strip_prefix("final_state_root="))
        .expect("replay printed final_state_root")
        .trim();

    assert_eq!(
        replay_root, daemon_root,
        "replay of the daemon's blocks reproduces the daemon's root"
    );
}
