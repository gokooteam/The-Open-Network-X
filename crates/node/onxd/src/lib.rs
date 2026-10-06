pub mod mempool;
pub mod producer;

use crate::mempool::Mempool;
use crate::producer::{run_producer_loop, ProducerConfig};
use onx_data_structures::AccountId;
use onx_storage::ChainStore;
use onx_telemetry::{serve_metrics, TelemetryConfig, TelemetryHandle};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

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
            "--help" | "-h" => {
                return Err(
                    "usage: onxd --config onxd.toml [--role full|validator|lite] [--tx-pool-dir DIR] [--fee-collector HEX] [--block-poll-interval-ms MS]".to_string(),
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
    // accounts anywhere in the pipeline.
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
    let fee_collector = AccountId::from_bytes(fee_collector_arr);

    let metrics = TelemetryHandle::new().map_err(|err| err.to_string())?;
    // Networking is frozen: reporting a peer count would imply a network
    // exists. Zero is the honest value until the real loop lands.
    metrics.set_connected_peers(0);
    metrics.set_tx_pool_size(0);

    let metrics_cfg = TelemetryConfig::default();
    let metrics_task = tokio::spawn(async move {
        let _ = serve_metrics(metrics_cfg).await;
    });

    // Networking stays frozen: refusing to pretend a network exists.
    if config.network_enabled {
        return Err(
            "networking is frozen until the deterministic-replay milestone passes: \
             refusing to start with network_enabled=true (there is no real network \
             loop yet). Set network_enabled=false to run the single-node producer."
                .to_string(),
        );
    }

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
    };
    let shutdown = Arc::new(AtomicBool::new(false));
    let producer_shutdown = shutdown.clone();
    let producer_task = tokio::task::spawn_blocking(move || {
        run_producer_loop(store, mempool, producer_cfg, producer_shutdown)
    });

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
    }

    // Graceful shutdown: the flag stops the producer after its current tick
    // — an in-flight commit_block is atomic, so the store is always left in
    // a fully-committed state. Uncommitted mempool messages stay in
    // pending/ and are re-proposed on the next startup.
    shutdown.store(true, Ordering::Relaxed);
    match producer_task.await {
        Ok(Ok(stats)) => eprintln!(
            "onxd: producer stopped cleanly: {} blocks, {} msgs committed, {} rejected",
            stats.blocks_produced, stats.msgs_committed, stats.txs_rejected
        ),
        Ok(Err(e)) => eprintln!("onxd: producer exited with error: {e}"),
        Err(e) => eprintln!("onxd: producer task panicked: {e}"),
    }

    metrics_task.abort();
    Ok(())
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
