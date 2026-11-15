#![cfg(all(feature = "test-hooks", debug_assertions))]
//! The M7 Debug control actions (`push_local_*`) reach the account that owns the hook, answer
//! counts only, and reject unknown fields, actions and a refused proxy address.

#[allow(dead_code, unused_imports)]
mod commands {
    include!("support/message_network.rs");
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio_tungstenite::tungstenite::Message;

    type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    async fn receive(socket: &mut Socket) -> Value {
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

    async fn call(socket: &mut Socket, id: u64, op: &str, args: Value) -> Value {
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
    async fn push_actions_use_the_owning_handle_answer_counts_and_reject_unknown_input() {
        let own = packet(1, &[11], 1, NOW, NOW + 600, NOW * 1_000_000);
        let backend = Arc::new(Backend::default());
        backend.packets.lock().unwrap().insert(
            derive_account(&Secret32::from([1; 32])).unwrap().peer_id(),
            own.clone(),
        );
        let a = Fixture::new(1, 11, &own, backend).await;
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
                .test_hooks_start("ws://127.0.0.1:47000".into(), "push-fixture".into()),
            server
        );
        started.unwrap();

        // A node sends only: an address, no platform. Nothing listens there; no request is
        // made without a token anyway.
        let configured = call(
            &mut socket,
            1,
            "push_local_configure",
            json!({"proxy_url":"http://127.0.0.1:9","platform":null}),
        )
        .await;
        assert_eq!(configured["ok"], true, "{configured}");
        let status = call(&mut socket, 2, "push_local_status", json!({})).await;
        assert_eq!(status["ok"], true, "{status}");
        assert_eq!(
            status["value"],
            json!({"enabled":true,"platform":null,"handles":0})
        );
        let counts = call(&mut socket, 3, "push_local_test_counts", json!({})).await;
        assert_eq!(
            counts["value"],
            json!({
                "handles":0,"announced":0,"own_handle":false,"peer_handles":0,
                "peer_withdrawn":0,"own_handles":0,"deliveries":0,
                "own_sessions":0,"read_clears":0,"read_items":0,
                "fetches":0,"fetches_online":0,"fetches_done":0,
            })
        );
        let renewed = call(&mut socket, 4, "push_local_renew_due", json!({})).await;
        assert_eq!(
            renewed["value"],
            json!({"issued":0,"announced":0,"withdrawn":0}),
            "no conversation, no handle: {renewed}"
        );
        let off = call(
            &mut socket,
            5,
            "push_local_set_enabled",
            json!({"enabled":false}),
        )
        .await;
        assert_eq!(off["ok"], true, "{off}");
        assert!(!a.account.push_status().await.unwrap().enabled);
        let token = call(
            &mut socket,
            6,
            "push_local_set_token",
            json!({"platform":"fcm","token":"e2e-fake-token"}),
        )
        .await;
        assert_eq!(token["ok"], true, "{token}");
        let status = call(&mut socket, 7, "push_local_status", json!({})).await;
        assert_eq!(
            status["value"]["enabled"], false,
            "push stays off: {status}"
        );

        let mut id = 7;
        for (op, args, code) in [
            (
                "push_local_configure",
                json!({"proxy_url":"ftp://127.0.0.1:9"}),
                "InvalidConfig",
            ),
            (
                "push_local_configure",
                json!({"proxy_url":null,"unknown":1}),
                "InvalidConfig",
            ),
            (
                "push_local_set_token",
                json!({"platform":"web","token":"x"}),
                "InvalidConfig",
            ),
            (
                "push_local_set_token",
                json!({"token":"x"}),
                "InvalidConfig",
            ),
            ("push_local_set_enabled", json!({}), "InvalidConfig"),
            (
                "push_local_status",
                json!({"unknown":true}),
                "InvalidConfig",
            ),
            (
                "push_local_test_counts",
                json!({"unknown":true}),
                "InvalidConfig",
            ),
            ("push_local_unknown", json!({}), "InvalidConfig"),
        ] {
            id += 1;
            let reply = call(&mut socket, id, op, args).await;
            assert_eq!(reply["error"]["code"], code, "{op}: {reply}");
            // Errors carry the fixed text, never the input.
            assert_eq!(
                reply["error"]["detail"], "Account action failed",
                "{op}: {reply}"
            );
        }
        assert!(
            !a.account.push_status().await.unwrap().enabled,
            "a rejected action changes nothing"
        );
        let _ = socket.close(None).await;
    }
}
