//! What the proxy hands to FCM or APNs, and the seam the senders plug into.
//!
//! Routes decide; an [`Upstream`] only delivers. Each platform is one
//! implementation, so a new platform adds an implementation, not a branch.
//! The HTTP plumbing the real senders share ([`crate::FcmUpstream`],
//! [`crate::ApnsUpstream`]) is here too.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_core::future::BoxFuture;
use push_wire::{Platform, Tag};
use rustls::RootCertStore;
use rustls::pki_types::CertificateDer;
use rustls_platform_verifier::BuilderVerifierExt;

/// One push to one device, built from an opened handle and a request item.
#[derive(Clone, PartialEq, Eq)]
pub struct Delivery {
    pub platform: Platform,
    pub token: String,
    pub tag: Tag,
    pub kind: DeliveryKind,
}

#[derive(Clone, PartialEq, Eq)]
pub enum DeliveryKind {
    /// `e` as the sender gave it: base64url of the stripped MLS ciphertext.
    Cipher(String),
    Wake,
    Read,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UpstreamResult {
    Sent,
    /// FCM `UNREGISTERED` / APNs 410. Only counted in the log (spec 9.2).
    TokenGone,
    Failed,
}

pub trait Upstream: Send + Sync {
    fn send(&self, delivery: Delivery) -> BoxFuture<'static, UpstreamResult>;
}

/// Records every delivery in memory and answers a fixed result.
pub struct FakeUpstream {
    received: Mutex<Vec<Delivery>>,
    result: UpstreamResult,
}

impl FakeUpstream {
    pub fn new() -> Self {
        Self::answering(UpstreamResult::Sent)
    }

    pub fn answering(result: UpstreamResult) -> Self {
        FakeUpstream {
            received: Mutex::new(Vec::new()),
            result,
        }
    }

    /// Deliveries received so far, in arrival order.
    pub fn deliveries(&self) -> Vec<Delivery> {
        self.received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Default for FakeUpstream {
    fn default() -> Self {
        Self::new()
    }
}

impl Upstream for FakeUpstream {
    fn send(&self, delivery: Delivery) -> BoxFuture<'static, UpstreamResult> {
        self.received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(delivery);
        let result = self.result;
        Box::pin(async move { result })
    }
}

/// Milliseconds since the Unix epoch, as [`crate::AppState::clock`].
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// How long FCM and APNs keep a push for an offline device: 28 days (plan 2).
pub(crate) const STORE_SECS: u64 = 2_419_200;
/// One HTTP exchange. A send makes at most two (OAuth token, then the
/// message), both inside the route's own 30 s bound.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest reply body read from FCM, Google OAuth or APNs. Theirs are a few
/// hundred bytes; anything longer is treated as a failed exchange.
const MAX_REPLY: usize = 16_384;

/// A sender that cannot be built. Names what is wrong, never its value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SenderError {
    /// A missing or malformed secret or setting: a service account field, a
    /// key, a header value, an endpoint.
    Invalid(&'static str),
    /// The TLS client could not be set up (e.g. no platform roots).
    Client,
}

impl fmt::Display for SenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SenderError::Invalid(what) => write!(f, "{what} is invalid"),
            SenderError::Client => f.write_str("the HTTPS client could not be built"),
        }
    }
}

impl std::error::Error for SenderError {}

/// One reusable HTTPS client: TLS on `ring`, ALPN `alpn` in order of
/// preference, the platform's roots or, with `root`, only that DER certificate.
/// Connections are pooled, so an HTTP/2 connection carries many sends.
pub(crate) fn https_client(
    alpn: &[&[u8]],
    root: Option<&[u8]>,
) -> Result<reqwest::Client, SenderError> {
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| SenderError::Client)?;
    let builder = match root {
        None => builder
            .with_platform_verifier()
            .map_err(|_| SenderError::Client)?,
        Some(der) => {
            let mut roots = RootCertStore::empty();
            roots
                .add(CertificateDer::from(der.to_vec()))
                .map_err(|_| SenderError::Invalid("root certificate"))?;
            builder.with_root_certificates(roots)
        }
    };
    let mut tls = builder.with_no_client_auth();
    tls.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|_| SenderError::Client)
}

/// The whole reply body, or `None` past [`MAX_REPLY`] or on a broken read.
/// Reading to the end also leaves an HTTP/1.1 connection reusable.
pub(crate) async fn read_reply(mut response: reqwest::Response) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len().saturating_add(chunk.len()) > MAX_REPLY {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Debug for Delivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Delivery")
            .field("platform", &self.platform)
            .field("token", &Redacted)
            .field("tag", &Redacted)
            .field("kind", &self.kind)
            .finish()
    }
}

impl fmt::Debug for DeliveryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeliveryKind::Cipher(_) => f.debug_tuple("Cipher").field(&Redacted).finish(),
            DeliveryKind::Wake => f.write_str("Wake"),
            DeliveryKind::Read => f.write_str("Read"),
        }
    }
}
