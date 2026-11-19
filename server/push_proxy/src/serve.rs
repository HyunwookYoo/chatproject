//! Plain HTTP/1 serving with a bound on reading the request head.
//!
//! `axum::serve` gives hyper no timer, so hyper's header-read timeout never
//! fires there and a client that never finishes its headers holds the
//! connection forever. This loop sets the timer; the rest of the request is
//! bounded by `Limits::request_timeout` inside the router.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::extract::ConnectInfo;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tower_service::Service;

/// Pause after a failed `accept` (for example out of file descriptors).
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Serves `app` on `listener` until the process ends. Each request carries
/// `ConnectInfo<SocketAddr>` of its TCP peer.
pub async fn serve(listener: TcpListener, app: Router, header_timeout: Duration) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                eprintln!("push_proxy: accept failed: {}", e.kind());
                tokio::time::sleep(ACCEPT_BACKOFF).await;
                continue;
            }
        };
        let app = app.clone();
        tokio::spawn(async move {
            let service = service_fn(move |mut request: hyper::Request<Incoming>| {
                request
                    .extensions_mut()
                    .insert(ConnectInfo::<SocketAddr>(peer));
                app.clone().call(request)
            });
            // A connection error (timeout, reset, bad request head) only ends
            // that connection; there is nothing to report without the peer.
            let _ = http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(header_timeout)
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}
