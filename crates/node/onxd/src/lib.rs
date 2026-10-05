use onx_storage::ChainStore;
use onx_telemetry::{serve_metrics, TelemetryConfig, TelemetryHandle};
use std::fs;
use std::path::Path;
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
            "--help" | "-h" => {
                return Err(
                    "usage: onxd --config onxd.toml [--role full|validator|lite]".to_string(),
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

    // Phase 4: the chain store is a single redb file. Opening it here keeps
    // the fail-fast behavior (a bad storage path aborts startup); the daemon
    // itself does not use it yet (networking is frozen until replay passes).
    let _store = ChainStore::open(Path::new(&config.storage_path).join("chain.redb"))
        .map_err(|err| format!("failed to initialize chain store: {err}"))?;

    if let Some(genesis_path) = &config.bootstrap_genesis {
        let genesis = fs::read(genesis_path)
            .map_err(|err| format!("failed to read bootstrap genesis {}: {err}", genesis_path))?;
        if genesis.is_empty() {
            return Err(format!("bootstrap genesis is empty: {genesis_path}"));
        }
    }

    let metrics = TelemetryHandle::new().map_err(|err| err.to_string())?;
    // Networking is frozen: reporting a peer count would imply a network
    // exists. Zero is the honest value until the real loop lands.
    metrics.set_connected_peers(0);
    metrics.set_tx_pool_size(0);

    let metrics_cfg = TelemetryConfig::default();
    let metrics_task = tokio::spawn(async move {
        let _ = serve_metrics(metrics_cfg).await;
    });

    let runtime_shutdown = config
        .shutdown_after_ms
        .map(|ms| sleep(Duration::from_millis(ms)));

    // Phase 5: the fake network/consensus loop is deleted. It constructed
    // components and discarded them in a 10ms ticker, and every node shared
    // the hardcoded secret `[1u8; 32]` — both are gone. Networking stays
    // frozen until the deterministic-replay milestone passes; refusing to
    // pretend a network exists.
    if config.network_enabled {
        return Err(
            "networking is frozen until the deterministic-replay milestone passes: \
             refusing to start with network_enabled=true (there is no real network \
             loop yet). Set network_enabled=false to run config/storage validation only."
                .to_string(),
        );
    }

    let shutdown = tokio::signal::ctrl_c();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|err| format!("failed to install SIGTERM watcher: {err}"))?;

    let outcome = tokio::select! {
        _ = shutdown => Ok(()),
        _ = sigterm.recv() => Ok(()),
        _ = async {
            if let Some(timer) = runtime_shutdown {
                timer.await;
            } else {
                std::future::pending::<()>().await;
            }
        } => Ok(()),
    };

    metrics_task.abort();
    outcome
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
