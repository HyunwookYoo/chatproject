//! Test hooks: a public-safe timing trace of each transport connection attempt, read by the e2e
//! harness (`network_trace`). Only fixed kinds, counts and milliseconds: never an address, an
//! endpoint id, a key or a ticket.
use iroh::{Endpoint, EndpointAddr, Watcher, endpoint::Connection};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

/// The newest attempts kept.
const LIMIT: usize = 32;

pub(crate) struct Trace {
    created: Instant,
    inner: Mutex<Inner>,
}
#[derive(Default)]
struct Inner {
    dials: VecDeque<Value>,
    binds: u64,
    bind_ms_max: u64,
    online_ms_max: u64,
    relay_drops: u64,
}
impl Default for Trace {
    fn default() -> Self {
        Self { created: Instant::now(), inner: Mutex::default() }
    }
}
fn ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
fn alpn_kind(alpn: &[u8]) -> &'static str {
    if alpn == super::frame::ALPN {
        "session"
    } else if alpn == crate::account::auth::ALPN {
        "auth"
    } else if alpn == crate::account::pair::ALPN {
        "pair"
    } else {
        "other"
    }
}
fn relay_up(endpoint: &Endpoint) -> bool {
    endpoint.home_relay_status().get().iter().any(|status| status.is_connected())
}
impl Trace {
    pub(crate) fn snapshot(&self) -> Value {
        let inner = self.inner.lock().unwrap();
        json!({
            "endpoint": {
                "binds": inner.binds,
                "bind_ms_max": inner.bind_ms_max,
                "online_ms_max": inner.online_ms_max,
                "relay_drops": inner.relay_drops,
            },
            "dials": inner.dials.iter().cloned().collect::<Vec<_>>(),
        })
    }
    /// One endpoint bind: how long the socket bind took, and the bind until the home relay
    /// was connected (equal when no relay is configured).
    pub(crate) fn bound(&self, bind: Duration, online: Duration) {
        let mut inner = self.inner.lock().unwrap();
        inner.binds += 1;
        inner.bind_ms_max = inner.bind_ms_max.max(ms(bind));
        inner.online_ms_max = inner.online_ms_max.max(ms(online));
    }
    /// Counts each loss of every home relay connection until `endpoint` closes.
    pub(crate) async fn watch_relay(&self, endpoint: Endpoint) {
        use futures_util::StreamExt;
        let mut statuses = endpoint.home_relay_status().stream();
        let mut was_up = false;
        while let Some(statuses) = statuses.next().await {
            let up = statuses.iter().any(|status| status.is_connected());
            if was_up && !up {
                self.inner.lock().unwrap().relay_drops += 1;
            }
            was_up = up;
        }
    }
    fn attempt(&self, endpoint: &Endpoint, incoming: bool) -> Attempt<'_> {
        Attempt {
            trace: self,
            started: Instant::now(),
            record: json!({
                "at_ms": ms(self.created.elapsed()),
                "alpn": "other",
                "incoming": incoming,
                "outcome": "dropped",
                "relay_up": relay_up(endpoint),
                "target_relays": null,
                "target_ips": null,
                "target_own_ips": null,
                "resolve_ms": null,
                "handshake_ms": null,
                "total_ms": 0,
                "path": "none",
                "paths_ip": 0,
                "paths_relay": 0,
                "rtt_ms": null,
                "tx": null,
                "rx": null,
                "lost": null,
            }),
        }
    }
    /// `endpoint.connect`, timed. An attempt dropped by its caller's deadline is kept as
    /// `dropped` with the phases it reached.
    pub(crate) async fn connect(
        &self,
        endpoint: &Endpoint,
        target: EndpointAddr,
        alpn: &[u8],
    ) -> Result<Connection, ()> {
        let mut attempt = self.attempt(endpoint, false);
        attempt.record["alpn"] = alpn_kind(alpn).into();
        let own: Vec<IpAddr> = endpoint.addr().ip_addrs().map(|a| a.ip()).collect();
        attempt.record["target_relays"] = target.relay_urls().count().into();
        attempt.record["target_ips"] = target.ip_addrs().count().into();
        attempt.record["target_own_ips"] =
            target.ip_addrs().filter(|a| own.contains(&a.ip())).count().into();
        let connecting = endpoint.connect_with_opts(target, alpn, Default::default()).await;
        attempt.record["resolve_ms"] = ms(attempt.started.elapsed()).into();
        let resolved = Instant::now();
        let result = match connecting {
            Ok(connecting) => connecting.await.map_err(|_| ()),
            Err(_) => Err(()),
        };
        match &result {
            Ok(connection) => attempt.connected(connection, resolved),
            Err(()) => attempt.record["outcome"] = "failed".into(),
        }
        result
    }
    /// An incoming attempt, timed from `accept` until the handshake completed.
    pub(crate) async fn accept(
        &self,
        endpoint: &Endpoint,
        incoming: iroh::endpoint::Incoming,
    ) -> Result<Connection, ()> {
        let mut attempt = self.attempt(endpoint, true);
        let started = attempt.started;
        let result = incoming.await.map_err(|_| ());
        match &result {
            Ok(connection) => {
                attempt.record["alpn"] = alpn_kind(connection.alpn()).into();
                attempt.connected(connection, started);
            }
            Err(()) => attempt.record["outcome"] = "failed".into(),
        }
        result
    }
}
struct Attempt<'a> {
    trace: &'a Trace,
    started: Instant,
    record: Value,
}
impl Attempt<'_> {
    fn connected(&mut self, connection: &Connection, handshake: Instant) {
        let record = &mut self.record;
        record["outcome"] = "ok".into();
        record["handshake_ms"] = ms(handshake.elapsed()).into();
        let paths = connection.paths();
        let selected = paths.iter().find(|path| path.is_selected());
        record["path"] = match &selected {
            Some(path) if path.is_ip() => "ip",
            Some(_) => "relay",
            None => "none",
        }
        .into();
        record["rtt_ms"] = selected.map(|path| ms(path.rtt())).into();
        record["paths_ip"] = paths.iter().filter(|path| path.is_ip()).count().into();
        record["paths_relay"] = paths.iter().filter(|path| path.is_relay()).count().into();
        let stats = connection.stats();
        record["tx"] = stats.udp_tx.datagrams.into();
        record["rx"] = stats.udp_rx.datagrams.into();
        record["lost"] = stats.lost_packets.into();
    }
}
impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        self.record["total_ms"] = ms(self.started.elapsed()).into();
        let mut inner = self.trace.inner.lock().unwrap();
        if inner.dials.len() == LIMIT {
            inner.dials.pop_front();
        }
        inner.dials.push_back(self.record.take());
    }
}
