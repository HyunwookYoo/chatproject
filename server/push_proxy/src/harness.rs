//! Debug-only upstream for the m7 end-to-end scenario (cargo feature `fake-upstream`, Ruling
//! R6): every [`Delivery`] is POSTed as one JSON object to the test harness on this machine
//! instead of FCM or APNs. The harness hands it to the emulator's debug push receiver.
//!
//! It keeps nothing and logs nothing. The URL must be `http://` on a loopback address, so a
//! delivery never leaves the machine. Body:
//! `{"platform":"fcm"|"apns","t":"<base64url tag>","kind":"cipher"|"wake"|"read"[,"e":"..."]}`.
//! The token stays here: the harness has one device and does not need it.

#[cfg(not(debug_assertions))]
compile_error!("the fake-upstream feature is for debug builds only");

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use futures_core::future::BoxFuture;
use push_wire::BASE64URL_NOPAD;
use reqwest::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::upstream::{Delivery, DeliveryKind, SenderError, Upstream, UpstreamResult};

/// One delivery to the harness: connect, write, read the status line.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest status line read back; the harness answers with a bare status.
const MAX_STATUS: usize = 256;

pub struct HarnessUpstream {
    addr: SocketAddr,
    path: String,
}

impl HarnessUpstream {
    /// `url`: `http://127.0.0.1:<port>/<path>` or `http://[::1]:<port>/<path>`.
    pub fn new(url: &str) -> Result<Self, SenderError> {
        const WHAT: &str = "fake upstream URL";
        let url = Url::parse(url).map_err(|_| SenderError::Invalid(WHAT))?;
        let ip: IpAddr = url
            .host_str()
            .map(|host| host.trim_start_matches('[').trim_end_matches(']'))
            .and_then(|host| host.parse().ok())
            .ok_or(SenderError::Invalid(WHAT))?;
        if url.scheme() != "http" || !ip.is_loopback() || url.query().is_some() {
            return Err(SenderError::Invalid(WHAT));
        }
        let port = url.port().ok_or(SenderError::Invalid(WHAT))?;
        Ok(HarnessUpstream {
            addr: SocketAddr::new(ip, port),
            path: url.path().to_owned(),
        })
    }
}

fn body(delivery: &Delivery) -> Vec<u8> {
    let mut value = serde_json::json!({
        "platform": delivery.platform,
        "t": BASE64URL_NOPAD.encode(&delivery.tag),
    });
    let (kind, e) = match &delivery.kind {
        DeliveryKind::Cipher(e) => ("cipher", Some(e)),
        DeliveryKind::Wake => ("wake", None),
        DeliveryKind::Read => ("read", None),
    };
    value["kind"] = kind.into();
    if let Some(e) = e {
        value["e"] = e.as_str().into();
    }
    value.to_string().into_bytes()
}

async fn exchange(addr: SocketAddr, request: Vec<u8>) -> Option<bool> {
    let mut stream = tokio::net::TcpStream::connect(addr).await.ok()?;
    stream.write_all(&request).await.ok()?;
    let mut reply = Vec::new();
    let mut chunk = [0u8; 64];
    while !reply.contains(&b'\n') && reply.len() < MAX_STATUS {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        reply.extend_from_slice(&chunk[..n]);
    }
    let line = reply.split(|b| *b == b'\n').next()?;
    let status = line.split(|b| *b == b' ').nth(1)?;
    Some(status.first() == Some(&b'2') && status.len() == 3)
}

impl Upstream for HarnessUpstream {
    fn send(&self, delivery: Delivery) -> BoxFuture<'static, UpstreamResult> {
        let body = body(&delivery);
        let mut request = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.path,
            self.addr,
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(&body);
        let addr = self.addr;
        Box::pin(async move {
            match tokio::time::timeout(EXCHANGE_TIMEOUT, exchange(addr, request)).await {
                Ok(Some(true)) => UpstreamResult::Sent,
                _ => UpstreamResult::Failed,
            }
        })
    }
}
