//! `onx-cli` — the ONX command-line wallet.
//!
//! What it does today, with no RPC and no network:
//! - `wallet create` generates a BIP-39 mnemonic and writes `wallet.json`.
//! - `wallet address` prints a wallet's public key and key-derived address
//!   (`ONX_ADDR_V1`, ADR-0006).
//! - `transfer` builds a real, signed `ONX_MSG_EXT_V1` external message
//!   (ADR-0002) and either drops it into an `onxd` tx-pool directory as
//!   `<hash>.msg` or prints its wire bytes as hex.
//!
//! There is no balance query and no nonce lookup: nothing in ONX serves
//! chain state over RPC yet. The caller supplies the nonce. Commands that
//! used to fake those answers (`wallet balance`, `deploy-contract`) were
//! removed rather than left looking like they work.

use bip39::{Language, Mnemonic};
use clap::{Args, Parser, Subcommand};
use onx_data_structures::AccountId;
use onx_primitives::SecretKey;
use onx_stf::{derive_address, ExternalMessage, MsgKind};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MnemonicEntry {
    pub words: Vec<String>,
}

/// Contents of `wallet.json`. `address_hex` is informational: it is
/// recomputed from the mnemonic whenever the wallet is loaded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletCreateResponse {
    pub wallet: String,
    pub mnemonic: Vec<String>,
    pub public_key_hex: String,
    #[serde(default)]
    pub address_hex: String,
}

#[derive(Debug, Clone, Parser)]
#[command(name = "onx-cli", version, about = "ONX command-line wallet")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Wallet key management.
    #[command(name = "wallet")]
    Wallet {
        #[command(subcommand)]
        kind: WalletCommand,
    },
    /// Build a signed transfer (an `ONX_MSG_EXT_V1` external message).
    #[command(name = "transfer")]
    Transfer(TransferArgs),
}

#[derive(Debug, Clone, Subcommand)]
pub enum WalletCommand {
    /// Generate a new mnemonic and write `<out>/wallet.json`. Refuses to
    /// overwrite an existing wallet file.
    #[command(name = "create")]
    Create {
        #[arg(long, default_value = "target/onx-cli-wallet")]
        out: PathBuf,
    },
    /// Print a wallet's public key and key-derived address.
    #[command(name = "address")]
    Address {
        #[arg(long)]
        wallet: PathBuf,
    },
}

#[derive(Debug, Clone, Args)]
pub struct TransferArgs {
    /// Genesis config (TOML) of the target chain; the chain ID is its
    /// genesis hash. Exactly one of --genesis / --chain-id is required.
    #[arg(
        long,
        conflicts_with = "chain_id",
        required_unless_present = "chain_id"
    )]
    pub genesis: Option<PathBuf>,
    /// Chain ID (genesis hash) as 64 hex characters.
    #[arg(long)]
    pub chain_id: Option<String>,
    /// Signing key: a `wallet.json` written by `wallet create`.
    #[arg(
        long,
        conflicts_with = "seed_file",
        required_unless_present = "seed_file"
    )]
    pub wallet: Option<PathBuf>,
    /// Signing key: a file holding a 32-byte Ed25519 seed as 64 hex
    /// characters. Read from a file so the secret never appears in argv.
    #[arg(long)]
    pub seed_file: Option<PathBuf>,
    /// Sender account (64 hex). Defaults to the key-derived address of the
    /// signing key. Genesis accounts with an explicit address need this.
    #[arg(long)]
    pub from: Option<String>,
    /// Recipient account (64 hex).
    #[arg(long)]
    pub to: String,
    /// Amount in nano-Onyxi. Must be non-zero.
    #[arg(long)]
    pub amount: u128,
    /// Fee in nano-Onyxi.
    #[arg(long, default_value_t = 0)]
    pub fee: u128,
    /// Sender nonce. Must equal the account's current nonce exactly; there
    /// is no RPC to look it up yet.
    #[arg(long)]
    pub nonce: u64,
    /// Reveal the public key in the message. Required on the first spend
    /// from a key-derived account (ADR-0006), rejected on every other.
    #[arg(long)]
    pub reveal_key: bool,
    /// tx-pool drop directory of an `onxd` node. The message is written as
    /// `<hash>.msg` (temp file + rename). Without it, the wire bytes are
    /// printed as hex.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

pub fn mnemonic_words() -> Result<Vec<String>, String> {
    let mnemonic = Mnemonic::generate(12).map_err(|err| err.to_string())?;
    Ok(mnemonic.words().map(|word| word.to_string()).collect())
}

pub fn generate_mnemonic() -> Result<MnemonicEntry, String> {
    let words = mnemonic_words()?;
    Ok(MnemonicEntry { words })
}

pub fn derive_ed25519_key_from_mnemonic(words: &[String]) -> Result<SecretKey, String> {
    let phrase = words.join(" ");
    let mnemonic =
        Mnemonic::parse_in_normalized(Language::English, &phrase).map_err(|err| err.to_string())?;
    let seed = mnemonic.to_seed_normalized("");
    let mut key = [0u8; 32];
    key.copy_from_slice(&seed[..32]);
    SecretKey::from_seed(&key).map_err(|err| err.to_string())
}

/// Generate a wallet and write `<out_dir>/wallet.json`.
///
/// The file holds the mnemonic in plain text, so it is created with mode
/// 0600 on Unix and never overwrites an existing file: losing a funded
/// wallet to a second `wallet create` is not recoverable.
pub fn wallet_create(out_dir: impl AsRef<Path>) -> Result<WalletCreateResponse, String> {
    let mnemonic = generate_mnemonic()?;
    let words = mnemonic.words;
    let secret = derive_ed25519_key_from_mnemonic(&words)?;
    let public_key = secret.public_key().encode();
    let response = WalletCreateResponse {
        wallet: "wallet-000".to_string(),
        mnemonic: words,
        public_key_hex: hex::encode(public_key),
        address_hex: hex::encode(derive_address(&public_key).to_bytes()),
    };

    let out_dir = out_dir.as_ref();
    fs::create_dir_all(out_dir).map_err(|err| err.to_string())?;
    let path = out_dir.join("wallet.json");
    let json = serde_json::to_string_pretty(&response).map_err(|err| err.to_string())?;
    // Write a private temp file, then hard-link it into place: `hard_link`
    // fails if `wallet.json` already exists (never overwrites), and a failed
    // write never leaves a partial `wallet.json` that would block a retry.
    let tmp = write_unique_temp(out_dir, "wallet", json.as_bytes(), true)?;
    let linked = fs::hard_link(&tmp, &path);
    let _ = fs::remove_file(&tmp);
    linked.map_err(|err| format!("cannot create {}: {err}", path.display()))?;
    Ok(response)
}

/// Load the signing key from a `wallet.json`.
pub fn load_wallet_key(path: impl AsRef<Path>) -> Result<SecretKey, String> {
    let path = path.as_ref();
    let text =
        fs::read_to_string(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let wallet: WalletCreateResponse = serde_json::from_str(&text)
        .map_err(|err| format!("invalid wallet file {}: {err}", path.display()))?;
    derive_ed25519_key_from_mnemonic(&wallet.mnemonic)
}

/// Load the signing key from a file holding a 32-byte seed as hex.
pub fn load_seed_file(path: impl AsRef<Path>) -> Result<SecretKey, String> {
    let path = path.as_ref();
    let text =
        fs::read_to_string(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let seed = parse_hex32(text.trim(), "seed")?;
    SecretKey::from_seed(&seed).map_err(|err| err.to_string())
}

/// Parse exactly 32 bytes of hex (64 characters, no prefix).
pub fn parse_hex32(text: &str, what: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(text).map_err(|err| format!("{what}: invalid hex: {err}"))?;
    bytes
        .try_into()
        .map_err(|b: Vec<u8>| format!("{what}: expected 32 bytes, got {}", b.len()))
}

/// The chain ID of a genesis config: the hash of its canonical document.
pub fn chain_id_from_genesis(path: impl AsRef<Path>) -> Result<[u8; 32], String> {
    let config = onx_genesis::parse_config(path).map_err(|err| format!("genesis: {err}"))?;
    let doc =
        onx_genesis::build_genesis_document(&config).map_err(|err| format!("genesis: {err}"))?;
    Ok(doc.genesis_hash())
}

/// Everything a transfer needs except the key.
#[derive(Debug, Clone)]
pub struct TransferRequest {
    pub chain_id: [u8; 32],
    /// `None` = the signing key's derived address.
    pub from: Option<AccountId>,
    pub to: AccountId,
    pub amount_nanos: u128,
    pub fee_nanos: u128,
    pub nonce: u64,
    pub reveal_key: bool,
}

/// Build and sign a transfer.
///
/// Runs the wallet handler's stateless checks first, so a message the node
/// would reject regardless of chain state is never produced. Stateful
/// checks (balance, nonce, whether a reveal is due) need chain state this
/// tool cannot see; the node applies them on intake.
pub fn build_transfer(
    req: &TransferRequest,
    secret: &SecretKey,
) -> Result<ExternalMessage, String> {
    if req.amount_nanos == 0 {
        return Err("amount must be non-zero (the node rejects zero-amount transfers)".into());
    }
    if req.amount_nanos.checked_add(req.fee_nanos).is_none() {
        return Err("amount + fee overflows u128".into());
    }
    let public_key = secret.public_key().encode();
    let derived = derive_address(&public_key);
    let from = req.from.unwrap_or(derived);
    if req.reveal_key && from != derived {
        return Err(format!(
            "--reveal-key: sender {} is not the key-derived address {} of this key; \
             the node would reject the reveal",
            hex::encode(from.to_bytes()),
            hex::encode(derived.to_bytes())
        ));
    }
    let pubkey = if req.reveal_key {
        public_key
    } else {
        [0u8; 32]
    };
    Ok(ExternalMessage::new_signed(
        req.chain_id,
        MsgKind::Transfer,
        from,
        req.nonce,
        req.to,
        req.amount_nanos,
        req.fee_nanos,
        Vec::new(),
        pubkey,
        secret,
    ))
}

/// Write `msg` into an `onxd` tx-pool drop directory as `<hash>.msg`.
///
/// Follows the pool's atomic-write convention: the bytes go to a temp file
/// the intake scan ignores (no `.msg` extension), unique to this call, are
/// synced, and are then renamed into place, so the node never reads a
/// half-written message even when the same message is submitted twice at
/// once.
pub fn write_to_pool(pool_dir: impl AsRef<Path>, msg: &ExternalMessage) -> Result<PathBuf, String> {
    let pool_dir = pool_dir.as_ref();
    let name = hex::encode(msg.hash());
    let dest = pool_dir.join(format!("{name}.msg"));
    let tmp = write_unique_temp(pool_dir, &name, &msg.to_bytes(), false)?;
    // Replacing an existing `<hash>.msg` is harmless: same name, same bytes.
    fs::rename(&tmp, &dest).map_err(|err| {
        let _ = fs::remove_file(&tmp);
        format!("cannot rename into {}: {err}", dest.display())
    })?;
    Ok(dest)
}

/// Write `bytes` to a new, uniquely named, hidden `.tmp` file in `dir` and
/// sync it. The file is created exclusively (`create_new`, which also
/// refuses to follow a pre-placed symlink), so two concurrent writers never
/// share or truncate one file. `private` sets mode 0600 on Unix (wallets);
/// pool messages keep default permissions so an `onxd` running as another
/// user can read them. On any failure the temp file is removed.
fn write_unique_temp(
    dir: &Path,
    stem: &str,
    bytes: &[u8],
    private: bool,
) -> Result<PathBuf, String> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let mut attempts = 0;
    let (tmp, mut file) = loop {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{stem}.{}.{seq}.tmp", std::process::id()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(not(unix))]
        let _ = private;
        match options.open(&tmp) {
            Ok(file) => break (tmp, file),
            // A stale temp from an earlier process with the same pid.
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists && attempts < 16 => {
                attempts += 1;
            }
            Err(err) => return Err(format!("cannot create {}: {err}", tmp.display())),
        }
    };
    if let Err(err) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&tmp);
        return Err(format!("cannot write {}: {err}", tmp.display()));
    }
    Ok(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    fn scratch_dir(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "onx-cli-unit-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn secret(byte: u8) -> SecretKey {
        SecretKey::from_seed(&[byte; 32]).unwrap()
    }

    fn request() -> TransferRequest {
        TransferRequest {
            chain_id: [7u8; 32],
            from: None,
            to: AccountId::from_bytes([9u8; 32]),
            amount_nanos: 500,
            fee_nanos: 10,
            nonce: 3,
            reveal_key: false,
        }
    }

    #[test]
    fn wallet_create_writes_loadable_wallet_and_refuses_overwrite() {
        let dir = scratch_dir("wallet");
        let res = wallet_create(&dir).unwrap();
        assert_eq!(res.mnemonic.len(), 12);
        let key = load_wallet_key(dir.join("wallet.json")).unwrap();
        let pk = key.public_key().encode();
        assert_eq!(hex::encode(pk), res.public_key_hex);
        assert_eq!(hex::encode(derive_address(&pk).to_bytes()), res.address_hex);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir.join("wallet.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let err = wallet_create(&dir).unwrap_err();
        assert!(err.contains("cannot create"), "{err}");
        // Neither the success nor the refusal leaves a temp file behind.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        // The original wallet survives.
        assert_eq!(
            load_wallet_key(dir.join("wallet.json"))
                .unwrap()
                .public_key()
                .encode(),
            pk
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mnemonic_derivation_is_deterministic() {
        let words: Vec<String> = "abandon abandon abandon abandon abandon abandon abandon \
                                  abandon abandon abandon abandon about"
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let a = derive_ed25519_key_from_mnemonic(&words).unwrap();
        let b = derive_ed25519_key_from_mnemonic(&words).unwrap();
        assert_eq!(a.public_key().encode(), b.public_key().encode());
        let mut bad = words.clone();
        bad[11] = "abandon".into(); // checksum no longer matches
        assert!(derive_ed25519_key_from_mnemonic(&bad).is_err());
    }

    #[test]
    fn parse_hex32_boundaries() {
        assert_eq!(parse_hex32(&"00".repeat(32), "x").unwrap(), [0u8; 32]);
        assert_eq!(parse_hex32(&"ff".repeat(32), "x").unwrap(), [0xff; 32]);
        assert!(parse_hex32(&"00".repeat(31), "x")
            .unwrap_err()
            .contains("31"));
        assert!(parse_hex32(&"00".repeat(33), "x")
            .unwrap_err()
            .contains("33"));
        assert!(parse_hex32("zz", "x").unwrap_err().contains("invalid hex"));
        assert!(parse_hex32("", "x").is_err());
    }

    #[test]
    fn seed_file_accepts_trailing_newline_only() {
        let dir = scratch_dir("seed");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seed");
        fs::write(&path, format!("{}\n", "11".repeat(32))).unwrap();
        assert_eq!(
            load_seed_file(&path).unwrap().public_key().encode(),
            secret(0x11).public_key().encode()
        );
        fs::write(&path, "11".repeat(16)).unwrap();
        assert!(load_seed_file(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn transfer_round_trips_and_verifies() {
        let key = secret(0x22);
        let msg = build_transfer(&request(), &key).unwrap();
        let parsed = ExternalMessage::from_bytes(&msg.to_bytes()).unwrap();
        assert_eq!(parsed, msg);
        assert_eq!(parsed.chain_id, [7u8; 32]);
        assert_eq!(parsed.from, derive_address(&key.public_key().encode()));
        assert_eq!(parsed.kind, MsgKind::Transfer);
        assert_eq!(parsed.nonce, 3);
        assert_eq!(parsed.amount_nanos, 500);
        assert_eq!(parsed.fee_nanos, 10);
        assert_eq!(parsed.pubkey, [0u8; 32]);
        parsed.verify_signature(&key.public_key()).unwrap();
        assert!(parsed.verify_signature(&secret(0x23).public_key()).is_err());
    }

    #[test]
    fn transfer_reveal_carries_pubkey() {
        let key = secret(0x22);
        let msg = build_transfer(
            &TransferRequest {
                reveal_key: true,
                ..request()
            },
            &key,
        )
        .unwrap();
        assert_eq!(msg.pubkey, key.public_key().encode());
    }

    #[test]
    fn transfer_stateless_rejections() {
        let key = secret(0x22);
        let zero = TransferRequest {
            amount_nanos: 0,
            ..request()
        };
        assert!(build_transfer(&zero, &key)
            .unwrap_err()
            .contains("non-zero"));
        let overflow = TransferRequest {
            amount_nanos: u128::MAX,
            fee_nanos: 1,
            ..request()
        };
        assert!(build_transfer(&overflow, &key)
            .unwrap_err()
            .contains("overflows"));
        let max_ok = TransferRequest {
            amount_nanos: u128::MAX,
            fee_nanos: 0,
            ..request()
        };
        assert!(build_transfer(&max_ok, &key).is_ok());
        let wrong_reveal = TransferRequest {
            from: Some(AccountId::from_bytes([1u8; 32])),
            reveal_key: true,
            ..request()
        };
        assert!(build_transfer(&wrong_reveal, &key)
            .unwrap_err()
            .contains("not the key-derived address"));
    }

    #[test]
    fn pool_write_is_named_by_hash_and_leaves_no_temp() {
        let dir = scratch_dir("pool");
        fs::create_dir_all(&dir).unwrap();
        let msg = build_transfer(&request(), &secret(0x22)).unwrap();
        let path = write_to_pool(&dir, &msg).unwrap();
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            format!("{}.msg", hex::encode(msg.hash()))
        );
        assert_eq!(fs::read(&path).unwrap(), msg.to_bytes());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_identical_pool_writes_never_publish_partial_bytes() {
        let dir = scratch_dir("pool-race");
        fs::create_dir_all(&dir).unwrap();
        let msg = build_transfer(&request(), &secret(0x22)).unwrap();
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let (dir, msg) = (dir.clone(), msg.clone());
                std::thread::spawn(move || {
                    let path = write_to_pool(&dir, &msg).unwrap();
                    // Whatever is published at any moment is the full message.
                    assert_eq!(fs::read(&path).unwrap(), msg.to_bytes());
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![format!("{}.msg", hex::encode(msg.hash()))]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unique_temps_are_distinct_and_private_only_on_request() {
        let dir = scratch_dir("temp");
        fs::create_dir_all(&dir).unwrap();
        let a = write_unique_temp(&dir, "x", b"one", false).unwrap();
        let b = write_unique_temp(&dir, "x", b"two", false).unwrap();
        assert_ne!(a, b);
        assert_eq!(fs::read(&a).unwrap(), b"one");
        assert_eq!(fs::read(&b).unwrap(), b"two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let p = write_unique_temp(&dir, "p", b"k", true).unwrap();
            assert_eq!(
                fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let q = write_unique_temp(&dir, "q", b"k", false).unwrap();
            assert_ne!(
                fs::metadata(&q).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
