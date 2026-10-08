//! Canonical genesis document: the deterministic initial state of one
//! workchain shard.
//!
//! The genesis document is consensus-critical: every node must derive
//! byte-identical initial state from the same document. The format is therefore
//! a fixed canonical byte encoding — never TOML, never free text — and the
//! domain-separated hash of the canonical bytes doubles as the chain ID.
//!
//! ## Canonical layout (all integers big-endian)
//!
//! ```text
//! magic:               "ONXG" (4 bytes)
//! version:             u32 = 1 (4 bytes)
//! workchain_id:        i32 (4 bytes)
//! shard_prefix_ident:  u64 (8 bytes)
//! validator_count:     u32 (4 bytes)
//! per validator, sorted by pubkey ascending:
//!     pubkey:          32 bytes
//!     stake:           u64 (8 bytes)
//! account_count:       u32 (4 bytes)
//! per account, ascending AccountId order:
//!     account_id:      32 bytes
//!     state_len:       u32 (4 bytes)
//!     state_bytes:     AccountState canonical encoding
//! -- version 2 only --
//! dag_count:           u32 (4 bytes, >= 1)
//! per contract DAG entry, ascending AccountId order:
//!     account_id:      32 bytes
//!     dags_len:        u32 (4 bytes)
//!     dags_bytes:      ContractCellDags canonical encoding
//! ```
//!
//! ## Contract cell DAGs (version 2, ADR-0041)
//!
//! An account record embeds only the *root* code/data cells. A root with
//! child references needs the children's content too: since ADR-0039,
//! `run()` resolves every child of the root code cell before the first
//! instruction, and `LDREF` needs data children. So a genesis contract
//! whose code or data root has references carries its complete
//! [`ContractCellDags`] in the document. The rule is canonical: a DAG
//! entry exists **iff** the account is a contract whose code or data root
//! has at least one reference, and the document is version 2 **iff** at
//! least one such entry exists. A genesis without such contracts is
//! byte-identical to version 1 — existing chain IDs do not change.
//!
//! ## Key derivation
//!
//! Human-readable config labels (e.g. `"onx:alice"`) are mapped to key
//! material deterministically:
//!
//! - A 64-character hex string is decoded literally: it is real key material
//!   supplied by the genesis ceremony.
//! - A 64-character string that is *not* valid hex is rejected (probable typo,
//!   never silently reinterpreted).
//! - Any other string is a label, mapped via a domain-separated hash. Derived
//!   keys have no known private key and are DEV-ONLY placeholders: they can
//!   hold genesis balances but can never sign.
//!
//! See `~/workspace/goals/open-network-x-development/hidden_files/phase2-genesis.md`
//! for the full derivation story and reproduction instructions.

use crate::account::AccountState;
use crate::boc::BagOfCells;
use crate::cell::Cell;
use crate::contract_cells::ContractCellDags;
use crate::error::StateModelError;
use crate::tree::{ShardStateTree, MAX_TRIE_VALUE_BYTES};
use onx_data_structures::{AccountId, ShardIdent, WorkchainIdent};
use onx_primitives::{domain_hash, DomainTag, Uint32, Uint64};
use std::collections::BTreeMap;

/// Magic bytes opening every canonical genesis document.
pub const GENESIS_MAGIC: [u8; 4] = *b"ONXG";
/// Base genesis document version: no contract cell DAG section.
pub const GENESIS_VERSION: u32 = 1;
/// Genesis document version carrying a contract cell DAG section. Emitted
/// only when at least one DAG entry exists (see module docs).
pub const GENESIS_VERSION_CONTRACT_DAGS: u32 = 2;

/// Domain tag for the genesis hash (which doubles as the chain ID).
pub const ONX_GENESIS_V1: DomainTag = DomainTag::from_ascii("ONX_GENESIS_V1");
/// Domain tag for deriving an [`AccountId`] from a config label.
pub const ONX_GENESIS_ADDR_V1: DomainTag = DomainTag::from_ascii("ONX_GENESIS_ADDR_V1");
/// Domain tag for deriving a validator public key from a config label
/// (DEV-ONLY: derived keys have no known private key).
pub const ONX_GENESIS_VALKEY_V1: DomainTag = DomainTag::from_ascii("ONX_GENESIS_VALKEY_V1");

/// A genesis validator: 32-byte public key plus stake weight.
///
/// Validators are metadata in the genesis document (sorted by pubkey for
/// determinism); they are NOT accounts. Stake-weighted validator-set
/// enforcement belongs to a later phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenesisValidator {
    pub pubkey: [u8; 32],
    pub stake: u64,
}

/// The canonical genesis document for one workchain shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisDocument {
    pub workchain: WorkchainIdent,
    pub shard: ShardIdent,
    /// Sorted by pubkey ascending (enforced at construction).
    pub validators: Vec<GenesisValidator>,
    /// Ascending AccountId order (BTreeMap iteration).
    pub accounts: BTreeMap<AccountId, AccountState>,
    /// Complete code/data DAGs for exactly the contract accounts whose code
    /// or data root has child references (see module docs). Validated at
    /// construction: rooted at the account's committed roots, complete (no
    /// dangling reference), and holding no unreachable cells.
    pub contract_cells: BTreeMap<AccountId, ContractCellDags>,
}

impl GenesisDocument {
    /// Builds a genesis document, enforcing determinism invariants:
    /// validators sorted by pubkey, shard bound to the workchain,
    /// no duplicate validator keys, at least one validator and one account.
    ///
    /// Carries no contract cell DAGs, so it rejects any contract whose code
    /// or data root has child references; use
    /// [`GenesisDocument::with_contract_cells`] for those.
    pub fn new(
        workchain: WorkchainIdent,
        shard: ShardIdent,
        validators: Vec<GenesisValidator>,
        accounts: BTreeMap<AccountId, AccountState>,
    ) -> Result<Self, StateModelError> {
        Self::with_contract_cells(workchain, shard, validators, accounts, BTreeMap::new())
    }

    /// [`GenesisDocument::new`] plus the contract cell DAGs for contracts
    /// whose code or data root has child references (ADR-0041).
    ///
    /// Fail-closed: a contract whose roots have references but no DAG, a
    /// DAG for an account that needs none (or is not a contract), a DAG
    /// rooted anywhere but the account's committed roots, a dangling
    /// reference, or an unreachable cell all reject the whole document. A
    /// genesis contract that could never execute is refused at the trust
    /// root instead of bouncing on every call.
    pub fn with_contract_cells(
        workchain: WorkchainIdent,
        shard: ShardIdent,
        mut validators: Vec<GenesisValidator>,
        accounts: BTreeMap<AccountId, AccountState>,
        contract_cells: BTreeMap<AccountId, ContractCellDags>,
    ) -> Result<Self, StateModelError> {
        if shard.workchain_id != workchain {
            return Err(StateModelError::InvalidGenesis(format!(
                "shard workchain {:?} does not match document workchain {:?}",
                shard.workchain_id, workchain
            )));
        }
        if validators.is_empty() {
            return Err(StateModelError::InvalidGenesis(
                "genesis requires at least one validator".to_string(),
            ));
        }
        if accounts.is_empty() {
            return Err(StateModelError::InvalidGenesis(
                "genesis requires at least one account".to_string(),
            ));
        }
        // F5: byte-equality here is a duplicate check on the raw bytes the
        // builder stored, nothing more. This layer never decodes keys —
        // canonicality is enforced upstream in `onx-genesis`
        // (`PublicKey::decode_exact`), so by the time a document reaches
        // `new`, distinct byte strings are distinct keys only if the
        // builder validated them. Do not read a soundness claim into this
        // comment that the code cannot keep.
        validators.sort_by_key(|a| a.pubkey);
        for pair in validators.windows(2) {
            if pair[0].pubkey == pair[1].pubkey {
                return Err(StateModelError::InvalidGenesis(
                    "duplicate validator public key in genesis".to_string(),
                ));
            }
        }
        // Bound total genesis stake to u64::MAX (checked sum). The block
        // verifier sums stake in u128 with checked arithmetic; a u64-bound
        // total at the trust root makes saturation there unreachable by
        // construction, not just by checked ops.
        {
            let mut total: u64 = 0;
            for v in &validators {
                total = total.checked_add(v.stake).ok_or_else(|| {
                    StateModelError::InvalidGenesis(
                        "total genesis validator stake overflows u64".to_string(),
                    )
                })?;
            }
        }
        // Every genesis account must be committable to the state trie
        // (spec §4.5: values over MAX_TRIE_VALUE_BYTES cannot be hashed
        // into the trie). Reject here — at the trust root — rather than
        // failing later at the first `state_root_hash()`.
        for (id, state) in &accounts {
            let len = state.to_bytes().len();
            if len > MAX_TRIE_VALUE_BYTES {
                return Err(StateModelError::InvalidGenesis(format!(
                    "genesis account {:x?} too large for state trie: {len} bytes (max {MAX_TRIE_VALUE_BYTES})",
                    &id.to_bytes()[..8],
                )));
            }
        }
        validate_contract_cells(&accounts, &contract_cells)?;
        Ok(Self {
            workchain,
            shard,
            validators,
            accounts,
            contract_cells,
        })
    }

    /// Canonical byte encoding of the document (see module docs).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&GENESIS_MAGIC);
        let version = if self.contract_cells.is_empty() {
            GENESIS_VERSION
        } else {
            GENESIS_VERSION_CONTRACT_DAGS
        };
        out.extend_from_slice(&Uint32(version).encode());
        out.extend_from_slice(&self.workchain.to_bytes());
        out.extend_from_slice(&self.shard.shard_prefix_ident.encode());
        out.extend_from_slice(&Uint32(self.validators.len() as u32).encode());
        for v in &self.validators {
            out.extend_from_slice(&v.pubkey);
            out.extend_from_slice(&Uint64(v.stake).encode());
        }
        out.extend_from_slice(&Uint32(self.accounts.len() as u32).encode());
        for (id, state) in &self.accounts {
            out.extend_from_slice(&id.to_bytes());
            let state_bytes = state.to_bytes();
            out.extend_from_slice(&Uint32(state_bytes.len() as u32).encode());
            out.extend_from_slice(&state_bytes);
        }
        if !self.contract_cells.is_empty() {
            out.extend_from_slice(&Uint32(self.contract_cells.len() as u32).encode());
            for (id, dags) in &self.contract_cells {
                out.extend_from_slice(&id.to_bytes());
                let dag_bytes = dags.to_bytes();
                out.extend_from_slice(&Uint32(dag_bytes.len() as u32).encode());
                out.extend_from_slice(&dag_bytes);
            }
        }
        out
    }

    /// Parses a canonical genesis document. Strict: magic, version, counts,
    /// shard validity, and sort order are all enforced; trailing bytes are
    /// rejected.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StateModelError> {
        let mut cursor = bytes;
        let take = |cursor: &mut &[u8], n: usize| -> Result<Vec<u8>, StateModelError> {
            if cursor.len() < n {
                return Err(StateModelError::DeserializationError(format!(
                    "genesis truncated: need {} bytes, have {}",
                    n,
                    cursor.len()
                )));
            }
            let (head, tail) = cursor.split_at(n);
            *cursor = tail;
            Ok(head.to_vec())
        };

        let magic = take(&mut cursor, 4)?;
        if magic.as_slice() != GENESIS_MAGIC {
            return Err(StateModelError::DeserializationError(format!(
                "bad genesis magic: expected ONXG, got {:02x?}",
                magic
            )));
        }
        let version_bytes = take(&mut cursor, 4)?;
        let version = u32::from_be_bytes(version_bytes.try_into().unwrap());
        if version != GENESIS_VERSION && version != GENESIS_VERSION_CONTRACT_DAGS {
            return Err(StateModelError::DeserializationError(format!(
                "unsupported genesis version: {}",
                version
            )));
        }
        let wc_bytes = take(&mut cursor, 4)?;
        let workchain = WorkchainIdent::from_bytes(wc_bytes.try_into().unwrap());
        let prefix_bytes = take(&mut cursor, 8)?;
        let shard = ShardIdent::new(
            workchain,
            u64::from_be_bytes(prefix_bytes.try_into().unwrap()),
        )
        .map_err(|e| StateModelError::DeserializationError(format!("bad shard ident: {e}")))?;

        let vcount_bytes = take(&mut cursor, 4)?;
        let vcount = u32::from_be_bytes(vcount_bytes.try_into().unwrap()) as usize;
        let mut validators = Vec::with_capacity(vcount.min(1024));
        for _ in 0..vcount {
            let pubkey = take(&mut cursor, 32)?;
            let stake_bytes = take(&mut cursor, 8)?;
            validators.push(GenesisValidator {
                pubkey: pubkey.try_into().unwrap(),
                stake: u64::from_be_bytes(stake_bytes.try_into().unwrap()),
            });
        }

        let account_bytes = take(&mut cursor, 4)?;
        let account = u32::from_be_bytes(account_bytes.try_into().unwrap()) as usize;
        let mut accounts = BTreeMap::new();
        let mut prev_id: Option<AccountId> = None;
        for _ in 0..account {
            let id_bytes = take(&mut cursor, 32)?;
            let id = AccountId::from_bytes(id_bytes.try_into().unwrap());
            if let Some(prev) = prev_id {
                if id <= prev {
                    return Err(StateModelError::DeserializationError(
                        "genesis accounts not in strictly ascending order".to_string(),
                    ));
                }
            }
            prev_id = Some(id);
            let len_bytes = take(&mut cursor, 4)?;
            let len = u32::from_be_bytes(len_bytes.try_into().unwrap()) as usize;
            let state_bytes = take(&mut cursor, len)?;
            let (state, consumed) = AccountState::from_bytes(&state_bytes).map_err(|e| {
                StateModelError::DeserializationError(format!("bad account state: {e}"))
            })?;
            if consumed != state_bytes.len() {
                return Err(StateModelError::DeserializationError(
                    "trailing bytes inside account state record".to_string(),
                ));
            }
            accounts.insert(id, state);
        }

        let mut contract_cells = BTreeMap::new();
        if version == GENESIS_VERSION_CONTRACT_DAGS {
            let dcount_bytes = take(&mut cursor, 4)?;
            let dcount = u32::from_be_bytes(dcount_bytes.try_into().unwrap()) as usize;
            // Canonical form: version 2 is emitted only with >= 1 entry, so
            // an empty section is a second encoding of a version-1 document.
            if dcount == 0 {
                return Err(StateModelError::DeserializationError(
                    "genesis version 2 with an empty contract DAG section".to_string(),
                ));
            }
            let mut prev_id: Option<AccountId> = None;
            for _ in 0..dcount {
                let id_bytes = take(&mut cursor, 32)?;
                let id = AccountId::from_bytes(id_bytes.try_into().unwrap());
                if let Some(prev) = prev_id {
                    if id <= prev {
                        return Err(StateModelError::DeserializationError(
                            "genesis contract DAGs not in strictly ascending order".to_string(),
                        ));
                    }
                }
                prev_id = Some(id);
                let len_bytes = take(&mut cursor, 4)?;
                let len = u32::from_be_bytes(len_bytes.try_into().unwrap()) as usize;
                let dag_bytes = take(&mut cursor, len)?;
                let dags = ContractCellDags::from_bytes(&dag_bytes).map_err(|e| {
                    StateModelError::DeserializationError(format!("bad contract cell DAGs: {e}"))
                })?;
                contract_cells.insert(id, dags);
            }
        }

        if !cursor.is_empty() {
            return Err(StateModelError::TrailingBytes {
                remaining: cursor.len(),
            });
        }

        Self::with_contract_cells(workchain, shard, validators, accounts, contract_cells)
    }

    /// The genesis hash: domain-separated hash of the canonical bytes.
    /// This IS the chain ID — it commits to every account, every validator,
    /// the workchain, and the shard.
    pub fn genesis_hash(&self) -> [u8; 32] {
        domain_hash(&ONX_GENESIS_V1, &self.to_bytes())
    }

    /// Builds the initial [`ShardStateTree`] from the genesis allocations.
    ///
    /// Every genesis contract also gets its contract cell DAGs (ADR-0041):
    /// the document's complete DAGs where its roots have children,
    /// otherwise single-root bags (the root is the whole DAG). The STF
    /// seeds the interpreter's cell store from these, so a genesis
    /// contract's first call can resolve its code children.
    pub fn state_tree(&self) -> ShardStateTree {
        let mut tree = ShardStateTree::new();
        for (id, state) in &self.accounts {
            // Validated at construction (`GenesisDocument::new` rejects
            // accounts over MAX_TRIE_VALUE_BYTES), so this cannot fail.
            tree.insert(*id, state.clone())
                .expect("genesis accounts are trie-committable by construction");
            if let AccountState::Active {
                code: Some(code),
                data,
                ..
            } = state
            {
                let dags = match self.contract_cells.get(id) {
                    Some(dags) => dags.clone(),
                    None => ContractCellDags {
                        code: BagOfCells::from_root(code.clone())
                            .expect("a single root cell is a valid bag"),
                        data: BagOfCells::from_root(genesis_data_root(data.as_ref()))
                            .expect("a single root cell is a valid bag"),
                    },
                };
                tree.set_contract_cells(*id, dags);
            }
        }
        tree
    }
}

/// The data root the STF executes a contract with: its data cell, or an
/// empty cell when it has none (the STF's calling convention).
fn genesis_data_root(data: Option<&Cell>) -> Cell {
    data.cloned()
        .unwrap_or_else(|| Cell::new(vec![], vec![]).expect("empty cell is valid"))
}

/// Check the contract cell DAG section against the accounts (ADR-0041).
fn validate_contract_cells(
    accounts: &BTreeMap<AccountId, AccountState>,
    contract_cells: &BTreeMap<AccountId, ContractCellDags>,
) -> Result<(), StateModelError> {
    let short = |id: &AccountId| -> String {
        id.to_bytes()[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    for (id, state) in accounts {
        let (code, data) = match state {
            AccountState::Active {
                code: Some(code),
                data,
                ..
            } => (code, data.as_ref()),
            _ => {
                if contract_cells.contains_key(id) {
                    return Err(StateModelError::InvalidGenesis(format!(
                        "genesis account {} has contract cell DAGs but no code",
                        short(id)
                    )));
                }
                continue;
            }
        };
        let data_root = genesis_data_root(data);
        let needs_dags = !code.cell_refs().is_empty() || !data_root.cell_refs().is_empty();
        let dags = match (needs_dags, contract_cells.get(id)) {
            (false, None) => continue,
            (false, Some(_)) => {
                return Err(StateModelError::InvalidGenesis(format!(
                    "genesis contract {} has contract cell DAGs but its code and data roots \
                     have no child references (non-canonical)",
                    short(id)
                )))
            }
            (true, None) => {
                return Err(StateModelError::InvalidGenesis(format!(
                    "genesis contract {} has a code or data root with child references \
                     but no contract cell DAGs: it could never execute",
                    short(id)
                )))
            }
            (true, Some(dags)) => dags,
        };
        for (label, boc, root) in [
            ("code", &dags.code, code.hash()),
            ("data", &dags.data, data_root.hash()),
        ] {
            check_complete_dag(boc, &root).map_err(|detail| {
                StateModelError::InvalidGenesis(format!(
                    "genesis contract {} {label} DAG: {detail}",
                    short(id)
                ))
            })?;
        }
    }
    for id in contract_cells.keys() {
        if !accounts.contains_key(id) {
            return Err(StateModelError::InvalidGenesis(format!(
                "contract cell DAGs for {} which is not a genesis account",
                short(id)
            )));
        }
    }
    Ok(())
}

/// A genesis DAG must be rooted at `root`, complete (every reference
/// reachable from the root resolves), hold every cell under its own hash,
/// and hold no unreachable cell (the wire form carries only the reachable
/// set, so an unreachable cell would not survive a `to_bytes`/`from_bytes`
/// round trip).
///
/// `BagOfCells::new` does not check that a map key is its cell's hash, so
/// this does: a cell stored under another hash would let a reference
/// resolve to content the committed root does not commit to, and the
/// document would fail `from_bytes` (which does check) after a restart.
fn check_complete_dag(boc: &BagOfCells, root: &[u8; 32]) -> Result<(), String> {
    if boc.root_hash() != root {
        return Err("not rooted at the account's committed root".to_string());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut stack = vec![*root];
    while let Some(hash) = stack.pop() {
        if !seen.insert(hash) {
            continue;
        }
        let cell = boc
            .get_cell(&hash)
            .ok_or_else(|| "dangling reference to a cell the DAG does not carry".to_string())?;
        if cell.hash() != hash {
            return Err("carries a cell under a hash that is not its own".to_string());
        }
        stack.extend(cell.cell_refs().iter().copied());
    }
    if seen.len() != boc.cells().len() {
        return Err("carries cells unreachable from the root".to_string());
    }
    Ok(())
}

/// Derives a deterministic [`AccountId`] from a human-readable config label.
///
/// DEV-ONLY provenance: the preimage is public, so no one can hold the
/// corresponding private key. Real ceremonies must use 64-hex-char literals
/// (see [`parse_or_derive_account_id`]).
pub fn derive_account_id(label: &str) -> AccountId {
    AccountId::from_bytes(domain_hash(&ONX_GENESIS_ADDR_V1, label.as_bytes()))
}

/// Derives a deterministic validator public key from a config label.
///
/// DEV-ONLY: derived keys have no known private key and can never sign.
/// A real network must supply explicit 32-byte keys.
pub fn derive_validator_pubkey(label: &str) -> [u8; 32] {
    domain_hash(&ONX_GENESIS_VALKEY_V1, label.as_bytes())
}

fn decode_hex_32(s: &str) -> Option<[u8; 32]> {
    if !is_explicit_hex_key(s) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16).unwrap();
        let lo = (chunk[1] as char).to_digit(16).unwrap();
        // Hex digits: `hi, lo <= 15`, so `hi * 16 + lo <= 255`; the saturating
        // ops never saturate, they just satisfy the arithmetic lint.
        out[i] = (hi as u8).saturating_mul(16).saturating_add(lo as u8);
    }
    Some(out)
}

/// Reports whether `s` is a 64-character hex literal — real key material —
/// as opposed to a DEV label. This is the first branch of the three-way
/// rule in [`parse_or_derive_pubkey`] / [`parse_or_derive_account_id`];
/// keep the two in sync. Callers that must apply strict key validation
/// (genesis validator keys) use this to tell "operator-supplied key
/// material, validate it" apart from "derived dev key, cannot sign".
pub fn is_explicit_hex_key(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Rejects strings that look like a botched key literal before they can
/// silently become a DEV label (a validator/account nobody can sign for).
/// Called by [`parse_or_derive_pubkey`] and [`parse_or_derive_account_id`]
/// for every input; the label allowlist in [`validate_label`] runs
/// afterwards on the label path only.
fn reject_key_lookalike(s: &str, what: &str) -> Result<(), StateModelError> {
    if s.len() != s.trim().len() {
        return Err(StateModelError::InvalidGenesis(format!(
            "{what} has leading/trailing whitespace; remove it or the key will not match: {s:?}"
        )));
    }
    if s.starts_with("0x") || s.starts_with("0X") {
        return Err(StateModelError::InvalidGenesis(format!(
            "{what} looks like a 0x-prefixed key; strip the 0x prefix (64 hex chars, no prefix): {s:?}"
        )));
    }
    // 64-char handled by the caller; nearby lengths of alnum text are
    // almost certainly a mistyped key, not a label anyone meant.
    if s.len() != 64 && (60..=68).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err(StateModelError::InvalidGenesis(format!(
            "{what} is {len} alphanumeric chars — looks like a mistyped 64-hex-char key, not a label: {s:?}",
            len = s.len(),
        )));
    }
    Ok(())
}

/// F3: the label allowlist. Anything that is not an explicit 64-hex key
/// must be label-shaped — non-empty ASCII `[a-z0-9][a-z0-9._:-]{0,47}` —
/// or it is rejected loudly instead of silently becoming a DEV label.
/// This closes the holes the blocklist missed: BOM/zero-width characters,
/// internal whitespace, Cyrillic lookalikes, 59/69-hex and 128-hex
/// keypairs, base64, and empty strings (all fail the charset/length gate).
/// All-hex strings of key-like length (16+) are key material, not names:
/// a 32-hex string is a truncated key. Short hex words ("beef", "cafe")
/// stay valid labels.
fn validate_label(s: &str, what: &str) -> Result<(), StateModelError> {
    fn invalid(s: &str, what: &str, why: &str) -> StateModelError {
        StateModelError::InvalidGenesis(format!(
            "{what} {s:?} is not a valid DEV label ({why}); labels are non-empty \
             ASCII `[a-z0-9][a-z0-9._:-]{{0,47}}`"
        ))
    }
    let mut bytes = s.bytes();
    let first_ok = matches!(bytes.next(), Some(b) if b.is_ascii_lowercase() || b.is_ascii_digit());
    let rest_ok = s.len() <= 48
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b':' | b'-')
        });
    if !first_ok || !rest_ok {
        return Err(invalid(s, what, "charset/length"));
    }
    // All-hex strings of key-like length are key material, not names: a
    // 32-hex string is a truncated key. Short hex words ("beef", "cafe")
    // stay valid labels.
    if s.len() >= 16 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid(
            s,
            what,
            "all-hex strings are key material, not names",
        ));
    }
    // Prefixed truncated keys (`ed25519:<32 hex>`) are key material with a
    // scheme prefix, not labels: reject a trailing all-hex run of key-like
    // length after a colon.
    if let Some((_, tail)) = s.rsplit_once(':') {
        if tail.len() >= 16 && tail.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid(
                s,
                what,
                "colon-prefixed hex run is key material, not a name",
            ));
        }
    }
    Ok(())
}

/// Maps a balance address string to an [`AccountId`]:
/// 64 hex chars are decoded literally (real key material);
/// a 64-char non-hex string is rejected as a probable typo;
/// key lookalikes (0x prefix, near-64 lengths, stray whitespace) are
/// rejected before they can silently become labels;
/// anything else is a label, derived deterministically.
pub fn parse_or_derive_account_id(s: &str) -> Result<AccountId, StateModelError> {
    reject_key_lookalike(s, "address")?;
    if s.len() == 64 {
        match decode_hex_32(s) {
            Some(bytes) => return Ok(AccountId::from_bytes(bytes)),
            None => {
                return Err(StateModelError::InvalidGenesis(format!(
                    "address looks like hex (64 chars) but is not valid hex: {s}"
                )))
            }
        }
    }
    validate_label(s, "address")?;
    Ok(derive_account_id(s))
}

/// Maps a validator key string to 32 bytes: same rule as
/// [`parse_or_derive_account_id`], including lookalike rejection.
/// `what` names the field for error messages (F8: balance-key errors must
/// not say "validator key").
pub fn parse_or_derive_pubkey(s: &str, what: &str) -> Result<[u8; 32], StateModelError> {
    reject_key_lookalike(s, what)?;
    if s.len() == 64 {
        match decode_hex_32(s) {
            Some(bytes) => return Ok(bytes),
            None => {
                return Err(StateModelError::InvalidGenesis(format!(
                    "{what} looks like hex (64 chars) but is not valid hex: {s}"
                )))
            }
        }
    }
    validate_label(s, what)?;
    Ok(derive_validator_pubkey(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::StorageStat;
    use std::collections::BTreeMap;

    fn sample_doc() -> GenesisDocument {
        let workchain = WorkchainIdent::MASTERCHAIN;
        let shard = ShardIdent::root(workchain);
        let validators = vec![
            GenesisValidator {
                pubkey: derive_validator_pubkey("validator-01"),
                stake: 1000,
            },
            GenesisValidator {
                pubkey: derive_validator_pubkey("validator-02"),
                stake: 2000,
            },
        ];
        let mut accounts = BTreeMap::new();
        accounts.insert(
            derive_account_id("onx:alice"),
            AccountState::Active {
                balance_nanos: 1_000_000,
                last_trans_lt: 0,
                code: None,
                data: None,
                storage_stat: StorageStat {
                    cell_count: 0,
                    byte_count: 0,
                    bit_count: 0,
                },
                pubkey: [0u8; 32],
                nonce: 0,
            },
        );
        accounts.insert(
            derive_account_id("onx:bob"),
            AccountState::Active {
                balance_nanos: 2_000_000,
                last_trans_lt: 0,
                code: None,
                data: None,
                storage_stat: StorageStat {
                    cell_count: 0,
                    byte_count: 0,
                    bit_count: 0,
                },
                pubkey: [0u8; 32],
                nonce: 0,
            },
        );
        GenesisDocument::new(workchain, shard, validators, accounts).unwrap()
    }

    #[test]
    fn round_trip_is_identity() {
        let doc = sample_doc();
        let bytes = doc.to_bytes();
        let parsed = GenesisDocument::from_bytes(&bytes).unwrap();
        assert_eq!(doc, parsed);
    }

    #[test]
    fn rejects_bad_magic_version_and_trailing_bytes() {
        let doc = sample_doc();
        let mut bytes = doc.to_bytes();
        bytes[0] = b'X';
        assert!(GenesisDocument::from_bytes(&bytes).is_err());

        let mut bytes = doc.to_bytes();
        bytes[4..8].copy_from_slice(&2u32.to_be_bytes());
        assert!(GenesisDocument::from_bytes(&bytes).is_err());

        let mut bytes = doc.to_bytes();
        bytes.push(0x00);
        assert!(matches!(
            GenesisDocument::from_bytes(&bytes),
            Err(StateModelError::TrailingBytes { .. })
        ));
    }

    #[test]
    fn rejects_empty_validators_and_accounts() {
        let workchain = WorkchainIdent::MASTERCHAIN;
        let shard = ShardIdent::root(workchain);
        assert!(GenesisDocument::new(workchain, shard, vec![], BTreeMap::new()).is_err());
        let mut accounts = BTreeMap::new();
        accounts.insert(derive_account_id("x"), AccountState::Uninitialized);
        assert!(GenesisDocument::new(workchain, shard, vec![], accounts).is_err());
    }

    #[test]
    fn rejects_shard_workchain_mismatch() {
        let shard = ShardIdent::root(WorkchainIdent::BASIC);
        let validators = vec![GenesisValidator {
            pubkey: [7u8; 32],
            stake: 1,
        }];
        let mut accounts = BTreeMap::new();
        accounts.insert(derive_account_id("x"), AccountState::Uninitialized);
        assert!(
            GenesisDocument::new(WorkchainIdent::MASTERCHAIN, shard, validators, accounts).is_err()
        );
    }

    #[test]
    fn rejects_duplicate_validator_pubkeys() {
        let workchain = WorkchainIdent::MASTERCHAIN;
        let shard = ShardIdent::root(workchain);
        let validators = vec![
            GenesisValidator {
                pubkey: [9u8; 32],
                stake: 1,
            },
            GenesisValidator {
                pubkey: [9u8; 32],
                stake: 2,
            },
        ];
        let mut accounts = BTreeMap::new();
        accounts.insert(derive_account_id("x"), AccountState::Uninitialized);
        assert!(GenesisDocument::new(workchain, shard, validators, accounts).is_err());
    }

    #[test]
    fn key_derivation_is_deterministic_and_label_sensitive() {
        assert_eq!(
            derive_account_id("onx:alice"),
            derive_account_id("onx:alice")
        );
        assert_ne!(derive_account_id("onx:alice"), derive_account_id("onx:bob"));
        assert_ne!(
            derive_account_id("onx:alice"),
            derive_account_id("onx:alice ")
        );
    }

    #[test]
    fn hex_literals_pass_through_and_bad_hex_is_rejected() {
        let hex = "ab".repeat(32);
        let id = parse_or_derive_account_id(&hex).unwrap();
        assert_eq!(id.to_bytes(), [0xabu8; 32]);
        // 64-char non-hex: probable typo, must error rather than derive.
        assert!(parse_or_derive_account_id(&"zz".repeat(32)).is_err());
        // Short strings are labels.
        assert_eq!(
            parse_or_derive_account_id("onx:alice").unwrap(),
            derive_account_id("onx:alice")
        );
    }

    #[test]
    fn genesis_hash_is_stable_and_sensitive() {
        let doc = sample_doc();
        let h1 = doc.genesis_hash();
        let h2 = GenesisDocument::from_bytes(&doc.to_bytes())
            .unwrap()
            .genesis_hash();
        assert_eq!(h1, h2);
        // Flipping one balance bit changes the chain ID.
        let mut doc2 = sample_doc();
        let alice = derive_account_id("onx:alice");
        doc2.accounts.insert(alice, AccountState::Uninitialized);
        assert_ne!(doc.genesis_hash(), doc2.genesis_hash());
    }

    #[test]
    fn key_lookalikes_are_rejected_before_label_derivation() {
        // 0x-prefixed keys: strip the prefix instead of deriving a label.
        assert!(parse_or_derive_pubkey(
            "0x3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29",
            "validator key"
        )
        .is_err());
        // Near-64-length alnum strings: almost certainly a mistyped key.
        assert!(parse_or_derive_pubkey(&"ab".repeat(31), "validator key").is_err()); // 62 chars
        assert!(parse_or_derive_pubkey(
            &"ab".repeat(32).chars().take(63).collect::<String>(),
            "validator key"
        )
        .is_err()); // 63 chars
        assert!(parse_or_derive_pubkey(&format!("{}a", "ab".repeat(32)), "validator key").is_err()); // 65 chars
                                                                                                     // Stray whitespace: the key would not match what the operator meant.
        assert!(parse_or_derive_pubkey(
            " 3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29",
            "validator key"
        )
        .is_err());
        // Same rule for account addresses.
        assert!(parse_or_derive_account_id(
            "0x3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29"
        )
        .is_err());
    }

    #[test]
    fn label_allowlist_rejects_key_material_lookalikes() {
        // F3: the cases the old blocklist missed must not silently become
        // DEV labels. Each is either non-allowlist charset/length or
        // all-hex key material.
        let honest = "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29";
        let bad = [
            format!("\u{feff}{honest}"),                               // BOM + key
            format!("{honest}\u{200b}"),                               // key + zero-width space
            format!("{} {}", &honest[..32], &honest[32..]),            // internal space
            format!("{}\n{}", &honest[..32], &honest[32..]),           // internal newline
            honest.replacen('a', "\u{430}", 1),                        // Cyrillic а for a
            honest[..59].to_string(),                                  // 59 hex chars
            honest[..32].to_string(),    // 32 hex chars (truncated key)
            format!("{honest}{honest}"), // 128 hex (keypair)
            "O0vqf8zr".repeat(5).chars().take(44).collect::<String>(), // base64-shaped
            String::new(),               // empty: unfilled template field
            "0x1234".to_string(),        // short 0x-prefixed
            "UPPERCASE".to_string(),     // uppercase not in allowlist
            "z".repeat(49),              // over the 48-char cap; "z" is not hex, so only
                                         // the cap (not the all-hex rule) can reject it
        ];
        for s in &bad {
            assert!(
                parse_or_derive_pubkey(s, "validator key").is_err(),
                "lookalike accepted as label: {s:?}"
            );
            assert!(
                parse_or_derive_account_id(s).is_err(),
                "lookalike accepted as address label: {s:?}"
            );
        }
        // The allowlist itself: ordinary labels still derive.
        for s in [
            "validator-01",
            "alice",
            "dev-key-1",
            "a",
            "a.b_c:d-e",
            &"x".repeat(48),
        ] {
            assert!(
                parse_or_derive_pubkey(s, "validator key").is_ok(),
                "valid label rejected: {s:?}"
            );
        }
    }

    #[test]
    fn label_allowlist_boundary_vectors() {
        // Boundary pins for the three thresholds a mutation pass showed
        // were untested: the 16-hex key-material cutoff, the 48-char cap,
        // and the lowercase-only charset.
        // 15 hex chars: a short hex word, still a label.
        assert!(parse_or_derive_pubkey("abcdef123456789", "validator key").is_ok());
        // 16 hex chars: key-like length, rejected.
        assert!(parse_or_derive_pubkey("abcdef1234567890", "validator key").is_err());
        assert!(parse_or_derive_account_id("abcdef1234567890").is_err());
        // Uppercase after the first character: charset is lowercase-only.
        assert!(parse_or_derive_pubkey("devAlice", "validator key").is_err());
        assert!(parse_or_derive_account_id("devAlice").is_err());
        // Digit first is fine; uppercase first is not.
        assert!(parse_or_derive_pubkey("1dev", "validator key").is_ok());
        assert!(parse_or_derive_pubkey("Dev", "validator key").is_err());
        // Prefixed truncated keys are key material, not labels.
        assert!(parse_or_derive_pubkey(
            "ed25519:3b6a27bcceb6a42d62a3a8d02a6f0d736",
            "validator key"
        )
        .is_err());
        assert!(parse_or_derive_account_id("ed25519:3b6a27bcceb6a42d62a3a8d02a6f0d736").is_err());
        // ...but ordinary colon labels still derive.
        assert!(parse_or_derive_account_id("onx:alice").is_ok());
    }

    #[test]
    fn plain_labels_still_derive() {
        // Ordinary labels are unaffected by lookalike rejection.
        assert_eq!(
            parse_or_derive_pubkey("validator-01", "validator key").unwrap(),
            derive_validator_pubkey("validator-01")
        );
        assert!(parse_or_derive_account_id("onx:alice").is_ok());
    }

    #[test]
    fn rejects_contract_dag_with_cell_under_wrong_hash() {
        // A code root with one child. The DAG stores an unrelated leaf
        // under the child's hash: every reference resolves, so only a
        // hash-vs-key check can catch it. The same DAG with the real child
        // is accepted, so the rejection is about the misindexed cell.
        let child = Cell::new(vec![0x01], vec![]).unwrap();
        let impostor = Cell::new(vec![0x02], vec![]).unwrap();
        let code = Cell::new(vec![0x00], vec![child.hash()]).unwrap();
        let contract = derive_account_id("onx:contract");

        let doc_with = |child_cell: Cell| {
            let mut cells = BTreeMap::new();
            cells.insert(code.hash(), code.clone());
            cells.insert(child.hash(), child_cell);
            let dags = ContractCellDags {
                code: BagOfCells::new(code.hash(), cells).unwrap(),
                data: BagOfCells::from_root(genesis_data_root(None)).unwrap(),
            };
            let mut accounts = sample_doc().accounts;
            accounts.insert(
                contract,
                AccountState::Active {
                    balance_nanos: 1,
                    last_trans_lt: 0,
                    code: Some(code.clone()),
                    data: None,
                    storage_stat: StorageStat {
                        cell_count: 0,
                        byte_count: 0,
                        bit_count: 0,
                    },
                    pubkey: [0u8; 32],
                    nonce: 0,
                },
            );
            let base = sample_doc();
            GenesisDocument::with_contract_cells(
                base.workchain,
                base.shard,
                base.validators,
                accounts,
                BTreeMap::from([(contract, dags)]),
            )
        };

        let doc = doc_with(child.clone()).expect("correctly indexed DAG is accepted");
        assert!(GenesisDocument::from_bytes(&doc.to_bytes()).is_ok());

        let err = doc_with(impostor).expect_err("misindexed cell must be refused");
        assert!(
            err.to_string().contains("under a hash that is not its own"),
            "{err}"
        );
    }

    // --- ADR-0041 canonical-form rejections --------------------------------
    //
    // The genesis hash is the chain ID, so each rule below is what keeps one
    // logical genesis from having two encodings. Every case is checked at
    // construction (`with_contract_cells`) and, where it is a wire-level
    // rule, on decode (`from_bytes`).

    fn contract_state(code: &Cell, data: Option<&Cell>) -> AccountState {
        AccountState::Active {
            balance_nanos: 1,
            last_trans_lt: 0,
            code: Some(code.clone()),
            data: data.cloned(),
            storage_stat: StorageStat {
                cell_count: 0,
                byte_count: 0,
                bit_count: 0,
            },
            pubkey: [0u8; 32],
            nonce: 0,
        }
    }

    /// `sample_doc()` plus the given extra accounts and DAG section.
    fn build_with(
        extra: Vec<(AccountId, AccountState)>,
        dags: BTreeMap<AccountId, ContractCellDags>,
    ) -> Result<GenesisDocument, StateModelError> {
        let base = sample_doc();
        let mut accounts = base.accounts;
        accounts.extend(extra);
        GenesisDocument::with_contract_cells(
            base.workchain,
            base.shard,
            base.validators,
            accounts,
            dags,
        )
    }

    /// A bag holding exactly `cells`, rooted at `root` (no completeness
    /// check: `BagOfCells::new` tolerates dangling refs).
    fn bag(root: &Cell, cells: &[&Cell]) -> BagOfCells {
        let mut map = BTreeMap::new();
        map.insert(root.hash(), root.clone());
        for c in cells {
            map.insert(c.hash(), (*c).clone());
        }
        BagOfCells::new(root.hash(), map).unwrap()
    }

    fn empty_data_bag() -> BagOfCells {
        BagOfCells::from_root(genesis_data_root(None)).unwrap()
    }

    /// Code root with one child, and its complete DAG.
    fn code_with_child(tag: u8) -> (Cell, ContractCellDags) {
        let child = Cell::new(vec![tag, 0x01], vec![]).unwrap();
        let code = Cell::new(vec![tag], vec![child.hash()]).unwrap();
        let dags = ContractCellDags {
            code: bag(&code, &[&child]),
            data: empty_data_bag(),
        };
        (code, dags)
    }

    /// Splits a version-2 document's bytes into the part before the DAG
    /// section and its entries, re-encoded one by one.
    fn split_dag_section(doc: &GenesisDocument) -> (Vec<u8>, Vec<Vec<u8>>) {
        let bytes = doc.to_bytes();
        let entries: Vec<Vec<u8>> = doc
            .contract_cells
            .iter()
            .map(|(id, dags)| {
                let mut e = id.to_bytes().to_vec();
                let d = dags.to_bytes();
                e.extend_from_slice(&(d.len() as u32).to_be_bytes());
                e.extend_from_slice(&d);
                e
            })
            .collect();
        let section_len = entries
            .iter()
            .map(Vec::len)
            .fold(4usize, usize::saturating_add);
        let head = bytes[..bytes.len().saturating_sub(section_len)].to_vec();
        (head, entries)
    }

    fn assemble(head: &[u8], entries: &[&Vec<u8>]) -> Vec<u8> {
        let mut out = head.to_vec();
        out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for e in entries {
            out.extend_from_slice(e);
        }
        out
    }

    #[test]
    fn version_2_wire_rules_are_enforced() {
        let (code_a, dags_a) = code_with_child(0xa0);
        let (code_b, dags_b) = code_with_child(0xb0);
        let (id_a, id_b) = (derive_account_id("onx:ca"), derive_account_id("onx:cb"));
        let doc = build_with(
            vec![
                (id_a, contract_state(&code_a, None)),
                (id_b, contract_state(&code_b, None)),
            ],
            BTreeMap::from([(id_a, dags_a), (id_b, dags_b)]),
        )
        .expect("valid two-contract genesis");
        let bytes = doc.to_bytes();
        assert_eq!(
            u32::from_be_bytes(bytes[4..8].try_into().unwrap()),
            GENESIS_VERSION_CONTRACT_DAGS
        );
        let (head, entries) = split_dag_section(&doc);
        assert_eq!(assemble(&head, &[&entries[0], &entries[1]]), bytes);
        assert!(GenesisDocument::from_bytes(&bytes).is_ok());

        // Out of order and duplicated entries.
        let swapped = assemble(&head, &[&entries[1], &entries[0]]);
        let err = GenesisDocument::from_bytes(&swapped).unwrap_err();
        assert!(err.to_string().contains("strictly ascending"), "{err}");
        let dup = assemble(&head, &[&entries[0], &entries[0], &entries[1]]);
        let err = GenesisDocument::from_bytes(&dup).unwrap_err();
        assert!(err.to_string().contains("strictly ascending"), "{err}");

        // A missing entry: the contract's roots have refs but no DAG.
        let missing = assemble(&head, &[&entries[0]]);
        let err = GenesisDocument::from_bytes(&missing).unwrap_err();
        assert!(err.to_string().contains("no contract cell DAGs"), "{err}");

        // The same accounts as a version-1 document (no section at all).
        let mut v1 = head.clone();
        v1[4..8].copy_from_slice(&GENESIS_VERSION.to_be_bytes());
        let err = GenesisDocument::from_bytes(&v1).unwrap_err();
        assert!(err.to_string().contains("no contract cell DAGs"), "{err}");

        // Truncated section.
        assert!(GenesisDocument::from_bytes(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn version_2_with_empty_dag_section_is_rejected() {
        // A version-1 document re-labelled version 2 with a zero count is a
        // second encoding of the same genesis.
        let mut bytes = sample_doc().to_bytes();
        bytes[4..8].copy_from_slice(&GENESIS_VERSION_CONTRACT_DAGS.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let err = GenesisDocument::from_bytes(&bytes).unwrap_err();
        assert!(
            err.to_string().contains("empty contract DAG section"),
            "{err}"
        );
    }

    #[test]
    fn dag_entry_must_belong_to_a_contract_that_needs_one() {
        let (code, dags) = code_with_child(0xc0);
        let id = derive_account_id("onx:c");

        // An id that is not a genesis account.
        let err = build_with(vec![], BTreeMap::from([(id, dags.clone())])).unwrap_err();
        assert!(err.to_string().contains("not a genesis account"), "{err}");

        // A genesis account without code (alice is a plain wallet).
        let alice = derive_account_id("onx:alice");
        let err = build_with(vec![], BTreeMap::from([(alice, dags.clone())])).unwrap_err();
        assert!(err.to_string().contains("but no code"), "{err}");

        // A contract whose roots have no references: the root is the whole
        // DAG, so an entry would be a second encoding.
        let leaf = Cell::new(vec![0x42], vec![]).unwrap();
        let leaf_dags = ContractCellDags {
            code: BagOfCells::from_root(leaf.clone()).unwrap(),
            data: empty_data_bag(),
        };
        let err = build_with(
            vec![(id, contract_state(&leaf, None))],
            BTreeMap::from([(id, leaf_dags)]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("non-canonical"), "{err}");

        // A contract whose roots have references but no entry.
        let err = build_with(vec![(id, contract_state(&code, None))], BTreeMap::new()).unwrap_err();
        assert!(err.to_string().contains("no contract cell DAGs"), "{err}");
        // `new` carries no DAGs, so it refuses the same contract.
        let base = sample_doc();
        let mut accounts = base.accounts;
        accounts.insert(id, contract_state(&code, None));
        assert!(
            GenesisDocument::new(base.workchain, base.shard, base.validators, accounts).is_err()
        );
    }

    #[test]
    fn dag_must_be_rooted_complete_and_minimal() {
        let child = Cell::new(vec![0x01], vec![]).unwrap();
        let code = Cell::new(vec![0x00], vec![child.hash()]).unwrap();
        let id = derive_account_id("onx:c");
        let reject = |dags: ContractCellDags, needle: &str| {
            let err = build_with(
                vec![(id, contract_state(&code, None))],
                BTreeMap::from([(id, dags)]),
            )
            .unwrap_err();
            assert!(err.to_string().contains(needle), "{needle}: {err}");
        };

        // Rooted elsewhere: a different code cell.
        let other = Cell::new(vec![0x09], vec![child.hash()]).unwrap();
        reject(
            ContractCellDags {
                code: bag(&other, &[&child]),
                data: empty_data_bag(),
            },
            "not rooted at the account's committed root",
        );
        // Data DAG rooted at something other than the (empty) data root.
        reject(
            ContractCellDags {
                code: bag(&code, &[&child]),
                data: BagOfCells::from_root(child.clone()).unwrap(),
            },
            "data DAG: not rooted",
        );
        // Dangling: the child's content is missing.
        reject(
            ContractCellDags {
                code: bag(&code, &[]),
                data: empty_data_bag(),
            },
            "dangling reference",
        );
        // Unreachable: an extra cell nothing references.
        let stray = Cell::new(vec![0x07], vec![]).unwrap();
        reject(
            ContractCellDags {
                code: bag(&code, &[&child, &stray]),
                data: empty_data_bag(),
            },
            "unreachable from the root",
        );
    }

    #[test]
    fn data_only_children_select_version_2_and_round_trip() {
        // Childless code, data root with a child: the other path into
        // version 2.
        let code = Cell::new(vec![0x00], vec![]).unwrap();
        let leaf = Cell::new(vec![0x05], vec![]).unwrap();
        let data = Cell::new(vec![0x06], vec![leaf.hash()]).unwrap();
        let id = derive_account_id("onx:d");
        let dags = ContractCellDags {
            code: BagOfCells::from_root(code.clone()).unwrap(),
            data: bag(&data, &[&leaf]),
        };
        let doc = build_with(
            vec![(id, contract_state(&code, Some(&data)))],
            BTreeMap::from([(id, dags.clone())]),
        )
        .expect("data-only DAG accepted");
        let bytes = doc.to_bytes();
        assert_eq!(
            u32::from_be_bytes(bytes[4..8].try_into().unwrap()),
            GENESIS_VERSION_CONTRACT_DAGS
        );
        let back = GenesisDocument::from_bytes(&bytes).expect("decodes");
        assert_eq!(back.to_bytes(), bytes);
        assert_eq!(back.state_tree().contract_cells(&id), Some(&dags));

        // Without the data child's content it is refused.
        let err = build_with(
            vec![(id, contract_state(&code, Some(&data)))],
            BTreeMap::from([(
                id,
                ContractCellDags {
                    code: BagOfCells::from_root(code.clone()).unwrap(),
                    data: bag(&data, &[]),
                },
            )]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("data DAG: dangling"), "{err}");
    }
}
