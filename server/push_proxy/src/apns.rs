//! APNs sender: HTTP/2 only (ALPN `h2`), token-based auth (spec 9.2; payloads:
//! plan 2, "프록시가 만드는 페이로드").
//!
//! The ES256 provider token lives in memory only and is reused for 50 minutes
//! (Apple: refresh no more than once every 20 minutes, use none older than 60).
//! Results: 200 `Sent`, 410 `TokenGone`, anything else `Failed`. `read` is
//! Android only and never sent.
//!
//! From `spikes/c-push/apns-probe`, with its M1b watch-item 7 flaws fixed: an
//! oversize payload fails the send instead of going out, times are `u64` with
//! saturating sums, and the tests check that `e` and `t` survive the trip.

use std::sync::{Arc, Mutex, PoisonError};

use futures_core::future::BoxFuture;
use jsonwebtoken::{Algorithm, Header};
use push_wire::{BASE64URL_NOPAD, MAX_PAYLOAD};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use reqwest::{StatusCode, Url};
use serde_json::json;

use crate::jwt::JwtKey;
use crate::upstream::{
    Clock, Delivery, DeliveryKind, STORE_SECS, SenderError, Upstream, UpstreamResult, https_client,
    read_reply,
};

pub const APNS_PRODUCTION: &str = "https://api.push.apple.com";
/// Development builds (Xcode debug) only; TestFlight uses production.
pub const APNS_SANDBOX: &str = "https://api.sandbox.push.apple.com";

const JWT_REUSE_MS: u64 = 50 * 60 * 1000;

pub struct ApnsUpstream {
    inner: Arc<Inner>,
    clock: Clock,
}

struct Inner {
    http: reqwest::Client,
    endpoint: Url,
    topic: HeaderValue,
    key_id: String,
    team_id: String,
    key: JwtKey,
    jwt: Mutex<Option<ProviderToken>>,
}

struct ProviderToken {
    bearer: HeaderValue,
    minted_at_ms: u64,
}

impl ApnsUpstream {
    /// `p8_pem`: the APNs auth key file's text. `topic`: the app's bundle id.
    /// `endpoint`: [`APNS_PRODUCTION`] or [`APNS_SANDBOX`].
    pub fn new(
        p8_pem: &str,
        key_id: &str,
        team_id: &str,
        topic: &str,
        endpoint: Url,
    ) -> Result<Self, SenderError> {
        Self::build(p8_pem, key_id, team_id, topic, endpoint, None)
    }

    /// [`Self::new`], trusting only the DER certificate `root` instead of the
    /// platform's roots.
    pub fn with_root(
        p8_pem: &str,
        key_id: &str,
        team_id: &str,
        topic: &str,
        endpoint: Url,
        root: &[u8],
    ) -> Result<Self, SenderError> {
        Self::build(p8_pem, key_id, team_id, topic, endpoint, Some(root))
    }

    /// Replaces the system clock (token `iat`, `apns-expiration`).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    fn build(
        p8_pem: &str,
        key_id: &str,
        team_id: &str,
        topic: &str,
        endpoint: Url,
        root: Option<&[u8]>,
    ) -> Result<Self, SenderError> {
        let key = JwtKey::p256_pem(p8_pem).ok_or(SenderError::Invalid("APNs key"))?;
        let topic = HeaderValue::from_str(topic).map_err(|_| SenderError::Invalid("APNs topic"))?;
        if endpoint.cannot_be_a_base() {
            return Err(SenderError::Invalid("APNs endpoint"));
        }
        Ok(ApnsUpstream {
            inner: Arc::new(Inner {
                http: https_client(&[b"h2"], root)?,
                endpoint,
                topic,
                key_id: key_id.to_owned(),
                team_id: team_id.to_owned(),
                key,
                jwt: Mutex::new(None),
            }),
            clock: Arc::new(crate::app::system_clock_ms),
        })
    }
}

impl Upstream for ApnsUpstream {
    fn send(&self, delivery: Delivery) -> BoxFuture<'static, UpstreamResult> {
        let (inner, clock) = (self.inner.clone(), self.clock.clone());
        Box::pin(async move { inner.deliver(&delivery, clock()).await })
    }
}

impl Inner {
    async fn deliver(&self, delivery: &Delivery, now_ms: u64) -> UpstreamResult {
        let Some(body) = payload(delivery) else {
            return UpstreamResult::Failed;
        };
        let Some(bearer) = self.bearer(now_ms) else {
            return UpstreamResult::Failed;
        };
        let mut url = self.endpoint.clone();
        // Each value is one percent-encoded segment: a token cannot reach
        // another path.
        let Ok(mut path) = url.path_segments_mut() else {
            return UpstreamResult::Failed;
        };
        path.pop_if_empty()
            .extend(["3", "device", delivery.token.as_str()]);
        drop(path);
        let expiration = (now_ms / 1000).saturating_add(STORE_SECS);
        let sent = self
            .http
            .post(url)
            .header(AUTHORIZATION, bearer)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .header("apns-topic", self.topic.clone())
            .header("apns-expiration", expiration)
            .body(body)
            .send()
            .await;
        let Ok(response) = sent else {
            return UpstreamResult::Failed;
        };
        let status = response.status();
        let _ = read_reply(response).await;
        match status {
            StatusCode::OK => UpstreamResult::Sent,
            StatusCode::GONE => UpstreamResult::TokenGone,
            _ => UpstreamResult::Failed,
        }
    }

    /// The provider token minted in the last 50 minutes, or a new one. Signing
    /// is quick and local, so it happens under the lock.
    fn bearer(&self, now_ms: u64) -> Option<HeaderValue> {
        let mut cached = self.jwt.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(token) = cached.as_ref().filter(|t| {
            (t.minted_at_ms..t.minted_at_ms.saturating_add(JWT_REUSE_MS)).contains(&now_ms)
        }) {
            return Some(token.bearer.clone());
        }
        let mut header = Header::new(Algorithm::ES256);
        // Apple documents `alg` and `kid` only.
        header.typ = None;
        header.kid = Some(self.key_id.clone());
        let jwt = self
            .key
            .sign(header, &json!({"iss": self.team_id, "iat": now_ms / 1000}))?;
        let mut bearer = HeaderValue::from_str(&format!("bearer {jwt}")).ok()?;
        bearer.set_sensitive(true);
        *cached = Some(ProviderToken {
            bearer: bearer.clone(),
            minted_at_ms: now_ms,
        });
        Some(bearer)
    }
}

/// The notification body, or `None` for `read` (Android only) and for a body
/// over [`MAX_PAYLOAD`]. Custom keys are peers of `aps`.
fn payload(delivery: &Delivery) -> Option<Vec<u8>> {
    let aps = json!({"mutable-content": 1, "alert": {"loc-key": "NEW_MSG"}});
    let t = BASE64URL_NOPAD.encode(&delivery.tag);
    let body = match &delivery.kind {
        DeliveryKind::Cipher(e) => json!({"aps": aps, "t": t, "e": e}),
        DeliveryKind::Wake => json!({"aps": aps, "t": t}),
        DeliveryKind::Read => return None,
    };
    serde_json::to_vec(&body)
        .ok()
        .filter(|b| b.len() <= MAX_PAYLOAD)
}
