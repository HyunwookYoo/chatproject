//! `/handle` and `/push` (spec 9.2). Routes decide; upstreams deliver.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Router, middleware};
use push_wire::{
    BASE64URL_NOPAD, HANDLE_LIFETIME_MS, HandleError, HandlePlain, HandleRequest, HandleResponse,
    MAX_BODY, MAX_ITEMS, MAX_TOKEN, PUSH_CIPHER_MAX, Platform, PushItem, PushItemKind, PushRequest,
    PushResponse, PushResult, Tag, open_handle, seal_handle,
};

use tokio::sync::OwnedSemaphorePermit;

use crate::limit::{Limiter, Limits};
use crate::log::{LogSink, Logger, PushCounts};
use crate::upstream::{Delivery, DeliveryKind, Upstream, UpstreamResult};

/// Upper end of the uniform send delay (spec 9.2).
const JITTER_MAX_MS: u32 = 3_000;
/// Bound on one upstream send; the real senders have their own, shorter ones.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(30);

pub struct AppState {
    /// Every key a handle may be sealed with; `current` seals new ones.
    pub keys: Vec<(u8, [u8; 32])>,
    pub current: u8,
    pub fcm: Arc<dyn Upstream>,
    /// `None`: APNs handles are refused at `/handle` and `bad` at `/push`.
    pub apns: Option<Arc<dyn Upstream>>,
    pub limits: Limits,
    /// Milliseconds since the Unix epoch.
    pub clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    /// Wait before each upstream send.
    pub jitter: Arc<dyn Fn() -> Duration + Send + Sync>,
    /// See [`crate::Config`] `PUSH_TRUST_PROXY`.
    pub trust_proxy: bool,
    pub log: LogSink,
}

impl AppState {
    /// Production defaults: system clock, uniform 0–3,000 ms jitter, spec
    /// limits, no APNs, peer-address IPs, log to stderr.
    pub fn new(keys: Vec<(u8, [u8; 32])>, current: u8, fcm: Arc<dyn Upstream>) -> Self {
        AppState {
            keys,
            current,
            fcm,
            apns: None,
            limits: Limits::default(),
            clock: Arc::new(system_clock_ms),
            jitter: Arc::new(uniform_jitter),
            trust_proxy: false,
            log: crate::log::stderr_sink(),
        }
    }

    fn upstream(&self, platform: Platform) -> Option<&Arc<dyn Upstream>> {
        match platform {
            Platform::Fcm => Some(&self.fcm),
            Platform::Apns => self.apns.as_ref(),
        }
    }
}

pub(crate) fn system_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn uniform_jitter() -> Duration {
    // A failing OS random source falls back to the longest delay, never to none.
    let ms = getrandom::u32().map_or(JITTER_MAX_MS, |r| r % (JITTER_MAX_MS + 1));
    Duration::from_millis(u64::from(ms))
}

struct Shared {
    state: AppState,
    limiter: Limiter,
    logger: Arc<Logger>,
}

pub fn router(state: AppState) -> Router {
    let logger = Arc::new(Logger {
        clock: state.clock.clone(),
        sink: state.log.clone(),
    });
    let shared = Arc::new(Shared {
        limiter: Limiter::new(&state.limits),
        state,
        logger: logger.clone(),
    });
    let request_timeout = shared.state.limits.request_timeout;
    Router::new()
        .route("/handle", post(handle))
        .route("/push", post(push))
        .with_state(shared)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(request_timeout, time_limit))
        .layer(middleware::from_fn_with_state(
            logger,
            crate::log::log_request,
        ))
}

/// Bounds everything after the request head, body reading included
/// (extractors run inside `next`).
async fn time_limit(State(limit): State<Duration>, request: Request, next: Next) -> Response {
    tokio::time::timeout(limit, next.run(request))
        .await
        .unwrap_or_else(|_| StatusCode::REQUEST_TIMEOUT.into_response())
}

async fn handle(
    State(shared): State<Arc<Shared>>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let state = &shared.state;
    let now = (state.clock)();
    let peer = peer.map(|Extension(ConnectInfo(addr))| addr);
    let ip = client_ip(peer, &headers, state.trust_proxy);
    if !shared.limiter.allow_ip(ip, now) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let body = match body {
        Ok(body) => body,
        Err(rejection) => return rejection.status().into_response(),
    };
    match issue(state, &body, now) {
        Ok(resp) => json(serde_json::to_vec(&resp)),
        Err(status) => status.into_response(),
    }
}

/// The client for the `/handle` limit. Behind a trusted reverse proxy that
/// appends the peer it saw, the last `X-Forwarded-For` entry; else the TCP peer.
fn client_ip(peer: Option<SocketAddr>, headers: &HeaderMap, trust_proxy: bool) -> IpAddr {
    let forwarded = || {
        let value = headers.get_all("x-forwarded-for").iter().next_back()?;
        value.to_str().ok()?.rsplit(',').next()?.trim().parse().ok()
    };
    if trust_proxy && let Some(ip) = forwarded() {
        return ip;
    }
    // No peer only happens in-process (tests, fakes): they share one bucket.
    peer.map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |addr| addr.ip())
}

fn issue(state: &AppState, body: &[u8], now: u64) -> Result<HandleResponse, StatusCode> {
    let req: HandleRequest = serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST)?;
    let tag: Tag = BASE64URL_NOPAD
        .decode(req.tag.as_bytes())
        .ok()
        .and_then(|v| Tag::try_from(v).ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    if req.token.is_empty() || req.token.len() > MAX_TOKEN || state.upstream(req.platform).is_none()
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let key = state
        .keys
        .iter()
        .find(|(id, _)| *id == state.current)
        .map(|(_, key)| key)
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let expires_at_ms = now.saturating_add(HANDLE_LIFETIME_MS);
    let plain = HandlePlain {
        platform: req.platform,
        token: req.token,
        tag,
        issued_at_ms: now,
        expires_at_ms,
    };
    let handle = seal_handle(key, state.current, &plain).map_err(|e| match e {
        HandleError::TokenTooLong => StatusCode::BAD_REQUEST,
        // Random (Ruling R7) and anything unexpected: no detail.
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    })?;
    Ok(HandleResponse {
        handle: BASE64URL_NOPAD.encode(&handle),
        expires_at_ms,
    })
}

async fn push(State(shared): State<Arc<Shared>>, body: Result<Bytes, BytesRejection>) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(rejection) => return rejection.status().into_response(),
    };
    let Ok(req) = serde_json::from_slice::<PushRequest>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    drop(body);
    if req.items.len() > MAX_ITEMS {
        let mut resp = StatusCode::BAD_REQUEST.into_response();
        resp.extensions_mut()
            .insert(PushCounts::refused(req.items.len()));
        return resp;
    }
    let now = (shared.state.clock)();
    let mut results = Vec::with_capacity(req.items.len());
    let mut sends = Vec::new();
    for item in req.items {
        match decide(&shared, item, now) {
            Ok(send) => {
                results.push(PushResult::Ok);
                sends.push(send);
            }
            Err(result) => results.push(result),
        }
    }
    // Answer now; each send waits out its own jitter in the background.
    for send in sends {
        spawn_send(&shared, send);
    }
    let counts = PushCounts::of(&results);
    let mut resp = json(serde_json::to_vec(&PushResponse { results }));
    resp.extensions_mut().insert(counts);
    resp
}

/// One accepted item: where it goes and the in-flight slot it holds.
struct Outgoing {
    upstream: Arc<dyn Upstream>,
    delivery: Delivery,
    permit: OwnedSemaphorePermit,
}

/// The result of one item: what to send where, or why not.
fn decide(shared: &Shared, item: PushItem, now: u64) -> Result<Outgoing, PushResult> {
    let state = &shared.state;
    let bytes = BASE64URL_NOPAD
        .decode(item.handle.as_bytes())
        .map_err(|_| PushResult::Gone)?;
    let plain = open_handle(&state.keys, &bytes).map_err(|_| PushResult::Gone)?;
    if now >= plain.expires_at_ms {
        return Err(PushResult::Gone);
    }
    let upstream = state.upstream(plain.platform).ok_or(PushResult::Bad)?;
    let kind = match item.kind {
        PushItemKind::Cipher { e } if cipher_fits(&e) => DeliveryKind::Cipher(e),
        PushItemKind::Cipher { .. } => return Err(PushResult::Bad),
        PushItemKind::Wake => DeliveryKind::Wake,
        // Read clearing is Android only (plan 2, proxy payload table).
        PushItemKind::Read if plain.platform == Platform::Apns => return Err(PushResult::Bad),
        PushItemKind::Read => DeliveryKind::Read,
    };
    // The slot first: a full proxy must not spend the handle's token.
    let permit = shared.limiter.send_permit().ok_or(PushResult::Limited)?;
    if !shared.limiter.allow_handle(&bytes, now) {
        return Err(PushResult::Limited);
    }
    Ok(Outgoing {
        upstream: upstream.clone(),
        delivery: Delivery {
            platform: plain.platform,
            token: plain.token,
            tag: plain.tag,
            kind,
        },
        permit,
    })
}

/// `e` is base64url of 1..=PUSH_CIPHER_MAX bytes. A payload is `e` plus a
/// fixed-size envelope, and at this bound the APNs body and the FCM data
/// payload are each at most MAX_PAYLOAD (`payload_with_max_cipher_fits_4096`
/// in tests/upstream.rs). The senders still measure what they build and fail
/// a send over MAX_PAYLOAD rather than send it.
fn cipher_fits(e: &str) -> bool {
    e.len() <= BASE64URL_NOPAD.encode_len(PUSH_CIPHER_MAX)
        && BASE64URL_NOPAD
            .decode(e.as_bytes())
            .is_ok_and(|bytes| !bytes.is_empty())
}

fn spawn_send(shared: &Shared, send: Outgoing) {
    let wait = (shared.state.jitter)();
    let logger = shared.logger.clone();
    tokio::spawn(async move {
        let Outgoing {
            upstream,
            delivery,
            permit,
        } = send;
        tokio::time::sleep(wait).await;
        let start = Instant::now();
        let result = tokio::time::timeout(UPSTREAM_TIMEOUT, upstream.send(delivery))
            .await
            .unwrap_or(UpstreamResult::Failed);
        drop(permit);
        logger.sent(result, start.elapsed());
    });
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
