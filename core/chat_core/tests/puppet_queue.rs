#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers this test does not use.
#![allow(dead_code, unused_imports)]
//! The M8 Debug control actions (`queue_local_*`, `mailbox_local_test_seed_uptime`) reach the
//! account that owns the hook, answer values only (never a queue id, key or ciphertext), and
//! reject unknown fields, actions and modes.
include!("support/queue_network.rs");

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

/// The harness side of the control connection.
mod control {
    use super::*;
    pub type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    pub async fn receive(socket: &mut Socket) -> Value {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Message::Text(text) = frame {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }

    pub async fn call(socket: &mut Socket, id: u64, op: &str, args: Value) -> Value {
        socket
            .send(Message::Text(
                json!({"type":"cmd","id":id,"op":op,"args":args})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        loop {
            let reply = receive(socket).await;
            if reply["type"] == "result" {
                return reply;
            }
        }
    }
}
use control::Socket;

/// Every value of `counts` is a number: no id, key or ciphertext rides a count.
fn numbers_only(counts: &Value) -> bool {
    counts
        .as_object()
        .is_some_and(|map| map.values().all(Value::is_u64))
}

#[tokio::test]
async fn queue_actions_answer_values_only_and_reject_unknown_input() {
    // A1 has a queue the server took and keeps B1's `QueueRef`; B1 keeps A1's.
    let (a1, b1, _net, _conv, _server) = announced().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:47000")
        .await
        .unwrap();
    let server = async {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        assert_eq!(control::receive(&mut socket).await["type"], "hello");
        socket
            .send(Message::Text(
                json!({"type":"config","relay":null}).to_string().into(),
            ))
            .await
            .unwrap();
        socket
    };
    let (started, mut socket) = tokio::join!(
        a1.slot
            .test_hooks_start("ws://127.0.0.1:47000".into(), "queue-fixture".into()),
        server
    );
    started.unwrap();

    let state = control::call(&mut socket, 1, "queue_local_state", json!({})).await;
    assert_eq!(state["ok"], true, "{state}");
    assert_eq!(
        state["value"],
        json!({"mode":"auto","has_mailbox":false,"using_queue":true,"uptime_7d_permille":0})
    );

    let counts = control::call(&mut socket, 2, "queue_local_test_counts", json!({})).await;
    assert_eq!(counts["ok"], true, "{counts}");
    let counts = &counts["value"];
    assert!(numbers_only(counts), "{counts}");
    let keys: Vec<&str> = counts
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "acked",
            "created",
            "fetched",
            "own_active",
            "own_retiring",
            "peer_dead",
            "peer_live",
            "peer_withdrawn",
            "refs_pending",
            "sends_message",
            "sends_standing",
        ]
    );
    assert_eq!(counts["own_active"], 1, "{counts}");
    assert_eq!(counts["own_retiring"], 0, "{counts}");
    assert_eq!(counts["created"], 1, "one CREATE taken: {counts}");
    assert_eq!(counts["peer_live"], 1, "B1's QueueRef: {counts}");
    assert_eq!(counts["peer_withdrawn"], 0, "{counts}");
    assert_eq!(counts["refs_pending"], 0, "announced: {counts}");

    // Narrowed to one device: B1's QueueRef is there, A1 keeps none of its own.
    let b1_id = installation(&b1).await;
    let of_b1 = control::call(
        &mut socket,
        3,
        "queue_local_test_counts",
        json!({"peer":b1_id}),
    )
    .await;
    assert_eq!(of_b1["value"]["peer_live"], 1, "{of_b1}");
    let a1_id = installation(&a1).await;
    let of_a1 = control::call(
        &mut socket,
        4,
        "queue_local_test_counts",
        json!({"peer":a1_id}),
    )
    .await;
    assert_eq!(of_a1["value"]["peer_live"], 0, "{of_a1}");
    assert_eq!(
        of_a1["value"]["own_active"], 1,
        "own rows are not narrowed: {of_a1}"
    );

    // The uptime hook (e2e m8 step 4): a qualified week as the mailbox role.
    let on = control::call(&mut socket, 5, "mailbox_local_set", json!({"enabled":true})).await;
    assert_eq!(on["ok"], true, "{on}");
    let seeded = control::call(
        &mut socket,
        6,
        "mailbox_local_test_seed_uptime",
        json!({"permille":950,"recorded_ms":7 * DAY_MS}),
    )
    .await;
    assert_eq!(seeded["ok"], true, "{seeded}");
    let uptime = a1.account.mailbox_state().await.unwrap().uptime_7d_permille;
    assert!((950..=1000).contains(&uptime), "{uptime}");

    let never = control::call(
        &mut socket,
        7,
        "queue_local_set_mode",
        json!({"mode":"never"}),
    )
    .await;
    assert_eq!(never["ok"], true, "{never}");
    assert_eq!(
        a1.account.queue_state().await.unwrap().mode,
        QueueMode::Never
    );
    let state = control::call(&mut socket, 8, "queue_local_state", json!({})).await;
    assert_eq!(state["value"]["mode"], "never", "{state}");

    let mut id = 8;
    for (op, args) in [
        ("queue_local_set_mode", json!({"mode":"sometimes"})),
        (
            "queue_local_set_mode",
            json!({"mode":"auto","unknown":true}),
        ),
        ("queue_local_set_mode", json!({})),
        ("queue_local_state", json!({"unknown":true})),
        (
            "queue_local_test_counts",
            json!({"peer":"not an installation"}),
        ),
        ("queue_local_test_counts", json!({"unknown":true})),
        ("mailbox_local_test_seed_uptime", json!({"permille":950})),
        ("queue_local_unknown", json!({})),
    ] {
        id += 1;
        let reply = control::call(&mut socket, id, op, args).await;
        assert_eq!(reply["error"]["code"], "InvalidConfig", "{op}: {reply}");
        // Errors carry the fixed text, never the input.
        assert_eq!(
            reply["error"]["detail"], "Account action failed",
            "{op}: {reply}"
        );
    }
    assert_eq!(
        a1.account.queue_state().await.unwrap().mode,
        QueueMode::Never,
        "a rejected action changes nothing"
    );
    let _ = socket.close(None).await;
}
