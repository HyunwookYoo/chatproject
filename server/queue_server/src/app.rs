//! `/q/create`, `/q/send`, `/q/recv`, `/q/ack`, `/q/delete` (spec 9.2).
//! Routes parse and answer; [`crate::command`] decides each item; the store
//! keeps it.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequest, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Router, middleware};
use http_guard::client_ip;
use queue_wire::{ItemResult, MAX_BODY, MAX_ITEMS, QueueRequest, QueueResponse};

use crate::command::{Ack, Command, Create, Ctx, Delete, Recv, Send};
use crate::limit::{Limiter, Limits};
use crate::log::{LogSink, Logger, ResultCounts, StoreFailed};
use crate::store::Store;

/// Milliseconds since the Unix epoch.
pub type Clock = Arc<dyn Fn() -> u64 + std::marker::Send + Sync>;

pub struct AppState {
    pub store: Store,
    pub limits: Limits,
    pub clock: Clock,
    /// See [`crate::Config`] `QUEUE_TRUST_PROXY`.
    pub trust_proxy: bool,
    pub log: LogSink,
}

impl AppState {
    /// Production defaults: system clock, spec limits, peer-address IPs,
    /// log to stderr.
    pub fn new(store: Store) -> Self {
        AppState {
            store,
            limits: Limits::default(),
            clock: Arc::new(system_clock_ms),
            trust_proxy: false,
            log: crate::log::stderr_sink(),
        }
    }
}

pub(crate) fn system_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

struct Shared {
    state: AppState,
    limiter: Arc<Limiter>,
}

pub fn router(state: AppState) -> Router {
    let logger = Arc::new(Logger {
        clock: state.clock.clone(),
        sink: state.log.clone(),
    });
    let shared = Arc::new(Shared {
        limiter: Arc::new(Limiter::new(&state.limits)),
        state,
    });
    let request_timeout = shared.state.limits.request_timeout;
    Router::new()
        .route(Create::NAME.path(), post(command::<Create>))
        .route(Send::NAME.path(), post(command::<Send>))
        .route(Recv::NAME.path(), post(command::<Recv>))
        .route(Ack::NAME.path(), post(command::<Ack>))
        .route(Delete::NAME.path(), post(command::<Delete>))
        .with_state(shared)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(request_timeout, time_limit))
        .layer(middleware::from_fn_with_state(
            logger,
            crate::log::log_request,
        ))
}

/// Bounds everything after the request head: body reading and the wait for
/// the store worker included (extractors run inside `next`).
async fn time_limit(State(limit): State<Duration>, request: Request, next: Next) -> Response {
    tokio::time::timeout(limit, next.run(request))
        .await
        .unwrap_or_else(|_| StatusCode::REQUEST_TIMEOUT.into_response())
}

/// One request of command `C`: a per-request limit before the body is read,
/// then every item decoded here and run in one store transaction.
async fn command<C: Command>(State(shared): State<Arc<Shared>>, request: Request) -> Response {
    let state = &shared.state;
    let now = (state.clock)();
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| *addr);
    let client = client_ip(peer, request.headers(), state.trust_proxy);
    if !C::admit(&shared.limiter, client, now) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let body = match Bytes::from_request(request, &()).await {
        Ok(body) => body,
        Err(rejection) => return rejection.status().into_response(),
    };
    let Ok(req) = serde_json::from_slice::<QueueRequest<C::Item>>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    drop(body);
    if req.items.len() > MAX_ITEMS {
        let mut resp = StatusCode::BAD_REQUEST.into_response();
        resp.extensions_mut()
            .insert(ResultCounts::refused(req.items.len()));
        return resp;
    }
    let items: Vec<Option<C>> = req
        .items
        .into_iter()
        .map(|item| C::decode(item, now))
        .collect();
    let limiter = shared.limiter.clone();
    let max_bytes = state.limits.max_bytes;
    let ran = state
        .store
        .run(move |tx| {
            let mut ctx = Ctx::new(now, client, &limiter, max_bytes);
            items
                .into_iter()
                .map(|item| match item {
                    Some(item) => item.run(tx, &mut ctx),
                    None => Ok(ItemResult::Bad),
                })
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .await;
    match ran {
        Ok(results) => {
            let counts = ResultCounts::of(&results);
            let mut resp = json(serde_json::to_vec(&QueueResponse { results }));
            resp.extensions_mut().insert(counts);
            resp
        }
        Err(e) => {
            let mut resp = StatusCode::INTERNAL_SERVER_ERROR.into_response();
            resp.extensions_mut().insert(StoreFailed(e));
            resp
        }
    }
}

fn json(body: Result<Vec<u8>, serde_json::Error>) -> Response {
    match body {
        Ok(body) => (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            body,
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
