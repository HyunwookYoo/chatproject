#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared M4 harness carries helpers this test does not use.
#![allow(dead_code, unused_imports)]
//! The puppet's ack-loss fixture between two M5 cores. A file of its own: the puppet control
//! port (47000) is fixed, and tests in one binary run in parallel.
include!("support/message_network.rs");

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

async fn receive(socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> Value {
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

async fn call(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    id: u64,
    op: &str,
    args: Value,
) -> Value {
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

/// Under MLS the recipient's only acknowledgement is the `Ack` in its response: a text received
/// over MLS makes no Delivered receipt job, so the fixture holds the response alone and reports
/// that there is no receipt to hold (the M4 path still holds both: `puppet_message.rs`).
#[tokio::test]
async fn ack_loss_holds_the_mls_ack_and_reports_no_receipt_job() {
    let (a, b, _) = pair().await;
    // `pair` makes M4-peer fixtures; these two are the updated (M5) apps.
    for f in [&a, &b] {
        f.account.message_test_m4_peer(false).await.unwrap();
    }
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:47000")
        .await
        .unwrap();
    let server = async {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        assert_eq!(receive(&mut socket).await["type"], "hello");
        socket
            .send(Message::Text(
                json!({"type":"config","relay":null}).to_string().into(),
            ))
            .await
            .unwrap();
        socket
    };
    let (started, mut socket) = tokio::join!(
        b.slot
            .test_hooks_start("ws://127.0.0.1:47000".into(), "mls-ack-fixture".into()),
        server
    );
    started.unwrap();
    let conversation = a.account.messages().open_dm(b.peer).await.unwrap().id;
    let key = chat_core::MessageKey {
        conversation,
        sender: a.peer,
        installation: a.account.snapshot().await.unwrap().installation.unwrap(),
        id: MessageId([82; 16]),
    };
    let armed = call(
        &mut socket,
        1,
        "message_local_test_ack_loss_arm",
        json!({"key":key}),
    )
    .await;
    assert_eq!(armed["ok"], true);
    a.account
        .messages()
        .send_text(b.peer, key.id, "PRIVATE_ACK_FIXTURE".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let page = b
                .account
                .messages()
                .load_messages(conversation, None, 50)
                .await
                .unwrap();
            if page.items.iter().any(|m| m.key == key) {
                break;
            }
            for f in [&a, &b] {
                f.clock.advance_monotonic_ms(1_000);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("B stored the text");
    let entered = call(
        &mut socket,
        2,
        "message_local_test_ack_loss_wait",
        json!({}),
    )
    .await;
    assert_eq!(entered["ok"], true, "{entered}");
    assert_eq!(entered["value"]["response_checkpoint"], true);
    assert_eq!(entered["value"]["receipt_job"], false);
    assert_eq!(entered["value"]["receipt_checkpoint"], false);
    // The Ack is held: A does not have it. (B's response writes count its answers to A's group
    // setup too, so they are no measure of the Ack here.)
    let page = a
        .account
        .messages()
        .load_messages(conversation, None, 50)
        .await
        .unwrap();
    let sent = page.items.iter().find(|m| m.key == key).unwrap();
    assert_ne!(sent.state, DeliveryState::Delivered);
    let released = call(
        &mut socket,
        4,
        "message_local_test_ack_loss_release",
        json!({}),
    )
    .await;
    assert_eq!(released["ok"], true);
    let _ = socket.close(None).await;
}
