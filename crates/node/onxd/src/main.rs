use onxd::{parse_cli_args, run_daemon};
use std::env;
use std::sync::Arc;
use std::time::Duration;

/// `sentry::TransportFactory` that builds the reqwest transport with a
/// timeout-configured client.
///
/// The default transport's client has no timeouts. If the Sentry endpoint
/// accepts and never replies (or the network drops packets silently), the
/// transport thread blocks inside the HTTP request forever — and dropping
/// the Sentry guard joins that thread with no time limit (unbounded
/// `handle.join()` in sentry 0.49's `TransportThread::drop`). The daemon
/// would then hang on shutdown instead of exiting, and `Restart=on-failure`
/// would never fire. Timeouts on the client make every request fail fast,
/// so neither the panic-hook flush nor the guard drop can hang the process.
#[derive(Debug, Clone)]
struct TimeoutTransportFactory {
    client: reqwest::Client,
}

impl sentry::TransportFactory for TimeoutTransportFactory {
    fn create_transport_with_options(
        &self,
        options: sentry::TransportOptions,
    ) -> Arc<dyn sentry::Transport> {
        Arc::new(
            sentry::transports::ReqwestHttpTransportOptions::from(options)
                .with_client(self.client.clone())
                .build(),
        )
    }
}

#[tokio::main]
async fn main() {
    // Sentry crash reporting, opt-in via SENTRY_DSN. The guard must live for
    // the whole process so queued events flush before exit. The DSN is never
    // hardcoded: it comes from the environment (systemd EnvironmentFile or
    // the shell), so the public repo carries no secrets.
    let _sentry_guard = env::var("SENTRY_DSN")
        .ok()
        .filter(|s| !s.is_empty())
        .and_then(|dsn| {
            // ClientOptions is non-exhaustive: configure via mutation, not struct syntax.
            let mut opts = sentry::ClientOptions::default();
            opts.release = sentry::release_name!();
            // Never send the machine hostname: for a validator it is
            // identifying (along with the OS/kernel/debug-images the
            // contexts integration would otherwise attach). A fixed label
            // is enough to group events. (send_default_pii stays off.)
            opts.server_name = Some("onxd".into());
            // Bound the panic-hook flush and the guard-drop shutdown. With
            // the timeout-configured transport below, nothing can block
            // longer than this anyway; without it, a slow endpoint stalls
            // the panicking thread up to the default 2s per panic.
            opts.shutdown_timeout = Duration::from_millis(500);
            // Timeout-configured transport (see struct docs above). A
            // reporting-only feature must never prevent startup: if the
            // client can't be built, log and run without Sentry.
            let client = match reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(5))
                .build()
            {
                Ok(client) => client,
                Err(err) => {
                    eprintln!("warning: sentry disabled: cannot build HTTP client: {err}");
                    return None;
                }
            };
            opts.transport = Some(Arc::new(TimeoutTransportFactory { client }));
            let guard = sentry::init((dsn, opts));
            // Skip sentry's panic hook entirely while the producer is inside
            // a deliberate containment probe (ADR-0029). A `before_send`
            // filter is NOT enough: the hook still runs its synchronous
            // `flush`, whose blocking send onto sentry's queue has no
            // timeout — with a full queue that's ~10s of stalled block
            // production per probe panic (~11 probes per hostile message).
            // Wrapping the hook avoids the flush completely (measured
            // 0.003–0.09ms per suppressed panic vs ~505ms–10s before).
            // The producer's own backtrace hook (installed later at producer
            // startup) wraps this one and still logs loudly to stderr.
            let sentry_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                if !onxd::producer::suppress_sentry_panic() {
                    sentry_hook(info);
                }
            }));
            Some(guard)
        });

    let args: Vec<String> = env::args().collect();
    let config = match parse_cli_args(&args) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(2);
        }
    };

    // Operator-controlled values, captured before `config` is moved into
    // `run_daemon`: some startup errors echo them, and they must not leave
    // the machine for Sentry (see `redact_sensitive_values`).
    let sensitive_values: Vec<String> = [
        &config.signing_key_path,
        &config.node_key_path,
        &config.bootstrap_genesis,
        &config.fee_collector,
    ]
    .into_iter()
    .flatten()
    .cloned()
    .collect();

    if let Err(err) = run_daemon(config).await {
        eprintln!("onxd failed: {err}");
        // The panic hook never fires for these exits (no panic), so without
        // this Sentry stays blind to the daemon's most common fatal path.
        // Redact operator-controlled values first (see above).
        // No-op when Sentry is not initialized.
        let redacted = redact_sensitive_values(&err, &sensitive_values);
        sentry::capture_message(&format!("onxd failed: {redacted}"), sentry::Level::Error);
        // Bounded flush: give the report a chance to leave, but never stall
        // shutdown on a slow endpoint. exit(1) below skips the guard drop,
        // so this is the only flush this path gets.
        if let Some(client) = sentry::Hub::current().client() {
            client.flush(Some(Duration::from_millis(500)));
        }
        std::process::exit(1);
    }
}

/// Remove operator-controlled values (filesystem paths, account IDs) from an
/// error message before it leaves the machine for Sentry. The values
/// themselves are configuration, not key material, but they don't help
/// diagnose the event.
fn redact_sensitive_values(err: &str, sensitive_values: &[String]) -> String {
    let mut out = err.to_string();
    for value in sensitive_values {
        out = out.replace(value.as_str(), "[redacted]");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_signing_key_path() {
        let err = "failed to read key at /home/onx/.onx/signing.key: permission denied";
        let out = redact_sensitive_values(err, &["/home/onx/.onx/signing.key".to_string()]);
        assert_eq!(out, "failed to read key at [redacted]: permission denied");
    }

    #[test]
    fn redacts_genesis_path() {
        let err = "genesis not found: /var/lib/onx/genesis.toml";
        let out = redact_sensitive_values(err, &["/var/lib/onx/genesis.toml".to_string()]);
        assert!(!out.contains("/var/lib/onx/genesis.toml"));
        assert!(out.contains("[redacted]"));
    }

    #[test]
    fn redacts_fee_collector_account() {
        let collector = "d04ab2326789abcdef0123456789abcdef0123456789abcdef0123456789abcd";
        let err = format!("fee collector {collector} has insufficient balance");
        let out = redact_sensitive_values(&err, &[collector.to_string()]);
        assert!(!out.contains("d04ab232"));
        assert!(out.contains("[redacted]"));
    }

    #[test]
    fn redacts_multiple_values() {
        let err = "key /a.key and genesis /b.toml failed";
        let out = redact_sensitive_values(err, &["/a.key".to_string(), "/b.toml".to_string()]);
        assert_eq!(out, "key [redacted] and genesis [redacted] failed");
    }

    #[test]
    fn leaves_clean_messages_untouched() {
        let err = "connection refused: timeout after 500ms";
        let out = redact_sensitive_values(err, &["/a.key".to_string()]);
        assert_eq!(out, err);
    }
}
