//! The m8 harness read (`harness-counts`, debug builds only): `queue_server --harness-counts`
//! prints how many queues the file holds and the entries each holds, most first, read from a
//! file the real router writes while its store stays open (as the harness reads the running
//! server). Counts only: no queue id, key or ciphertext.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use queue_server::{AppState, Store, router};
use queue_wire::{
    AckItem, BASE64URL_NOPAD, CommandName, CreateItem, ItemResult, QueueRequest, QueueResponse,
    RecvItem, SendItem, Signed, public_key, sign, signing_bytes,
};
use serde::Serialize;
use tower::ServiceExt;

const T0: u64 = 1_800_000_000_000;

fn b64(bytes: &[u8]) -> String {
    BASE64URL_NOPAD.encode(bytes)
}

fn sig(secret: &[u8; 32], signed: &Signed, id: &[u8; 32]) -> String {
    b64(&sign(secret, &signing_bytes(signed, id, T0)))
}

async fn post<T: Serialize>(app: &Router, cmd: CommandName, items: Vec<T>) -> Vec<ItemResult> {
    let body = serde_json::to_vec(&QueueRequest { items }).unwrap();
    let mut req = Request::post(cmd.path())
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 10], 40_000))));
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice::<QueueResponse>(&bytes)
        .unwrap()
        .results
}

/// What `queue_server --harness-counts` prints for the file at `db`; it must end within 10 s
/// (a server would keep listening).
fn counts(db: &Path) -> serde_json::Value {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_queue_server"))
        .arg("--harness-counts")
        .env("QUEUE_DB", db)
        .env("QUEUE_LISTEN", "127.0.0.1:0")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            panic!("queue_server --harness-counts did not end");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[tokio::test]
async fn counts_are_queues_and_entries_each_most_first() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("queue.db");
    let store = Store::open(&db).unwrap();
    let mut state = AppState::new(store.clone());
    state.clock = Arc::new(|| T0);
    let app = router(state);

    assert_eq!(counts(&db), serde_json::json!({"queues":0,"entries":[]}));

    // Two queues; three entries in the second.
    let queues = [
        ([0x51; 32], [0x52; 32], [0x53; 32]),
        ([0x61; 32], [0x62; 32], [0x63; 32]),
    ];
    let creates: Vec<CreateItem> = queues
        .iter()
        .map(|(id, recipient, sender)| {
            let sender_pub = public_key(sender);
            CreateItem {
                queue_id: b64(id),
                recipient_pub: b64(&public_key(recipient)),
                sender_pub: b64(&sender_pub),
                ts: T0,
                sig: sig(
                    recipient,
                    &Signed::Create {
                        sender_pub: &sender_pub,
                    },
                    id,
                ),
            }
        })
        .collect();
    assert!(
        post(&app, CommandName::Create, creates)
            .await
            .iter()
            .all(|r| *r == ItemResult::Ok)
    );
    let (id, recipient, sender) = queues[1];
    let sends: Vec<SendItem> = [b"one".as_slice(), b"two", b"three"]
        .into_iter()
        .map(|ct| SendItem {
            queue_id: b64(&id),
            ct: b64(ct),
            ts: T0,
            sig: sig(&sender, &Signed::Send { ct }, &id),
        })
        .collect();
    assert!(
        post(&app, CommandName::Send, sends)
            .await
            .iter()
            .all(|r| *r == ItemResult::Ok)
    );
    let read = counts(&db);
    assert_eq!(read, serde_json::json!({"queues":2,"entries":[3,0]}));
    let text = read.to_string();
    assert!(!text.contains(&b64(&id)), "no queue id: {text}");

    // An `ACK` up to the second entry's seq (read with a `RECV`) leaves one.
    let recv = RecvItem {
        queue_id: b64(&id),
        since_seq: 0,
        ts: T0,
        sig: sig(&recipient, &Signed::Recv { since_seq: 0 }, &id),
    };
    let [ItemResult::Entries { entries }] = &post(&app, CommandName::Recv, vec![recv]).await[..]
    else {
        panic!("no entries");
    };
    let upto_seq = entries[1].seq;
    let ack = AckItem {
        queue_id: b64(&id),
        upto_seq,
        ts: T0,
        sig: sig(&recipient, &Signed::Ack { upto_seq }, &id),
    };
    assert_eq!(
        post(&app, CommandName::Ack, vec![ack]).await,
        [ItemResult::Ok]
    );
    assert_eq!(counts(&db), serde_json::json!({"queues":2,"entries":[1,0]}));
    drop(store);
}
