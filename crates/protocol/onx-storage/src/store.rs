//! [`ChainStore`]: atomic, crash-safe chain storage on redb.
//!
//! Schema (all tables are `&[u8] -> &[u8]`; keys are fixed-size big-endian
//! so iteration order is canonical):
//!
//! | Table           | Key                  | Value                          |
//! |-----------------|----------------------|--------------------------------|
//! | `cells`         | cell hash `[u8;32]`  | cell bytes                     |
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
use onx_state_model::{AccountState, Cell, GenesisDocument, ShardStateTree};
use onx_stf::{apply_block, Block, BlockBody, BlockHeader, Receipts, State};
use redb::{Database, ReadableTable, TableDefinition};
use std::collections::BTreeSet;
use std::path::Path;

/// Current schema version. Bump when the table layout changes; `open`
/// refuses databases written by a newer version (fail-closed).
pub const SCHEMA_VERSION: u32 = 1;

const CELLS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("cells");
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

impl ChainStore {
    /// Open (or create) the database file at `path`. Parent directories
    /// are created. Refuses databases with a newer schema version.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let db = Database::create(path)?;
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
            wtxn.open_table(CELLS)?;
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
    /// Writes all genesis accounts, the genesis trie cells, the seqno-0
    /// pseudo-entry, and the chain-identity meta keys in one transaction,
    /// and sets the head to `(0, genesis_hash)`.
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
            {
                let mut cells = wtxn.open_table(CELLS)?;
                for (ch, cell) in tree.trie_cells()? {
                    insert_cell(&mut cells, &ch, &cell)?;
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
    /// persists the block body, block header, new trie cells, touched
    /// accounts, the seqno→hash index entry, the post-state root, and the
    /// head pointer in **one redb write transaction**.
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
            let mut cells_tbl = wtxn.open_table(CELLS)?;
            let mut accts_tbl = wtxn.open_table(ACCOUNTS)?;
            let mut roots_tbl = wtxn.open_table(STATE_ROOTS)?;
            let mut meta_tbl = wtxn.open_table(META)?;

            // Idempotency + fork check inside the txn (single-writer
            // serialization makes this airtight).
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

            // Block identity and data.
            bodies_tbl.insert(block_hash.as_slice(), encode_body(&block.body).as_slice())?;
            headers_tbl.insert(block_hash.as_slice(), h.to_bytes().as_slice())?;

            // New trie cells (content-addressed: re-insertion is idempotent).
            for (ch, cell) in new_state.tree.trie_cells()? {
                insert_cell(&mut cells_tbl, &ch, &cell)?;
            }

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
            Some(v) => {
                let b = v.value();
                if b.len() != 36 {
                    return Err(StorageError::Corrupt(format!(
                        "head value has {} bytes, expected 36",
                        b.len()
                    )));
                }
                let seqno = u32::from_be_bytes(b[0..4].try_into().expect("len checked"));
                let mut hash = [0u8; 32];
                hash.copy_from_slice(&b[4..36]);
                Ok(Some((seqno, hash)))
            }
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

        let meta = rtxn.open_table(META)?;
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
            seqno,
            last_lt,
            last_hash,
        }))
    }
}

/// Accounts a block may have touched: every sender and receiver, plus the
/// fee collector. Inclusion is deliberately generous rather than clever:
/// the collector is included even when a block's fees are all zero (in
/// which case it is simply absent from the new tree and skipped). What
/// matters is that no account the STF wrote is ever missing from this set
/// — that property is covered by `storage_dirty_set_complete`.
fn dirty_accounts(block: &Block, receipts: &Receipts) -> BTreeSet<[u8; 32]> {
    let mut set = BTreeSet::new();
    for r in &receipts.0 {
        set.insert(r.sender.to_bytes());
        set.insert(r.receiver.to_bytes());
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

/// Insert a trie cell, content-addressed. Re-insertion of an identical
/// cell is a no-op write of identical bytes.
fn insert_cell(
    tbl: &mut redb::Table<&[u8], &[u8]>,
    hash: &[u8; 32],
    cell: &Cell,
) -> Result<(), StorageError> {
    tbl.insert(hash.as_slice(), cell.to_bytes().as_slice())?;
    Ok(())
}
