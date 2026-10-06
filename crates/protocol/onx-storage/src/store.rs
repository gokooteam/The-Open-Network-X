//! [`ChainStore`]: atomic, crash-safe chain storage on redb.
//!
//! Schema (all tables are `&[u8] -> &[u8]`; keys are fixed-size big-endian
//! so iteration order is canonical):
//!
//! | Table           | Key                  | Value                          |
//! |-----------------|----------------------|--------------------------------|
//! | `accounts`      | account id `[u8;32]` | `AccountState` bytes           |
//! | `block_headers` | block hash `[u8;32]` | 148-byte canonical header      |
//! | `block_bodies`  | block hash `[u8;32]` | `encode_body` bytes            |
//! | `seqno_to_hash` | seqno `u32` BE       | block hash `[u8;32]`           |
//! | `state_roots`   | seqno `u32` BE       | state root `[u8;32]`           |
//! | `meta`          | static key bytes     | bytes                          |
//!
//! `meta` keys: `b"schema_version"` (u32 BE), `b"genesis_hash"` (32 bytes),
//! `b"chain_id"` (32 bytes, genesis hash — the chain's identity),
//! `b"workchain"` (i32 BE), `b"head"` (seqno u32 BE ++ block hash).
//!
//! Note: schema v1 had a seventh table, `cells` (cell hash -> cell bytes),
//! written on every commit but never read by anything. It was dropped in
//! v2: it caused O(cells) write amplification per block with unbounded
//! growth and no GC, served no reader, and historical state remains
//! reconstructible via replay from genesis + persisted block bodies.
//! v1 databases are rejected (fail-closed `SchemaMismatch`); resync from
//! genesis — v1 was never released, so no migration is provided.
//!
//! Seqno 0 is the genesis pseudo-entry: `seqno_to_hash[0]` and
//! `state_roots[0]` exist, but there is no block header/body for it.
//!
//! **Deviation from the design doc (follows the Phase 3 code):** seqno
//! keys are `u32` big-endian, not `u64` — `BlockHeader.seqno` and
//! `State.seqno` are `u32` in the STF, and the store must not invent a
//! wider type than the types it persists.

use crate::encoding::{decode_body, encode_body};
use crate::error::StorageError;
use onx_data_structures::AccountId;
use onx_state_model::{AccountState, GenesisDocument, ShardStateTree};
use onx_stf::{apply_block, Block, BlockBody, BlockHeader, Receipts, State};
use redb::{Database, ReadableTable, TableDefinition};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Current schema version. Bump when the table layout changes; `open`
/// refuses databases written by a different version (fail-closed).
/// v1 -> v2: dropped the write-only `cells` table (see schema docs above).
/// v1 was never released, so no migration is provided — resync from genesis.
pub const SCHEMA_VERSION: u32 = 2;

const ACCOUNTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("accounts");
const BLOCK_HEADERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("block_headers");
const BLOCK_BODIES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("block_bodies");
const SEQNO_TO_HASH: TableDefinition<&[u8], &[u8]> = TableDefinition::new("seqno_to_hash");
const STATE_ROOTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("state_roots");
const META: TableDefinition<&[u8], &[u8]> = TableDefinition::new("meta");

/// Atomic chain store.
///
/// All mutation goes through [`ChainStore::init_genesis`] and
/// [`ChainStore::commit_block`], each of which is a single redb write
/// transaction. There is no write-ahead journal to tear: redb's
/// copy-on-write commit *is* the atomicity mechanism (this closes Phase 0
/// bug 3 by elimination).
pub struct ChainStore {
    db: Database,
}

/// Sibling temp path used for crash-atomic first-time initialization.
/// `<db>.init-tmp` is created and then atomically renamed to `<db>`.
fn init_tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".init-tmp");
    PathBuf::from(s)
}

/// Map a `Database::create` failure on open. redb surfaces a torn or
/// foreign header as an `InvalidData` I/O error; the init-temp file was
/// already cleaned by the caller, so a bad magic on an existing,
/// non-empty database means real damage or foreign data: fail closed as
/// [`StorageError::Corrupt`] instead of leaking redb's raw error, and
/// never silently reinitialize over it.
fn map_open_error(e: redb::DatabaseError, path: &Path) -> StorageError {
    let bad_magic = matches!(
        &e,
        redb::DatabaseError::Storage(redb::StorageError::Io(io_e))
            if io_e.kind() == std::io::ErrorKind::InvalidData
    );
    let nonempty = path.metadata().map(|m| m.len() > 0).unwrap_or(false);
    if bad_magic && nonempty {
        StorageError::Corrupt(format!(
            "database file '{}' has an invalid header (torn write or foreign data); refusing to open rather than reinitializing",
            path.display()
        ))
    } else {
        StorageError::from(e)
    }
}

impl ChainStore {
    /// Open (or create) the database file at `path`. Parent directories
    /// are created. Refuses databases with a newer schema version.
    ///
    /// First-time initialization is crash-atomic: the database is
    /// created at `<path>.init-tmp` and atomically renamed into place,
    /// so a SIGKILL landing inside redb's initial `Database::create`
    /// (before the magic number is written) can only tear the temp file,
    /// which is deleted on the next open. `path` either doesn't exist or
    /// is a fully initialized database. A bad magic number on an
    /// existing, non-empty database is [`StorageError::Corrupt`]
    /// (fail-closed): it implies a torn header or foreign data, and
    /// silently reinitializing could destroy committed state.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let tmp = init_tmp_path(path);
        if tmp.exists() {
            // Leftover from a crashed init: it can never contain committed
            // data (init never completed), so removal is safe.
            std::fs::remove_file(&tmp)?;
        }
        if !path.exists() {
            Database::create(&tmp)?;
            std::fs::rename(&tmp, path)?;
        }
        let db = Database::create(path).map_err(|e| map_open_error(e, path))?;
        let store = Self { db };
        store.create_tables()?;
        store.ensure_schema_version()?;
        Ok(store)
    }

    /// Create all tables if missing. redb only creates tables on write-txn
    /// open; a read-txn open of a missing table errors, so every table must
    /// exist from the first open.
    fn create_tables(&self) -> Result<(), StorageError> {
        let wtxn = self.db.begin_write()?;
        {
            wtxn.open_table(ACCOUNTS)?;
            wtxn.open_table(BLOCK_HEADERS)?;
            wtxn.open_table(BLOCK_BODIES)?;
            wtxn.open_table(SEQNO_TO_HASH)?;
            wtxn.open_table(STATE_ROOTS)?;
            wtxn.open_table(META)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Write the schema version on first open; refuse newer versions.
    fn ensure_schema_version(&self) -> Result<(), StorageError> {
        let rtxn = self.db.begin_read()?;
        let meta = rtxn.open_table(META)?;
        if let Some(v) = meta.get(b"schema_version".as_slice())? {
            let found = u32_from_value(v.value(), "schema_version")?;
            if found != SCHEMA_VERSION {
                return Err(StorageError::SchemaMismatch {
                    found,
                    supported: SCHEMA_VERSION,
                });
            }
            return Ok(());
        }
        drop(meta);
        drop(rtxn);
        // Fresh database: claim the schema version inside a write txn,
        // re-checking in case another opener raced us.
        let wtxn = self.db.begin_write()?;
        {
            let mut meta = wtxn.open_table(META)?;
            if meta.get(b"schema_version".as_slice())?.is_none() {
                meta.insert(
                    b"schema_version".as_slice(),
                    SCHEMA_VERSION.to_be_bytes().as_slice(),
                )?;
            }
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Initialize the chain from a genesis document. Idempotent: calling it
    /// again with the same document is a no-op; a *different* genesis hash
    /// is corruption (the database already has an identity).
    ///
    /// Writes all genesis accounts, the seqno-0 pseudo-entry, and the
    /// chain-identity meta keys in one transaction, and sets the head to
    /// `(0, genesis_hash)`. Trie cells are NOT persisted: the state root is
    /// recomputed from accounts on every load (see `load_state`), and no
    /// reader ever needed the cells table (dropped in schema v2).
    pub fn init_genesis(&self, doc: &GenesisDocument) -> Result<(), StorageError> {
        let genesis_hash = doc.genesis_hash();
        let wtxn = self.db.begin_write()?;
        {
            let mut meta = wtxn.open_table(META)?;
            if let Some(existing) = meta.get(b"genesis_hash".as_slice())? {
                let existing_hash = hash32_from_value(existing.value(), "genesis_hash")?;
                if existing_hash == genesis_hash {
                    return Ok(()); // idempotent re-init
                }
                return Err(StorageError::Corrupt(
                    "database already initialized with a different genesis".to_string(),
                ));
            }

            let tree = doc.state_tree();
            let root = tree.state_root_hash()?;

            {
                let mut accts = wtxn.open_table(ACCOUNTS)?;
                for (id, st) in tree.accounts() {
                    accts.insert(id.to_bytes().as_slice(), st.to_bytes().as_slice())?;
                }
            }

            let zero = 0u32.to_be_bytes();
            wtxn.open_table(SEQNO_TO_HASH)?
                .insert(zero.as_slice(), genesis_hash.as_slice())?;
            wtxn.open_table(STATE_ROOTS)?
                .insert(zero.as_slice(), root.as_slice())?;

            meta.insert(b"genesis_hash".as_slice(), genesis_hash.as_slice())?;
            meta.insert(b"chain_id".as_slice(), genesis_hash.as_slice())?;
            meta.insert(
                b"workchain".as_slice(),
                doc.workchain.0 .0.to_be_bytes().as_slice(),
            )?;
            meta.insert(b"head".as_slice(), head_value(0, &genesis_hash).as_slice())?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Commit a block atomically.
    ///
    /// Runs the pure STF first (validating seqno, prev-hash, workchain,
    /// logical time, txs_root, and the claimed post-state root), then
    /// persists the block body, block header, touched accounts, the
    /// seqno→hash index entry, the post-state root, and the head pointer in
    /// **one redb write transaction**.
    ///
    /// Idempotent: committing the same block twice is a no-op. Committing
    /// a *different* block at an already-committed seqno is
    /// [`StorageError::ForkDetected`]. The idempotency/fork check runs
    /// inside the write transaction so concurrent committers cannot
    /// interleave a fork.
    pub fn commit_block(&self, state: &State, block: &Block) -> Result<(), StorageError> {
        let h = &block.header;
        let block_hash = h.hash();

        // 1. Pure STF: fail-closed validation of the whole block.
        let (new_state, receipts) = apply_block(state, block)?;

        // 2. One atomic write transaction for everything below. Each table
        // is opened exactly once: redb rejects opening the same table
        // twice within one write transaction.
        let wtxn = self.db.begin_write()?;
        {
            let mut seq_tbl = wtxn.open_table(SEQNO_TO_HASH)?;
            let mut bodies_tbl = wtxn.open_table(BLOCK_BODIES)?;
            let mut headers_tbl = wtxn.open_table(BLOCK_HEADERS)?;
            let mut accts_tbl = wtxn.open_table(ACCOUNTS)?;
            let mut roots_tbl = wtxn.open_table(STATE_ROOTS)?;
            let mut meta_tbl = wtxn.open_table(META)?;

            // Idempotency + fork check inside the txn (single-writer
            // serialization makes this airtight). This runs first so that
            // re-committing an already-committed block stays a no-op even
            // though it does not satisfy the continuity check below.
            if let Some(existing) = seq_tbl.get(h.seqno.to_be_bytes().as_slice())? {
                let existing_hash = hash32_from_value(existing.value(), "seqno_to_hash")?;
                if existing_hash == block_hash {
                    return Ok(()); // already committed: skip
                }
                return Err(StorageError::ForkDetected {
                    seqno: h.seqno,
                    existing: existing_hash,
                    incoming: block_hash,
                });
            }

            // Continuity (gap 1 fix): the block must build directly on the
            // stored head — (seqno, prev_hash) == (head.seqno + 1,
            // head.block_hash). The STF above tied the block to the
            // caller's in-memory state; this ties the caller's state to
            // the database. Without it, a stale or diverged state could
            // commit a block that skips seqnos or rebases the chain onto
            // the wrong base: the seqno-exists check alone cannot see a
            // wrong base, only a missing or duplicate seqno.
            let (head_seqno, head_hash) = match meta_tbl.get(b"head".as_slice())? {
                None => {
                    return Err(StorageError::Corrupt(
                        "commit_block with no stored head: init_genesis was never called"
                            .to_string(),
                    ))
                }
                Some(v) => parse_head_value(v.value())?,
            };
            let expected_seqno = head_seqno.checked_add(1).ok_or_else(|| {
                StorageError::Corrupt("stored head seqno is u32::MAX; cannot advance".to_string())
            })?;
            if h.seqno != expected_seqno || h.prev_hash != head_hash {
                return Err(StorageError::HeadMismatch {
                    head_seqno,
                    head_hash,
                    block_seqno: h.seqno,
                    block_prev_hash: h.prev_hash,
                });
            }

            // Block identity and data.
            bodies_tbl.insert(block_hash.as_slice(), encode_body(&block.body).as_slice())?;
            headers_tbl.insert(block_hash.as_slice(), h.to_bytes().as_slice())?;

            // Touched accounts only — no full-shard rewrite.
            for id in dirty_accounts(block, &receipts) {
                if let Some(st) = new_state.tree.get(&AccountId::from_bytes(id)) {
                    accts_tbl.insert(id.as_slice(), st.to_bytes().as_slice())?;
                }
                // Absent from the new tree: the account was never
                // created (e.g. an untouched fee collector). Nothing
                // to persist.
            }

            // Index, post-state root, and head advance together. The head
            // pointer can therefore only ever reference a fully committed
            // block: there is no observable torn state.
            let seq_key = h.seqno.to_be_bytes();
            let new_root = new_state.tree.state_root_hash()?;
            seq_tbl.insert(seq_key.as_slice(), block_hash.as_slice())?;
            roots_tbl.insert(seq_key.as_slice(), new_root.as_slice())?;
            meta_tbl.insert(
                b"head".as_slice(),
                head_value(h.seqno, &block_hash).as_slice(),
            )?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Current head: `(seqno, block_hash)`. `None` before genesis init.
    pub fn head(&self) -> Result<Option<(u32, [u8; 32])>, StorageError> {
        let rtxn = self.db.begin_read()?;
        let meta = rtxn.open_table(META)?;
        match meta.get(b"head".as_slice())? {
            None => Ok(None),
            Some(v) => parse_head_value(v.value()).map(Some),
        }
    }

    /// Block hash committed at `seqno`, if any.
    pub fn block_hash_for_seqno(&self, seqno: u32) -> Result<Option<[u8; 32]>, StorageError> {
        let rtxn = self.db.begin_read()?;
        let tbl = rtxn.open_table(SEQNO_TO_HASH)?;
        match tbl.get(seqno.to_be_bytes().as_slice())? {
            None => Ok(None),
            Some(v) => Ok(Some(hash32_from_value(v.value(), "seqno_to_hash")?)),
        }
    }

    /// Post-state root after `seqno`, if committed.
    pub fn state_root_at(&self, seqno: u32) -> Result<Option<[u8; 32]>, StorageError> {
        let rtxn = self.db.begin_read()?;
        let tbl = rtxn.open_table(STATE_ROOTS)?;
        match tbl.get(seqno.to_be_bytes().as_slice())? {
            None => Ok(None),
            Some(v) => Ok(Some(hash32_from_value(v.value(), "state_roots")?)),
        }
    }

    /// Block header by block hash, if stored.
    pub fn get_block_header(&self, hash: &[u8; 32]) -> Result<Option<BlockHeader>, StorageError> {
        let rtxn = self.db.begin_read()?;
        let tbl = rtxn.open_table(BLOCK_HEADERS)?;
        match tbl.get(hash.as_slice())? {
            None => Ok(None),
            Some(v) => BlockHeader::from_bytes(v.value())
                .map(Some)
                .map_err(|e| StorageError::Corrupt(format!("stored header undecodable: {e}"))),
        }
    }

    /// Block body by block hash, if stored.
    pub fn get_block_body(&self, hash: &[u8; 32]) -> Result<Option<BlockBody>, StorageError> {
        let rtxn = self.db.begin_read()?;
        let tbl = rtxn.open_table(BLOCK_BODIES)?;
        match tbl.get(hash.as_slice())? {
            None => Ok(None),
            Some(v) => decode_body(v.value()).map(Some),
        }
    }

    /// Account state by id, if present.
    pub fn get_account(&self, id: &AccountId) -> Result<Option<AccountState>, StorageError> {
        let rtxn = self.db.begin_read()?;
        let tbl = rtxn.open_table(ACCOUNTS)?;
        match tbl.get(id.to_bytes().as_slice())? {
            None => Ok(None),
            Some(v) => {
                let (st, used) = AccountState::from_bytes(v.value())?;
                if used != v.value().len() {
                    return Err(StorageError::Corrupt(
                        "trailing bytes in stored account record".to_string(),
                    ));
                }
                Ok(Some(st))
            }
        }
    }

    /// Genesis hash this database was initialized with, if any.
    pub fn genesis_hash(&self) -> Result<Option<[u8; 32]>, StorageError> {
        let rtxn = self.db.begin_read()?;
        let meta = rtxn.open_table(META)?;
        match meta.get(b"genesis_hash".as_slice())? {
            None => Ok(None),
            Some(v) => Ok(Some(hash32_from_value(v.value(), "genesis_hash")?)),
        }
    }

    /// The chain's identity (genesis hash), for binding external messages.
    /// `None` before genesis init. The mempool uses this to validate the
    /// chain ID at the door.
    pub fn chain_id(&self) -> Result<Option<[u8; 32]>, StorageError> {
        let rtxn = self.db.begin_read()?;
        let meta = rtxn.open_table(META)?;
        match meta.get(b"chain_id".as_slice())? {
            None => Ok(None),
            Some(v) => Ok(Some(hash32_from_value(v.value(), "chain_id")?)),
        }
    }

    /// Reconstruct the full in-memory [`State`] at the head.
    ///
    /// `None` before genesis init. This is the resume path after a crash:
    /// reopen, read the head pointer, rebuild the state, continue.
    pub fn load_state(&self) -> Result<Option<State>, StorageError> {
        let Some((seqno, last_hash)) = self.head()? else {
            return Ok(None);
        };
        let rtxn = self.db.begin_read()?;

        let mut tree = ShardStateTree::new();
        {
            let accts = rtxn.open_table(ACCOUNTS)?;
            for entry in accts.iter()? {
                let (k, v) = entry?;
                let id = AccountId::from_bytes(hash32_from_value(k.value(), "accounts key")?);
                let (st, used) = AccountState::from_bytes(v.value())?;
                if used != v.value().len() {
                    return Err(StorageError::Corrupt(
                        "trailing bytes in stored account record".to_string(),
                    ));
                }
                tree.insert(id, st);
            }
        }

        // Gap 2 fix: verify the rebuilt state hashes to the stored
        // post-state root for the head seqno. Replaying external blocks
        // catches corruption at the *next* block — but a self-producing
        // node would build its next block on top of corrupt state, and
        // there is no next external block to catch it. Fail closed here
        // instead of handing a corrupt state to the caller.
        let rebuilt_root = tree.state_root_hash()?;
        let roots_tbl = rtxn.open_table(STATE_ROOTS)?;
        let stored_root = roots_tbl
            .get(seqno.to_be_bytes().as_slice())?
            .map(|v| hash32_from_value(v.value(), "state_roots"))
            .transpose()?
            .ok_or_else(|| {
                StorageError::Corrupt(format!("no stored state root for head seqno {seqno}"))
            })?;
        if rebuilt_root != stored_root {
            return Err(StorageError::StateRootMismatch {
                seqno,
                stored: stored_root,
                rebuilt: rebuilt_root,
            });
        }

        let meta = rtxn.open_table(META)?;
        let chain_id = match meta.get(b"chain_id".as_slice())? {
            None => {
                return Err(StorageError::Corrupt(
                    "genesis initialized but chain_id missing from meta".to_string(),
                ))
            }
            Some(v) => hash32_from_value(v.value(), "chain_id")?,
        };
        let workchain = match meta.get(b"workchain".as_slice())? {
            None => {
                return Err(StorageError::Corrupt(
                    "genesis initialized but workchain missing from meta".to_string(),
                ))
            }
            Some(v) => {
                let b: [u8; 4] = v.value().try_into().map_err(|_| {
                    StorageError::Corrupt("workchain value not 4 bytes".to_string())
                })?;
                i32::from_be_bytes(b)
            }
        };

        // last_lt: seqno 0 is genesis (lt 0); otherwise the head header's lt.
        let last_lt = if seqno == 0 {
            0
        } else {
            let headers = rtxn.open_table(BLOCK_HEADERS)?;
            let hbytes = headers.get(last_hash.as_slice())?.ok_or_else(|| {
                StorageError::Corrupt("head hash has no stored header".to_string())
            })?;
            BlockHeader::from_bytes(hbytes.value())
                .map_err(|e| StorageError::Corrupt(format!("stored head header undecodable: {e}")))?
                .lt
        };

        Ok(Some(State {
            tree,
            workchain,
            chain_id,
            seqno,
            last_lt,
            last_hash,
        }))
    }
}

/// Accounts a block may have touched: every external sender, every
/// internal-message source and destination (including bounces), plus the
/// fee collector. Inclusion is deliberately generous rather than clever:
/// the collector is included even when a block's fees are all zero (in
/// which case it is simply absent from the new tree and skipped). What
/// matters is that no account the STF wrote is ever missing from this set
/// — that property is covered by `storage_dirty_set_complete`.
fn dirty_accounts(block: &Block, receipts: &Receipts) -> BTreeSet<[u8; 32]> {
    let mut set = BTreeSet::new();
    for r in &receipts.0 {
        set.insert(r.sender.to_bytes());
        for d in &r.deliveries {
            set.insert(d.src.to_bytes());
            set.insert(d.dest.to_bytes());
        }
    }
    set.insert(block.header.fee_collector.to_bytes());
    set
}

/// `meta[b"head"]` value: seqno u32 BE ++ block hash.
fn head_value(seqno: u32, hash: &[u8; 32]) -> Vec<u8> {
    let mut v = Vec::with_capacity(36);
    v.extend_from_slice(&seqno.to_be_bytes());
    v.extend_from_slice(hash);
    v
}

/// Parse a `meta[b"head"]` value. Shared by [`ChainStore::head`] and the
/// continuity check inside `commit_block`'s write transaction.
fn parse_head_value(b: &[u8]) -> Result<(u32, [u8; 32]), StorageError> {
    if b.len() != 36 {
        return Err(StorageError::Corrupt(format!(
            "head value has {} bytes, expected 36",
            b.len()
        )));
    }
    let seqno = u32::from_be_bytes(b[0..4].try_into().expect("len checked"));
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&b[4..36]);
    Ok((seqno, hash))
}

fn hash32_from_value(v: &[u8], what: &str) -> Result<[u8; 32], StorageError> {
    v.try_into().map_err(|_| {
        StorageError::Corrupt(format!("{what} value has {} bytes, expected 32", v.len()))
    })
}

fn u32_from_value(v: &[u8], what: &str) -> Result<u32, StorageError> {
    let b: [u8; 4] = v.try_into().map_err(|_| {
        StorageError::Corrupt(format!("{what} value has {} bytes, expected 4", v.len()))
    })?;
    Ok(u32::from_be_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{
        test_accounts, test_block_lt, test_block_txs, test_fee_collector, test_genesis,
    };
    use onx_stf::propose_block;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db_path(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("onx-cells-{tag}-{}-{nanos}", std::process::id()))
    }

    /// Schema v2 dropped the write-only `cells` table (Claude gap 3): it
    /// was written on every commit but read by nothing — O(cells) write
    /// amplification per block with unbounded growth and no GC, serving no
    /// reader. This test proves the table is gone: a full genesis +
    /// block-commit cycle runs end-to-end without it, and opening `cells`
    /// in a read transaction fails closed with `TableDoesNotExist`.
    /// Historical state remains reconstructible via replay from genesis +
    /// persisted block bodies (the phase5 replay suite covers that path).
    #[test]
    fn schema_v2_has_no_cells_table() {
        let path = temp_db_path("no-cells");
        let _ = std::fs::remove_file(&path);
        let store = ChainStore::open(&path).expect("open");
        store.init_genesis(&test_genesis()).expect("init_genesis");

        // A real block commit exercises the store end-to-end with no cells
        // table anywhere in the path.
        let accounts = test_accounts();
        let state = store
            .load_state()
            .expect("load_state")
            .expect("genesis state");
        let block = propose_block(
            &state,
            test_block_txs(7, 1, &accounts, state.chain_id),
            test_block_lt(1),
            test_fee_collector(),
        )
        .expect("propose_block");
        store.commit_block(&state, &block).expect("commit_block");
        assert_eq!(store.head().expect("head").expect("head").0, 1);

        // The cells table must not exist.
        let rtxn = store.db.begin_read().expect("read txn");
        let err = rtxn
            .open_table(TableDefinition::<&[u8], &[u8]>::new("cells"))
            .expect_err("cells table must not exist in schema v2");
        assert!(
            matches!(err, redb::TableError::TableDoesNotExist(_)),
            "expected TableDoesNotExist, got: {err:?}"
        );

        // And the schema version records the break from v1 (which had it).
        assert_eq!(SCHEMA_VERSION, 2);

        drop(rtxn);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }
}
