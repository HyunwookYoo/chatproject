//! The push proxy over HTTPS (spec 9.2): `POST <base>/handle` and `POST <base>/push` with the
//! `push_wire` JSON bodies. TLS on `ring` with the bundled web roots (no aws-lc, no platform
//! verifier: Android would need JNI set up for it). One pooled client, so one HTTP/2 connection
//! carries many requests. Every request is bounded in time and in reply size.
use std::{sync::Arc, time::Duration};

use push_wire::{HandleRequest, HandleResponse, MAX_BODY, PushRequest, PushResponse};
pub use reqwest::Url;
use serde::{Serialize, de::DeserializeOwned};

use super::{PushError, PushFuture, PushProxy, check_scheme};
use crate::{CoreError, ErrorCode};

/// One whole request (connect, send, read the reply).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct HttpPushProxy {
    handle: Url,
    push: Url,
    client: reqwest::Client,
}

impl HttpPushProxy {
    /// A client for the proxy at `base`. Release builds take `https://` only, debug builds also
    /// `http://` (a local proxy); anything else is `InvalidConfig`.
    pub fn new(base: Url) -> Result<Self, CoreError> {
        let release = !cfg!(debug_assertions);
        check_scheme(base.as_str(), !release)?;
        let invalid = || CoreError::new(ErrorCode::InvalidConfig, "push_proxy_url");
        if base.cannot_be_a_base() || base.query().is_some() || base.fragment().is_some() {
            return Err(invalid());
        }
        // `<base>/handle` keeps a path the base has (`https://host/proxy` -> `/proxy/handle`).
        let mut root = base;
        if !root.path().ends_with('/') {
            let path = format!("{}/", root.path());
            root.set_path(&path);
        }
        let handle = root.join("handle").map_err(|_| invalid())?;
        let push = root.join("push").map_err(|_| invalid())?;
        let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| CoreError::new(ErrorCode::NotAvailable, "push_tls"))?
        .with_root_certificates(rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        })
        .with_no_client_auth();
        tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let client = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .https_only(release)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|_| CoreError::new(ErrorCode::NotAvailable, "push_client"))?;
        Ok(Self {
            handle,
            push,
            client,
        })
    }
}

impl PushProxy for HttpPushProxy {
    fn handle(&self, request: HandleRequest) -> PushFuture<HandleResponse> {
        Box::pin(post(self.client.clone(), self.handle.clone(), request))
    }
    fn push(&self, request: PushRequest) -> PushFuture<PushResponse> {
        Box::pin(post(self.client.clone(), self.push.clone(), request))
    }
}

async fn post<T: Serialize, U: DeserializeOwned>(
    client: reqwest::Client,
    url: Url,
    body: T,
) -> Result<U, PushError> {
    let body = serde_json::to_vec(&body).map_err(|_| PushError::Malformed)?;
    let work = async {
        let mut response = client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(failure)?;
        let status = response.status().as_u16();
        let mut reply = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(failure)? {
            if reply.len().saturating_add(chunk.len()) > MAX_BODY {
                return Err(PushError::Malformed);
            }
            reply.extend_from_slice(&chunk);
        }
        if status != 200 {
            return Err(PushError::Status(status));
        }
        serde_json::from_slice(&reply).map_err(|_| PushError::Malformed)
    };
    // The client's own limit covers this too; the outer one also bounds the body reads.
    tokio::time::timeout(REQUEST_TIMEOUT, work)
        .await
        .unwrap_or(Err(PushError::Timeout))
}

fn failure(error: reqwest::Error) -> PushError {
    if error.is_timeout() {
        PushError::Timeout
    } else {
        PushError::Network
    }
}
