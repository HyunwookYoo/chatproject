#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared M4 harness carries helpers this test does not use.
#![allow(dead_code, unused_imports)]
//! Task 8: the puppet's message commands between two M5 cores. A file of its own: the puppet
//! control port (47000) is fixed, and tests in one binary run in parallel.
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

/// The puppet's send command between two M5 cores: the text goes out sealed (kind 17).
#[tokio::test]
async fn send_command_goes_out_sealed_between_m5_cores() {
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
        a.slot
            .test_hooks_start("ws://127.0.0.1:47000".into(), "mls-fixture".into()),
        server
    );
    started.unwrap();
    let id = MessageId([81; 16]);
    let reply = call(
        &mut socket,
        1,
        "message_local_send_text",
        json!({"peer":b.peer,"id":id,"body":"PRIVATE_M5_BODY_SENTINEL"}),
    )
    .await;
    assert_eq!(reply["ok"], true);
    let conversation = a.account.messages().open_dm(b.peer).await.unwrap().id;
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let page = a
                .account
                .messages()
                .load_messages(conversation, None, 50)
                .await
                .unwrap();
            if page
                .items
                .iter()
                .any(|m| m.key.id == id && m.state == DeliveryState::Delivered)
            {
                break;
            }
            for f in [&a, &b] {
                f.clock.advance_monotonic_ms(1_000);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("delivered over MLS");
    let frames = a.account.message_test_sent_frames().await.unwrap();
    assert!(
        frames
            .iter()
            .any(|f| chat_core::storage_fixture::mls_app_from_wire(f).is_some()),
        "the text went out as an MLS application message"
    );
    for frame in &frames {
        assert_eq!(frame[2], 17, "only kind 17 frames between M5 cores");
        assert!(
            !frame
                .windows(b"PRIVATE_M5_BODY_SENTINEL".len())
                .any(|w| w == b"PRIVATE_M5_BODY_SENTINEL")
        );
    }
    let _ = socket.close(None).await;
}
