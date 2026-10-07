//! Phase 4 regression tests: atomic storage and crash recovery.
//!
//! The crash test (`storage_crash_kill9_recovery`) SIGKILLs a real child
//! process mid-commit, reopens the database, verifies full-or-nothing
//! semantics, resumes from the head pointer, and requires the resumed chain
//! to match an uninterrupted run byte-for-byte. Set `ONX_CRASH_ITERS` to
//! control the iteration count (default 100).

use onx_state_model::AccountState;
use onx_stf::{apply_block, propose_block, Block, State};

/// Placeholder signature entries for storage tests that don't exercise auth.
/// `commit_block` stores (never verifies) the section; only non-emptiness
/// is enforced at this layer.
fn dummy_sigs() -> Vec<onx_stf::SigEntry> {
    vec![onx_stf::SigEntry {
        validator_index: 0,
        sig: [0xAB; 64],
    }]
}
use onx_storage::error::StorageError;
use onx_storage::store::SCHEMA_VERSION;
use onx_storage::support::{
    test_accounts, test_block_lt, test_block_txs, test_block_txs_with_nonces, test_fee_collector,
    test_genesis, TestTxGen,
};
use onx_storage::{decode_body, encode_body, ChainStore};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_db_path(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("onx-phase4-{tag}-{}-{nanos}", std::process::id()))
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

fn crash_iters() -> usize {
    std::env::var("ONX_CRASH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100)
}

/// Chain identity for the test genesis: every generated external message
/// must carry the genesis hash of the chain it authorizes on (ADR-0005).
fn chain_id() -> [u8; 32] {
    test_genesis().genesis_hash()
}

/// Read live account nonces out of a state: adversarial tests tamper with
/// `seqno`/`last_hash` but the transactions must still authorize against
/// the *actual* account nonces, or `propose_block` fails before the store's
/// continuity check is even reached.
fn nonces_of(
    state: &State,
    accounts: &[onx_data_structures::AccountId],
) -> BTreeMap<onx_data_structures::AccountId, u64> {
    let mut nonces = BTreeMap::new();
    for id in accounts {
        if let Some(AccountState::Active { nonce, .. }) = state.tree.get(id) {
            nonces.insert(*id, *nonce);
        }
    }
    nonces
}

/// Drive a chain to `num_blocks` in this process (open/resume safe), and
/// return the final state root. This is both the reference run and the
/// resume path used after each kill.
fn run_chain_to(store: &ChainStore, num_blocks: u32, seed: u64) -> Result<[u8; 32], StorageError> {
    let doc = test_genesis();
    if store.genesis_hash()?.is_none() {
        store.init_genesis(&doc)?;
    }
    let accounts = test_accounts();
    let collector = test_fee_collector();
    let mut state = store.load_state()?.expect("genesis state present");
    while state.seqno < num_blocks {
        let next = state.seqno + 1;
        let block = propose_block(
            &state,
            test_block_txs(seed, next, &accounts, chain_id()),
            test_block_lt(next),
            collector,
            1,
            0,
        )
        .expect("propose must succeed");
        store.commit_block(&state, &block, &dummy_sigs())?;
        state = store.load_state()?.expect("state present");
    }
    Ok(state.state_root()?)
}

#[test]
fn storage_genesis_init_idempotent() -> Result<(), StorageError> {
    let path = temp_db_path("genesis");
    let store = ChainStore::open(&path)?;
    let doc = test_genesis();
    store.init_genesis(&doc)?;
    // Second init with the same document: no-op.
    store.init_genesis(&doc)?;
    assert_eq!(store.head()?, Some((0, doc.genesis_hash())));
    assert_eq!(store.genesis_hash()?, Some(doc.genesis_hash()));
    // Genesis state loads with seqno 0 and the genesis root.
    let state = store.load_state()?.expect("genesis state");
    assert_eq!(state.seqno, 0);
    assert_eq!(state.last_lt, 0);
    assert_eq!(state.last_hash, doc.genesis_hash());
    assert_eq!(state.state_root()?, doc.state_tree().state_root_hash()?);
    cleanup(&path);
    Ok(())
}

#[test]
fn storage_commit_roundtrip_matches_pure_stf() -> Result<(), StorageError> {
    let path = temp_db_path("roundtrip");
    let store = ChainStore::open(&path)?;
    let seed = 0x1234u64;
    let accounts = test_accounts();
    let collector = test_fee_collector();

    let mut state: State = {
        let doc = test_genesis();
        store.init_genesis(&doc)?;
        store.load_state()?.expect("genesis state")
    };

    for _ in 0..5 {
        let next = state.seqno + 1;
        let block: Block = propose_block(
            &state,
            test_block_txs(seed, next, &accounts, chain_id()),
            test_block_lt(next),
            collector,
            1,
            0,
        )
        .expect("propose");
        // Pure in-memory expectation.
        let (expected, _) = apply_block(&state, &block).expect("apply");
        store.commit_block(&state, &block, &dummy_sigs())?;

        // Loaded state must equal the pure-STF state exactly.
        let loaded = store.load_state()?.expect("state");
        assert_eq!(loaded.tree.accounts(), expected.tree.accounts());
        assert_eq!(loaded.seqno, expected.seqno);
        assert_eq!(loaded.last_lt, expected.last_lt);
        assert_eq!(loaded.last_hash, expected.last_hash);
        assert_eq!(loaded.workchain, expected.workchain);

        // Header and body round-trip by block hash.
        let hash = block.header.hash();
        let hdr = store.get_block_header(&hash)?.expect("header");
        assert_eq!(hdr, block.header);
        let body = store.get_block_body(&hash)?.expect("body");
        assert_eq!(body, block.body);
        assert_eq!(store.block_hash_for_seqno(next)?, Some(hash));
        assert_eq!(store.state_root_at(next)?, Some(expected.state_root()?));

        state = loaded;
    }
    // Dense prefix invariant: roots for 0..=5, no gaps.
    for k in 0..=5 {
        assert!(store.state_root_at(k)?.is_some(), "gap at {k}");
        assert!(store.block_hash_for_seqno(k)?.is_some(), "gap at {k}");
    }
    cleanup(&path);
    Ok(())
}

#[test]
fn storage_body_encoding_roundtrip() {
    use onx_stf::block::BlockBody;
    let body = BlockBody { messages: vec![] };
    assert_eq!(decode_body(&encode_body(&body)).expect("decode"), body);
}

#[test]
fn storage_idempotent_recommit() -> Result<(), StorageError> {
    let path = temp_db_path("idempotent");
    let store = ChainStore::open(&path)?;
    let doc = test_genesis();
    store.init_genesis(&doc)?;
    let accounts = test_accounts();
    let collector = test_fee_collector();
    let state = store.load_state()?.expect("genesis state");
    let block = propose_block(
        &state,
        test_block_txs(7, 1, &accounts, chain_id()),
        1,
        collector,
        1,
        0,
    )
    .expect("propose");

    store.commit_block(&state, &block, &dummy_sigs())?;
    let head1 = store.head()?;
    let root1 = store.state_root_at(1)?;
    // Recommit the identical block: no-op, no error, no state change.
    store.commit_block(&state, &block, &dummy_sigs())?;
    assert_eq!(store.head()?, head1);
    assert_eq!(store.state_root_at(1)?, root1);
    cleanup(&path);
    Ok(())
}

#[test]
fn storage_fork_detected() -> Result<(), StorageError> {
    let path = temp_db_path("fork");
    let store = ChainStore::open(&path)?;
    let doc = test_genesis();
    store.init_genesis(&doc)?;
    let accounts = test_accounts();
    let collector = test_fee_collector();
    let state = store.load_state()?.expect("genesis state");

    let block_a = propose_block(
        &state,
        test_block_txs(7, 1, &accounts, chain_id()),
        1,
        collector,
        1,
        0,
    )
    .expect("propose");
    let block_b = propose_block(
        &state,
        test_block_txs(999, 1, &accounts, chain_id()),
        1,
        collector,
        1,
        0,
    )
    .expect("propose");
    assert_ne!(block_a.header.hash(), block_b.header.hash());

    store.commit_block(&state, &block_a, &dummy_sigs())?;
    let err = store
        .commit_block(&state, &block_b, &dummy_sigs())
        .unwrap_err();
    assert!(
        matches!(err, StorageError::ForkDetected { seqno: 1, .. }),
        "expected ForkDetected, got: {err}"
    );
    // The fork attempt changed nothing.
    assert_eq!(store.head()?.expect("head").0, 1);
    assert_eq!(store.block_hash_for_seqno(1)?, Some(block_a.header.hash()));
    cleanup(&path);
    Ok(())
}

#[test]
fn storage_rejects_bad_block_atomically() -> Result<(), StorageError> {
    let path = temp_db_path("badblock");
    let store = ChainStore::open(&path)?;
    let doc = test_genesis();
    store.init_genesis(&doc)?;
    let accounts = test_accounts();
    let collector = test_fee_collector();
    let state = store.load_state()?.expect("genesis state");

    let mut block = propose_block(
        &state,
        test_block_txs(7, 1, &accounts, chain_id()),
        1,
        collector,
        1,
        0,
    )
    .expect("propose");
    // Tamper with the claimed post-state root: the STF must reject it, and
    // the failed commit must persist nothing.
    block.header.state_root = [0xFF; 32];
    let err = store
        .commit_block(&state, &block, &dummy_sigs())
        .unwrap_err();
    assert!(
        matches!(err, StorageError::Stf(_)),
        "expected STF rejection, got: {err}"
    );
    assert_eq!(
        store.head()?.expect("head").0,
        0,
        "head moved on failed commit"
    );
    assert!(store.block_hash_for_seqno(1)?.is_none());
    cleanup(&path);
    Ok(())
}

#[test]
fn storage_dirty_set_complete() -> Result<(), StorageError> {
    // Every account the STF wrote must be persisted. The strongest check:
    // the loaded tree must equal the pure in-memory tree account-for-account,
    // including the fee collector (which only some blocks touch).
    let path = temp_db_path("dirtyset");
    let store = ChainStore::open(&path)?;
    let doc = test_genesis();
    store.init_genesis(&doc)?;
    let accounts = test_accounts();
    let collector = test_fee_collector();
    let mut state = store.load_state()?.expect("genesis state");
    // Stateful generator: the seed alternates per block, so nonces cannot
    // be replayed from any single seed's history.
    let mut gen = TestTxGen::new(chain_id());

    for n in 1..=6u32 {
        // Alternate fee patterns so the collector is touched on some blocks.
        let seed = if n % 2 == 0 { 0xFEEu64 } else { 0xBEEu64 };
        let block = propose_block(
            &state,
            gen.block_txs(seed, n, &accounts),
            test_block_lt(n),
            collector,
            1,
            0,
        )
        .expect("propose");
        let (expected, _) = apply_block(&state, &block).expect("apply");
        store.commit_block(&state, &block, &dummy_sigs())?;
        for (id, expected_acct) in expected.tree.accounts() {
            let stored = store
                .get_account(id)?
                .unwrap_or_else(|| panic!("touched account {id:?} missing from store"));
            assert_eq!(&stored, expected_acct, "account {id:?} diverged");
        }
        state = store.load_state()?.expect("state");
    }
    // The collector must exist in the store (fees were nonzero on even blocks).
    let _ = store
        .get_account(&collector)?
        .expect("fee collector missing");
    cleanup(&path);
    Ok(())
}

#[test]
fn storage_refuses_newer_schema() -> Result<(), StorageError> {
    let path = temp_db_path("schemarefuse");
    {
        let store = ChainStore::open(&path)?;
        store.init_genesis(&test_genesis())?;
    }
    // Bump the schema version behind the store's back with raw redb.
    {
        let db = redb::Database::create(&path)?;
        let wtxn = db.begin_write()?;
        {
            let mut meta = wtxn.open_table(redb::TableDefinition::<&[u8], &[u8]>::new("meta"))?;
            meta.insert(
                b"schema_version".as_slice(),
                (SCHEMA_VERSION + 1).to_be_bytes().as_slice(),
            )?;
        }
        wtxn.commit()?;
    }
    let err = match ChainStore::open(&path) {
        Ok(_) => panic!("open with newer schema version should have failed"),
        Err(e) => e,
    };
    assert!(
        matches!(err, StorageError::SchemaMismatch { .. }),
        "expected SchemaMismatch, got: {err}"
    );
    cleanup(&path);
    Ok(())
}

/// Gap 1: a block built on a diverged in-memory state must be rejected,
/// even though the pure STF accepts it. The STF can only check the block
/// against the state it is given; only the store can check that state
/// against the stored head.
#[test]
fn storage_rejects_diverged_state_commit() -> Result<(), StorageError> {
    let path = temp_db_path("diverged");
    let store = ChainStore::open(&path)?;
    let doc = test_genesis();
    store.init_genesis(&doc)?;
    let accounts = test_accounts();
    let collector = test_fee_collector();

    // Honest block 1: head = (1, h1).
    let state0 = store.load_state()?.expect("genesis state");
    let block1 = propose_block(
        &state0,
        test_block_txs(7, 1, &accounts, chain_id()),
        1,
        collector,
        1,
        0,
    )
    .expect("propose");
    store.commit_block(&state0, &block1, &dummy_sigs())?;
    let (head_seqno, head_hash) = store.head()?.expect("head");
    assert_eq!(head_seqno, 1);

    // Diverged in-memory state: right seqno, wrong base hash. propose_block
    // (and the STF inside commit_block) accept the resulting block — the
    // corruption is invisible without the stored head to compare against.
    // Transactions are signed with nonces read from the diverged state
    // itself: they must authorize, or propose fails before the store check.
    let mut bad_state = store.load_state()?.expect("state");
    bad_state.last_hash = [0xAA; 32];
    let nonces = nonces_of(&bad_state, &accounts);
    let bad_block = propose_block(
        &bad_state,
        test_block_txs_with_nonces(8, 2, &accounts, &nonces, chain_id()),
        2,
        collector,
        1,
        0,
    )
    .expect("propose");
    assert_eq!(bad_block.header.seqno, 2);
    assert_eq!(bad_block.header.prev_hash, [0xAA; 32]);

    let err = store
        .commit_block(&bad_state, &bad_block, &dummy_sigs())
        .unwrap_err();
    assert!(
        matches!(err, StorageError::HeadMismatch { .. }),
        "expected HeadMismatch, got: {err}"
    );
    // Fail-closed: head, indexes, and roots are untouched; the write
    // transaction was dropped before any insert.
    assert_eq!(store.head()?, Some((head_seqno, head_hash)));
    assert!(store.block_hash_for_seqno(2)?.is_none());
    assert!(store.state_root_at(2)?.is_none());
    cleanup(&path);
    Ok(())
}

/// Gap 1: a block that skips seqnos must be rejected, even with a correct
/// prev_hash. Without the continuity check the store would persist a
/// gapped chain (dense-prefix invariants broken, replay order undefined).
#[test]
fn storage_rejects_skipped_seqno_commit() -> Result<(), StorageError> {
    let path = temp_db_path("skipseq");
    let store = ChainStore::open(&path)?;
    let doc = test_genesis();
    store.init_genesis(&doc)?;
    let accounts = test_accounts();
    let collector = test_fee_collector();

    let state0 = store.load_state()?.expect("genesis state");
    let block1 = propose_block(
        &state0,
        test_block_txs(7, 1, &accounts, chain_id()),
        1,
        collector,
        1,
        0,
    )
    .expect("propose");
    store.commit_block(&state0, &block1, &dummy_sigs())?;

    // State claims seqno 5 while the stored head is at 1. The proposed
    // block carries the real head hash as prev_hash — only the seqno half
    // of the continuity check can catch it. Transactions authorize against
    // the state's actual nonces (seed-7 block 1 applied), not the claimed
    // seqno, or propose fails before the store check.
    let mut bad_state = store.load_state()?.expect("state");
    bad_state.seqno = 5;
    let nonces = nonces_of(&bad_state, &accounts);
    let bad_block = propose_block(
        &bad_state,
        test_block_txs_with_nonces(8, 6, &accounts, &nonces, chain_id()),
        6,
        collector,
        1,
        0,
    )
    .expect("propose");
    assert_eq!(bad_block.header.seqno, 6);
    assert_eq!(bad_block.header.prev_hash, store.head()?.expect("head").1);

    let err = store
        .commit_block(&bad_state, &bad_block, &dummy_sigs())
        .unwrap_err();
    assert!(
        matches!(err, StorageError::HeadMismatch { .. }),
        "expected HeadMismatch, got: {err}"
    );
    assert_eq!(store.head()?.expect("head").0, 1);
    assert!(store.block_hash_for_seqno(6)?.is_none());
    cleanup(&path);
    Ok(())
}

/// Gap 2: `load_state` must fail closed when the rebuilt state does not
/// hash to the stored post-state root — otherwise a self-producing node
/// would build its next block on corrupt state.
#[test]
fn storage_load_state_rejects_tampered_root() -> Result<(), StorageError> {
    let path = temp_db_path("tamperedroot");
    let store = ChainStore::open(&path)?;
    let doc = test_genesis();
    store.init_genesis(&doc)?;
    let accounts = test_accounts();
    let collector = test_fee_collector();

    let state0 = store.load_state()?.expect("genesis state");
    let block1 = propose_block(
        &state0,
        test_block_txs(7, 1, &accounts, chain_id()),
        1,
        collector,
        1,
        0,
    )
    .expect("propose");
    store.commit_block(&state0, &block1, &dummy_sigs())?;
    // Sanity: the untampered database loads fine.
    assert!(store.load_state()?.is_some());
    drop(store);

    // Tamper with the stored post-state root behind the store's back.
    {
        let db = redb::Database::create(&path)?;
        let wtxn = db.begin_write()?;
        {
            let mut roots =
                wtxn.open_table(redb::TableDefinition::<&[u8], &[u8]>::new("state_roots"))?;
            roots.insert(1u32.to_be_bytes().as_slice(), [0xBB; 32].as_slice())?;
        }
        wtxn.commit()?;
    }

    let store = ChainStore::open(&path)?;
    let err = match store.load_state() {
        Ok(_) => panic!("load_state succeeded on a tampered root"),
        Err(e) => e,
    };
    assert!(
        matches!(err, StorageError::StateRootMismatch { seqno: 1, .. }),
        "expected StateRootMismatch, got: {err}"
    );
    cleanup(&path);
    Ok(())
}

/// Verify the post-crash invariants on a reopened database. Returns the
/// head seqno, or 0 with a flag for "genesis never completed".
fn verify_crash_invariants(store: &ChainStore) -> Result<u32, StorageError> {
    let Some((seqno, hash)) = store.head()? else {
        // Kill landed before/during genesis init. init_genesis is one atomic
        // txn, so genesis is either fully present or absent — never partial.
        assert!(
            store.genesis_hash()?.is_none(),
            "head is None but genesis_hash is present: torn init"
        );
        return Ok(0);
    };
    // Head must resolve through every index.
    assert_eq!(
        store
            .block_hash_for_seqno(seqno)?
            .expect("head seqno missing from index"),
        hash,
        "head hash not in seqno index"
    );
    let root = store.state_root_at(seqno)?.expect("head root missing");
    // Dense prefix 0..=seqno with no gaps.
    for k in 0..=seqno {
        assert!(
            store.block_hash_for_seqno(k)?.is_some(),
            "gap in seqno_to_hash at {k}"
        );
        assert!(
            store.state_root_at(k)?.is_some(),
            "gap in state_roots at {k}"
        );
    }
    // The loaded state must agree with the head on every field.
    let state = store.load_state()?.expect("state present");
    assert_eq!(state.seqno, seqno, "loaded seqno != head seqno");
    assert_eq!(state.last_hash, hash, "loaded last_hash != head hash");
    assert_eq!(state.state_root()?, root, "loaded root != stored root");
    // For seqno > 0 the head block's header and body must be fully present:
    // the head pointer advanced in the same txn, so a present head implies
    // present block data. This is the no-torn-state property.
    if seqno > 0 {
        let hdr = store
            .get_block_header(&hash)?
            .expect("head header missing: torn commit");
        assert_eq!(hdr.seqno, seqno);
        let body = store
            .get_block_body(&hash)?
            .expect("head body missing: torn commit");
        assert_eq!(body.messages.len() as u32, hdr.msg_count);
    }
    Ok(seqno)
}

#[test]
fn storage_crash_kill9_recovery() -> Result<(), StorageError> {
    let iters = crash_iters();
    let num_blocks = 10u32;
    let probe = env!("CARGO_BIN_EXE_onx-crash-probe");

    // Reference: one uninterrupted in-process run.
    let ref_path = temp_db_path("crashref");
    let reference_root = {
        let store = ChainStore::open(&ref_path)?;
        let root = run_chain_to(&store, num_blocks, 0xC0FFEE)?;
        drop(store);
        root
    };
    cleanup(&ref_path);

    let mut ever_committed = false;
    for i in 0..iters {
        // NOTE: the seed is constant across iterations — the kill *timing*
        // (random sleep below) is what varies. Comparing against one
        // reference root requires identical transaction sequences.
        let seed = 0xC0FFEEu64;
        let path = temp_db_path(&format!("crash{i}"));

        let mut child = Command::new(probe)
            .arg(&path)
            .arg(num_blocks.to_string())
            .arg(seed.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn crash probe");

        // Kill at a random point: 1..40ms. Sometimes the probe finishes
        // first — that exercises the already-complete path instead.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos() as u64;
        let sleep_ms = 1 + (nanos.wrapping_add((i as u64).wrapping_mul(0x9E3779B97F4A7C15)) % 40);
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
        // SIGKILL on unix; best-effort if the child already exited.
        let _ = child.kill();
        let _ = child.wait();

        // Reopen (this IS the recovery) and verify full-or-nothing.
        let store = ChainStore::open(&path)?;
        let crashed_at = verify_crash_invariants(&store)?;
        if crashed_at > 0 {
            ever_committed = true;
        }

        // Resume from the head pointer to completion.
        let resumed_root = run_chain_to(&store, num_blocks, seed)?;
        assert_eq!(
            resumed_root, reference_root,
            "iteration {i} (crashed at seqno {crashed_at}): resumed root diverged from uninterrupted run"
        );
        drop(store);
        cleanup(&path);
    }
    // The probe must actually commit blocks: if every iteration dies at
    // seqno 0, the test is hollow (this is how the empty-sig rejection
    // silently neutered the probe).
    assert!(
        ever_committed,
        "crash probe never committed a block in {iters} iterations — test is hollow"
    );
    Ok(())
}

/// Schema v2 dropped the write-only `cells` table (Claude gap 3). A v1
/// database — identifiable by its stored schema version — must be rejected
/// fail-closed, not silently opened with a stale layout. v1 was never
/// released, so no migration is provided: resync from genesis.
#[test]
fn storage_rejects_v1_schema_with_cells_table() -> Result<(), StorageError> {
    let path = temp_db_path("v1reject");
    // Forge a v1 database behind the store's back: schema_version = 1 and
    // a populated `cells` table, as v1 would have written.
    {
        let db = redb::Database::create(&path)?;
        let wtxn = db.begin_write()?;
        {
            let mut meta = wtxn.open_table(redb::TableDefinition::<&[u8], &[u8]>::new("meta"))?;
            meta.insert(b"schema_version".as_slice(), 1u32.to_be_bytes().as_slice())?;
            let mut cells = wtxn.open_table(redb::TableDefinition::<&[u8], &[u8]>::new("cells"))?;
            cells.insert([0xABu8; 32].as_slice(), [0xCDu8; 10].as_slice())?;
        }
        wtxn.commit()?;
    }
    let err = match ChainStore::open(&path) {
        Ok(_) => panic!("open of a v1 database should have failed"),
        Err(e) => e,
    };
    assert!(
        matches!(
            err,
            StorageError::SchemaMismatch {
                found: 1,
                supported: 4
            }
        ),
        "expected SchemaMismatch {{ found: 1, supported: 3 }}, got: {err}"
    );
    cleanup(&path);
    Ok(())
}

/// CI green-up: a torn `<db>.init-tmp` left by a SIGKILL inside redb's
/// first `Database::create` must not poison the next open. The temp can
/// never contain committed data, so `open` deletes it and initializes
/// cleanly; the resulting database must be fully usable.
#[test]
fn storage_open_recovers_torn_init_temp_file() -> Result<(), StorageError> {
    let path = temp_db_path("torninit");
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".init-tmp");
    let tmp = PathBuf::from(tmp);
    // Plant a torn temp file: garbage where a half-written redb header
    // would be. (The real torn file comes from a killed Database::create;
    // the content is irrelevant -- only its presence matters.)
    std::fs::write(&tmp, [0x5Au8; 64]).expect("write torn tmp");
    assert!(tmp.exists());

    let store = ChainStore::open(&path)?;
    // Temp cleaned up, real database initialized in its place.
    assert!(!tmp.exists(), "torn init-tmp should have been removed");
    assert!(path.exists(), "database file should exist after open");
    // And the database is fully usable: genesis init works.
    store.init_genesis(&test_genesis())?;
    let (seqno, _) = store.head()?.expect("head set by genesis init");
    assert_eq!(seqno, 0);

    cleanup(&path);
    Ok(())
}

/// CI green-up: a bad magic number on an existing, non-empty database is
/// corruption, not an invitation to reinitialize. `open` must fail closed
/// with `StorageError::Corrupt` and leave the file untouched.
#[test]
fn storage_open_rejects_bad_magic_fail_closed() -> Result<(), StorageError> {
    let path = temp_db_path("badmagic");
    let garbage = [0x5Au8; 128];
    std::fs::write(&path, garbage).expect("write bad-magic file");

    let err = match ChainStore::open(&path) {
        Ok(_) => panic!("open of a bad-magic database should have failed"),
        Err(e) => e,
    };
    assert!(
        matches!(err, StorageError::Corrupt(_)),
        "expected Corrupt, got: {err}"
    );
    // Fail-closed means the file is preserved, not wiped or replaced.
    let after = std::fs::read(&path).expect("read back bad-magic file");
    assert_eq!(after, garbage, "bad-magic file must be preserved untouched");

    cleanup(&path);
    Ok(())
}
