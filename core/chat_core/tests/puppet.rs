#![cfg(all(feature = "test-hooks", debug_assertions))]

use chat_core::{
    CoreConfig, CoreSlot, ErrorCode, EventBus, NetworkHandle, NetworkOp, NetworkReply, NetworkState,
};
use std::time::Duration;

async fn info(network: &NetworkHandle) -> chat_core::NetworkInfo {
    let NetworkReply::Info(info) = network.execute(NetworkOp::Info).await.unwrap() else {
        panic!()
    };
    info
}
async fn online(network: &NetworkHandle) -> chat_core::NetworkInfo {
    network.execute(NetworkOp::Start).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let info = info(network).await;
            if info.state == NetworkState::Online {
                return info;
            }
            assert_ne!(info.state, NetworkState::Failed);
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn local_configuration_precedes_endpoint_and_is_idempotent_online() {
    let dir = tempfile::tempdir().unwrap();
    let slot = CoreSlot::new();
    slot.start(
        CoreConfig {
            data_dir: dir.path().into(),
        },
        &EventBus::new(),
    )
    .unwrap();
    let network = slot.network().unwrap();
    network.configure_local(None).await.unwrap();
    let stopped = info(&network).await;
    assert_eq!(stopped.state, NetworkState::Stopped);
    assert!(stopped.endpoint_id.is_none());
    let first = online(&network).await;
    network.configure_local(None).await.unwrap();
    assert_eq!(info(&network).await.endpoint_id, first.endpoint_id);
    assert_eq!(
        network
            .configure_local(Some("http://127.0.0.1:3340".parse().unwrap()))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidConfig
    );
    network.execute(NetworkOp::Stop).await.unwrap();
}
#[tokio::test]
async fn offline_rejects_send_and_rebinds_same_key() {
    let dir = tempfile::tempdir().unwrap();
    let slot = CoreSlot::new();
    slot.start(
        CoreConfig {
            data_dir: dir.path().into(),
        },
        &EventBus::new(),
    )
    .unwrap();
    let network = slot.network().unwrap();
    network.configure_local(None).await.unwrap();
    let first = online(&network).await;
    assert_eq!(
        network.set_offline(true).await.unwrap().state,
        NetworkState::Stopped
    );
    assert_eq!(
        network
            .execute(NetworkOp::Send {
                peer_id: "missing".into(),
                text: "must not replay".into()
            })
            .await
            .unwrap_err()
            .code,
        ErrorCode::PeerNotConnected
    );
    network.set_offline(false).await.unwrap();
    assert_eq!(online(&network).await.endpoint_id, first.endpoint_id);
    network.execute(NetworkOp::Stop).await.unwrap();
}

#[test]
fn protocol_rejects_malformed_shapes_arguments_and_duplicate_ids() {
    use chat_core::puppet::protocol::{self, RequestIds};
    for bad in [
        r#"{}"#,
        r#"{"type":"config"}"#,
        r#"{"type":"cmd","id":-1,"op":"network_info","args":{}}"#,
        r#"{"type":"cmd","id":1,"op":"network_info","args":{},"extra":0}"#,
    ] {
        assert_eq!(
            protocol::parse(bad).unwrap_err().code,
            ErrorCode::InvalidConfig
        );
    }
    for (op, args, code) in [
        (
            "network_info",
            serde_json::json!([]),
            ErrorCode::InvalidConfig,
        ),
        (
            "connect_peer",
            serde_json::json!(["peer"]),
            ErrorCode::InvalidConfig,
        ),
        (
            "network_info",
            serde_json::json!({"extra":true}),
            ErrorCode::InvalidConfig,
        ),
        (
            "connect_peer",
            serde_json::json!({"target":1}),
            ErrorCode::InvalidConfig,
        ),
        (
            "send_probe_text",
            serde_json::json!({"peer_id":"p"}),
            ErrorCode::InvalidConfig,
        ),
        (
            "set_offline",
            serde_json::json!({"offline":"true"}),
            ErrorCode::InvalidConfig,
        ),
        (
            "clock_advance",
            serde_json::json!({}),
            ErrorCode::NotAvailable,
        ),
    ] {
        assert_eq!(protocol::command(op, args).unwrap_err().code, code);
    }
    let mut ids = RequestIds::default();
    assert!(ids.accept(0).is_err());
    ids.accept(1).unwrap();
    assert!(ids.accept(1).is_err());
    ids.accept(3).unwrap();
    assert!(ids.accept(2).is_err());
    RequestIds::default().accept(1).unwrap();
}
#[test]
fn protocol_limits_urls_and_serializes_typed_results() {
    use chat_core::puppet::protocol::{self, Outgoing};
    for bad in [
        "wss://127.0.0.1:47000",
        "ws://localhost:47000",
        "ws://127.0.0.1:3340",
        "ws://192.0.2.1:47000",
        "ws://127.0.0.1:47000/path",
    ] {
        assert!(protocol::validate_control(bad).is_err());
    }
    for bad in [
        "https://127.0.0.1:3340",
        "http://localhost:3340",
        "http://127.0.0.1:3341",
        "http://user@127.0.0.1:3340",
    ] {
        assert!(protocol::validate_relay(bad).is_err());
    }
    assert_eq!(
        serde_json::to_value(Outgoing::Result {
            id: 5,
            result: Err(chat_core::CoreError::new(
                ErrorCode::PeerNotConnected,
                "peer is disconnected"
            ))
            .into(),
        })
        .unwrap(),
        serde_json::json!({"type":"result","id":5,"ok":false,"error":{"code":"PeerNotConnected","detail":"peer is disconnected"}})
    );
    protocol::validate_control("ws://127.0.0.1:47000").unwrap();
    protocol::validate_relay("http://127.0.0.1:3340/").unwrap();
    assert_eq!(
        serde_json::to_value(Outgoing::Result {
            id: 4,
            result: Ok(NetworkReply::Done).into()
        })
        .unwrap(),
        serde_json::json!({"type":"result","id":4,"ok":true,"value":null})
    );
}
#[tokio::test]
async fn hook_start_rejects_nonlocal_control_before_connecting() {
    let slot = CoreSlot::new();
    let dir = tempfile::tempdir().unwrap();
    slot.start(
        CoreConfig {
            data_dir: dir.path().into(),
        },
        &EventBus::new(),
    )
    .unwrap();
    assert_eq!(
        slot.test_hooks_start("ws://example.com:47000".into(), "test".into())
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidConfig
    );
}

#[tokio::test]
async fn control_ready_reconnect_sequence_pending_rpc_and_disconnect_preserve_network() {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio_tungstenite::{accept_async, tungstenite::Message};
    async fn receive(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> Value {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Message::Text(text) = message {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:47000")
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let bus = EventBus::new();
    let slot = Arc::new(CoreSlot::new());
    let core = slot
        .start(
            CoreConfig {
                data_dir: dir.path().into(),
            },
            &bus,
        )
        .unwrap();
    let network = slot.network().unwrap();
    let copy = slot.clone();
    let mut startup = tokio::spawn(async move {
        copy.test_hooks_start("ws://127.0.0.1:47000".into(), "fixture".into())
            .await
    });
    let mut ws = accept_async(
        tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap()
            .0,
    )
    .await
    .unwrap();
    let hello = receive(&mut ws).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["instance"], core.instance_id);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut startup)
            .await
            .is_err()
    );
    assert!(info(&network).await.endpoint_id.is_none());
    ws.send(Message::Text(
        json!({"type":"config","relay":null}).to_string().into(),
    ))
    .await
    .unwrap();
    startup.await.unwrap().unwrap();
    let first = receive(&mut ws).await;
    assert_eq!(first["seq"], 1);
    let started = online(&network).await;
    // A connect to a loopback address with no server stays pending while events flow.
    let target = iroh_tickets::endpoint::EndpointTicket::new(iroh::EndpointAddr::from_parts(
        iroh::SecretKey::generate().public(),
        [iroh::TransportAddr::Ip("127.0.0.1:9".parse().unwrap())],
    ))
    .to_string();
    ws.send(Message::Text(
        json!({"type":"cmd","id":1,"op":"connect_peer","args":{"target":target}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let mut last_seq = 1;
    loop {
        let message = receive(&mut ws).await;
        assert_ne!(
            message["type"], "result",
            "connect completed before connecting event"
        );
        assert_eq!(message["seq"].as_u64().unwrap(), last_seq + 1);
        last_seq += 1;
        if message["event"]["kind"] == "peer_changed" {
            break;
        }
    }
    ws.send(Message::Text(
        json!({"type":"cmd","id":2,"op":"network_info","args":{}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    loop {
        let message = receive(&mut ws).await;
        if message["type"] == "result" {
            assert_eq!(message["id"], 2);
            break;
        }
        last_seq += 1;
        assert_eq!(message["seq"], last_seq);
    }
    ws.close(None).await.unwrap();
    // Drain records already delivered before the close handshake. They are not
    // lost events; abandoning them here would manufacture a reconnect gap.
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(message)) = ws.next().await {
            match message {
                Message::Text(text) => {
                    let record: Value = serde_json::from_str(&text).unwrap();
                    if record["type"] == "event" {
                        last_seq += 1;
                        assert_eq!(record["seq"], last_seq);
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    drop(ws);
    // Wait for control cleanup, with a bounded retry only for Busy.
    let copy = slot.clone();
    let startup = tokio::spawn(async move {
        loop {
            match copy
                .test_hooks_start("ws://127.0.0.1:47000".into(), "fixture".into())
                .await
            {
                Err(error) if error.code == ErrorCode::Busy => tokio::task::yield_now().await,
                result => return result,
            }
        }
    });
    let mut ws = accept_async(
        tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap()
            .0,
    )
    .await
    .unwrap();
    assert_eq!(receive(&mut ws).await["instance"], core.instance_id);
    ws.send(Message::Text(
        json!({"type":"config","relay":null}).to_string().into(),
    ))
    .await
    .unwrap();
    startup.await.unwrap().unwrap();
    let event = receive(&mut ws).await;
    assert_eq!(event["seq"], last_seq + 1);
    assert_eq!(info(&network).await.state, NetworkState::Online);
    assert_eq!(info(&network).await.endpoint_id, started.endpoint_id);
    ws.close(None).await.unwrap();
    network.execute(NetworkOp::Stop).await.unwrap();
    drop(slot);
    let fresh = Arc::new(CoreSlot::new());
    let fresh_bus = EventBus::new();
    let fresh_core = fresh
        .start(
            CoreConfig {
                data_dir: dir.path().into(),
            },
            &fresh_bus,
        )
        .unwrap();
    assert_ne!(fresh_core.instance_id, core.instance_id);
    let copy = fresh.clone();
    let startup = tokio::spawn(async move {
        copy.test_hooks_start("ws://127.0.0.1:47000".into(), "fixture".into())
            .await
    });
    let mut ws = accept_async(
        tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap()
            .0,
    )
    .await
    .unwrap();
    receive(&mut ws).await;
    ws.send(Message::Text(
        json!({"type":"config","relay":null}).to_string().into(),
    ))
    .await
    .unwrap();
    startup.await.unwrap().unwrap();
    assert_eq!(receive(&mut ws).await["seq"], 1);
    struct Errors(tokio::sync::mpsc::Sender<chat_core::CoreEvent>);
    impl chat_core::EventSink for Errors {
        fn send(&self, event: chat_core::CoreEvent) -> bool {
            if matches!(event, chat_core::CoreEvent::Error { .. }) {
                let _ = self.0.try_send(event);
            }
            true
        }
    }
    let (tx, mut errors) = tokio::sync::mpsc::channel(8);
    fresh_bus.subscribe(Arc::new(Errors(tx)));
    // Stop consuming the websocket and publish faster than its bounded sink can drain.
    for _ in 0..10000 {
        fresh_bus.publish(chat_core::CoreEvent::ProbeTextReceived {
            peer_id: "fixture".into(),
            text: "x".repeat(4096),
        });
    }
    let error = tokio::time::timeout(Duration::from_secs(8), errors.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(error, chat_core::CoreEvent::Error { code: ErrorCode::TransportFailed, detail } if detail == "control event queue overflow")
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(message)) = ws.next().await {
            if matches!(message, Message::Close(_)) {
                return;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn offline_preserves_reconnect_intent_but_never_replays_failed_text() {
    use chat_core::{CoreEvent, EventSink, PeerPhase};
    use std::sync::Arc;
    struct Sink(tokio::sync::mpsc::Sender<CoreEvent>);
    impl EventSink for Sink {
        fn send(&self, event: CoreEvent) -> bool {
            self.0.try_send(event).is_ok()
        }
    }
    let a = CoreSlot::new();
    let b = CoreSlot::new();
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let bus = EventBus::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    bus.subscribe(Arc::new(Sink(tx)));
    a.start(
        CoreConfig {
            data_dir: ad.path().into(),
        },
        &EventBus::new(),
    )
    .unwrap();
    b.start(
        CoreConfig {
            data_dir: bd.path().into(),
        },
        &bus,
    )
    .unwrap();
    let an = a.network().unwrap();
    let bn = b.network().unwrap();
    an.configure_local(None).await.unwrap();
    bn.configure_local(None).await.unwrap();
    let ai = online(&an).await;
    let bi = online(&bn).await;
    an.execute(NetworkOp::Connect {
        target: bi.ticket.unwrap(),
    })
    .await
    .unwrap();
    let id = bi.endpoint_id.unwrap();
    an.set_offline(true).await.unwrap();
    assert_eq!(
        an.execute(NetworkOp::Send {
            peer_id: id.clone(),
            text: "failed-text".into()
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::PeerNotConnected
    );
    an.set_offline(true).await.unwrap(); // idempotent offline does not discard intent
    an.set_offline(false).await.unwrap();
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let snapshot = info(&an).await;
            if snapshot
                .peers
                .iter()
                .any(|p| p.phase == PeerPhase::Connected)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(info(&an).await.endpoint_id, ai.endpoint_id);
    an.execute(NetworkOp::Send {
        peer_id: id,
        text: "fresh-text".into(),
    })
    .await
    .unwrap();
    loop {
        if let CoreEvent::ProbeTextReceived { text, .. } =
            tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap()
        {
            assert_eq!(text, "fresh-text");
            break;
        }
    }
    an.set_offline(true).await.unwrap();
    an.execute(NetworkOp::Stop).await.unwrap();
    an.set_offline(false).await.unwrap();
    assert_eq!(info(&an).await.state, NetworkState::Stopped);
    assert!(
        info(&an)
            .await
            .peers
            .iter()
            .all(|p| p.phase == PeerPhase::Disconnected)
    );
    bn.execute(NetworkOp::Stop).await.unwrap();
    online(&an).await;
    assert!(
        info(&an)
            .await
            .peers
            .iter()
            .all(|p| p.phase == PeerPhase::Disconnected)
    );
    an.execute(NetworkOp::Stop).await.unwrap();
}
