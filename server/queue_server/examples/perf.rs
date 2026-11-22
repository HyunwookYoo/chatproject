//! Load for the Task 3 performance check, against a running queue_server
//! started with `QUEUE_TRUST_PROXY=1` (each batch comes from its own
//! `X-Forwarded-For` address, so the per-client limits do not throttle it).
//!
//! `perf <host:port> fill <queues> <entries per queue> <ct bytes>`
//! `perf <host:port> recv <queues> <requests> <since: 0|max>`: RECV batches
//! of 100 queues; prints latency percentiles over HTTP, signing excluded.

mod common;

use std::time::Instant;

use common::{Q, body, post, post_raw};
use queue_wire::{CommandName, ItemResult, MAX_ITEMS, QueueResponse};

fn client(kind: u8, n: usize) -> String {
    format!("10.{kind}.{}.{}", (n >> 8) & 255, n & 255)
}

fn arg(args: &[String], i: usize) -> usize {
    args.get(i)
        .and_then(|a| a.parse().ok())
        .unwrap_or_else(|| usage())
}

fn usage() -> ! {
    eprintln!(
        "usage: perf <host:port> fill <queues> <per queue> <ct bytes> | recv <queues> <requests> <0|max>"
    );
    std::process::exit(2)
}

fn fill(addr: &str, queues: usize, per_queue: usize, ct_bytes: usize) {
    let start = Instant::now();
    let mut not_ok = 0usize;
    for (b, chunk) in (0..queues as u32)
        .collect::<Vec<_>>()
        .chunks(MAX_ITEMS)
        .enumerate()
    {
        let now = common::now_ms();
        let items = chunk.iter().map(|&n| Q::new(n).create(now)).collect();
        let results = post(addr, CommandName::Create, items, Some(&client(1, b)));
        not_ok += results.iter().filter(|r| **r != ItemResult::Ok).count();
    }
    println!(
        "created {queues} queues in {} ms, not ok: {not_ok}",
        start.elapsed().as_millis()
    );

    let start = Instant::now();
    let mut not_ok = 0usize;
    for round in 0..per_queue {
        for chunk in (0..queues as u32).collect::<Vec<_>>().chunks(MAX_ITEMS) {
            let now = common::now_ms();
            let items = chunk
                .iter()
                .map(|&n| {
                    let mut ct = vec![0x5a; ct_bytes];
                    ct[..4].copy_from_slice(&n.to_be_bytes());
                    ct[4..8].copy_from_slice(&(round as u32).to_be_bytes());
                    Q::new(n).send(&ct, now)
                })
                .collect();
            let results = post(addr, CommandName::Send, items, None);
            not_ok += results.iter().filter(|r| **r != ItemResult::Ok).count();
        }
    }
    let sent = queues * per_queue;
    let ms = start.elapsed().as_millis();
    println!(
        "sent {sent} entries of {ct_bytes} B in {ms} ms ({} entries/s), not ok: {not_ok}",
        sent as u128 * 1000 / ms.max(1)
    );
}

fn recv(addr: &str, queues: usize, requests: usize, since: u64) {
    let now = common::now_ms();
    let bodies: Vec<Vec<u8>> = (0..requests)
        .map(|i| {
            let first = (i * MAX_ITEMS) % queues;
            let items = (first..first + MAX_ITEMS)
                .map(|n| Q::new((n % queues) as u32).recv(since, now))
                .collect::<Vec<_>>();
            body(items)
        })
        .collect();
    let mut times = Vec::with_capacity(requests);
    let (mut errors, mut entries, mut limited, mut answer_bytes) = (0, 0, 0, 0);
    let start = Instant::now();
    for (i, body) in bodies.iter().enumerate() {
        let t = Instant::now();
        let (status, answer) = post_raw(addr, "/q/recv", body, Some(&client(2, i))).expect("http");
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        answer_bytes += answer.len();
        let Ok(resp) = serde_json::from_slice::<QueueResponse>(&answer) else {
            errors += 1;
            continue;
        };
        if status != 200 || resp.results.len() != MAX_ITEMS {
            errors += 1;
        }
        for r in resp.results {
            match r {
                ItemResult::Entries { entries: e } => entries += e.len(),
                ItemResult::Limited => limited += 1,
                _ => errors += 1,
            }
        }
    }
    let total = start.elapsed().as_millis();
    times.sort_by(f64::total_cmp);
    let at = |p: f64| times[((times.len() as f64 * p).ceil() as usize).clamp(1, times.len()) - 1];
    println!(
        "recv {requests} x {MAX_ITEMS} queues since={since}: p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms, max {:.2} ms, total {total} ms",
        at(0.50),
        at(0.95),
        at(0.99),
        at(1.0)
    );
    println!(
        "  entries returned {entries}, queues limited {limited}, errors {errors}, mean answer {} B",
        answer_bytes / requests.max(1)
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let addr = args.first().unwrap_or_else(|| usage());
    match args.get(1).map(String::as_str) {
        Some("fill") => fill(addr, arg(&args, 2), arg(&args, 3), arg(&args, 4)),
        Some("recv") => {
            let since = match args.get(4).map(String::as_str) {
                Some("0") => 0,
                Some("max") => u64::MAX,
                _ => usage(),
            };
            recv(addr, arg(&args, 2), arg(&args, 3), since)
        }
        _ => usage(),
    }
}
