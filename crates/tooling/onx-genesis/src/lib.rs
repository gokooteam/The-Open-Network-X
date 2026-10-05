#![deny(clippy::disallowed_types)] // Genesis construction is consensus-critical: deterministic iteration only.
use onx_data_structures::{ShardIdent, WorkchainIdent};
use onx_primitives::PublicKey;
use onx_state_model::{
    parse_or_derive_account_id, parse_or_derive_pubkey, AccountState, GenesisDocument,
    GenesisValidator, StorageStat,
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
    /// When present, the key must be a valid curve point — rejected
    /// otherwise, so no genesis account can ever carry an unverifiable key.
    #[serde(default)]
    pub public_key: Option<String>,
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
    for v in &config.validators {
        let pubkey = parse_or_derive_pubkey(&v.public_key).map_err(|e| e.to_string())?;
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
        // Genesis accounts are plain value accounts: no code, no data,
        // logical time zero, nonce zero. Code-bearing accounts arrive via
        // transactions. The public key is optional: without one the account
        // is keyless (can receive, never spend).
        let pubkey = match &b.public_key {
            Some(key_str) => {
                let bytes = parse_or_derive_pubkey(key_str).map_err(|e| e.to_string())?;
                PublicKey::decode_exact(&bytes).map_err(|e| {
                    format!(
                        "onx-genesis failed: balance {:?} has invalid public_key: {e}",
                        b.address
                    )
                })?;
                bytes
            }
            None => [0u8; 32],
        };
        accounts.insert(
            id,
            AccountState::Active {
                balance_nanos: b.amount as u128,
                last_trans_lt: 0,
                code_hash: [0u8; 32],
                data_hash: [0u8; 32],
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
