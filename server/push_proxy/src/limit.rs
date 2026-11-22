//! The proxy's rate limits (spec 9.3), on in-memory token buckets. Lost on
//! restart by design: the proxy keeps no state that outlives the process.

use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use http_guard::{Buckets, client_key};
use push_wire::Platform;
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Rates per minute and burst sizes, and the bounds on waiting and in-flight work.
/// The connection bounds are [`http_guard::ServeLimits`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    /// `/push` items per handle.
    pub handle_per_minute: u32,
    pub handle_burst: u32,
    /// `/push` items per push token, over all of its handles: `/handle` mints
    /// any number of handles for one token, so this is the device's budget.
    pub token_per_minute: u32,
    pub token_burst: u32,
    /// `/handle` requests per client: an IPv4 address or an IPv6 /64.
    pub ip_per_minute: u32,
    pub ip_burst: u32,
    /// Upstream sends waiting out their jitter or in progress, all handles
    /// together. Beyond it an item is `limited`.
    pub max_sends: usize,
    /// From the request head being read to the response: covers a body that
    /// stalls. Over it: 408.
    pub request_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            handle_per_minute: 60,
            handle_burst: 20,
            // Two handles' worth: a device with many conversations still gets
            // more than one busy conversation, never a handle-count multiple.
            token_per_minute: 120,
            token_burst: 40,
            ip_per_minute: 30,
            ip_burst: 30,
            // About 5 KB each at most (token + `e`): tens of MB in the worst case.
            max_sends: 10_000,
            request_timeout: Duration::from_secs(10),
        }
    }
}

pub(crate) struct Limiter {
    handles: Mutex<Buckets<[u8; 32]>>,
    tokens: Mutex<Buckets<[u8; 32]>>,
    ips: Mutex<Buckets<IpAddr>>,
    sends: Arc<Semaphore>,
}

impl Limiter {
    pub(crate) fn new(limits: &Limits) -> Self {
        Limiter {
            handles: Mutex::new(Buckets::new(limits.handle_per_minute, limits.handle_burst)),
            tokens: Mutex::new(Buckets::new(limits.token_per_minute, limits.token_burst)),
            ips: Mutex::new(Buckets::new(limits.ip_per_minute, limits.ip_burst)),
            sends: Arc::new(Semaphore::new(limits.max_sends.min(Semaphore::MAX_PERMITS))),
        }
    }

    /// A slot for one upstream send, held until the send ends. Never waits.
    pub(crate) fn send_permit(&self) -> Option<OwnedSemaphorePermit> {
        self.sends.clone().try_acquire_owned().ok()
    }

    /// Keyed by SHA-256 of the handle bytes, so the map holds no handle.
    pub(crate) fn allow_handle(&self, handle: &[u8], now_ms: u64) -> bool {
        let key: [u8; 32] = Sha256::digest(handle).into();
        self.handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(key, now_ms)
    }

    /// Keyed by SHA-256 of the platform and the token, so the map holds no token.
    pub(crate) fn allow_token(&self, platform: Platform, token: &str, now_ms: u64) -> bool {
        let mut hash = Sha256::new();
        hash.update(match platform {
            Platform::Fcm => b"f",
            Platform::Apns => b"a",
        });
        hash.update(token.as_bytes());
        self.tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(hash.finalize().into(), now_ms)
    }

    pub(crate) fn allow_ip(&self, ip: IpAddr, now_ms: u64) -> bool {
        self.ips
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(client_key(ip), now_ms)
    }
}
