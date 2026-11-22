//! Drives a running queue_server over HTTP, signing with queue_wire.
//!
//! `smoke <host:port> before`: CREATE, two SENDs, RECV both, ACK the first.
//! Stop the server, start it on the same QUEUE_DB, then
//! `smoke <host:port> after`: RECV must give exactly the unacked second
//! entry; then ACK it and DELETE the queue.

mod common;

use common::{Q, now_ms, post};
use queue_wire::{BASE64URL_NOPAD, CommandName, Entry, ItemResult};

const FIRST: &[u8] = b"smoke entry one";
const SECOND: &[u8] = b"smoke entry two";

fn recv(addr: &str, q: &Q, since: u64) -> Vec<Entry> {
    match post(addr, CommandName::Recv, vec![q.recv(since, now_ms())], None).remove(0) {
        ItemResult::Entries { entries } => entries,
        other => panic!("RECV gave {other:?}"),
    }
}

fn cts(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .map(|e| {
            let ct = BASE64URL_NOPAD.decode(e.ct.as_bytes()).expect("base64");
            String::from_utf8_lossy(&ct).into_owned()
        })
        .collect()
}

fn ok(step: &str, result: ItemResult) {
    assert_eq!(result, ItemResult::Ok, "{step}");
    println!("{step}: ok");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [addr, phase] = &args[..] else {
        eprintln!("usage: smoke <host:port> before|after");
        std::process::exit(2);
    };
    let q = Q::new(7);
    match phase.as_str() {
        "before" => {
            let created = post(addr, CommandName::Create, vec![q.create(now_ms())], None);
            ok("CREATE", created[0].clone());
            let sent = post(
                addr,
                CommandName::Send,
                vec![q.send(FIRST, now_ms()), q.send(SECOND, now_ms())],
                None,
            );
            assert_eq!(sent, vec![ItemResult::Ok, ItemResult::Ok], "SEND");
            println!("SEND x2: ok ok");
            let entries = recv(addr, &q, 0);
            assert_eq!(cts(&entries), ["smoke entry one", "smoke entry two"]);
            println!(
                "RECV since 0: seqs {:?} cts {:?}",
                entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
                cts(&entries)
            );
            let acked = post(
                addr,
                CommandName::Ack,
                vec![q.ack(entries[0].seq, now_ms())],
                None,
            );
            ok(&format!("ACK upto {}", entries[0].seq), acked[0].clone());
            let left = recv(addr, &q, 0);
            assert_eq!(cts(&left), ["smoke entry two"]);
            println!("RECV since 0 after ACK: {:?}", cts(&left));
        }
        "after" => {
            let left = recv(addr, &q, 0);
            assert_eq!(cts(&left), ["smoke entry two"], "after restart");
            println!(
                "RECV since 0 after restart: seq {} {:?}",
                left[0].seq,
                cts(&left)
            );
            let acked = post(
                addr,
                CommandName::Ack,
                vec![q.ack(left[0].seq, now_ms())],
                None,
            );
            ok("ACK", acked[0].clone());
            assert_eq!(recv(addr, &q, 0), vec![]);
            println!("RECV after ACK: empty");
            let deleted = post(addr, CommandName::Delete, vec![q.delete(now_ms())], None);
            ok("DELETE", deleted[0].clone());
            let gone = post(addr, CommandName::Recv, vec![q.recv(0, now_ms())], None);
            assert_eq!(gone, vec![ItemResult::Gone]);
            println!("RECV after DELETE: gone");
        }
        _ => {
            eprintln!("usage: smoke <host:port> before|after");
            std::process::exit(2);
        }
    }
}
