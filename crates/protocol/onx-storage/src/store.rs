//! [`ChainStore`]: atomic, crash-safe chain storage on redb.
//!
//! Schema (all tables are `&[u8] -> &[u8]`; keys are fixed-size big-endian
//! so iteration order is canonical):
//!
//! | Table           | Key                  | Value                          |
//! |-----------------|----------------------|--------------------------------|
//! | `accounts`      | account id `[u8;32]` | `AccountState` bytes           |
//! | `block_headers` | block hash `[u8;32]` | 160-byte canonical header      |
//! | `block_bodies`  | block hash `[u8;32]` | `encode_body` bytes            |
//! | `seqno_to_hash` | seqno `u32` BE       | block hash `[u8;32]`           |
//! | `state_roots`   | seqno `u32` BE       | state root `[u8;32]`           |
//! | `contract_cells`| account id `[u8;32]` | `ContractCellDags` bytes       |
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
use onx_state_model::{AccountState, ContractCellDags, GenesisDocument, ShardStateTree};
use onx_stf::{apply_block, Block, BlockBody, BlockHeader, Receipts, SigEntry, State};
use redb::{Database, ReadableTable, TableDefinition};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Current schema version. Bump when the table layout changes; `open`
/// refuses databases written by a different version (fail-closed).
/// v1 -> v2: dropped the write-only `cells` table (see schema docs above).
/// v1 was never released, so no migration is provided — resync from genesis.
/// v2 -> v3: added the `contract_cells` table (account id -> contract cell
/// DAGs). The table is additive and auto-created by `create_tables`, so the
/// migration only bumps the version; no data moves. Unlike the v1 `cells`
/// table, this one has a reader: the STF seeds the TVM interpreter's cell
/// store from it on contract load.
/// v3 -> v4: added the `block_sigs` table (block hash -> signature section
/// bytes). ONXBLK05 (ADR-0032): signatures are committed atomically with
/// the block — TRAP 2 (signature only in the file) is a consensus hazard
/// on crash recovery. A v3 database WITH blocks is refused (SchemaMismatch):
/// its blocks have no signatures and cannot be upgraded. An empty v3
/// database migrates by creating the table and bumping the version.
pub const SCHEMA_VERSION: u32 = 4;

const ACCOUNTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("accounts");
const BLOCK_HEADERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("block_headers");
const BLOCK_BODIES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("block_bodies");
const SEQNO_TO_HASH: TableDefinition<&[u8], &[u8]> = TableDefinition::new("seqno_to_hash");
const STATE_ROOTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("state_roots");
const META: TableDefinition<&[u8], &[u8]> = TableDefinition::new("meta");
/// Contract cell DAGs: account id `[u8;32]` -> `ContractCellDags` bytes
/// (code BoC + data BoC). Written on every contract execution (the
/// account's DAGs are rebuilt from the interpreter's drained cell store);
/// read on contract load to seed the interpreter. Overwritten in place —
/// each account has exactly one entry, so growth is bounded by the number
/// of contract accounts, not by history.
const CONTRACT_CELLS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("contract_cells");
/// Block signature sections: block hash `[u8;32]` -> sig-section bytes
/// (`count(u32be) || [validator_index(u32be) || sig(64)]*`). Written
/// atomically with the block in `commit_block` (ONXBLK05, ADR-0032).
const BLOCK_SIGS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("block_sigs");

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

/// Decode persisted [`ContractCellDags`] bytes under the *persisted* BoC
/// profile (ADR-0029).
///
/// Two BoC profiles exist in this codebase and must not be confused:
///
/// - **Proof profile** (`BagOfCells::from_bytes`, shared with
///   `MerkleProof::from_bytes`): dangling references are *allowed* — a
///   Merkle proof legitimately commits sibling subtree hashes without
///   including the sibling cells.
/// - **Persisted profile** (this function): every reference must resolve
///   to a cell in the bag. The TVM's `LDREF` needs the actual child
///   *content*: a dangling ref here fails closed at `LDREF`
///   (`AbsentNode`) and the delivery bounces — while a node holding the
///   full DAG executes. That is a silent state-root divergence, so a
///   dangling ref in persisted content is a LOCAL FAULT, failed loudly
///   here: never a bounce, never "invalid", never silent.
///
/// This is local node-fault behavior, not consensus: it changes nothing
/// about block validity, only what this node refuses to run on.
fn decode_contract_dags_persisted(bytes: &[u8]) -> Result<ContractCellDags, String> {
    let dags = ContractCellDags::from_bytes(bytes).map_err(|e| e.to_string())?;
    for (label, boc) in [("code", &dags.code), ("data", &dags.data)] {
        for cell in boc.cells().values() {
            for r in cell.cell_refs() {
                if !boc.cells().contains_key(r) {
                    return Err(format!(
                        "persisted {label} DAG has a dangling reference to missing cell {} \
                         (persisted DAGs must be complete; the proof profile used by Merkle \
                         proofs is the one that tolerates dangling refs)",
                        hex32(r),
                    ));
                }
            }
        }
    }
    Ok(dags)
}

/// Full 64-char hex of a hash, for messages that must name the exact root.
fn hex32(h: &[u8; 32]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// TEST-ONLY: when set, [`ChainStore::commit_block`] panics immediately
/// after the STF's `apply_block` returns, simulating an internal
/// interpreter/STF panic during real block application. Lets tests assert
/// the panic policy (ADR-0029): the panic unwinds out of `commit_block` —
/// never caught, never converted to a `StorageError` ("invalid block") —
/// and the uncommitted write transaction is dropped, so nothing partial
/// persists.
#[cfg(test)]
static INJECT_COMMIT_PANIC: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// TEST-ONLY. Arms/disarms [`INJECT_COMMIT_PANIC`].
#[cfg(test)]
pub(crate) fn test_set_inject_commit_panic(v: bool) {
    INJECT_COMMIT_PANIC.store(v, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
fn inject_commit_panic() -> bool {
    INJECT_COMMIT_PANIC.load(std::sync::atomic::Ordering::SeqCst)
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
        // ADR-0029 startup invariant: after any migration, every committed
        // code/data root must have a COMPLETE DAG in `contract_cells`. A
        // database migrated from v2 has an empty table while a
        // genesis-replayed database has full DAGs — the first LDREF into
        // a child cell would then DIVERGE (one node bounces, the other
        // executes). Node-fatal by design: replay from genesis or restore
        // from a backup.
        store.verify_contract_cell_dag_completeness()?;
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
            wtxn.open_table(BLOCK_SIGS)?;
            wtxn.open_table(SEQNO_TO_HASH)?;
            wtxn.open_table(STATE_ROOTS)?;
            wtxn.open_table(CONTRACT_CELLS)?;
            wtxn.open_table(META)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// v3 -> v4 migration: the `block_sigs` table is additive, but only an
    /// EMPTY v3 database may migrate. A v3 database holding blocks is
    /// True if the seqno→hash index holds any entry above genesis
    /// (seqno 0). Used by the migration guards: a database with blocks
    /// committed under an older schema cannot be upgraded in place.
    fn has_blocks_above_genesis(&self) -> Result<bool, StorageError> {
        let rtxn = self.db.begin_read()?;
        let seq_tbl = rtxn.open_table(SEQNO_TO_HASH)?;
        for e in seq_tbl.iter()? {
            let (k, _) = e?;
            let seqno = u32::from_be_bytes(
                k.value()
                    .try_into()
                    .map_err(|_| StorageError::Corrupt("bad seqno key".to_string()))?,
            );
            if seqno > 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// refused — its blocks were committed without signatures and cannot
    /// be upgraded (TRAP 2: resync from genesis).
    fn migrate_v3_to_v4(&self) -> Result<(), StorageError> {
        // Check for blocks first (read txn): any seqno->hash entry ABOVE
        // genesis (seqno 0) means the v3 DB holds blocks and cannot migrate.
        // Genesis itself is fine — it carries no signatures.
        if self.has_blocks_above_genesis()? {
            return Err(StorageError::SchemaMismatch {
                found: 3,
                supported: SCHEMA_VERSION,
            });
        }
        // Empty: create the table and bump the version.
        let wtxn = self.db.begin_write()?;
        {
            let mut meta = wtxn.open_table(META)?;
            let found = match meta.get(b"schema_version".as_slice())? {
                Some(v) => Some(u32_from_value(v.value(), "schema_version")?),
                None => None,
            };
            match found {
                Some(3) => {
                    // Create the table (auto-created by create_tables on
                    // fresh DBs; explicit here for the migration path).
                    let _ = wtxn.open_table(BLOCK_SIGS)?;
                    meta.insert(
                        b"schema_version".as_slice(),
                        SCHEMA_VERSION.to_be_bytes().as_slice(),
                    )?;
                }
                Some(found) if found != SCHEMA_VERSION => {
                    return Err(StorageError::SchemaMismatch {
                        found,
                        supported: SCHEMA_VERSION,
                    });
                }
                _ => {
                    meta.insert(
                        b"schema_version".as_slice(),
                        SCHEMA_VERSION.to_be_bytes().as_slice(),
                    )?;
                }
            }
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Write the schema version on first open; refuse newer versions.
    /// v2 -> v3 is a supported migration: the `contract_cells` table is
    /// purely additive (auto-created by `create_tables` above), so the
    /// migration only bumps the version key.
    /// v3 -> v4 is a CONDITIONAL migration: the `block_sigs` table is
    /// additive, but a v3 database that already holds blocks cannot be
    /// upgraded — its blocks have no signatures (TRAP 2). Empty v3
    /// databases migrate; non-empty ones get fail-closed `SchemaMismatch`.
    /// Anything else mismatched is fail-closed `SchemaMismatch`.
    fn ensure_schema_version(&self) -> Result<(), StorageError> {
        let rtxn = self.db.begin_read()?;
        let meta = rtxn.open_table(META)?;
        if let Some(v) = meta.get(b"schema_version".as_slice())? {
            let found = u32_from_value(v.value(), "schema_version")?;
            if found == SCHEMA_VERSION {
                return Ok(());
            }
            if found == 2 && SCHEMA_VERSION == 4 {
                drop(meta);
                drop(rtxn);
                return self.migrate_v2_to_v4();
            }
            if found == 3 && SCHEMA_VERSION == 4 {
                drop(meta);
                drop(rtxn);
                return self.migrate_v3_to_v4();
            }
            return Err(StorageError::SchemaMismatch {
                found,
                supported: SCHEMA_VERSION,
            });
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

    /// Migrate a v2 database to v3: the `contract_cells` table was already
    /// created by `create_tables`; there is no data to move (contract cell
    /// DAGs are rebuilt from execution going forward, and historical DAGs
    /// are reconstructible via replay). Just bump the version key,
    /// re-checking inside the write transaction in case another opener
    /// raced us.
    ///
    /// Like the v3 path, a v2 database WITH blocks is refused: its blocks
    /// were committed without signatures (148-byte headers) and cannot be
    /// upgraded — resync from genesis.
    fn migrate_v2_to_v4(&self) -> Result<(), StorageError> {
        if self.has_blocks_above_genesis()? {
            return Err(StorageError::SchemaMismatch {
                found: 2,
                supported: SCHEMA_VERSION,
            });
        }
        let wtxn = self.db.begin_write()?;
        {
            let mut meta = wtxn.open_table(META)?;
            // Read the version first (ending the immutable borrow) before
            // taking the mutable borrow for the bump.
            let found = match meta.get(b"schema_version".as_slice())? {
                Some(v) => Some(u32_from_value(v.value(), "schema_version")?),
                None => None,
            };
            match found {
                Some(2) => {
                    meta.insert(
                        b"schema_version".as_slice(),
                        SCHEMA_VERSION.to_be_bytes().as_slice(),
                    )?;
                }
                Some(found) if found != SCHEMA_VERSION => {
                    return Err(StorageError::SchemaMismatch {
                        found,
                        supported: SCHEMA_VERSION,
                    });
                }
                // `None`: the version key vanished between the read and
                // write txns — claim it fresh (same as the first-open
                // path). `Some(SCHEMA_VERSION)`: a racing opener already
                // migrated; re-writing the same value is a no-op.
                _ => {
                    meta.insert(
                        b"schema_version".as_slice(),
                        SCHEMA_VERSION.to_be_bytes().as_slice(),
                    )?;
                }
            }
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Startup invariant (ADR-0029): every committed code/data root must
    /// have a COMPLETE DAG in `contract_cells`.
    ///
    /// Background: schema v3 added the `contract_cells` table, but the
    /// v2→v3 migration only bumped the version key. A database migrated
    /// from v2 therefore has an EMPTY `contract_cells` table while a
    /// database replayed from genesis has full DAGs — and the first
    /// `LDREF` into a child cell DIVERGES: the migrated node fails closed
    /// (`AbsentNode`) and bounces the delivery, the replayed node executes
    /// it. Different receipts, different state roots, no error anywhere:
    /// a silent consensus split.
    ///
    /// So on every open (after any migration has run), this walks every
    /// account and requires: for each `Active` account with a committed
    /// code or data root, a `contract_cells` entry exists, decodes under
    /// the persisted (strict) BoC profile, and is rooted at exactly the
    /// account's committed root hash. Anything less is node-fatal
    /// [`StorageError::IncompleteContractCellDag`], naming the account and
    /// the offending root hash.
    ///
    /// This is deliberately NEVER silent and NEVER a later divergence: a
    /// node that cannot prove its DAGs complete refuses to start. It is
    /// also never consensus and never block validity — purely local
    /// node-fault behavior, never mapped to "invalid block" or a bounce.
    /// Recovery is explicit: replay from genesis or restore from a backup.
    fn verify_contract_cell_dag_completeness(&self) -> Result<(), StorageError> {
        let rtxn = self.db.begin_read()?;
        let accts = rtxn.open_table(ACCOUNTS)?;
        let cells_tbl = rtxn.open_table(CONTRACT_CELLS)?;
        for entry in accts.iter()? {
            let (k, v) = entry?;
            let id = AccountId::from_bytes(hash32_from_value(k.value(), "accounts key")?);
            let (st, used) = AccountState::from_bytes(v.value())?;
            if used != v.value().len() {
                return Err(StorageError::Corrupt(
                    "trailing bytes in stored account record".to_string(),
                ));
            }
            let (code, data) = match &st {
                AccountState::Active { code, data, .. } => (code.as_ref(), data.as_ref()),
                _ => continue,
            };
            // The committed roots that must have complete DAGs.
            let mut roots: Vec<(&str, [u8; 32])> = Vec::new();
            if let Some(c) = code {
                roots.push(("code", c.hash()));
            }
            if let Some(d) = data {
                roots.push(("data", d.hash()));
            }
            if roots.is_empty() {
                continue;
            }
            let raw = cells_tbl.get(id.to_bytes().as_slice())?.ok_or_else(|| {
                StorageError::IncompleteContractCellDag {
                    account: id.to_bytes(),
                    root: roots[0].1,
                    detail: format!(
                        "no contract_cells entry for an account with a committed {} root \
                         (v2→v3 migration leaves this table empty)",
                        roots[0].0,
                    ),
                }
            })?;
            let dags = decode_contract_dags_persisted(raw.value()).map_err(|detail| {
                StorageError::IncompleteContractCellDag {
                    account: id.to_bytes(),
                    root: roots[0].1,
                    detail,
                }
            })?;
            for (label, root) in &roots {
                let boc = if *label == "code" {
                    &dags.code
                } else {
                    &dags.data
                };
                if boc.root_hash() != root {
                    return Err(StorageError::IncompleteContractCellDag {
                        account: id.to_bytes(),
                        root: *root,
                        detail: format!(
                            "persisted {label} DAG is rooted at {}, not at the account's committed {label} root",
                            hex32(boc.root_hash()),
                        ),
                    });
                }
            }
        }
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
    /// Contract cell DAGs for genesis-installed contracts ARE persisted
    /// (the genesis document's complete DAGs, or single-root bags where
    /// the roots have no children — ADR-0041), so the ADR-0029 startup
    /// invariant holds on a fresh database.
    pub fn init_genesis(&self, doc: &GenesisDocument) -> Result<(), StorageError> {
        let genesis_hash = doc.genesis_hash();
        let wtxn = self.db.begin_write()?;
        {
            let mut meta = wtxn.open_table(META)?;
            let existing_hash: Option<[u8; 32]> = match meta.get(b"genesis_hash".as_slice())? {
                Some(v) => Some(hash32_from_value(v.value(), "genesis_hash")?),
                None => None,
            };
            if let Some(existing_hash) = existing_hash {
                if existing_hash == genesis_hash {
                    // Idempotent re-init — but backfill genesis_doc for
                    // databases initialized before ONXBLK05 step 5 added it
                    // (TRAP 4): the startup signing-key check needs the
                    // canonical genesis bytes.
                    let needs_backfill = meta.get(b"genesis_doc".as_slice())?.is_none();
                    if needs_backfill {
                        meta.insert(b"genesis_doc".as_slice(), doc.to_bytes().as_slice())?;
                    }
                    drop(meta);
                    wtxn.commit()?;
                    return Ok(());
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

            // Contract cell DAGs for genesis-installed contracts, exactly
            // as `GenesisDocument::state_tree` built them (ADR-0041): the
            // document's complete DAGs for contracts whose roots have
            // children, single-root bags otherwise. Without these entries
            // the startup invariant would refuse to open a database whose
            // only contract activity predates any execution, and a genesis
            // contract with code children would bounce on every call.
            {
                let mut cells_tbl = wtxn.open_table(CONTRACT_CELLS)?;
                for (id, dags) in tree.all_contract_cells() {
                    cells_tbl.insert(id.to_bytes().as_slice(), dags.to_bytes().as_slice())?;
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
            // The canonical genesis document bytes, so the node can verify
            // its signing key against the genesis validators at startup
            // (ONXBLK05 TRAP 4) without re-parsing the TOML.
            meta.insert(b"genesis_doc".as_slice(), doc.to_bytes().as_slice())?;
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
    pub fn commit_block(
        &self,
        state: &State,
        block: &Block,
        sig_entries: &[SigEntry],
    ) -> Result<(), StorageError> {
        let h = &block.header;
        let block_hash = h.hash();

        // An empty signature section can never satisfy the >2/3 stake rule,
        // so it is never a valid committed block. Reject here (defense in
        // depth alongside the acceptance layer's full verification) rather
        // than persisting a block no verifier will accept.
        if sig_entries.is_empty() {
            return Err(StorageError::Corrupt(
                "commit_block: empty signature section".to_string(),
            ));
        }

        // 1. Pure STF: fail-closed validation of the whole block.
        //
        // PANIC POLICY (ADR-0029): a panic inside `apply_block` above —
        // real block application, not the producer dry-run — is NEVER
        // caught here. It unwinds through this function, aborting the
        // not-yet-opened write transaction (nothing partial is ever
        // persisted), and halts the node. That is the correct outcome: a
        // panic means this node's own execution is broken, and mapping it
        // to "invalid block" or bouncing the message would let a broken
        // node keep running and silently diverge from honest nodes. Panic
        // containment lives in the producer dry-run (`propose_block` in
        // onxd); this path must not have it.
        let (new_state, receipts) = apply_block(state, block)?;

        // TEST-ONLY: simulate a panic inside real block application (see
        // ADR-0029). This must unwind out of `commit_block` — never
        // caught, never mapped to a `StorageError`.
        #[cfg(test)]
        if inject_commit_panic() {
            panic!(
                "injected test panic: simulated panic during apply_block (real block application)"
            );
        }

        // 2. One atomic write transaction for everything below. Each table
        // is opened exactly once: redb rejects opening the same table
        // twice within one write transaction.
        let wtxn = self.db.begin_write()?;
        {
            let mut seq_tbl = wtxn.open_table(SEQNO_TO_HASH)?;
            let mut bodies_tbl = wtxn.open_table(BLOCK_BODIES)?;
            let mut headers_tbl = wtxn.open_table(BLOCK_HEADERS)?;
            let mut sigs_tbl = wtxn.open_table(BLOCK_SIGS)?;
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

            // Block identity and data. The signature section is committed in
            // the SAME write txn (ONXBLK05, ADR-0032 TRAP 2): a crash between
            // block commit and signature persist must never leave a block
            // without its signatures.
            bodies_tbl.insert(block_hash.as_slice(), encode_body(&block.body).as_slice())?;
            headers_tbl.insert(block_hash.as_slice(), h.to_bytes().as_slice())?;
            sigs_tbl.insert(
                block_hash.as_slice(),
                onx_stf::encode_sig_section(sig_entries).as_slice(),
            )?;

            // Touched accounts only — no full-shard rewrite.
            for id in dirty_accounts(block, &receipts) {
                if let Some(st) = new_state.tree.get(&AccountId::from_bytes(id)) {
                    accts_tbl.insert(id.as_slice(), st.to_bytes().as_slice())?;
                }
                // Absent from the new tree: the account was never
                // created (e.g. an untouched fee collector). Nothing
                // to persist.
            }

            // Contract cell DAGs rebuilt by this block's executions.
            // Only changed DAGs are written (the map is carried over
            // unchanged for contracts that did not execute).
            {
                let mut cells_tbl = wtxn.open_table(CONTRACT_CELLS)?;
                for (id, dags) in new_state.tree.all_contract_cells() {
                    let changed = match state.tree.contract_cells(id) {
                        Some(old) => old != dags,
                        None => true,
                    };
                    if changed {
                        cells_tbl.insert(id.to_bytes().as_slice(), dags.to_bytes().as_slice())?;
                    }
                }
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

    /// Signature section bytes for a block, if present. Raw bytes — decode
    /// with the strict section decoder (acceptance layer).
    pub fn get_block_sigs(&self, hash: &[u8; 32]) -> Result<Option<Vec<u8>>, StorageError> {
        let rtxn = self.db.begin_read()?;
        let tbl = rtxn.open_table(BLOCK_SIGS)?;
        match tbl.get(hash.as_slice())? {
            None => Ok(None),
            Some(v) => Ok(Some(v.value().to_vec())),
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

    /// The canonical genesis document, if the store was initialized.
    pub fn genesis_document(
        &self,
    ) -> Result<Option<onx_state_model::GenesisDocument>, StorageError> {
        let rtxn = self.db.begin_read()?;
        let meta = rtxn.open_table(META)?;
        match meta.get(b"genesis_doc".as_slice())? {
            None => Ok(None),
            Some(v) => {
                let doc = onx_state_model::GenesisDocument::from_bytes(v.value())
                    .map_err(|e| StorageError::Corrupt(format!("bad genesis_doc: {e}")))?;
                Ok(Some(doc))
            }
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
                tree.insert(id, st)?;
            }
        }

        // Contract cell DAGs: auxiliary execution state (not part of the
        // state root). Loaded here so the next contract invocation's
        // interpreter can be seeded with the full code/data DAGs.
        // Decoded under the persisted (strict) BoC profile (ADR-0029): a
        // dangling reference in persisted content is a local fault, failed
        // loudly — never a bounce. (The proof profile, which tolerates
        // dangling refs, belongs to Merkle proofs, not to this table.)
        {
            let cells_tbl = rtxn.open_table(CONTRACT_CELLS)?;
            for entry in cells_tbl.iter()? {
                let (k, v) = entry?;
                let id = AccountId::from_bytes(hash32_from_value(k.value(), "contract_cells key")?);
                let dags = decode_contract_dags_persisted(v.value()).map_err(|detail| {
                    StorageError::Corrupt(format!(
                        "stored contract cell DAGs for account {id:?} fail the persisted (strict) BoC profile: {detail}"
                    ))
                })?;
                tree.set_contract_cells(id, dags);
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

    /// Placeholder sig entries: commit_block stores (never verifies) the
    /// section; only non-emptiness is enforced at this layer.
    fn dummy_sigs() -> Vec<SigEntry> {
        vec![SigEntry {
            validator_index: 0,
            sig: [0xAB; 64],
        }]
    }

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
    fn schema_v4_has_no_legacy_cells_table() {
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
            1,
            0,
        )
        .expect("propose_block");
        store
            .commit_block(&state, &block, &dummy_sigs())
            .expect("commit_block");
        assert_eq!(store.head().expect("head").expect("head").0, 1);

        // The legacy v1 `cells` table must not exist.
        let rtxn = store.db.begin_read().expect("read txn");
        let err = rtxn
            .open_table(TableDefinition::<&[u8], &[u8]>::new("cells"))
            .expect_err("cells table must not exist in schema v3");
        assert!(
            matches!(err, redb::TableError::TableDoesNotExist(_)),
            "expected TableDoesNotExist, got: {err:?}"
        );

        // And the schema version records the break from v1 (which had it):
        // v4 is current (v3 added `contract_cells`, v4 added `block_sigs`).
        assert_eq!(SCHEMA_VERSION, 4);

        drop(rtxn);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    // ----- ADR-0029: migration startup invariant + panic policy -----

    #[test]
    fn migrate_v2_to_v4_bumps_version() {
        let path = temp_db_path("v2-migrate");
        let _ = std::fs::remove_file(&path);
        let store = ChainStore::open(&path).expect("open");
        store.init_genesis(&test_genesis()).expect("init_genesis");

        // Simulate a v2 database by rolling the version key back to 2.
        {
            let wtxn = store.db.begin_write().expect("write txn");
            {
                let mut meta = wtxn.open_table(META).expect("meta");
                meta.insert(b"schema_version".as_slice(), 2u32.to_be_bytes().as_slice())
                    .expect("rollback version");
            }
            wtxn.commit().expect("commit");
        }
        drop(store);

        // Re-opening must migrate v2 -> v3 (not fail closed).
        let store = ChainStore::open(&path).expect("re-open migrates");
        let rtxn = store.db.begin_read().expect("read txn");
        let meta = rtxn.open_table(META).expect("meta");
        let v = meta
            .get(b"schema_version".as_slice())
            .expect("get")
            .expect("version present");
        assert_eq!(u32_from_value(v.value(), "schema_version").expect("u32"), 4);
        // The `contract_cells` table is available after migration.
        rtxn.open_table(CONTRACT_CELLS)
            .expect("contract_cells exists");

        drop(rtxn);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    // ----- ONXBLK05 v4: signature persistence -----

    #[test]
    fn migrate_v3_to_v4_empty_db() {
        let path = temp_db_path("v3-to-v4-empty");
        let _ = std::fs::remove_file(&path);
        let store = ChainStore::open(&path).expect("open");
        store.init_genesis(&test_genesis()).expect("init_genesis");

        // Simulate a v3 database by rolling the version key back to 3.
        {
            let wtxn = store.db.begin_write().expect("write txn");
            {
                let mut meta = wtxn.open_table(META).expect("meta");
                meta.insert(b"schema_version".as_slice(), 3u32.to_be_bytes().as_slice())
                    .expect("rollback version");
            }
            wtxn.commit().expect("commit");
        }
        drop(store);

        // Re-opening must migrate v3 -> v4 (empty DB: no blocks).
        let store = ChainStore::open(&path).expect("re-open migrates");
        let rtxn = store.db.begin_read().expect("read txn");
        let meta = rtxn.open_table(META).expect("meta");
        let v = meta
            .get(b"schema_version".as_slice())
            .expect("get")
            .expect("version present");
        assert_eq!(u32_from_value(v.value(), "schema_version").expect("u32"), 4);
        // The `block_sigs` table is available after migration.
        rtxn.open_table(BLOCK_SIGS).expect("block_sigs exists");

        drop(rtxn);
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn migrate_v3_to_v4_refuses_db_with_blocks() {
        use onx_stf::{propose_block, State as StfState};
        let path = temp_db_path("v3-to-v4-blocks");
        let _ = std::fs::remove_file(&path);
        let store = ChainStore::open(&path).expect("open");
        let doc = test_genesis();
        store.init_genesis(&doc).expect("init_genesis");
        let state = store.load_state().expect("load").expect("state");

        // Commit a block (v4 API), then roll the version back to 3 to
        // simulate a v3 database that holds blocks.
        let block = propose_block(
            &StfState::from_genesis(&doc),
            vec![],
            state.last_lt + 1,
            onx_data_structures::AccountId::from_bytes([0xcc; 32]),
            1,
            0,
        )
        .expect("propose");
        let sig = onx_stf::SigEntry {
            validator_index: 0,
            sig: [0x5a; 64],
        };
        store
            .commit_block(&state, &block, &[sig])
            .expect("commit_block");
        {
            let wtxn = store.db.begin_write().expect("write txn");
            {
                let mut meta = wtxn.open_table(META).expect("meta");
                meta.insert(b"schema_version".as_slice(), 3u32.to_be_bytes().as_slice())
                    .expect("rollback version");
            }
            wtxn.commit().expect("commit");
        }
        drop(store);

        // Re-opening must FAIL: v3 DB with blocks cannot migrate.
        let err = match ChainStore::open(&path) {
            Ok(_) => panic!("must refuse v3 DB with blocks"),
            Err(e) => e,
        };
        assert!(
            matches!(err, StorageError::SchemaMismatch { found: 3, .. }),
            "unexpected error: {:?}",
            err
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Audit blocker 3: a v2 database WITH blocks must be refused, exactly
    /// like the v3 path — its blocks carry 148-byte unsigned headers and
    /// cannot be upgraded in place.
    #[test]
    fn migrate_v2_to_v4_refuses_db_with_blocks() {
        use onx_stf::{propose_block, State as StfState};
        let path = temp_db_path("v2-to-v4-blocks");
        let _ = std::fs::remove_file(&path);
        let store = ChainStore::open(&path).expect("open");
        let doc = test_genesis();
        store.init_genesis(&doc).expect("init_genesis");
        let state = store.load_state().expect("load").expect("state");

        let block = propose_block(
            &StfState::from_genesis(&doc),
            vec![],
            state.last_lt + 1,
            onx_data_structures::AccountId::from_bytes([0xcc; 32]),
            1,
            0,
        )
        .expect("propose");
        let sig = onx_stf::SigEntry {
            validator_index: 0,
            sig: [0x5a; 64],
        };
        store
            .commit_block(&state, &block, &[sig])
            .expect("commit_block");
        {
            let wtxn = store.db.begin_write().expect("write txn");
            {
                let mut meta = wtxn.open_table(META).expect("meta");
                meta.insert(b"schema_version".as_slice(), 2u32.to_be_bytes().as_slice())
                    .expect("rollback version");
            }
            wtxn.commit().expect("commit");
        }
        drop(store);

        // Re-opening must FAIL: v2 DB with blocks cannot migrate.
        let err = match ChainStore::open(&path) {
            Ok(_) => panic!("must refuse v2 DB with blocks"),
            Err(e) => e,
        };
        assert!(
            matches!(err, StorageError::SchemaMismatch { found: 2, .. }),
            "unexpected error: {:?}",
            err
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn commit_block_persists_sigs_atomically() {
        use onx_stf::{propose_block, State as StfState};
        let path = temp_db_path("v4-sigs");
        let _ = std::fs::remove_file(&path);
        let store = ChainStore::open(&path).expect("open");
        let doc = test_genesis();
        store.init_genesis(&doc).expect("init_genesis");
        let state = store.load_state().expect("load").expect("state");

        let block = propose_block(
            &StfState::from_genesis(&doc),
            vec![],
            state.last_lt + 1,
            onx_data_structures::AccountId::from_bytes([0xcc; 32]),
            1,
            0,
        )
        .expect("propose");
        let sig = onx_stf::SigEntry {
            validator_index: 0,
            sig: [0x5a; 64],
        };
        store
            .commit_block(&state, &block, &[sig])
            .expect("commit_block");

        // The signature section is in the DB, keyed by block hash.
        let hash = block.header.hash();
        let stored = store
            .get_block_sigs(&hash)
            .expect("get sigs")
            .expect("present");
        assert_eq!(stored, onx_stf::encode_sig_section(&[sig]));

        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    use onx_state_model::{BagOfCells, Cell, StorageStat};
    use std::collections::BTreeMap;

    fn contract_account(code: Option<Cell>, data: Option<Cell>) -> AccountState {
        AccountState::Active {
            balance_nanos: 1_000_000,
            last_trans_lt: 0,
            code,
            data,
            storage_stat: StorageStat {
                cell_count: 0,
                byte_count: 0,
                bit_count: 0,
            },
            pubkey: [0u8; 32],
            nonce: 0,
        }
    }

    /// A complete code/data DAG pair: the code root references one child
    /// whose content is present; the data root is childless.
    fn complete_dags() -> (Cell, Cell, ContractCellDags) {
        let child = Cell::new(vec![0xAA], vec![]).unwrap();
        let code_root = Cell::new(vec![0xC0], vec![child.hash()]).unwrap();
        let data_root = Cell::new(vec![0xDA], vec![]).unwrap();
        let mut code_cells = BTreeMap::new();
        code_cells.insert(child.hash(), child);
        code_cells.insert(code_root.hash(), code_root.clone());
        let mut data_cells = BTreeMap::new();
        data_cells.insert(data_root.hash(), data_root.clone());
        let dags = ContractCellDags {
            code: BagOfCells::new(code_root.hash(), code_cells).unwrap(),
            data: BagOfCells::new(data_root.hash(), data_cells).unwrap(),
        };
        (code_root, data_root, dags)
    }

    /// Directly write an account record and (optionally) its
    /// `contract_cells` entry, simulating what `commit_block` persists.
    fn write_account_raw(
        store: &ChainStore,
        id: &AccountId,
        st: &AccountState,
        dags: Option<&ContractCellDags>,
    ) {
        let wtxn = store.db.begin_write().expect("write txn");
        {
            let mut accts = wtxn.open_table(ACCOUNTS).expect("accounts");
            accts
                .insert(id.to_bytes().as_slice(), st.to_bytes().as_slice())
                .expect("insert account");
            if let Some(d) = dags {
                let mut cells = wtxn.open_table(CONTRACT_CELLS).expect("contract_cells");
                cells
                    .insert(id.to_bytes().as_slice(), d.to_bytes().as_slice())
                    .expect("insert dags");
            }
        }
        wtxn.commit().expect("commit");
    }

    /// Roll the schema version key back to 2, simulating a pre-wave-3
    /// database about to be migrated on next open.
    fn rollback_schema_to_v2(store: &ChainStore) {
        let wtxn = store.db.begin_write().expect("write txn");
        {
            let mut meta = wtxn.open_table(META).expect("meta");
            meta.insert(b"schema_version".as_slice(), 2u32.to_be_bytes().as_slice())
                .expect("rollback version");
        }
        wtxn.commit().expect("commit");
    }

    /// v2 store → migrate → startup check PASSES when the DAGs are
    /// complete: the migrated node can prove it will execute LDREF
    /// exactly like a genesis-replayed node.
    #[test]
    fn migrate_v2_to_v4_startup_invariant_passes_with_complete_dags() {
        let path = temp_db_path("v2-invariant-ok");
        let _ = std::fs::remove_file(&path);
        let store = ChainStore::open(&path).expect("open");
        store.init_genesis(&test_genesis()).expect("init_genesis");

        let id = AccountId::from_bytes([0xC0; 32]);
        let (code_root, data_root, dags) = complete_dags();
        write_account_raw(
            &store,
            &id,
            &contract_account(Some(code_root), Some(data_root)),
            Some(&dags),
        );
        rollback_schema_to_v2(&store);
        drop(store);

        // Re-open: migration runs, then the startup invariant verifies the
        // complete DAGs and the open succeeds. (No load_state here: the
        // account was written raw, bypassing the trie, so the stored
        // state root legitimately doesn't cover it — the invariant is
        // what this test exercises.)
        let store =
            ChainStore::open(&path).expect("re-open after v2->v3 migration with complete DAGs");
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    /// A committed code root with NO `contract_cells` entry — exactly what
    /// the v2→v3 migration leaves behind — fails startup LOUDLY, naming
    /// the offending root hash and the recovery path.
    #[test]
    fn startup_invariant_fails_loudly_on_missing_contract_cells_entry() {
        let path = temp_db_path("v2-invariant-missing");
        let _ = std::fs::remove_file(&path);
        let store = ChainStore::open(&path).expect("open");
        store.init_genesis(&test_genesis()).expect("init_genesis");

        let id = AccountId::from_bytes([0xC0; 32]);
        let code_root = Cell::new(vec![0xC0], vec![]).unwrap();
        write_account_raw(
            &store,
            &id,
            &contract_account(Some(code_root.clone()), None),
            None, // no DAG entry: the v2-migration gap
        );
        rollback_schema_to_v2(&store);
        drop(store);

        let err = match ChainStore::open(&path) {
            Err(e) => e,
            Ok(_) => panic!("open must fail: committed code root with no DAG entry"),
        };
        match &err {
            StorageError::IncompleteContractCellDag {
                account,
                root,
                detail,
            } => {
                assert_eq!(*account, id.to_bytes());
                assert_eq!(*root, code_root.hash());
                assert!(
                    detail.contains("no contract_cells entry"),
                    "detail names the problem: {detail}"
                );
            }
            other => panic!("expected IncompleteContractCellDag, got {other:?}"),
        }
        // The operator-facing message names the exact root hash and the
        // recovery instruction.
        let msg = err.to_string();
        assert!(
            msg.contains(&super::hex32(&code_root.hash())),
            "message names the offending root hash: {msg}"
        );
        assert!(
            msg.contains("replay from genesis"),
            "message instructs the operator: {msg}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A persisted DAG with a dangling reference fails startup loudly.
    /// The non-strict (proof-profile) decode accepts this input — that
    /// path's behavior is intentionally unchanged — but the persisted
    /// profile must not.
    #[test]
    fn startup_invariant_fails_loudly_on_dangling_reference() {
        let path = temp_db_path("v2-invariant-dangling");
        let _ = std::fs::remove_file(&path);
        let store = ChainStore::open(&path).expect("open");
        store.init_genesis(&test_genesis()).expect("init_genesis");

        let missing_child = [0x55; 32];
        let code_root = Cell::new(vec![0xC0], vec![missing_child]).unwrap();
        let mut code_cells = BTreeMap::new();
        code_cells.insert(code_root.hash(), code_root.clone());
        // Dangling refs are tolerated at construction (proof carve-out).
        let code_boc = BagOfCells::new(code_root.hash(), code_cells).unwrap();
        let data_root = Cell::new(vec![], vec![]).unwrap();
        let data_boc = BagOfCells::from_root(data_root).unwrap();
        let dags = ContractCellDags {
            code: code_boc,
            data: data_boc,
        };
        // Sanity: the proof-profile decode still accepts the dangling ref
        // (that path's behavior is unchanged by this work).
        let rt = ContractCellDags::from_bytes(&dags.to_bytes()).expect("non-strict decode accepts");
        assert_eq!(rt, dags);

        let id = AccountId::from_bytes([0xC0; 32]);
        write_account_raw(
            &store,
            &id,
            &contract_account(Some(code_root.clone()), None),
            Some(&dags),
        );
        rollback_schema_to_v2(&store);
        drop(store);

        let err = match ChainStore::open(&path) {
            Err(e) => e,
            Ok(_) => panic!("open must fail: DAG with dangling reference"),
        };
        match &err {
            StorageError::IncompleteContractCellDag { root, detail, .. } => {
                assert_eq!(*root, code_root.hash());
                assert!(
                    detail.contains("dangling reference"),
                    "detail names the problem: {detail}"
                );
            }
            other => panic!("expected IncompleteContractCellDag, got {other:?}"),
        }
        assert!(
            err.to_string().contains(&super::hex32(&code_root.hash())),
            "message names the offending root hash"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Persisted DAGs rooted at a DIFFERENT hash than the account's
    /// committed code root fail startup: stale/wrong content would seed
    /// the interpreter with the wrong cells.
    #[test]
    fn startup_invariant_fails_loudly_on_root_mismatch() {
        let path = temp_db_path("v2-invariant-mismatch");
        let _ = std::fs::remove_file(&path);
        let store = ChainStore::open(&path).expect("open");
        store.init_genesis(&test_genesis()).expect("init_genesis");

        let (code_root, data_root, mut dags) = complete_dags();
        let wrong_root = Cell::new(vec![0xFF], vec![]).unwrap();
        dags.code = BagOfCells::from_root(wrong_root).unwrap();
        let id = AccountId::from_bytes([0xC0; 32]);
        write_account_raw(
            &store,
            &id,
            &contract_account(Some(code_root.clone()), Some(data_root)),
            Some(&dags),
        );
        rollback_schema_to_v2(&store);
        drop(store);

        let err = match ChainStore::open(&path) {
            Err(e) => e,
            Ok(_) => panic!("open must fail: DAG root mismatch"),
        };
        match &err {
            StorageError::IncompleteContractCellDag { root, detail, .. } => {
                assert_eq!(*root, code_root.hash());
                assert!(
                    detail.contains("not at the account's committed code root"),
                    "detail names the problem: {detail}"
                );
            }
            other => panic!("expected IncompleteContractCellDag, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    /// `init_genesis` persists single-root DAGs for genesis-installed
    /// contracts, so a fresh database passes the startup invariant on
    /// re-open (this is the `onx replay` + tvm_replay.rs path).
    #[test]
    fn init_genesis_writes_single_root_dags_for_genesis_contracts() {
        use onx_data_structures::{ShardIdent, WorkchainIdent};
        use onx_state_model::{GenesisDocument, GenesisValidator};

        let path = temp_db_path("genesis-contract");
        let _ = std::fs::remove_file(&path);
        let store = ChainStore::open(&path).expect("open");
        let contract_id = AccountId::from_bytes([0xC0; 32]);
        let code = Cell::new(vec![0xC0], vec![]).unwrap(); // childless
        let mut accounts = BTreeMap::new();
        accounts.insert(contract_id, contract_account(Some(code.clone()), None));
        let doc = GenesisDocument::new(
            WorkchainIdent::new(0),
            ShardIdent::root(WorkchainIdent::new(0)),
            vec![GenesisValidator {
                pubkey: [0xA5; 32],
                stake: 1_000,
            }],
            accounts,
        )
        .expect("genesis with contract account");
        store.init_genesis(&doc).expect("init_genesis");
        drop(store);

        let store = ChainStore::open(&path).expect("re-open passes with genesis-written DAGs");
        let state = store.load_state().expect("load_state").expect("state");
        let dags = state
            .tree
            .contract_cells(&contract_id)
            .expect("genesis DAGs present");
        assert_eq!(*dags.code.root_hash(), code.hash());
        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    /// Panic policy (ADR-0029), apply_block side: a panic during REAL
    /// block application propagates out of `commit_block` — it is never
    /// caught and never mapped to a `StorageError` ("invalid block").
    /// The uncommitted write transaction is dropped, so nothing partial
    /// persists, and the same block commits cleanly afterwards (proving
    /// the panic was never an invalid-block verdict).
    #[test]
    fn commit_block_panic_propagates_and_halts() {
        let path = temp_db_path("commit-panic");
        let _ = std::fs::remove_file(&path);
        let store = ChainStore::open(&path).expect("open");
        store.init_genesis(&test_genesis()).expect("init_genesis");
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
            1,
            0,
        )
        .expect("propose_block");

        // Simulate an interpreter/STF panic during real block application.
        // (The test harness catches the unwind to observe it; the node
        // itself has no handler on this path — the process dies.)
        test_set_inject_commit_panic(true);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.commit_block(&state, &block, &dummy_sigs())
        }));
        test_set_inject_commit_panic(false);
        assert!(
            outcome.is_err(),
            "a panic in apply_block must propagate out of commit_block — never caught, never mapped to a StorageError"
        );

        // Atomicity: the panic landed before the write transaction opened
        // — the head is untouched and nothing partial was persisted.
        assert_eq!(
            store.head().expect("head").expect("head present").0,
            0,
            "panicked commit must not advance the head"
        );

        // The panic was never an "invalid block": the same block commits
        // cleanly once the injection is disarmed.
        store
            .commit_block(&state, &block, &dummy_sigs())
            .expect("commit_block after disarmed panic injection");
        assert_eq!(
            store.head().expect("head").expect("head present").0,
            1,
            "block commits normally after the panic"
        );

        drop(store);
        let _ = std::fs::remove_file(&path);
    }

    /// ONXBLK05 TRAP 4: databases initialized before step 5 have no
    /// `genesis_doc` key. Re-running `init_genesis` with the same genesis
    /// (the idempotent path) must backfill it, or the startup signing-key
    /// check fails on upgraded nodes.
    #[test]
    fn init_genesis_backfills_genesis_doc() {
        let path = temp_db_path("genesis-doc-backfill");
        let _ = std::fs::remove_file(&path);
        let doc = test_genesis();
        {
            let store = ChainStore::open(&path).expect("open");
            store.init_genesis(&doc).expect("init_genesis");
            assert!(store.genesis_document().expect("read").is_some());
        } // drop the store to release the file lock

        // Simulate a pre-step-5 database: delete the key directly.
        {
            let db = redb::Database::open(&path).expect("reopen");
            let wtxn = db.begin_write().expect("wtxn");
            {
                let mut meta = wtxn.open_table(META).expect("meta");
                meta.remove(b"genesis_doc".as_slice()).expect("remove");
            }
            wtxn.commit().expect("commit");
        } // drop the raw handle

        // Idempotent re-init backfills it.
        let store = ChainStore::open(&path).expect("reopen store");
        assert!(store.genesis_document().expect("read").is_none());
        store.init_genesis(&doc).expect("re-init");
        let backfilled = store.genesis_document().expect("read").expect("backfilled");
        assert_eq!(backfilled.to_bytes(), doc.to_bytes());
    }

    /// commit_block rejects an empty signature section: it can never
    /// satisfy the >2/3 stake rule, so persisting it would store a block
    /// no verifier accepts.
    #[test]
    fn commit_block_rejects_empty_sig_section() {
        let path = temp_db_path("empty-sigs");
        let _ = std::fs::remove_file(&path);
        let store = ChainStore::open(&path).expect("open");
        store.init_genesis(&test_genesis()).expect("init_genesis");
        let state = store.load_state().expect("load").expect("genesis state");
        let accounts = test_accounts();
        let block = propose_block(
            &state,
            test_block_txs(7, 1, &accounts, state.chain_id),
            test_block_lt(1),
            test_fee_collector(),
            1,
            0,
        )
        .expect("propose");
        let err = store.commit_block(&state, &block, &[]).unwrap_err();
        assert!(
            matches!(err, StorageError::Corrupt(_)),
            "expected Corrupt for empty sig section, got: {err:?}"
        );
    }
}
