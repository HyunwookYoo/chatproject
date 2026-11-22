//! Plain HTTP/1 serving with bounds on connections.
//!
//! `axum::serve` gives hyper no timer, so hyper's header-read timeout never
//! fires there and a client that never finishes its headers holds the
//! connection forever. This loop sets the timer, caps how many connections are
//! open at once and how long each one lives; the rest of a request is bounded
//! by `Limits::request_timeout` inside the router.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::ConnectInfo;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tower_service::Service;

use crate::limit::Limits;

/// Pause after a failed `accept` (for example out of file descriptors).
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Serves `app` on `listener` until the process ends. Each request carries
/// `ConnectInfo<SocketAddr>` of its TCP peer.
pub async fn serve(listener: TcpListener, app: Router, limits: Limits) {
    let open = Arc::new(Semaphore::new(
        limits.max_connections.min(Semaphore::MAX_PERMITS),
    ));
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                eprintln!("push_proxy: accept failed: {}", e.kind());
                tokio::time::sleep(ACCEPT_BACKOFF).await;
                continue;
            }
        };
        // Full: close it now rather than queue it. Accepting first keeps it
        // out of the kernel backlog, where it would wait unbounded.
        let Ok(slot) = open.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let app = app.clone();
        tokio::spawn(async move {
            let service = service_fn(move |mut request: hyper::Request<Incoming>| {
                request
                    .extensions_mut()
                    .insert(ConnectInfo::<SocketAddr>(peer));
                app.clone().call(request)
            });
            let connection = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(limits.header_timeout)
                .serve_connection(TokioIo::new(stream), service);
            let mut connection = std::pin::pin!(connection);
            // A connection error (timeout, reset, bad request head) only ends
            // that connection; there is nothing to report without the peer.
            tokio::select! {
                _ = connection.as_mut() => {}
                () = tokio::time::sleep(limits.connection_lifetime) => {
                    connection.as_mut().graceful_shutdown();
                    let _ = tokio::time::timeout(limits.connection_grace, connection).await;
                }
            }
            drop(slot);
        });
    }
}
