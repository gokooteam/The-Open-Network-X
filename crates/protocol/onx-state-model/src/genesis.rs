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
//! ```
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
use crate::error::StateModelError;
use crate::tree::{ShardStateTree, MAX_TRIE_VALUE_BYTES};
use onx_data_structures::{AccountId, ShardIdent, WorkchainIdent};
use onx_primitives::{domain_hash, DomainTag, Uint32, Uint64};
use std::collections::BTreeMap;

/// Magic bytes opening every canonical genesis document.
pub const GENESIS_MAGIC: [u8; 4] = *b"ONXG";
/// Current genesis document version. Bump only with a format change.
pub const GENESIS_VERSION: u32 = 1;

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
}

impl GenesisDocument {
    /// Builds a genesis document, enforcing determinism invariants:
    /// validators sorted by pubkey, shard bound to the workchain,
    /// no duplicate validator keys, at least one validator and one account.
    pub fn new(
        workchain: WorkchainIdent,
        shard: ShardIdent,
        mut validators: Vec<GenesisValidator>,
        accounts: BTreeMap<AccountId, AccountState>,
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
        Ok(Self {
            workchain,
            shard,
            validators,
            accounts,
        })
    }

    /// Canonical byte encoding of the document (see module docs).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&GENESIS_MAGIC);
        out.extend_from_slice(&Uint32(GENESIS_VERSION).encode());
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
        if version != GENESIS_VERSION {
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

        let acount_bytes = take(&mut cursor, 4)?;
        let acount = u32::from_be_bytes(acount_bytes.try_into().unwrap()) as usize;
        let mut accounts = BTreeMap::new();
        let mut prev_id: Option<AccountId> = None;
        for _ in 0..acount {
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

        if !cursor.is_empty() {
            return Err(StateModelError::TrailingBytes {
                remaining: cursor.len(),
            });
        }

        Self::new(workchain, shard, validators, accounts)
    }

    /// The genesis hash: domain-separated hash of the canonical bytes.
    /// This IS the chain ID — it commits to every account, every validator,
    /// the workchain, and the shard.
    pub fn genesis_hash(&self) -> [u8; 32] {
        domain_hash(&ONX_GENESIS_V1, &self.to_bytes())
    }

    /// Builds the initial [`ShardStateTree`] from the genesis allocations.
    pub fn state_tree(&self) -> ShardStateTree {
        let mut tree = ShardStateTree::new();
        for (id, state) in &self.accounts {
            // Validated at construction (`GenesisDocument::new` rejects
            // accounts over MAX_TRIE_VALUE_BYTES), so this cannot fail.
            tree.insert(*id, state.clone())
                .expect("genesis accounts are trie-committable by construction");
        }
        tree
    }
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
/// All-hex strings are rejected at any length: a 32-hex string is a
/// truncated key, not a name.
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
            "a".repeat(49),              // over the 48-char cap
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
    fn plain_labels_still_derive() {
        // Ordinary labels are unaffected by lookalike rejection.
        assert_eq!(
            parse_or_derive_pubkey("validator-01", "validator key").unwrap(),
            derive_validator_pubkey("validator-01")
        );
        assert!(parse_or_derive_account_id("onx:alice").is_ok());
    }
}
