use onxd::{parse_cli_args, run_daemon};
use std::env;

#[tokio::main]
async fn main() {
    // Sentry crash reporting, opt-in via SENTRY_DSN. The guard must live for
    // the whole process so queued events flush before exit. The DSN is never
    // hardcoded: it comes from the environment (systemd EnvironmentFile or
    // the shell), so the public repo carries no secrets. Panics are captured
    // by sentry's default integrations; send_default_pii stays off.
    let _sentry_guard = env::var("SENTRY_DSN")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|dsn| {
            // ClientOptions is non-exhaustive: configure via mutation, not struct syntax.
            let mut opts = sentry::ClientOptions::default();
            opts.release = sentry::release_name!();
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
        std::process::exit(1);
    }
}
