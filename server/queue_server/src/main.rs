//! `queue_server`: plain HTTP on `QUEUE_LISTEN`; TLS is the reverse proxy's
//! job (spec 9.1). Configuration: see `Config` and README.md.

use std::process::ExitCode;

use http_guard::serve;
use queue_server::{AppState, Config, Store, router, run_sweeper, serve_limits};

#[tokio::main]
async fn main() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("queue_server: {e}");
            return ExitCode::FAILURE;
        }
    };
    #[cfg(feature = "harness-counts")]
    if harness_counts() {
        return match queue_server::held_counts(&config.db) {
            Ok(entries) => {
                println!("{}", queue_server::counts_line(&entries));
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("queue_server: cannot read QUEUE_DB: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let store = match Store::open(&config.db) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("queue_server: cannot open QUEUE_DB: {e}");
            return ExitCode::FAILURE;
        }
    };
    let listener = match tokio::net::TcpListener::bind(config.listen).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("queue_server: cannot listen on {}: {e}", config.listen);
            return ExitCode::FAILURE;
        }
    };
    let mut state = AppState::new(store.clone());
    state.limits.max_bytes = config.max_bytes;
    state.trust_proxy = config.trust_proxy;
    tokio::spawn(run_sweeper(store, state.clock.clone(), state.log.clone()));
    serve(listener, router(state), serve_limits()).await;
    ExitCode::SUCCESS
}

/// `--harness-counts` (debug builds with the `harness-counts` feature): print the counts of
/// `QUEUE_DB` and exit, serving nothing.
#[cfg(feature = "harness-counts")]
fn harness_counts() -> bool {
    let mut args = std::env::args().skip(1);
    matches!(
        (args.next().as_deref(), args.next()),
        (Some("--harness-counts"), None)
    )
}
