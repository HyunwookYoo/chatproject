#![cfg(all(feature = "test-hooks", debug_assertions))]
//! The M6 Debug control actions (`mailbox_local_*`) reach the account that owns the hook, and
//! reject unknown fields and actions.

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
    async fn mailbox_actions_use_the_owning_handle_and_reject_unknown_input() {
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
                .test_hooks_start("ws://127.0.0.1:47000".into(), "mailbox-fixture".into()),
            server
        );
        started.unwrap();

        let state = call(&mut socket, 1, "mailbox_local_state", json!({})).await;
        assert_eq!(state["ok"], true, "{state}");
        assert_eq!(state["value"]["enabled"], false);
        assert_eq!(state["value"]["first_link_shown"], false);
        let set = call(&mut socket, 2, "mailbox_local_set", json!({"enabled":true})).await;
        assert_eq!(set["ok"], true, "{set}");
        assert!(a.account.mailbox_state().await.unwrap().enabled);
        let state = call(&mut socket, 3, "mailbox_local_state", json!({})).await;
        assert_eq!(state["value"]["enabled"], true);
        let sync = call(&mut socket, 4, "mailbox_local_sync_now", json!({})).await;
        assert_eq!(sync["ok"], true, "{sync}");
        let background = call(&mut socket, 5, "mailbox_local_background", json!({})).await;
        assert_eq!(background["value"], json!({"handed_over":0}));
        let counters = call(&mut socket, 6, "mailbox_local_test_counters", json!({})).await;
        assert_eq!(
            counters["value"],
            json!({"missing":0,"notifications":0,"notified":0})
        );

        let mut id = 6;
        for (op, args) in [
            ("mailbox_local_set", json!({"enabled":false,"unknown":true})),
            ("mailbox_local_set", json!({})),
            ("mailbox_local_sync_now", json!({"unknown":true})),
            ("mailbox_local_test_counters", json!({"unknown":true})),
            ("mailbox_local_unknown", json!({})),
        ] {
            id += 1;
            let reply = call(&mut socket, id, op, args).await;
            assert_eq!(reply["error"]["code"], "InvalidConfig", "{op}: {reply}");
        }
        assert!(
            a.account.mailbox_state().await.unwrap().enabled,
            "a rejected action changes nothing"
        );
        let _ = socket.close(None).await;
    }
}
