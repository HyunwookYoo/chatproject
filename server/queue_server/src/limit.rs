//! The server's rate limits (spec 9.4) and size cap (spec 9.3), on in-memory
//! token buckets. Buckets are lost on restart by design; the stored entries
//! are what the cap counts.

use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use http_guard::{Buckets, ServeLimits, client_key};
use queue_wire::QueueId;

/// Open connections: the bound on memory across requests. A SEND upload near
/// `MAX_BODY` (1 MiB) held open measured 1.73 MiB of private memory per
/// connection (500 held: 2,948 KiB idle, 888,932 KiB held). 128 × 1.73 MiB
/// ≈ 221 MiB, under the ~256 MB this server may take on the 952 MB VM it
/// shares with push_proxy. A RECV answer is at most `MAX_BODY`, below that
/// per-connection cost.
pub const MAX_CONNECTIONS: usize = 128;

/// The connection bounds [`http_guard::serve`] runs this server with.
pub fn serve_limits() -> ServeLimits {
    ServeLimits {
        max_connections: MAX_CONNECTIONS,
        ..ServeLimits::default()
    }
}

/// Rates and bursts, the size cap and the request bound. The connection
/// bounds are [`serve_limits`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    /// `CREATE` items per client (an IPv4 address or an IPv6 /64), per hour.
    pub create_per_hour: u32,
    pub create_burst: u32,
    /// `SEND` items per queue.
    pub send_per_minute: u32,
    pub send_burst: u32,
    /// `RECV` requests per client; `ACK` requests have their own bucket of
    /// the same size. Over it: 429 before the body is read.
    pub recv_per_minute: u32,
    pub recv_burst: u32,
    /// `DELETE` requests per client. Over it: 429 before the body is read.
    pub delete_per_minute: u32,
    pub delete_burst: u32,
    /// The database file's used bytes. A `SEND` that would pass it is `limited`.
    pub max_bytes: u64,
    /// From the request head being read to the response. Over it: 408.
    pub request_timeout: Duration,
}

/// 2 GiB.
pub const DEFAULT_MAX_BYTES: u64 = 2 << 30;

impl Default for Limits {
    fn default() -> Self {
        Limits {
            create_per_hour: 500,
            create_burst: 200,
            send_per_minute: 60,
            send_burst: 20,
            recv_per_minute: 120,
            recv_burst: 120,
            delete_per_minute: 10,
            delete_burst: 10,
            max_bytes: DEFAULT_MAX_BYTES,
            request_timeout: Duration::from_secs(10),
        }
    }
}

pub(crate) struct Limiter {
    creates: Mutex<Buckets<IpAddr>>,
    sends: Mutex<Buckets<QueueId>>,
    recvs: Mutex<Buckets<IpAddr>>,
    acks: Mutex<Buckets<IpAddr>>,
    deletes: Mutex<Buckets<IpAddr>>,
}

impl Limiter {
    pub(crate) fn new(limits: &Limits) -> Self {
        Limiter {
            creates: Mutex::new(Buckets::per_hour(
                limits.create_per_hour,
                limits.create_burst,
            )),
            sends: Mutex::new(Buckets::new(limits.send_per_minute, limits.send_burst)),
            recvs: Mutex::new(Buckets::new(limits.recv_per_minute, limits.recv_burst)),
            acks: Mutex::new(Buckets::new(limits.recv_per_minute, limits.recv_burst)),
            deletes: Mutex::new(Buckets::new(limits.delete_per_minute, limits.delete_burst)),
        }
    }

    pub(crate) fn allow_create(&self, ip: IpAddr, now_ms: u64) -> bool {
        take(&self.creates, client_key(ip), now_ms)
    }

    pub(crate) fn allow_send(&self, queue: &QueueId, now_ms: u64) -> bool {
        take(&self.sends, *queue, now_ms)
    }

    pub(crate) fn allow_recv(&self, ip: IpAddr, now_ms: u64) -> bool {
        take(&self.recvs, client_key(ip), now_ms)
    }

    pub(crate) fn allow_ack(&self, ip: IpAddr, now_ms: u64) -> bool {
        take(&self.acks, client_key(ip), now_ms)
    }

    pub(crate) fn allow_delete(&self, ip: IpAddr, now_ms: u64) -> bool {
        take(&self.deletes, client_key(ip), now_ms)
    }
}

fn take<K: std::hash::Hash + Eq>(buckets: &Mutex<Buckets<K>>, key: K, now_ms: u64) -> bool {
    buckets
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take(key, now_ms)
}
