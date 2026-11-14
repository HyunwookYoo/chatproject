//! `push_proxy`: plain HTTP on `PUSH_LISTEN`; TLS is the reverse proxy's job
//! (spec 9.1). Configuration: see `Config`.

use std::process::ExitCode;
use std::sync::Arc;

use futures_core::future::BoxFuture;
use push_proxy::{AppState, Config, Delivery, Upstream, UpstreamResult, router, serve};

/// Stands in until the FCM sender exists (plan Task 3): every send fails and
/// is counted as `failed` in the log.
struct NoSender;

impl Upstream for NoSender {
    fn send(&self, _delivery: Delivery) -> BoxFuture<'static, UpstreamResult> {
        Box::pin(async { UpstreamResult::Failed })
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("push_proxy: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut state = AppState::new(config.keys(), config.key_id, Arc::new(NoSender));
    state.trust_proxy = config.trust_proxy;
    let listener = match tokio::net::TcpListener::bind(config.listen).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("push_proxy: cannot listen on {}: {e}", config.listen);
            return ExitCode::FAILURE;
        }
    };
    let header_timeout = state.limits.header_timeout;
    serve(listener, router(state), header_timeout).await;
    ExitCode::SUCCESS
}
