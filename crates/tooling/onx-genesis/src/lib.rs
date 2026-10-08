#![deny(clippy::disallowed_types)] // Genesis construction is consensus-critical: deterministic iteration only.
use onx_data_structures::{ShardIdent, WorkchainIdent};
use onx_primitives::PublicKey;
use onx_state_model::{
    is_explicit_hex_key, parse_or_derive_account_id, parse_or_derive_pubkey, AccountState, Cell,
    GenesisDocument, GenesisValidator, StorageStat,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Balance {
    pub address: String,
    pub amount: u64,
    /// Optional Ed25519 public key (hex literal or label, same three-way
    /// rule as validator keys) authorizing spends from this account.
    /// Absent means *keyless*: the account can receive but never spend.
    /// When present, the key must be a canonical, on-curve, large-order
    /// Ed25519 point — rejected otherwise, so no genesis account can ever
    /// carry an unverifiable key.
    #[serde(default)]
    pub public_key: Option<String>,
    /// Optional contract code, as hex of the canonical cell bytes
    /// (`Cell::to_bytes`). When present the account is born a contract;
    /// the code cell is embedded in the account state (and therefore in
    /// the state root). Rejected if the hex or the cell bytes are malformed.
    #[serde(default)]
    pub code_hex: Option<String>,
    /// Optional initial contract data, as hex of the canonical cell bytes.
    /// Only meaningful alongside `code_hex`; rejected if malformed.
    #[serde(default)]
    pub data_hex: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Validator {
    pub public_key: String,
    pub stake: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Workchain {
    /// Signed workchain id per the protocol definition:
    /// -1 = masterchain, 0 = basic workchain.
    pub id: i32,
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenesisConfig {
    pub balances: Vec<Balance>,
    pub validators: Vec<Validator>,
    pub workchains: Vec<Workchain>,
}

impl Default for GenesisConfig {
    fn default() -> Self {
        Self {
            balances: vec![Balance {
                address: "onx:genesis-account".to_string(),
                amount: 5_000_000_000_000_000_000,
                public_key: None,
                code_hex: None,
                data_hex: None,
            }],
            validators: vec![Validator {
                public_key: "validator-pubkey-00".to_string(),
                stake: 1_000_000,
            }],
            workchains: vec![Workchain {
                id: -1,
                name: "masterchain".to_string(),
                enabled: true,
            }],
        }
    }
}

pub fn secure_output_dir(output: impl AsRef<Path>) -> Result<PathBuf, String> {
    let out = output.as_ref();
    if out.is_absolute() {
        return Err(
            "onx-genesis failed: --out must be a relative path within the current directory"
                .to_string(),
        );
    }

    for comp in out.components() {
        match comp {
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => {
                return Err("onx-genesis failed: --out path must not contain parent, root, or absolute path components".to_string())
            }
            _ => {}
        }
    }

    let root = env::current_dir().map_err(|err| {
        format!("onx-genesis failed: could not determine current directory: {err}")
    })?;
    let mut out_path = root.clone();
    for comp in out.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(part) => out_path.push(part),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("onx-genesis failed: --out path must not contain parent, root, or absolute path components".to_string())
            }
        }
    }

    fs::create_dir_all(&out_path)
        .map_err(|err| format!("onx-genesis failed: could not create output directory: {err}"))?;

    let root_canon = fs::canonicalize(&root)
        .map_err(|err| format!("onx-genesis failed: could not canonicalize root: {err}"))?;
    let out_canon = fs::canonicalize(&out_path).map_err(|err| {
        format!("onx-genesis failed: could not canonicalize output directory: {err}")
    })?;

    if !out_canon.starts_with(&root_canon) {
        return Err(
            "onx-genesis failed: output directory must be within the current directory".to_string(),
        );
    }

    Ok(out_canon)
}
pub fn parse_config(path: impl AsRef<Path>) -> Result<GenesisConfig, String> {
    let raw = fs::read_to_string(path.as_ref()).map_err(|err| {
        format!(
            "failed to read genesis config {}: {err}",
            path.as_ref().display()
        )
    })?;
    let config: GenesisConfig = toml::from_str(&raw).map_err(|err| err.to_string())?;
    Ok(config)
}

/// Validates the human-readable config against the protocol definition and
/// builds the canonical [`GenesisDocument`].
///
/// Reconciliation rules (per `onx_data_structures::WorkchainIdent`):
/// - id `-1` is the masterchain and must be named `"masterchain"`;
/// - id `0` is the basic workchain and must be named `"basic"`;
/// - anything else is rejected: label drift like "workchain 0 named
///   masterchain" is exactly the bug class Phase 2 eliminates.
///   Exactly one workchain is supported (single-workchain replay scope).
pub fn build_genesis_document(config: &GenesisConfig) -> Result<GenesisDocument, String> {
    if config.workchains.len() != 1 {
        return Err(format!(
            "onx-genesis failed: exactly one workchain is supported in the replay scope, found {}",
            config.workchains.len()
        ));
    }
    let wc = &config.workchains[0];
    if !wc.enabled {
        return Err("onx-genesis failed: the single workchain must be enabled".to_string());
    }
    let workchain = match (wc.id, wc.name.as_str()) {
        (-1, "masterchain") => WorkchainIdent::MASTERCHAIN,
        (0, "basic") => WorkchainIdent::BASIC,
        _ => {
            return Err(format!(
            "onx-genesis failed: workchain id {} named {:?} contradicts the protocol definition \
                 (masterchain = -1, basic workchain = 0)",
            wc.id, wc.name
        ))
        }
    };
    // Single-workchain, single-shard scope: the shard is always the root shard.
    let shard = ShardIdent::root(workchain);

    if config.validators.is_empty() {
        return Err("onx-genesis failed: at least one validator is required".to_string());
    }
    let mut validators = Vec::with_capacity(config.validators.len());
    for (idx, v) in config.validators.iter().enumerate() {
        let pubkey =
            parse_or_derive_pubkey(&v.public_key, "validator key").map_err(|e| e.to_string())?;
        // An explicit 64-hex-char key is real key material — the only form
        // suitable for a real network — so it must clear the full strict
        // predicate in `PublicKey::decode_exact`: canonical encoding (the
        // decoded point recompresses to the input bytes), on-curve, and
        // large-order — the same bar `verify_strict` applies at
        // verification time. A small-order, off-curve, or non-canonical
        // validator key would poison the trust root: once block headers
        // are producer-signed, anyone could forge explorer-accepted
        // headers under it. Label-derived keys are DEV-only (no known
        // private key, can never sign) and pass through.
        if is_explicit_hex_key(&v.public_key) {
            PublicKey::decode_strict(&pubkey).map_err(|e| {
                format!(
                    "onx-genesis failed: validator #{idx} has invalid public_key \
                     (must be a canonical, on-curve, large-order Ed25519 point: \
                     recompressed bytes must match the input): {e}"
                )
            })?;
        }
        validators.push(GenesisValidator {
            pubkey,
            stake: v.stake,
        });
    }

    if config.balances.is_empty() {
        return Err("onx-genesis failed: at least one genesis balance is required".to_string());
    }
    let mut accounts = BTreeMap::new();
    for b in &config.balances {
        let id = parse_or_derive_account_id(&b.address).map_err(|e| e.to_string())?;
        if accounts.contains_key(&id) {
            return Err(format!(
                "onx-genesis failed: duplicate genesis address {:?}",
                b.address
            ));
        }
        // Genesis accounts start with logical time zero and nonce zero.
        // The public key is optional: without one the account is keyless
        // (can receive, never spend). Contract code/data are optional:
        // when `code_hex` is present the account is born a contract whose
        // code and data cells are embedded in its state.
        let pubkey = match &b.public_key {
            Some(key_str) => {
                let bytes =
                    parse_or_derive_pubkey(key_str, "balance key").map_err(|e| e.to_string())?;
                // F4: every balance key clears `decode_exact` — canonical
                // encoding, on-curve, large-order — exactly as main did
                // before PR #10. Label-derived keys are domain-hash output
                // (pseudorandom bytes); about half are off-curve and must
                // be rejected here, otherwise genesis carries unverifiable
                // keys that the `Balance.public_key` doc, the STF, and the
                // mempool all assume cannot exist. (The old comment's "1/8"
                // rationale confused the torsion-free fraction with the
                // small-order fraction: only 8 of ~2^255 points are
                // small-order, so the large-order check itself rejects
                // essentially no random label — it is the on-curve check
                // that does the work.)
                PublicKey::decode_exact(&bytes).map_err(|e| {
                    format!(
                        "onx-genesis failed: balance {:?} has invalid public_key \
                         (must be a canonical, on-curve, large-order Ed25519 point: \
                         recompressed bytes must match the input): {e}",
                        b.address
                    )
                })?;
                bytes
            }
            None => [0u8; 32],
        };
        let code = match &b.code_hex {
            Some(hex) => Some(parse_cell_hex(hex, &b.address, "code_hex")?),
            None => None,
        };
        let data = match &b.data_hex {
            Some(hex) => Some(parse_cell_hex(hex, &b.address, "data_hex")?),
            None => None,
        };
        if data.is_some() && code.is_none() {
            return Err(format!(
                "onx-genesis failed: balance {:?} has data_hex without code_hex",
                b.address
            ));
        }
        accounts.insert(
            id,
            AccountState::Active {
                balance_nanos: b.amount as u128,
                last_trans_lt: 0,
                code,
                data,
                storage_stat: StorageStat {
                    cell_count: 0,
                    byte_count: 0,
                },
                pubkey,
                nonce: 0,
            },
        );
    }

    GenesisDocument::new(workchain, shard, validators, accounts).map_err(|e| e.to_string())
}

/// Parse hex of canonical cell bytes (`Cell::to_bytes`) into a `Cell`.
/// Fail-closed: bad hex or malformed cell bytes reject the whole genesis.
fn parse_cell_hex(hex: &str, address: &str, field: &str) -> Result<Cell, String> {
    fn hex_val(c: u8) -> Result<u8, String> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err(format!("invalid hex character: {}", c as char)),
        }
    }
    let hex = hex.as_bytes();
    if !hex.len().is_multiple_of(2) {
        return Err(format!(
            "onx-genesis failed: balance {address:?} has odd-length hex in {field}"
        ));
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.chunks(2) {
        let hi = hex_val(pair[0]).map_err(|e| {
            format!("onx-genesis failed: balance {address:?} has bad hex in {field}: {e}")
        })?;
        let lo = hex_val(pair[1]).map_err(|e| {
            format!("onx-genesis failed: balance {address:?} has bad hex in {field}: {e}")
        })?;
        bytes.push(hi << 4 | lo);
    }
    let (cell, consumed) = Cell::from_bytes(&bytes).map_err(|e| {
        format!("onx-genesis failed: balance {address:?} has malformed cell in {field}: {e}")
    })?;
    if consumed != bytes.len() {
        return Err(format!(
            "onx-genesis failed: balance {address:?} has trailing bytes after cell in {field}"
        ));
    }
    Ok(cell)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    s
}

pub fn generate_genesis(config: &GenesisConfig, output_dir: PathBuf) -> Result<(), String> {
    fs::create_dir_all(&output_dir).map_err(|err| {
        format!(
            "failed to create output dir {}: {err}",
            output_dir.display()
        )
    })?;

    let doc = build_genesis_document(config)?;
    let genesis_bytes = doc.to_bytes();
    let genesis_hash = doc.genesis_hash();
    let chain_id_hex = hex_encode(&genesis_hash);

    let genesis_boc_path = output_dir.join("genesis.boc");
    fs::write(&genesis_boc_path, &genesis_bytes).map_err(|err| err.to_string())?;

    // The chain ID is the genesis hash: it commits to every account, every
    // validator, the workchain, and the shard. Print it so operators and
    // node configs can pin the exact chain they are joining.
    println!("onx-genesis: genesis hash (chain ID): {chain_id_hex}");

    for idx in 0..4 {
        let node_cfg = format!(
            "role = \"validator\"\nstorage_path = \"target/onxd-node-{}\"\nchain_id = \"{}\"\nnetwork_enabled = true\nnetwork_bind = \"127.0.0.1:{}\"\npeers = \"127.0.0.1:{}\"\nbootstrap_genesis = \"{}\"\n",
            idx,
            chain_id_hex,
            10_000 + idx * 1_000,
            10_001 + idx * 1_000,
            genesis_boc_path.display()
        );
        fs::write(output_dir.join(format!("node-{}.toml", idx)), node_cfg)
            .map_err(|err| err.to_string())?;
    }

    Ok(())
}

pub fn write_docs(output_dir: PathBuf) -> Result<(), String> {
    let doc = "# Launch Guide\n\nThis repository ships a deterministic `onx-genesis` bootstrap generator.\n\nUse `onx-genesis --config config/genesis.toml --out target/onx-genesis` to create `genesis.boc` and four `node-*.toml` files.\n\nThen launch four `onxd` boot nodes from the same generated `genesis.boc` by pointing each node at its generated configuration file, verifying that the first committed masterchain block is `#0` and that a shared shard header bootstrap path is emitted.\n";
    fs::write(output_dir.join("launch_guide.md"), doc).map_err(|err| err.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::secure_output_dir;
    use std::env;
    use std::path::Path;

    #[test]
    fn secure_output_dir_rejects_parent_components() {
        let result = secure_output_dir(Path::new("../../escape"));
        assert!(result.is_err(), "expected traversal path to be rejected");
    }

    #[test]
    fn secure_output_dir_accepts_relative_path_inside_workspace() {
        let cwd = env::current_dir().unwrap();
        let result = secure_output_dir(Path::new("target/onx-genesis-test"));
        assert!(result.is_ok(), "expected safe relative path to be accepted");
        let out = result.unwrap();
        assert!(out.starts_with(cwd));
    }
}
