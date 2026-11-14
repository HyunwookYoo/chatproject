//! FCM HTTP v1 sender (spec 9.2; payloads: plan 2, "프록시가 만드는 페이로드").
//!
//! The OAuth access token from the service account lives in memory only and is
//! reused until 60 s before it expires. One refresh runs at a time: concurrent
//! sends wait for it instead of each asking Google.
//!
//! Results: 2xx `Sent`; 404 whose FCM error code is `UNREGISTERED` `TokenGone`;
//! everything else `Failed`. A 400 `INVALID_ARGUMENT` stays `Failed`: FCM uses
//! it for a malformed token but also for a bad message, and counting a proxy
//! fault as a dead token would hide it in the log.

use std::sync::Arc;

use futures_core::future::BoxFuture;
use jsonwebtoken::{Algorithm, Header};
use push_wire::{BASE64URL_NOPAD, MAX_PAYLOAD};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use reqwest::{StatusCode, Url};
use serde_json::{Value, json};

use crate::jwt::JwtKey;
use crate::upstream::{
    Clock, Delivery, DeliveryKind, STORE_SECS, SenderError, Upstream, UpstreamResult, https_client,
    read_reply,
};

/// FCM API root; messages go to `v1/projects/<project_id>/messages:send` under it.
pub const FCM_ENDPOINT: &str = "https://fcm.googleapis.com/";
/// Google's OAuth 2.0 token endpoint, also the assertion's audience.
pub const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

const SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
/// Google accepts assertions of at most an hour.
const ASSERTION_SECS: u64 = 3_600;
const REFRESH_BEFORE_MS: u64 = 60_000;
const GRANT: &str = "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion=";

pub struct FcmUpstream {
    inner: Arc<Inner>,
    clock: Clock,
}

struct Inner {
    http: reqwest::Client,
    send_url: Url,
    token_url: Url,
    client_email: String,
    key_id: Option<String>,
    key: JwtKey,
    token: tokio::sync::Mutex<Option<AccessToken>>,
}

struct AccessToken {
    bearer: HeaderValue,
    fetched_at_ms: u64,
    refresh_at_ms: u64,
}

impl FcmUpstream {
    /// `service_account_json`: the Firebase service account key file's text.
    /// `endpoint`: [`FCM_ENDPOINT`]. `token_url`: [`GOOGLE_TOKEN_URL`].
    pub fn new(
        service_account_json: &str,
        endpoint: Url,
        token_url: Url,
    ) -> Result<Self, SenderError> {
        Self::build(service_account_json, endpoint, token_url, None)
    }

    /// [`Self::new`], trusting only the DER certificate `root` instead of the
    /// platform's roots.
    pub fn with_root(
        service_account_json: &str,
        endpoint: Url,
        token_url: Url,
        root: &[u8],
    ) -> Result<Self, SenderError> {
        Self::build(service_account_json, endpoint, token_url, Some(root))
    }

    /// Replaces the system clock (token and assertion times).
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    fn build(
        service_account_json: &str,
        endpoint: Url,
        token_url: Url,
        root: Option<&[u8]>,
    ) -> Result<Self, SenderError> {
        let account: Value = serde_json::from_str(service_account_json)
            .map_err(|_| SenderError::Invalid("service account JSON"))?;
        let field = |name: &'static str| {
            account
                .get(name)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .ok_or(SenderError::Invalid(name))
        };
        let project_id = field("project_id")?;
        let client_email = field("client_email")?.to_owned();
        let key =
            JwtKey::rsa_pem(field("private_key")?).ok_or(SenderError::Invalid("private_key"))?;
        let key_id = field("private_key_id").ok().map(str::to_owned);
        let mut send_url = endpoint;
        send_url
            .path_segments_mut()
            .map_err(|_| SenderError::Invalid("FCM endpoint"))?
            .pop_if_empty()
            .extend(["v1", "projects", project_id, "messages:send"]);
        if token_url.cannot_be_a_base() {
            return Err(SenderError::Invalid("token URL"));
        }
        Ok(FcmUpstream {
            inner: Arc::new(Inner {
                http: https_client(&[b"h2", b"http/1.1"], root)?,
                send_url,
                token_url,
                client_email,
                key_id,
                key,
                token: tokio::sync::Mutex::new(None),
            }),
            clock: Arc::new(crate::app::system_clock_ms),
        })
    }
}

impl Upstream for FcmUpstream {
    fn send(&self, delivery: Delivery) -> BoxFuture<'static, UpstreamResult> {
        let (inner, clock) = (self.inner.clone(), self.clock.clone());
        Box::pin(async move { inner.deliver(&delivery, &clock).await })
    }
}

impl Inner {
    async fn deliver(&self, delivery: &Delivery, clock: &Clock) -> UpstreamResult {
        let Some(body) = message(delivery) else {
            return UpstreamResult::Failed;
        };
        let Some(bearer) = self.bearer(clock).await else {
            return UpstreamResult::Failed;
        };
        let sent = self
            .http
            .post(self.send_url.clone())
            .header(AUTHORIZATION, bearer)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await;
        let Ok(response) = sent else {
            return UpstreamResult::Failed;
        };
        let status = response.status();
        let reply = read_reply(response).await;
        if status.is_success() {
            UpstreamResult::Sent
        } else if status == StatusCode::NOT_FOUND && reply.is_some_and(|r| unregistered(&r)) {
            UpstreamResult::TokenGone
        } else {
            UpstreamResult::Failed
        }
    }

    /// The cached token, or a fresh one. The lock is held across the refresh,
    /// so concurrent sends make one token request between them.
    async fn bearer(&self, clock: &Clock) -> Option<HeaderValue> {
        let mut cached = self.token.lock().await;
        let now = clock();
        if let Some(token) = cached
            .as_ref()
            .filter(|t| (t.fetched_at_ms..t.refresh_at_ms).contains(&now))
        {
            return Some(token.bearer.clone());
        }
        let fresh = self.fetch_token(now).await?;
        let bearer = fresh.bearer.clone();
        *cached = Some(fresh);
        Some(bearer)
    }

    /// Service account JWT bearer grant (RFC 7523).
    async fn fetch_token(&self, now_ms: u64) -> Option<AccessToken> {
        let iat = now_ms / 1000;
        let mut header = Header::new(Algorithm::RS256);
        header.kid = self.key_id.clone();
        let claims = json!({
            "iss": self.client_email,
            "scope": SCOPE,
            "aud": self.token_url.as_str(),
            "iat": iat,
            "exp": iat.saturating_add(ASSERTION_SECS),
        });
        let assertion = self.key.sign(header, &claims)?;
        let response = self
            .http
            .post(self.token_url.clone())
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(format!("{GRANT}{assertion}"))
            .send()
            .await
            .ok()?;
        let ok = response.status().is_success();
        let reply = read_reply(response).await?;
        if !ok {
            return None;
        }
        let reply: Value = serde_json::from_slice(&reply).ok()?;
        let access = reply.get("access_token")?.as_str()?;
        let lifetime_ms = reply.get("expires_in")?.as_u64()?.saturating_mul(1000);
        let mut bearer = HeaderValue::from_str(&format!("Bearer {access}")).ok()?;
        bearer.set_sensitive(true);
        Some(AccessToken {
            bearer,
            fetched_at_ms: now_ms,
            refresh_at_ms: now_ms
                .saturating_add(lifetime_ms)
                .saturating_sub(REFRESH_BEFORE_MS),
        })
    }
}

/// The `messages:send` body, or `None` if its data payload is over
/// [`MAX_PAYLOAD`]: FCM counts the data, not the token or options, against
/// its limit.
fn message(delivery: &Delivery) -> Option<Vec<u8>> {
    let t = BASE64URL_NOPAD.encode(&delivery.tag);
    let (priority, data) = match &delivery.kind {
        DeliveryKind::Cipher(e) => ("HIGH", json!({"t": t, "e": e})),
        DeliveryKind::Wake => ("HIGH", json!({"t": t, "w": "1"})),
        DeliveryKind::Read => ("NORMAL", json!({"t": t, "r": "1"})),
    };
    if serde_json::to_vec(&data).ok()?.len() > MAX_PAYLOAD {
        return None;
    }
    serde_json::to_vec(&json!({"message": {
        "token": delivery.token,
        "android": {"priority": priority, "ttl": format!("{STORE_SECS}s")},
        "data": data,
    }}))
    .ok()
}

/// FCM v1 error body: `error.details[].errorCode == "UNREGISTERED"`.
fn unregistered(reply: &[u8]) -> bool {
    let Ok(reply) = serde_json::from_slice::<Value>(reply) else {
        return false;
    };
    reply
        .pointer("/error/details")
        .and_then(Value::as_array)
        .is_some_and(|details| {
            details
                .iter()
                .any(|d| d.get("errorCode").and_then(Value::as_str) == Some("UNREGISTERED"))
        })
}
