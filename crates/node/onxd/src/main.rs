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
        .map(|dsn| {
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
            // The producer deliberately catches panics while isolating
            // hostile messages (ADR-0029, `propose_block_caught`). Reporting
            // each probe would stall block production on the hook's
            // synchronous flush and flood Sentry until rate-limited; one
            // summary event is sent for the isolated message instead.
            opts.before_send = Some(Arc::new(|event: sentry::protocol::Event<'static>| {
                if onxd::producer::suppress_sentry_panic() {
                    None
                } else {
                    Some(event)
                }
            }));
            // Timeout-configured transport (see struct docs above).
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client with timeouts builds");
            opts.transport = Some(Arc::new(TimeoutTransportFactory { client }));
            sentry::init((dsn, opts))
        });

    let args: Vec<String> = env::args().collect();
    let config = match parse_cli_args(&args) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(2);
        }
    };

    if let Err(err) = run_daemon(config).await {
        eprintln!("onxd failed: {err}");
        // The panic hook never fires for these exits (no panic), so without
        // this Sentry stays blind to the daemon's most common fatal path.
        // No-op when Sentry is not initialized.
        sentry::capture_message(&format!("onxd failed: {err}"), sentry::Level::Error);
        // Bounded flush: give the report a chance to leave, but never stall
        // shutdown on a slow endpoint. exit(1) below skips the guard drop,
        // so this is the only flush this path gets.
        if let Some(client) = sentry::Hub::current().client() {
            client.flush(Some(Duration::from_millis(500)));
        }
        std::process::exit(1);
    }
}
