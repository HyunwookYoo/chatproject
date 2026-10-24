#![cfg(all(feature = "test-hooks", debug_assertions))]

use chat_core::puppet::protocol;
use serde_json::json;
use std::time::Duration;

#[allow(dead_code, unused_imports)]
mod commands {
    include!("support/message_network.rs");
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio_tungstenite::tungstenite::Message;

    async fn receive(
        socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) -> Value {
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

    #[tokio::test]
    async fn seven_commands_use_the_owning_handle_and_reject_unknown_fields() {
        let (a, b, _) = pair().await;
        contacts(&a, &b).await;
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
                .test_hooks_start("ws://127.0.0.1:47000".into(), "message-fixture".into()),
            server
        );
        started.unwrap();
        let conversation = a.account.messages().open_dm(b.peer).await.unwrap().id;
        let key = chat_core::MessageKey {
            conversation,
            sender: a.peer,
            installation: a.account.snapshot().await.unwrap().installation.unwrap(),
            id: MessageId([71; 16]),
        };
        let actions = [
            ("open_dm", json!({"peer":b.peer})),
            ("list_conversations", json!({"before":null,"limit":50})),
            (
                "send_text",
                json!({"peer":b.peer,"id":key.id,"body":"PRIVATE_M4_BODY_SENTINEL"}),
            ),
            (
                "load_messages",
                json!({"conversation":conversation,"before":null,"limit":50}),
            ),
            ("cancel_send", json!({"key":key})),
            ("retry_send", json!({"key":key})),
            ("mark_read", json!({"conversation":conversation,"keys":[]})),
        ];
        let mut id = 0;
        for (op, args) in actions {
            id += 1;
            socket
                .send(Message::Text(
                    json!({"type":"cmd","id":id,"op":format!("message_local_{op}"),"args":args})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            loop {
                let reply = receive(&mut socket).await;
                if reply["type"] == "result" {
                    // Retrying a cancelled row may fail by facade policy; it must not be an unknown command.
                    if op == "retry_send" {
                        assert_ne!(reply["error"]["code"], "InvalidConfig");
                    } else {
                        assert_eq!(reply["ok"], true, "{op}");
                    }
                    break;
                }
            }
            id += 1;
            let mut invalid = args.clone();
            invalid["unknown"] = json!(true);
            socket
                .send(Message::Text(
                    json!({"type":"cmd","id":id,"op":format!("message_local_{op}"),"args":invalid})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            loop {
                let reply = receive(&mut socket).await;
                if reply["type"] == "result" {
                    assert_eq!(reply["error"]["code"], "InvalidConfig");
                    break;
                }
            }
        }
        let stored = a
            .account
            .messages()
            .load_messages(conversation, None, 50)
            .await
            .unwrap();
        assert_eq!(stored.items.len(), 1);
        assert_eq!(stored.items[0].key, key);
        assert_eq!(
            call(&mut socket, id + 1, "clock_advance", json!({"seconds":1})).await["error"]["code"],
            "NotAvailable"
        );
        let _ = socket.close(None).await;

        // The fault controls must be backed by the existing authenticated path.
        contacts(&b, &a).await;
        a.account.connect_account(b.peer).await.unwrap();
        let server = async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            receive(&mut socket).await;
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
                .test_hooks_start("ws://127.0.0.1:47000".into(), "ack-fixture".into()),
            server
        );
        started.unwrap();
        let mut lost_key = key;
        lost_key.id = MessageId([72; 16]);
        assert_eq!(
            call(
                &mut socket,
                1,
                "message_local_test_ack_loss_arm",
                json!({"key":lost_key})
            )
            .await["ok"],
            true
        );
        assert_eq!(
            call(
                &mut socket,
                2,
                "message_local_test_ack_loss_arm",
                json!({"key":lost_key})
            )
            .await["error"]["code"],
            "Busy"
        );
        a.account
            .messages()
            .send_text(b.peer, lost_key.id, "PRIVATE_ACK_FIXTURE".into())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if b.account
                    .messages()
                    .load_messages(conversation, None, 50)
                    .await
                    .unwrap()
                    .items
                    .iter()
                    .any(|v| v.key == lost_key)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        b.clock.advance_monotonic_ms(1000);
        let entered = call(
            &mut socket,
            3,
            "message_local_test_ack_loss_wait",
            json!({}),
        )
        .await;
        assert_eq!(entered["ok"], true);
        assert_eq!(entered["value"]["response_checkpoint"], true);
        assert_eq!(entered["value"]["receipt_checkpoint"], true);
        let counts = call(&mut socket, 4, "message_local_test_write_counts", json!({})).await;
        assert_eq!(counts["value"]["payload_writes"], 0);
        assert_eq!(counts["value"]["response_writes"], 0);
        assert_eq!(
            call(
                &mut socket,
                5,
                "message_local_test_ack_loss_release",
                json!({})
            )
            .await["ok"],
            true
        );
        let _ = socket.close(None).await;

        let dir = tempfile::tempdir().unwrap();
        let slot = CoreSlot::new();
        slot.start(
            CoreConfig {
                data_dir: dir.path().into(),
            },
            &EventBus::new(),
        )
        .unwrap();
        let server = async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            receive(&mut socket).await;
            socket
                .send(Message::Text(
                    json!({"type":"config","relay":null,
                "records":"ws://127.0.0.1:47000/records","controlled_time":true})
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            socket
        };
        let (started, mut socket) = tokio::join!(
            slot.test_hooks_start("ws://127.0.0.1:47000".into(), "clock-fixture".into()),
            server
        );
        started.unwrap();
        assert_eq!(
            call(&mut socket, 1, "clock_advance", json!({"seconds":1})).await["error"]["code"],
            "NotAvailable"
        );
        let actor = slot.open_account(support::protector()).unwrap();
        let before =
            call(&mut socket, 2, "clock_advance", json!({"seconds":0})).await["value"].clone();
        let mut id = 2;
        for args in [
            json!({}),
            json!([]),
            json!({"seconds":-1}),
            json!({"seconds":1.5}),
            json!({"seconds":"1"}),
            json!({"seconds":null}),
            json!({"seconds":31_536_001}),
            json!({"seconds":u64::MAX}),
            json!({"seconds":1,"unknown":true}),
        ] {
            id += 1;
            assert_eq!(
                call(&mut socket, id, "clock_advance", args).await["error"]["code"],
                "InvalidConfig"
            );
        }
        id += 1;
        assert_eq!(
            call(&mut socket, id, "clock_advance", json!({"seconds":0})).await["value"],
            before
        );
        id += 1;
        let after = call(
            &mut socket,
            id,
            "clock_advance",
            json!({"seconds":31_536_000}),
        )
        .await;
        assert_eq!(after["value"]["monotonic_ms"], 31_536_000_000u64);
        assert_eq!(
            after["value"]["unix_seconds"].as_u64(),
            Some(before["unix_seconds"].as_u64().unwrap() + 31_536_000)
        );
        assert_eq!(
            actor.snapshot().await.unwrap().phase,
            chat_core::account::AccountPhase::Unconfigured
        );
        let _ = socket.close(None).await;
        actor.request_suspend();
        tokio::time::timeout(Duration::from_secs(5), async {
            while actor.lifecycle_status() == 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(actor.lifecycle_status(), 0);
    }
}

#[test]
fn controlled_clock_is_an_explicit_immutable_config_opt_in() {
    assert!(
        protocol::parse(
            &json!({"type":"config","relay":null,
        "records":"ws://127.0.0.1:47000/records","controlled_time":true})
            .to_string()
        )
        .is_ok()
    );
    assert!(protocol::parse(r#"{"type":"config","relay":null}"#).is_ok());
    for value in [json!(null), json!(1), json!("true")] {
        assert!(
            protocol::parse(
                &json!({"type":"config","relay":null,
            "controlled_time":value})
                .to_string()
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn checked_clock_advance_is_atomic_and_wakes_only_after_commit() {
    use chat_core::account::clock::{Clock, FakeClock};
    for (wall, mono, delta) in [
        (10, u64::MAX - 999, 1),
        (u64::MAX, 0, 1),
        (u64::MAX / 1000, 0, 1),
        (10, 20, 31_536_001),
    ] {
        let clock = FakeClock::new(wall, mono);
        assert_eq!(clock.advance_seconds(delta), None);
        assert_eq!((clock.unix_seconds(), clock.monotonic_ms()), (wall, mono));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), clock.wake().unwrap().notified())
                .await
                .is_err()
        );
    }
    let clock = FakeClock::new(1_800_000_000, 7);
    assert_eq!(
        clock.advance_seconds(31_536_000),
        Some((1_831_536_000, 31_536_000_007))
    );
    tokio::time::timeout(Duration::from_secs(1), clock.wake().unwrap().notified())
        .await
        .unwrap();
    assert_eq!(
        clock.advance_seconds(0),
        Some((1_831_536_000, 31_536_000_007))
    );
    let old = FakeClock::new(1, u64::MAX);
    old.advance_monotonic_ms(1);
    assert_eq!(old.monotonic_ms(), 0);
}

#[test]
fn concurrent_advances_do_not_lose_time_or_return_partial_pairs() {
    use chat_core::account::clock::{Clock, FakeClock};
    let clock = std::sync::Arc::new(FakeClock::new(0, 0));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let clock = clock.clone();
            scope.spawn(move || {
                for _ in 0..1000 {
                    let (wall, mono) = clock.advance_seconds(1).unwrap();
                    assert_eq!(wall * 1000, mono);
                }
            });
        }
    });
    assert_eq!(clock.unix_seconds(), 8000);
    assert_eq!(clock.monotonic_ms(), 8_000_000);
}
