#![cfg(all(feature = "test-hooks", debug_assertions))]
use chat_core::account::{
    AccountPhase, AccountServices, Secret32, StoreId, encode_recovery,
    storage::{KeyProtector, Store, StoredPlaintext},
};
use chat_core::{CoreConfig, CoreError, CoreSlot, ErrorCode, EventBus, NetworkOp, NetworkReply};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};
#[path = "support/account_store.rs"]
#[allow(dead_code)]
mod support;
type Socket = WebSocketStream<tokio::net::TcpStream>;
async fn receive(socket: &mut Socket) -> Value {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(text) = frame {
            return serde_json::from_str(&text).unwrap();
        }
    }
}
async fn connect(
    listener: &tokio::net::TcpListener,
    slot: Arc<CoreSlot>,
) -> (Socket, tokio::task::JoinHandle<Result<(), CoreError>>) {
    let start = tokio::spawn(async move {
        slot.test_hooks_start("ws://127.0.0.1:47000".into(), "account-fixture".into())
            .await
    });
    let stream = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap()
        .0;
    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
    assert_eq!(receive(&mut socket).await["type"], "hello");
    socket
        .send(Message::Text(
            json!({"type":"config", "relay":null, "records":"ws://127.0.0.1:47000/records"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    (socket, start)
}
fn start(root: &std::path::Path) -> Arc<CoreSlot> {
    let slot = Arc::new(CoreSlot::new());
    slot.start(
        CoreConfig {
            data_dir: root.into(),
        },
        &EventBus::new(),
    )
    .unwrap();
    slot
}

// One sequential owner of port 47000 in this test binary. All seed material is
// the existing public zero-entropy fixture; no generated phrase is printed.
#[tokio::test]
async fn account_fixture_is_installed_before_open_retained_after_busy_and_rejects_late_install() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:47000")
        .await
        .unwrap();
    let late_dir = tempfile::tempdir().unwrap();
    let late = start(late_dir.path());
    late.network().unwrap().configure_local(None).await.unwrap();
    let late_actor = late
        .open_account_with_services(support::protector(), AccountServices::offline_for_test())
        .unwrap();
    late_actor.prepare_candidate().await.unwrap();
    let (mut late_socket, late_start) = connect(&listener, late.clone()).await;
    assert_eq!(
        late_start.await.unwrap().unwrap_err().code,
        ErrorCode::InvalidConfig
    );
    late.network().unwrap().configure_local(None).await.unwrap();
    assert_eq!(
        late_actor.snapshot().await.unwrap().phase,
        AccountPhase::Candidate
    );
    let _ = late_socket.close(None).await;
    drop(late_actor);
    drop(late);

    let dir = tempfile::tempdir().unwrap();
    let busy = Store::open(dir.path(), support::protector()).unwrap();
    let slot = start(dir.path());
    let (mut socket, startup) = connect(&listener, slot.clone()).await;
    startup.await.unwrap().unwrap();
    assert_eq!(
        slot.open_account(support::protector()).err().unwrap().code,
        ErrorCode::WriterBusy
    );
    drop(busy);
    let actor = slot.open_account(support::protector()).unwrap();
    assert_eq!(
        actor.snapshot().await.unwrap().phase,
        AccountPhase::Unconfigured
    );
    socket
        .send(Message::Text(
            json!({"type":"cmd","id":1,"op":"account_local_prepare_candidate","args":{}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    loop {
        let reply = receive(&mut socket).await;
        if reply["type"] == "result" {
            assert_eq!(reply["ok"], true);
            assert_eq!(
                reply["value"]["installation_id"],
                json!(actor.snapshot().await.unwrap().installation)
            );
            break;
        }
    }
    // A separate /records connection proves failed opening did not consume the
    // injected backend. The normal actor signs; this fixture only transports it.
    let backend = tokio::spawn(async move {
        let mut published = None;
        for _ in 0..3 {
            let stream = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap()
                .0;
            let mut record = tokio_tungstenite::accept_hdr_async(
                stream,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert_eq!(request.uri().path(), "/records");
                    Ok(response)
                },
            )
            .await
            .unwrap();
            let request = receive(&mut record).await;
            let response = if request["op"] == "lookup" {
                published
                    .as_ref()
                    .map_or(json!({"absent":true}), |packet| json!({"packet":packet}))
            } else {
                assert_eq!(request["op"], "publish");
                let bytes: Vec<u8> = serde_json::from_value(request["packet"].clone()).unwrap();
                assert!((104..=1000).contains(&bytes.len()));
                let key = pkarr::PublicKey::try_from(&bytes[..32]).unwrap();
                pkarr::SignedPacket::from_relay_payload(&key, &bytes[32..].to_vec().into())
                    .unwrap();
                published = Some(bytes);
                json!({"ok":true})
            };
            record
                .send(Message::Text(response.to_string().into()))
                .await
                .unwrap();
            let _ = record.close(None).await;
        }
        assert!(published.is_some());
    });
    actor
        .recover(encode_recovery(&Secret32::from([0; 32])).unwrap(), true)
        .await
        .unwrap();
    let public = actor.snapshot().await.unwrap();
    actor
        .refresh_record(public.public_peer_id.unwrap())
        .await
        .unwrap();
    backend.await.unwrap();
    assert_eq!(actor.snapshot().await.unwrap().phase, AccountPhase::Active);
    let NetworkReply::Info(info) = slot
        .network()
        .unwrap()
        .execute(NetworkOp::Info)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(info.endpoint_id.is_some());
    socket.send(Message::Text(json!({"type":"cmd","id":2,"op":"account_local_route","args":{"ticket":info.ticket.unwrap()}}).to_string().into())).await.unwrap();
    loop {
        let reply = receive(&mut socket).await;
        if reply["type"] == "result" {
            assert_eq!(reply["ok"], true);
            break;
        }
    }
    socket
        .send(Message::Text(
            json!({"type":"config", "relay":null, "records":"ws://127.0.0.1:47000/records"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(frame)) = socket.next().await {
            if matches!(frame, Message::Close(_)) {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(actor.snapshot().await.unwrap().phase, AccountPhase::Active);
}
