//! iroh-probe: Spike A connectivity probe.
//!
//! `listen` prints a ticket and echoes pings. `dial` connects to a ticket, measures the
//! connect time, watches the connection's paths (relay vs direct) and prints one JSON summary.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use iroh::{
    Endpoint, EndpointAddr, RelayMode, RelayUrl, TransportAddr,
    endpoint::{Connection, Incoming, presets},
};
use iroh_tickets::endpoint::EndpointTicket;
use n0_future::StreamExt;
use serde_json::{Value, json};

const ALPN: &[u8] = b"probe/1";
const PAYLOAD_LEN: usize = 64;
/// Pause between pings, so `--count` also sets how long we watch for a direct path.
const PING_INTERVAL: Duration = Duration::from_millis(200);
const ONLINE_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Parser)]
#[command(version, about = "Spike A: iroh connect time and direct-path probe")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print a ticket to stdout and echo pings on ALPN probe/1 until Ctrl-C.
    Listen {
        /// Use only this relay instead of the n0 public relays.
        #[arg(long)]
        relay: Option<RelayUrl>,
    },
    /// Connect to a ticket, ping, and print a JSON summary to stdout.
    Dial {
        ticket: EndpointTicket,
        /// Use only this relay instead of the n0 public relays.
        #[arg(long)]
        relay: Option<RelayUrl>,
        /// Number of 64-byte ping-pong round trips, 200 ms apart.
        #[arg(long, default_value_t = 50)]
        count: u32,
        /// Ignore the ticket's direct addresses and dial via its relay URL only, like
        /// production discovery (pkarr publishes the relay URL only).
        #[arg(long)]
        relay_only: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    match Cli::parse().cmd {
        Cmd::Listen { relay } => listen(relay).await,
        Cmd::Dial {
            ticket,
            relay,
            count,
            relay_only,
        } => {
            let mut addr = ticket.endpoint_addr().clone();
            if relay_only {
                addr.addrs.retain(|a| a.is_relay());
            }
            let result = dial(addr, relay, count).await;
            let summary = match &result {
                Ok(summary) => summary.clone(),
                Err(err) => json!({ "error": format!("{err:#}") }),
            };
            println!("{}", serde_json::to_string_pretty(&summary)?);
            result.map(|_| ())
        }
    }
}

/// Minimal preset: crypto provider only, so no pkarr/DNS address lookup and nothing
/// talks to dns.iroh.link. Relays are added explicitly.
async fn bind(relay: Option<RelayUrl>, alpns: Vec<Vec<u8>>) -> Result<Endpoint> {
    let relay_mode = match relay {
        Some(url) => RelayMode::custom([url]),
        None => RelayMode::Default,
    };
    let builder = Endpoint::builder(presets::Minimal)
        .relay_mode(relay_mode)
        .alpns(alpns);
    // Android keeps its DNS servers behind JNI, and a CLI has no JVM: the default resolver's
    // system lookup panics (caught, but printed). Use the public fallback resolvers directly.
    #[cfg(target_os = "android")]
    let builder = builder.dns_resolver(iroh::dns::DnsResolver::builder().build());
    let endpoint = builder.bind().await?;
    tokio::time::timeout(ONLINE_TIMEOUT, endpoint.online())
        .await
        .context("no home relay connection within 30 s")?;
    Ok(endpoint)
}

async fn listen(relay: Option<RelayUrl>) -> Result<()> {
    let endpoint = bind(relay, vec![ALPN.to_vec()]).await?;
    let addr = endpoint.addr();
    eprintln!("endpoint id: {}", addr.id);
    for transport_addr in &addr.addrs {
        eprintln!("  {transport_addr}");
    }
    eprintln!("ticket (stdout):");
    println!("{}", EndpointTicket::new(addr));
    eprintln!("waiting for connections, Ctrl-C to stop");

    while let Some(incoming) = endpoint.accept().await {
        tokio::spawn(async move {
            if let Err(err) = echo(incoming).await {
                eprintln!("connection error: {err:#}");
            }
        });
    }
    Ok(())
}

async fn echo(incoming: Incoming) -> Result<()> {
    let conn = incoming.await?;
    let remote = conn.remote_id();
    eprintln!("accepted {remote}");
    if let Ok((mut send, mut recv)) = conn.accept_bi().await {
        // Echo until the dialer closes the connection, which ends the copy with an error.
        tokio::io::copy(&mut recv, &mut send).await.ok();
    }
    eprintln!("closed {remote}: {}", conn.closed().await);
    Ok(())
}

async fn dial(addr: EndpointAddr, relay: Option<RelayUrl>, count: u32) -> Result<Value> {
    let bind_start = Instant::now();
    let endpoint = bind(relay, vec![]).await?;
    let online_ms = ms(bind_start.elapsed());

    let t0 = Instant::now();
    let conn = tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(addr.clone(), ALPN))
        .await
        .context("connect timed out after 30 s")??;
    let connect_ms = ms(t0.elapsed());

    // Record every change of the selected path. paths_stream() yields the current
    // snapshot first (the path at connect time), then one per change, until close.
    let path_changes = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn({
        let conn = conn.clone();
        let path_changes = path_changes.clone();
        async move {
            let mut last = None;
            let mut snapshots = conn.paths_stream();
            while let Some(paths) = snapshots.next().await {
                let selected = paths
                    .iter()
                    .find(|p| p.is_selected())
                    .map(|p| p.remote_addr().clone());
                if selected != last {
                    path_changes.lock().unwrap().push(json!({
                        "t_ms": ms(t0.elapsed()),
                        "path": selected.as_ref().map_or("none", kind),
                        "remote": selected.as_ref().map(|a| a.to_string()),
                    }));
                    last = selected;
                }
            }
        }
    });

    // Ping-pong on one bidirectional stream. A sample whose selected path changed
    // between send and receive is tagged "mixed".
    let (mut send, mut recv) = conn.open_bi().await?;
    let mut rtts: BTreeMap<&str, Vec<f64>> =
        BTreeMap::from([("relay", vec![]), ("direct", vec![])]);
    let mut echoed = [0u8; PAYLOAD_LEN];
    for i in 0..count {
        if i > 0 {
            tokio::time::sleep(PING_INTERVAL).await;
        }
        let payload = [i as u8; PAYLOAD_LEN];
        let before = selected_path(&conn);
        let sent_at = Instant::now();
        send.write_all(&payload).await?;
        recv.read_exact(&mut echoed).await?;
        let rtt = ms(sent_at.elapsed());
        ensure!(echoed == payload, "echo mismatch on ping {i}");
        let after = selected_path(&conn);
        let tag = if before == after { before } else { "mixed" };
        rtts.entry(tag).or_default().push(rtt);
    }

    let final_path = selected_path(&conn);
    let paths_at_end: Vec<Value> = conn
        .paths()
        .iter()
        .map(|p| {
            json!({
                "path": kind(p.remote_addr()),
                "selected": p.is_selected(),
                "remote": p.remote_addr().to_string(),
                "local": format!("{:?}", p.local_addr()),
                "quic_rtt_ms": ms(p.rtt()),
            })
        })
        .collect();
    let relay_used = conn.paths().iter().find_map(|p| match p.remote_addr() {
        TransportAddr::Relay(url) => Some(url.to_string()),
        _ => None,
    });
    let path_changes = path_changes.lock().unwrap().clone();
    let time_to_direct_ms = path_changes
        .iter()
        .find(|c| c["path"] == "direct")
        .map(|c| c["t_ms"].clone());
    let rtt_ms: serde_json::Map<_, _> = rtts
        .into_iter()
        .map(|(tag, v)| (tag.to_string(), stats(v)))
        .collect();
    let local = endpoint.addr();

    send.finish()?;
    conn.close(0u32.into(), b"done");
    endpoint.close().await;

    Ok(json!({
        "connect_ms": connect_ms,
        "time_to_direct_ms": time_to_direct_ms,
        "final_path": final_path,
        "rtt_ms": rtt_ms,
        "count": count,
        "online_ms": online_ms,
        "relay_used": relay_used,
        "local": {
            "endpoint_id": local.id.to_string(),
            "home_relay": local.relay_urls().next().map(|u| u.to_string()),
            "addrs": local.ip_addrs().map(|a| a.to_string()).collect::<Vec<_>>(),
        },
        "remote": {
            "endpoint_id": addr.id.to_string(),
            "dialed_addrs": addr.addrs.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
        },
        "path_changes": path_changes,
        "paths_at_end": paths_at_end,
    }))
}

/// Kind of the path currently selected for application data ("none" if there is none).
fn selected_path(conn: &Connection) -> &'static str {
    conn.paths()
        .iter()
        .find(|p| p.is_selected())
        .map_or("none", |p| kind(p.remote_addr()))
}

fn kind(addr: &TransportAddr) -> &'static str {
    if addr.is_ip() {
        "direct"
    } else if addr.is_relay() {
        "relay"
    } else {
        "other"
    }
}

/// Nearest-rank p50/p95.
fn stats(mut v: Vec<f64>) -> Value {
    if v.is_empty() {
        return json!({ "n": 0, "p50": null, "p95": null });
    }
    v.sort_by(f64::total_cmp);
    let pct = |p: f64| v[((p * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1];
    json!({ "n": v.len(), "p50": pct(0.50), "p95": pct(0.95) })
}

/// Milliseconds with microsecond precision.
fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1e6).round() / 1e3
}
