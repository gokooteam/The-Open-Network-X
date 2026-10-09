pub mod blockfiles;
pub mod follower;
pub mod mempool;
pub mod producer;

use crate::mempool::Mempool;
use crate::producer::{run_producer_loop, ProducerConfig};
use onx_data_structures::AccountId;
use onx_primitives::SecretKey;
use onx_storage::ChainStore;
use onx_telemetry::{serve_metrics, TelemetryConfig, TelemetryHandle};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

/// Best-effort memory wipe for key material. Uses volatile writes so the
/// compiler cannot optimize the wipe away.
fn zeroize(buf: &mut [u8]) {
    for b in buf.iter_mut() {
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

/// Load a 32-byte key seed from a file with the node's key-file discipline.
///
/// - The file must contain exactly 32 bytes (the seed).
/// - The file must have mode 0600 (owner read/write only) and be owned
///   by the effective uid.
/// - The file is opened with O_NOFOLLOW (never a symlink) and all checks
///   run against the open file descriptor, so there is no stat→read race.
///
/// The seed buffer is zeroized after the key is derived. Any violation is
/// a startup refusal, never a warning.
fn load_key_seed(path: &str, what: &str) -> Result<[u8; 32], String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    // Open with O_NOFOLLOW: the key path must be a real file, never a
    // symlink. This closes the symlink race that a symlink_metadata check
    // alone leaves open.
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| format!("{what} {path}: cannot open (not a regular file?): {e}"))?;
    // Stat the open fd — the checks below apply to the exact bytes we read.
    let metadata = file
        .metadata()
        .map_err(|e| format!("{what} {path}: cannot stat open file: {e}"))?;
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Err(format!(
            "{what} {path}: bad permissions {mode:o} (must be 600)"
        ));
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(format!("{what} {path}: not owned by the current user"));
    }
    let mut seed = Vec::with_capacity(32);
    file.read_to_end(&mut seed)
        .map_err(|e| format!("{what} {path}: cannot read: {e}"))?;
    if seed.len() != 32 {
        // Zero the buffer before returning: it may hold a partial key.
        zeroize(&mut seed);
        return Err(format!(
            "{what} {path}: must be 32 bytes, got {}",
            seed.len()
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&seed);
    // The buffer has served its purpose; wipe it from memory. (SecretKey
    // manages its own material from here.)
    zeroize(&mut seed);
    Ok(arr)
}

/// Load a validator signing key (ONXBLK05, TRAP 4).
///
/// Same file discipline as [`load_key_seed`], plus: the derived pubkey
/// must match a genesis validator's pubkey. Any violation is a startup
/// refusal, never a warning.
fn load_signing_key(path: &str, store: &ChainStore) -> Result<SecretKey, String> {
    let mut seed = load_key_seed(path, "signing key")?;
    let secret =
        SecretKey::from_seed(&seed).map_err(|e| format!("signing key {path}: bad seed: {e}"))?;
    // The seed has served its purpose; wipe it now that the key is derived.
    // (SecretKey manages its own material from here.)
    zeroize(&mut seed);
    let pubkey = secret.public_key().encode();

    // The key must belong to a genesis validator.
    let doc = store
        .genesis_document()
        .map_err(|e| format!("signing key: cannot load genesis: {e}"))?
        .ok_or_else(|| "signing key: no genesis in store".to_string())?;
    let matches = doc.validators.iter().any(|v| v.pubkey == pubkey);
    if !matches {
        return Err(
            "signing key: derived pubkey matches no genesis validator — refusing to start"
                .to_string(),
        );
    }
    Ok(secret)
}

/// Load the node's ADNL identity key (M5).
///
/// Same file discipline as [`load_key_seed`], but NO genesis-validator
/// match is required: followers aren't validators, and a producer's
/// network identity is separate from its block-signing key (different
/// keys, different jobs — the signing key never touches the network).
fn load_node_key(path: &str) -> Result<SecretKey, String> {
    let mut seed = load_key_seed(path, "node key")?;
    let key = SecretKey::from_seed(&seed).map_err(|e| format!("node key {path}: bad seed: {e}"))?;
    // Same wipe discipline as the signing key: the seed must not linger in
    // memory after the key is derived.
    zeroize(&mut seed);
    Ok(key)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    FullNode,
    ValidatorNode,
    LiteServerNode,
}

impl NodeRole {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "full" | "full-node" | "fullnode" => Ok(Self::FullNode),
            "validator" | "validator-node" | "validatornode" => Ok(Self::ValidatorNode),
            "lite" | "lite-server" | "liteserver" | "lite-server-node" => Ok(Self::LiteServerNode),
            _ => Err(format!("unknown node role: {s}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnxdConfig {
    pub role: NodeRole,
    pub storage_path: String,
    pub network_enabled: bool,
    pub network_bind: String,
    pub peers: Vec<String>,
    pub bootstrap_genesis: Option<String>,
    pub shutdown_after_ms: Option<u64>,
    /// Directory where signed transaction files are dropped for the
    /// mempool. Created with `pending/` and `rejected/` subdirectories.
    pub tx_pool_dir: String,
    /// Account receiving the validator half of fees. Hex-encoded 32 bytes.
    /// Required: block production is explicit about who collects.
    pub fee_collector: Option<String>,
    /// Mempool idle poll interval (liveness only, never in block content).
    pub block_poll_interval_ms: u64,
    /// Mempool bound; new submissions are rejected when full.
    pub mempool_max_txs: usize,
    /// Path to the validator signing key file (32-byte seed). If set, the
    /// producer signs blocks (ONXBLK05); on startup the key's pubkey must
    /// match a genesis validator and the file must be mode 0600.
    pub signing_key_path: Option<String>,
    /// Follower mode (M5): instead of producing blocks, sync them from the
    /// producer named in `peers` (exactly one). No signing key, no mempool,
    /// no fee collector needed — the follower verifies everything itself.
    pub follower: bool,
    /// Path to the node's ADNL identity key file (32-byte seed, mode 0600).
    /// Required when `network_enabled` is true: the node binds its ADNL
    /// transport under this identity, and peers pin it from config.
    pub node_key_path: Option<String>,
}

impl Default for OnxdConfig {
    fn default() -> Self {
        Self {
            role: NodeRole::FullNode,
            storage_path: "./onx-data".to_string(),
            network_enabled: true,
            network_bind: "127.0.0.1:0".to_string(),
            peers: Vec::new(),
            bootstrap_genesis: None,
            shutdown_after_ms: None,
            tx_pool_dir: "./onx-txpool".to_string(),
            fee_collector: None,
            block_poll_interval_ms: 200,
            mempool_max_txs: 10_000,
            signing_key_path: None,
            follower: false,
            node_key_path: None,
        }
    }
}

pub fn load_config(path: impl AsRef<Path>) -> Result<OnxdConfig, String> {
    let raw = fs::read_to_string(path.as_ref()).map_err(|err| {
        format!(
            "failed to read config file {}: {err}",
            path.as_ref().display()
        )
    })?;

    let mut role = NodeRole::FullNode;
    let mut storage_path = "./onx-data".to_string();
    let mut network_enabled = true;
    let mut network_bind = "127.0.0.1:0".to_string();
    let mut peers = Vec::new();
    let mut bootstrap_genesis = None;
    let mut shutdown_after_ms = None;
    let mut tx_pool_dir = "./onx-txpool".to_string();
    let mut fee_collector: Option<String> = None;
    let mut block_poll_interval_ms = 200u64;
    let mut mempool_max_txs = 10_000usize;
    let mut signing_key_path: Option<String> = None;
    let mut follower = false;
    let mut node_key_path: Option<String> = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            match key {
                "role" => role = NodeRole::parse(value)?,
                "storage_path" => storage_path = value.to_string(),
                "network_enabled" => {
                    network_enabled = matches!(
                        value.to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    )
                }
                "network_bind" => network_bind = value.to_string(),
                "peers" => {
                    peers = value
                        .split(',')
                        .map(|p| p.trim().to_string())
                        .filter(|p| !p.is_empty())
                        .collect();
                }
                "bootstrap_genesis" => bootstrap_genesis = Some(value.to_string()),
                "shutdown_after_ms" => {
                    shutdown_after_ms = Some(
                        value
                            .parse::<u64>()
                            .map_err(|_| format!("invalid shutdown_after_ms value: {value}"))?,
                    );
                }
                "tx_pool_dir" => tx_pool_dir = value.to_string(),
                "fee_collector" => fee_collector = Some(value.to_string()),
                "block_poll_interval_ms" => {
                    block_poll_interval_ms = value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid block_poll_interval_ms value: {value}"))?;
                }
                "mempool_max_txs" => {
                    mempool_max_txs = value
                        .parse::<usize>()
                        .map_err(|_| format!("invalid mempool_max_txs value: {value}"))?;
                }
                "signing_key_path" => signing_key_path = Some(value.to_string()),
                "follower" => {
                    follower = matches!(
                        value.to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    )
                }
                "node_key_path" => node_key_path = Some(value.to_string()),
                _ => {}
            }
        }
    }

    Ok(OnxdConfig {
        role,
        storage_path,
        network_enabled,
        network_bind,
        peers,
        bootstrap_genesis,
        shutdown_after_ms,
        tx_pool_dir,
        fee_collector,
        block_poll_interval_ms,
        mempool_max_txs,
        signing_key_path,
        follower,
        node_key_path,
    })
}

pub fn parse_cli_args(args: &[String]) -> Result<OnxdConfig, String> {
    let mut config = OnxdConfig::default();
    let mut config_path = None;

    for idx in 1..args.len() {
        if args[idx].as_str() == "--config" {
            if idx + 1 >= args.len() {
                return Err("--config requires a path".to_string());
            }
            config_path = Some(args[idx + 1].clone());
        }
    }

    if let Some(path) = config_path {
        config = load_config(path)?;
    }

    let mut idx = 1;
    while idx < args.len() {
        match args[idx].as_str() {
            "--role" => {
                idx += 1;
                if idx >= args.len() {
                    return Err("--role requires a value".to_string());
                }
                config.role = NodeRole::parse(&args[idx])?;
            }
            "--storage-path" => {
                idx += 1;
                if idx >= args.len() {
                    return Err("--storage-path requires a value".to_string());
                }
                config.storage_path = args[idx].clone();
            }
            "--network-bind" => {
                idx += 1;
                if idx >= args.len() {
                    return Err("--network-bind requires a value".to_string());
                }
                config.network_bind = args[idx].clone();
            }
            "--peer" => {
                idx += 1;
                if idx >= args.len() {
                    return Err("--peer requires a value".to_string());
                }
                config.peers.push(args[idx].clone());
            }
            "--tx-pool-dir" => {
                idx += 1;
                if idx >= args.len() {
                    return Err("--tx-pool-dir requires a value".to_string());
                }
                config.tx_pool_dir = args[idx].clone();
            }
            "--fee-collector" => {
                idx += 1;
                if idx >= args.len() {
                    return Err("--fee-collector requires a value".to_string());
                }
                config.fee_collector = Some(args[idx].clone());
            }
            "--block-poll-interval-ms" => {
                idx += 1;
                if idx >= args.len() {
                    return Err("--block-poll-interval-ms requires a value".to_string());
                }
                config.block_poll_interval_ms = args[idx]
                    .parse::<u64>()
                    .map_err(|_| "invalid --block-poll-interval-ms value".to_string())?;
            }
            "--signing-key" => {
                idx += 1;
                if idx >= args.len() {
                    return Err("--signing-key requires a path".to_string());
                }
                config.signing_key_path = Some(args[idx].clone());
            }
            "--follower" => {
                config.follower = true;
            }
            "--node-key" => {
                idx += 1;
                if idx >= args.len() {
                    return Err("--node-key requires a path".to_string());
                }
                config.node_key_path = Some(args[idx].clone());
            }
            "--help" | "-h" => {
                return Err(
                    "usage: onxd --config onxd.toml [--role full|validator|lite] [--tx-pool-dir DIR] [--fee-collector HEX] [--block-poll-interval-ms MS] [--signing-key PATH] [--follower] [--node-key PATH]".to_string(),
                );
            }
            _ => {}
        }
        idx += 1;
    }
    Ok(config)
}

pub async fn run_daemon(config: OnxdConfig) -> Result<(), String> {
    fs::create_dir_all(&config.storage_path).map_err(|err| {
        format!(
            "failed to initialize storage path {}: {err}",
            config.storage_path
        )
    })?;

    let store = ChainStore::open(Path::new(&config.storage_path).join("chain.redb"))
        .map_err(|err| format!("failed to initialize chain store: {err}"))?;

    // Genesis is real now, not a non-empty check: parse the TOML config,
    // build the canonical document, and initialize the store (idempotent
    // for the same genesis, fail-closed on a different one).
    match &config.bootstrap_genesis {
        Some(genesis_path) => {
            let cfg = onx_genesis::parse_config(genesis_path)
                .map_err(|err| format!("genesis config {genesis_path}: {err}"))?;
            let doc = onx_genesis::build_genesis_document(&cfg)
                .map_err(|err| format!("genesis build {genesis_path}: {err}"))?;
            store
                .init_genesis(&doc)
                .map_err(|err| format!("genesis init: {err}"))?;
        }
        None => {
            let has_genesis = store
                .genesis_hash()
                .map_err(|err| format!("failed to read genesis marker: {err}"))?
                .is_some();
            if !has_genesis {
                return Err(
                    "no genesis: set bootstrap_genesis to a genesis TOML config \
                     (the store is empty and no genesis was provided)"
                        .to_string(),
                );
            }
        }
    }

    // Fee collector: explicit operator identity, parsed fail-fast. No magic
    // accounts anywhere in the pipeline. A follower produces no blocks and
    // collects no fees, so it doesn't need one.
    let fee_collector = if config.follower {
        None
    } else {
        let fee_collector_hex = config.fee_collector.as_deref().ok_or_else(|| {
            "no fee collector: set fee_collector to the 64-char hex account id \
             receiving the validator half of fees"
                .to_string()
        })?;
        let fee_collector_bytes = hex::decode(fee_collector_hex)
            .map_err(|_| format!("fee_collector is not valid hex: {fee_collector_hex}"))?;
        if fee_collector_bytes.len() != 32 {
            return Err(format!(
                "fee_collector must be 32 bytes hex, got {} bytes",
                fee_collector_bytes.len()
            ));
        }
        let mut fee_collector_arr = [0u8; 32];
        fee_collector_arr.copy_from_slice(&fee_collector_bytes);
        Some(AccountId::from_bytes(fee_collector_arr))
    };

    // Validator signing key (ONXBLK05, TRAP 4): if configured, load the
    // 32-byte seed, require mode 0600, and refuse to start unless the
    // derived pubkey matches a genesis validator.
    let signing_key = match &config.signing_key_path {
        None => None,
        Some(path) => Some(load_signing_key(path, &store)?),
    };

    // Fail fast before spawning anything: a producer without a signing key
    // cannot produce a single valid block. Refusing here (not inside the
    // producer thread) guarantees the process exits non-zero at startup
    // instead of sitting idle behind a healthy-looking PID. Followers don't
    // sign, so they don't need one.
    if signing_key.is_none() && !config.follower {
        return Err(
            "no signing key configured (signing_key_path in config or --signing-key): \
             refusing to start"
                .to_string(),
        );
    }

    let metrics = TelemetryHandle::new().map_err(|err| err.to_string())?;
    // The M5 sync protocol is datagram-based: there is no connection state
    // to count, so zero stays the honest value (it counts nothing yet,
    // rather than implying a network that isn't measured).
    metrics.set_connected_peers(0);
    metrics.set_tx_pool_size(0);

    let metrics_cfg = TelemetryConfig::default();
    let metrics_task = tokio::spawn(async move {
        let _ = serve_metrics(metrics_cfg).await;
    });

    if config.network_enabled {
        return run_networked(
            config,
            store,
            metrics,
            metrics_task,
            signing_key,
            fee_collector,
        )
        .await;
    }

    // A follower with networking disabled would fall through to the
    // single-node producer below and panic on the missing producer
    // credentials (`expect("checked above")`). There is no offline follower
    // mode — following means fetching blocks from a peer over the network —
    // so refuse loudly at startup instead of panicking.
    if config.follower {
        return Err(
            "follower=true requires network_enabled=true: a follower fetches blocks from its \
             configured peer over the network; there is no offline follower mode"
                .to_string(),
        );
    }

    run_single_node(
        config,
        store,
        metrics,
        metrics_task,
        signing_key.expect("checked above"),
        fee_collector.expect("checked above"),
    )
    .await
}

/// M5 networked mode: bind the ADNL transport and run either the follower
/// loop or the producer loop plus the block-sync server.
async fn run_networked(
    config: OnxdConfig,
    store: ChainStore,
    metrics: TelemetryHandle,
    metrics_task: tokio::task::JoinHandle<()>,
    signing_key: Option<SecretKey>,
    fee_collector: Option<AccountId>,
) -> Result<(), String> {
    use crate::follower::{parse_sync_peer, run_follower_loop, FollowerConfig};
    use onx_networking::{
        AdnlTransportNode, SharedAdnlTransport, SyncClient, SyncConfig, SyncServer,
    };

    // Node identity: the ADNL transport binds under this key, and peers pin
    // it from their config. Separate from the block-signing key — the
    // signing key never touches the network.
    let node_key_path = config.node_key_path.as_deref().ok_or_else(|| {
        "network_enabled=true requires node_key_path (the node's ADNL identity key file)"
            .to_string()
    })?;
    let node_key = load_node_key(node_key_path)?;
    let bind_addr: std::net::SocketAddr = config
        .network_bind
        .parse()
        .map_err(|e| format!("network_bind is not a socket address: {e}"))?;
    let node = AdnlTransportNode::bind(node_key, bind_addr)
        .await
        .map_err(|e| format!("failed to bind ADNL transport on {bind_addr}: {e}"))?;
    eprintln!(
        "onxd: ADNL node bound on {} as {} (pubkey {})",
        node.local_addr()
            .map_err(|e| format!("failed to read bound address: {e}"))?,
        hex::encode(node.abstract_address().0),
        hex::encode(node.public_key().encode())
    );

    // Static peer list (ADR-0043, as amended): an empty list with networking
    // on fails closed — a node that cannot name a peer cannot sync.
    if config.peers.is_empty() {
        return Err(
            "network_enabled=true with an empty peers list: set peers to at least one \
             <pubkey-hex>@<host:port> descriptor (ADR-0043)"
                .to_string(),
        );
    }
    // Parse + resolve the static peer list off the async worker:
    // `parse_sync_peer` resolves DNS hostnames (blocking) once at startup.
    let peer_strs = config.peers.clone();
    let peers = tokio::task::spawn_blocking(move || {
        peer_strs
            .iter()
            .map(|p| parse_sync_peer(p))
            .collect::<Result<Vec<_>, _>>()
    })
    .await
    .map_err(|e| format!("peer list parse task failed: {e}"))??;

    // Pre-establish an ADNL channel with every configured peer. `send_datagram`
    // creates the sender's session on demand, but `recv_datagram` can only
    // decrypt a FastPacket when the channel already exists in our own
    // `channel_to_peer` map — a peer that only ever receives would otherwise
    // answer every inbound datagram with DecryptionFailed. Channel state is
    // deterministic and symmetric (X25519 DH both ways, canonical address
    // ordering), so both sides independently derive the same channel_id and
    // shared secret for the pair.
    let node = Arc::new(node);
    for p in &peers {
        node.connect_peer(p.public_key, p.endpoint);
    }
    eprintln!(
        "onxd: ADNL channels established with {} configured peer(s)",
        peers.len()
    );

    let shutdown = Arc::new(AtomicBool::new(false));
    let runtime_shutdown = config
        .shutdown_after_ms
        .map(|ms| sleep(Duration::from_millis(ms)));
    let shutdown_sig = tokio::signal::ctrl_c();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|err| format!("failed to install SIGTERM watcher: {err}"))?;

    if config.follower {
        // Follower mode: exactly one producer peer. Following two chains is
        // not following.
        if peers.len() != 1 {
            return Err(format!(
                "follower mode requires exactly one peer (the producer), got {}",
                peers.len()
            ));
        }
        let peer = peers.into_iter().next().expect("length checked");
        eprintln!(
            "onxd: follower mode: syncing from producer {}",
            hex::encode(peer.address.0)
        );
        let transport = SharedAdnlTransport::new(node.clone());
        let client = SyncClient::new(transport, peer, SyncConfig::default());
        let follower_cfg = FollowerConfig {
            blocks_dir: Path::new(&config.storage_path).join("blocks"),
            poll_interval: Duration::from_millis(config.block_poll_interval_ms),
            sync: SyncConfig::default(),
            telemetry: Some(metrics),
        };
        let follower_shutdown = shutdown.clone();
        let mut follower_task = tokio::spawn(async move {
            run_follower_loop(store, client, follower_cfg, follower_shutdown).await
        });

        tokio::select! {
            _ = shutdown_sig => {},
            _ = sigterm.recv() => {},
            _ = async {
                if let Some(timer) = runtime_shutdown {
                    timer.await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {},
            // The follower task is supervised like the producer: if it
            // exits (error or panic) the daemon must not sit idle behind
            // a healthy PID.
            res = &mut follower_task => {
                match res {
                    Ok(Ok(stats)) => {
                        eprintln!(
                            "onxd: follower stopped cleanly: {} blocks synced, {} waits, {} rejected",
                            stats.blocks_synced, stats.waits, stats.rejected
                        );
                        metrics_task.abort();
                        return Ok(());
                    }
                    Ok(Err(e)) => {
                        metrics_task.abort();
                        return Err(format!("onxd: follower exited with error: {e}"));
                    }
                    Err(e) => {
                        metrics_task.abort();
                        return Err(format!("onxd: follower task panicked: {e}"));
                    }
                }
            }
        }

        // Abort is crash-safe here: the abort can only take effect at an
        // await point (fetch or sleep) — never inside the synchronous
        // commit_block (atomic) or the atomic block-file write. This is the
        // same recovery story as kill -9, which the follower is built for.
        shutdown.store(true, Ordering::Relaxed);
        follower_task.abort();
        let result = match follower_task.await {
            Ok(Ok(stats)) => {
                eprintln!(
                    "onxd: follower stopped cleanly: {} blocks synced, {} waits, {} rejected",
                    stats.blocks_synced, stats.waits, stats.rejected
                );
                Ok(())
            }
            Ok(Err(e)) => Err(format!("onxd: follower exited with error: {e}")),
            Err(e) if e.is_cancelled() => Ok(()),
            Err(e) => Err(format!("onxd: follower task panicked: {e}")),
        };
        metrics_task.abort();
        return result;
    }

    // Producer mode: the existing single-node loop, plus the sync server
    // answering follower block requests from the blocks dir.
    let blocks_dir = Path::new(&config.storage_path).join("blocks");
    let server = SyncServer::new(
        SharedAdnlTransport::new(node.clone()),
        peers,
        blocks_dir.clone(),
        SyncConfig::default(),
    );
    let mut server_task = tokio::spawn(async move { server.run().await });
    eprintln!("onxd: producer mode: serving block sync to configured peers");

    // Block production runs on a dedicated blocking thread: propose/commit
    // are synchronous store operations. The async side only watches for
    // shutdown and joins the producer afterwards.
    //
    // Role note: the loop runs for every role in this milestone. There is
    // one honest producer and no validator set yet; role-differentiated
    // block production is consensus-phase work.
    let mempool_chain_id = store
        .chain_id()
        .map_err(|err| format!("failed to read chain id: {err}"))?
        .ok_or_else(|| "no genesis: chain id unavailable (genesis not initialized)".to_string())?;
    let mempool = Mempool::new(
        Path::new(&config.tx_pool_dir),
        config.mempool_max_txs,
        mempool_chain_id,
        fee_collector.expect("checked above"),
    )?;
    fs::create_dir_all(&config.tx_pool_dir).map_err(|err| {
        format!(
            "failed to initialize tx pool dir {}: {err}",
            config.tx_pool_dir
        )
    })?;
    let producer_cfg = ProducerConfig {
        fee_collector: fee_collector.expect("checked above"),
        poll_interval: Duration::from_millis(config.block_poll_interval_ms),
        tx_pool_dir: Path::new(&config.tx_pool_dir).to_path_buf(),
        blocks_dir,
        telemetry: Some(metrics),
        signing_key,
    };
    let producer_shutdown = shutdown.clone();
    let mut producer_task = tokio::task::spawn_blocking(move || {
        run_producer_loop(store, mempool, producer_cfg, producer_shutdown)
    });

    tokio::select! {
        _ = shutdown_sig => {},
        _ = sigterm.recv() => {},
        _ = async {
            if let Some(timer) = runtime_shutdown {
                timer.await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {},
        // Both tasks are supervised: if either exits (error or panic) the
        // daemon must not sit idle behind a healthy PID.
        res = &mut producer_task => {
            server_task.abort();
            match res {
                Ok(Ok(stats)) => {
                    eprintln!(
                        "onxd: producer stopped cleanly: {} blocks, {} msgs committed, {} rejected",
                        stats.blocks_produced, stats.msgs_committed, stats.txs_rejected
                    );
                    metrics_task.abort();
                    return Ok(());
                }
                Ok(Err(e)) => {
                    metrics_task.abort();
                    return Err(format!("onxd: producer exited with error: {e}"));
                }
                Err(e) => {
                    metrics_task.abort();
                    return Err(format!("onxd: producer task panicked: {e}"));
                }
            }
        }
        res = &mut server_task => {
            // serve_one only fails on our own transport — a dead socket
            // means the node cannot serve anyone. run() never returns Ok.
            let detail = match &res {
                Ok(Err(e)) => format!("{e}"),
                Ok(Ok(())) => "exited unexpectedly".to_string(),
                Err(e) => format!("task panicked: {e}"),
            };
            shutdown.store(true, Ordering::Relaxed);
            metrics_task.abort();
            return Err(format!("onxd: sync server failed: {detail}"));
        }
    }

    // Graceful shutdown: stop the server first (no state there — pure
    // network I/O), then let the producer finish its tick.
    server_task.abort();
    shutdown.store(true, Ordering::Relaxed);
    let result = match producer_task.await {
        Ok(Ok(stats)) => {
            eprintln!(
                "onxd: producer stopped cleanly: {} blocks, {} msgs committed, {} rejected",
                stats.blocks_produced, stats.msgs_committed, stats.txs_rejected
            );
            Ok(())
        }
        // A producer that fails on its final tick after the shutdown signal
        // is still a failure: propagate it as a non-zero exit, don't just
        // log it.
        Ok(Err(e)) => Err(format!("onxd: producer exited with error: {e}")),
        Err(e) => Err(format!("onxd: producer task panicked: {e}")),
    };

    metrics_task.abort();
    result
}

/// Single-node producer mode (`network_enabled = false`): the pre-M5 path,
/// unchanged.
async fn run_single_node(
    config: OnxdConfig,
    store: ChainStore,
    metrics: TelemetryHandle,
    metrics_task: tokio::task::JoinHandle<()>,
    signing_key: SecretKey,
    fee_collector: AccountId,
) -> Result<(), String> {
    // Block production runs on a dedicated blocking thread: propose/commit
    // are synchronous store operations. The async side only watches for
    // shutdown and joins the producer afterwards.
    //
    // Role note: the loop runs for every role in this milestone. There is
    // one honest producer and no validator set yet; role-differentiated
    // block production is consensus-phase work.
    let mempool_chain_id = store
        .chain_id()
        .map_err(|err| format!("failed to read chain id: {err}"))?
        .ok_or_else(|| "no genesis: chain id unavailable (genesis not initialized)".to_string())?;
    let mempool = Mempool::new(
        Path::new(&config.tx_pool_dir),
        config.mempool_max_txs,
        mempool_chain_id,
        fee_collector,
    )?;
    fs::create_dir_all(&config.tx_pool_dir).map_err(|err| {
        format!(
            "failed to initialize tx pool dir {}: {err}",
            config.tx_pool_dir
        )
    })?;
    let producer_cfg = ProducerConfig {
        fee_collector,
        poll_interval: Duration::from_millis(config.block_poll_interval_ms),
        tx_pool_dir: Path::new(&config.tx_pool_dir).to_path_buf(),
        blocks_dir: Path::new(&config.storage_path).join("blocks"),
        telemetry: Some(metrics),
        signing_key: Some(signing_key),
    };
    let shutdown = Arc::new(AtomicBool::new(false));
    let producer_shutdown = shutdown.clone();
    let producer_task = tokio::task::spawn_blocking(move || {
        run_producer_loop(store, mempool, producer_cfg, producer_shutdown)
    });
    let mut producer_task = producer_task;

    let runtime_shutdown = config
        .shutdown_after_ms
        .map(|ms| sleep(Duration::from_millis(ms)));

    let shutdown_sig = tokio::signal::ctrl_c();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|err| format!("failed to install SIGTERM watcher: {err}"))?;

    tokio::select! {
        _ = shutdown_sig => {},
        _ = sigterm.recv() => {},
        _ = async {
            if let Some(timer) = runtime_shutdown {
                timer.await;
            } else {
                std::future::pending::<()>().await;
            }
        } => {},
        // The producer task is supervised: if it exits (error or panic)
        // the daemon must not sit idle behind a healthy PID — break out
        // and propagate the failure as a non-zero exit below.
        res = &mut producer_task => {
            match res {
                Ok(Ok(stats)) => {
                    eprintln!(
                        "onxd: producer stopped cleanly: {} blocks, {} msgs committed, {} rejected",
                        stats.blocks_produced, stats.msgs_committed, stats.txs_rejected
                    );
                    return Ok(());
                }
                Ok(Err(e)) => {
                    return Err(format!("onxd: producer exited with error: {e}"));
                }
                Err(e) => {
                    return Err(format!("onxd: producer task panicked: {e}"));
                }
            }
        }
    }

    // Graceful shutdown: the flag stops the producer after its current tick
    // — an in-flight commit_block is atomic, so the store is always left in
    // a fully-committed state. Uncommitted mempool messages stay in
    // pending/ and are re-proposed on the next startup.
    shutdown.store(true, Ordering::Relaxed);
    let result = match producer_task.await {
        Ok(Ok(stats)) => {
            eprintln!(
                "onxd: producer stopped cleanly: {} blocks, {} msgs committed, {} rejected",
                stats.blocks_produced, stats.msgs_committed, stats.txs_rejected
            );
            Ok(())
        }
        // A producer that fails on its final tick after the shutdown signal
        // is still a failure: propagate it as a non-zero exit, don't just
        // log it.
        Ok(Err(e)) => Err(format!("onxd: producer exited with error: {e}")),
        Err(e) => Err(format!("onxd: producer task panicked: {e}")),
    };

    metrics_task.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parsing_and_role_mapping_round_trip() {
        let tmp = std::env::temp_dir().join(format!("onxd-config-{}.toml", std::process::id()));
        fs::write(&tmp, "role = \"validator\"\nstorage_path = \"./state\"\nnetwork_enabled = true\nnetwork_bind = \"127.0.0.1:9001\"\npeers = \"p1,p2\"\nbootstrap_genesis = \"genesis.boc\"\n").unwrap();
        let config = load_config(&tmp).unwrap();
        assert_eq!(config.role, NodeRole::ValidatorNode);
        assert_eq!(config.storage_path, "./state");
        assert_eq!(config.network_bind, "127.0.0.1:9001");
        assert_eq!(config.peers, vec!["p1", "p2"]);
        assert_eq!(config.bootstrap_genesis.as_deref(), Some("genesis.boc"));
        let _ = fs::remove_file(tmp);
    }

    #[test]
    fn cli_parse_accepts_role_and_config_file() {
        let cfg = parse_cli_args(&[
            "onxd".to_string(),
            "--role".to_string(),
            "lite".to_string(),
            "--config".to_string(),
            "does-not-exist.toml".to_string(),
        ]);
        assert!(cfg.is_err());

        let parsed =
            parse_cli_args(&["onxd".to_string(), "--role".to_string(), "full".to_string()])
                .unwrap();
        assert_eq!(parsed.role, NodeRole::FullNode);
    }
}
