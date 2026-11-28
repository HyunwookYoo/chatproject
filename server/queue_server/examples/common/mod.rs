//! What the examples share: signed items for a queue with keys derived from
//! a number, and one HTTP/1.1 POST over a plain TCP connection. Each example
//! uses only part of it.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use queue_wire::{
    AckItem, BASE64URL_NOPAD, CommandName, CreateItem, DeleteItem, ItemResult, QueueId,
    QueueRequest, QueueResponse, RecvItem, SendItem, Signed, public_key, sign, signing_bytes,
};
use serde::Serialize;

pub fn b64(bytes: &[u8]) -> String {
    BASE64URL_NOPAD.encode(bytes)
}

/// A queue whose id and keys come from `n`, so a later run can rebuild them.
#[derive(Clone, Copy)]
pub struct Q {
    pub id: QueueId,
    recipient: [u8; 32],
    sender: [u8; 32],
}

impl Q {
    pub fn new(n: u32) -> Self {
        let seed = |tag: u8| {
            let mut out = [tag; 32];
            out[..4].copy_from_slice(&n.to_be_bytes());
            out
        };
        Q {
            id: seed(0xa1),
            recipient: seed(0xa2),
            sender: seed(0xa3),
        }
    }

    fn sig(&self, secret: &[u8; 32], signed: &Signed, ts: u64) -> String {
        b64(&sign(secret, &signing_bytes(signed, &self.id, ts)))
    }

    pub fn create(&self, ts: u64) -> CreateItem {
        let sender_pub = public_key(&self.sender);
        CreateItem {
            queue_id: b64(&self.id),
            recipient_pub: b64(&public_key(&self.recipient)),
            sender_pub: b64(&sender_pub),
            ts,
            sig: self.sig(
                &self.recipient,
                &Signed::Create {
                    sender_pub: &sender_pub,
                },
                ts,
            ),
        }
    }

    pub fn send(&self, ct: &[u8], ts: u64) -> SendItem {
        SendItem {
            queue_id: b64(&self.id),
            ct: b64(ct),
            ts,
            sig: self.sig(&self.sender, &Signed::Send { ct }, ts),
        }
    }

    pub fn recv(&self, since_seq: u64, ts: u64) -> RecvItem {
        RecvItem {
            queue_id: b64(&self.id),
            since_seq,
            ts,
            sig: self.sig(&self.recipient, &Signed::Recv { since_seq }, ts),
        }
    }

    pub fn ack(&self, upto_seq: u64, ts: u64) -> AckItem {
        AckItem {
            queue_id: b64(&self.id),
            upto_seq,
            ts,
            sig: self.sig(&self.recipient, &Signed::Ack { upto_seq }, ts),
        }
    }

    pub fn delete(&self, ts: u64) -> DeleteItem {
        DeleteItem {
            queue_id: b64(&self.id),
            ts,
            sig: self.sig(&self.recipient, &Signed::Delete, ts),
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

pub fn body<T: Serialize>(items: Vec<T>) -> Vec<u8> {
    serde_json::to_vec(&QueueRequest { items }).expect("json")
}

/// POSTs `body` to `addr` (`host:port`) and returns the status and response
/// body. `forwarded` goes in `X-Forwarded-For`, for a server run with `QUEUE_TRUST_PROXY=1`.
pub fn post_raw(
    addr: &str,
    path: &str,
    body: &[u8],
    forwarded: Option<&str>,
) -> std::io::Result<(u16, Vec<u8>)> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let xff = forwarded.map_or(String::new(), |ip| format!("X-Forwarded-For: {ip}\r\n"));
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n{xff}\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer)?;
    let split = answer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("no header end"))?;
    let head = String::from_utf8_lossy(&answer[..split]).to_string();
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        return Err(std::io::Error::other("chunked answer"));
    }
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::other("no status"))?;
    Ok((status, answer[split + 4..].to_vec()))
}

/// POSTs one command batch; panics unless the answer is 200 with one result per item.
pub fn post<T: Serialize>(
    addr: &str,
    cmd: CommandName,
    items: Vec<T>,
    forwarded: Option<&str>,
) -> Vec<ItemResult> {
    let n = items.len();
    let (status, answer) = post_raw(addr, cmd.path(), &body(items), forwarded).expect("http");
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&answer));
    let results = serde_json::from_slice::<QueueResponse>(&answer)
        .expect("answer json")
        .results;
    assert_eq!(results.len(), n);
    results
}
