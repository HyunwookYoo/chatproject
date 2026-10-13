#![cfg(debug_assertions)]

use chat_core::transport::{
    NetworkHandle, NetworkInfo, NetworkOp, NetworkProfile, NetworkReply, NetworkState, PeerPhase,
};
use chat_core::{CoreEvent, ErrorCode, EventBus, EventSink};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;
struct Sink {
    tx: mpsc::Sender<CoreEvent>,
    overflow: Arc<AtomicBool>,
}
impl EventSink for Sink {
    fn send(&self, event: CoreEvent) -> bool {
        if self.tx.try_send(event).is_err() {
            self.overflow.store(true, Ordering::SeqCst);
            return false;
        }
        true
    }
}
fn network() -> (NetworkHandle, mpsc::Receiver<CoreEvent>, Arc<AtomicBool>) {
    let bus = EventBus::new();
    let (tx, rx) = mpsc::channel(256);
    let overflow = Arc::new(AtomicBool::new(false));
    bus.subscribe(Arc::new(Sink {
        tx,
        overflow: overflow.clone(),
    }));
    (
        NetworkHandle::spawn(
            &tokio::runtime::Handle::current(),
            NetworkProfile::Local { relay: None },
            bus,
        ),
        rx,
        overflow,
    )
}
async fn online(net: &NetworkHandle) -> NetworkInfo {
    net.execute(NetworkOp::Start).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let NetworkReply::Info(info) = net.execute(NetworkOp::Info).await.unwrap() else {
                panic!()
            };
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
async fn received(rx: &mut mpsc::Receiver<CoreEvent>, expected: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(CoreEvent::ProbeTextReceived { text, .. }) = rx.recv().await {
                assert_eq!(text, expected);
                break;
            }
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn local_pair_connects_without_discovery() {
    let (a, mut ar, ao) = network();
    let (b, mut br, bo) = network();
    let ai = online(&a).await;
    let bi = online(&b).await;
    let NetworkReply::Peer(peer) = a
        .execute(NetworkOp::Connect {
            target: bi.ticket.unwrap(),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(peer.phase, PeerPhase::Connected);
    assert_ne!(ai.endpoint_id, Some(peer.peer_id.clone()));
    assert!(peer.connect_ms.is_some());
    a.execute(NetworkOp::Send {
        peer_id: peer.peer_id.clone(),
        text: "안녕하세요".into(),
    })
    .await
    .unwrap();
    received(&mut br, "안녕하세요").await;
    b.execute(NetworkOp::Send {
        peer_id: ai.endpoint_id.unwrap(),
        text: "반갑습니다".into(),
    })
    .await
    .unwrap();
    received(&mut ar, "반갑습니다").await;
    a.execute(NetworkOp::Stop).await.unwrap();
    b.execute(NetworkOp::Stop).await.unwrap();
    assert!(!ao.load(Ordering::SeqCst) && !bo.load(Ordering::SeqCst));
}
#[tokio::test]
async fn invalid_self_and_stopped_addresses_are_rejected() {
    let (a, _, _) = network();
    assert_eq!(
        a.execute(NetworkOp::Connect {
            target: "bad".into()
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::NetworkNotStarted
    );
    let ai = online(&a).await;
    assert_eq!(
        a.execute(NetworkOp::Connect {
            target: "bad".into()
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::InvalidPeerAddress
    );
    assert_eq!(
        a.execute(NetworkOp::Connect {
            target: ai.endpoint_id.unwrap()
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::SelfConnection
    );
    a.execute(NetworkOp::Stop).await.unwrap();
}

async fn info(net: &NetworkHandle) -> NetworkInfo {
    let NetworkReply::Info(info) = net.execute(NetworkOp::Info).await.unwrap() else {
        panic!()
    };
    info
}
async fn wait_connected(net: &NetworkHandle, peer_id: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if info(net)
                .await
                .peers
                .iter()
                .any(|p| p.peer_id == peer_id && p.phase == PeerPhase::Connected)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn simultaneous_connects_converge_and_disconnected_text_is_not_replayed() {
    let (a, mut ar, _) = network();
    let (b, mut br, _) = network();
    let ai = online(&a).await;
    let bi = online(&b).await;
    let (ac, bc) = tokio::join!(
        a.execute(NetworkOp::Connect {
            target: bi.ticket.clone().unwrap()
        }),
        b.execute(NetworkOp::Connect {
            target: ai.ticket.unwrap()
        })
    );
    ac.unwrap();
    bc.unwrap();
    let aid = ai.endpoint_id.unwrap();
    let bid = bi.endpoint_id.unwrap();
    wait_connected(&a, &bid).await;
    wait_connected(&b, &aid).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(info(&a).await.peers.len(), 1);
    assert_eq!(info(&b).await.peers.len(), 1);
    a.execute(NetworkOp::Send {
        peer_id: bid.clone(),
        text: "하나".into(),
    })
    .await
    .unwrap();
    received(&mut br, "하나").await;
    b.execute(NetworkOp::Send {
        peer_id: aid.clone(),
        text: "둘".into(),
    })
    .await
    .unwrap();
    received(&mut ar, "둘").await;
    a.execute(NetworkOp::Disconnect {
        peer_id: bid.clone(),
    })
    .await
    .unwrap();
    assert_eq!(
        a.execute(NetworkOp::Send {
            peer_id: bid.clone(),
            text: "실패".into()
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::PeerNotConnected
    );
    a.execute(NetworkOp::Connect {
        target: bi.ticket.unwrap(),
    })
    .await
    .unwrap();
    a.execute(NetworkOp::Send {
        peer_id: bid,
        text: "새 글".into(),
    })
    .await
    .unwrap();
    received(&mut br, "새 글").await;
    a.execute(NetworkOp::Stop).await.unwrap();
    b.execute(NetworkOp::Stop).await.unwrap();
}

async fn raw_endpoint(alpn: &[u8]) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![alpn.to_vec()])
        .bind()
        .await
        .unwrap()
}
fn raw_ticket(endpoint: &iroh::Endpoint) -> String {
    let addrs = endpoint.bound_sockets().into_iter().map(|mut addr| {
        if addr.ip().is_unspecified() {
            addr.set_ip(if addr.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        iroh::TransportAddr::Ip(addr)
    });
    iroh_tickets::endpoint::EndpointTicket::new(iroh::EndpointAddr::from_parts(
        endpoint.id(),
        addrs,
    ))
    .to_string()
}
#[tokio::test]
async fn wrong_alpn_and_magic_are_rejected() {
    let (a, _, _) = network();
    online(&a).await;
    let wrong = raw_endpoint(b"wrong").await;
    let connecting = a.execute(NetworkOp::Connect {
        target: raw_ticket(&wrong),
    });
    let incoming = async {
        let _ = wrong.accept().await.unwrap().await;
    };
    let (reply, _) = tokio::join!(connecting, incoming);
    assert_eq!(reply.unwrap_err().code, ErrorCode::TransportFailed);
    wrong.close().await;
    let wrong = raw_endpoint(chat_core::transport::frame::ALPN).await;
    let incoming = async {
        let conn = wrong.accept().await.unwrap().await.unwrap();
        let mut stream = conn.open_uni().await.unwrap();
        stream.write_all(b"BAD!").await.unwrap();
        conn.closed().await;
    };
    let (reply, _) = tokio::join!(
        a.execute(NetworkOp::Connect {
            target: raw_ticket(&wrong)
        }),
        incoming
    );
    assert_eq!(reply.unwrap_err().code, ErrorCode::TransportFailed);
    a.execute(NetworkOp::Stop).await.unwrap();
    wrong.close().await;
}
#[tokio::test]
async fn extra_stream_and_malformed_frame_close_the_session() {
    for extra in [true, false] {
        let (a, _, _) = network();
        let ai = online(&a).await;
        let raw = raw_endpoint(chat_core::transport::frame::ALPN).await;
        let ticket = ai
            .ticket
            .unwrap()
            .parse::<iroh_tickets::endpoint::EndpointTicket>()
            .unwrap();
        let conn = raw
            .connect(
                ticket.endpoint_addr().clone(),
                chat_core::transport::frame::ALPN,
            )
            .await
            .unwrap();
        let mut writer = conn.open_uni().await.unwrap();
        writer.write_all(b"CPM2").await.unwrap();
        let mut reader = conn.accept_uni().await.unwrap();
        let mut magic = [0; 4];
        reader.read_exact(&mut magic).await.unwrap();
        wait_connected(&a, &raw.id().to_string()).await;
        if extra {
            let mut second = conn.open_uni().await.unwrap();
            second.write_all(b"x").await.unwrap();
        } else {
            writer.write_all(&[0, 0, 0, 1, 255]).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(3), conn.closed())
            .await
            .unwrap();
        a.execute(NetworkOp::Stop).await.unwrap();
        raw.close().await;
    }
}
#[tokio::test]
async fn stop_during_stream_preparation_resolves_pending_dial_and_retains_key() {
    let (a, _, _) = network();
    let first = online(&a).await;
    let raw = raw_endpoint(chat_core::transport::frame::ALPN).await;
    let dial_a = a.clone();
    let target = raw_ticket(&raw);
    let dial = tokio::spawn(async move { dial_a.execute(NetworkOp::Connect { target }).await });
    let connection = raw.accept().await.unwrap().await.unwrap();
    assert_eq!(
        a.execute(NetworkOp::Connect {
            target: raw_ticket(&raw)
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::Busy
    );
    tokio::time::timeout(Duration::from_secs(3), a.execute(NetworkOp::Stop))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        dial.await.unwrap().unwrap_err().code,
        ErrorCode::NetworkNotStarted
    );
    tokio::time::timeout(Duration::from_secs(3), connection.closed())
        .await
        .unwrap();
    let second = online(&a).await;
    assert_eq!(first.endpoint_id, second.endpoint_id);
    assert!(second.revision > first.revision);
    a.execute(NetworkOp::Stop).await.unwrap();
    raw.close().await;
}
#[tokio::test]
async fn eight_pending_peers_reserve_slots_and_stop_cancels_all() {
    let (a, _, _) = network();
    online(&a).await;
    let mut endpoints = vec![];
    let mut dials = vec![];
    let mut connections = vec![];
    for _ in 0..8 {
        let raw = raw_endpoint(chat_core::transport::frame::ALPN).await;
        let net = a.clone();
        let target = raw_ticket(&raw);
        dials.push(tokio::spawn(async move {
            net.execute(NetworkOp::Connect { target }).await
        }));
        connections.push(raw.accept().await.unwrap().await.unwrap());
        endpoints.push(raw);
    }
    let ninth = iroh::SecretKey::generate().public().to_string();
    assert_eq!(
        a.execute(NetworkOp::Connect { target: ninth })
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    a.execute(NetworkOp::Stop).await.unwrap();
    for dial in dials {
        assert_eq!(
            dial.await.unwrap().unwrap_err().code,
            ErrorCode::NetworkNotStarted
        );
    }
    for endpoint in endpoints {
        endpoint.close().await;
    }
}

#[tokio::test]
async fn start_is_idempotent_and_stop_during_relay_startup_is_prompt() {
    let bus = EventBus::new();
    let net = NetworkHandle::spawn(
        &tokio::runtime::Handle::current(),
        NetworkProfile::Local {
            relay: Some("http://127.0.0.1:9".parse().unwrap()),
        },
        bus,
    );
    let NetworkReply::Info(first) = net.execute(NetworkOp::Start).await.unwrap() else {
        panic!()
    };
    let NetworkReply::Info(second) = net.execute(NetworkOp::Start).await.unwrap() else {
        panic!()
    };
    assert_eq!(first.state, NetworkState::Starting);
    assert_eq!(first.revision, second.revision);
    tokio::task::yield_now().await;
    assert_eq!(
        net.execute(NetworkOp::Send {
            peer_id: "none".into(),
            text: "x".into()
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::NetworkNotStarted
    );
    tokio::time::timeout(Duration::from_secs(3), net.execute(NetworkOp::Stop))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info(&net).await.state, NetworkState::Stopped);
}
#[tokio::test]
async fn selected_direct_path_count_and_event_revisions_are_consistent() {
    let (a, mut events, overflow) = network();
    let (b, _, _) = network();
    online(&a).await;
    let bi = online(&b).await;
    a.execute(NetworkOp::Connect {
        target: bi.ticket.unwrap(),
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if info(&a)
                .await
                .peers
                .iter()
                .any(|p| p.path == chat_core::transport::PeerPath::Direct)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let connected = info(&a).await;
    assert!(connected.peers[0].direct_ms >= connected.peers[0].connect_ms);
    a.execute(NetworkOp::Stop).await.unwrap();
    let mut revision = 0;
    let mut direct = 0;
    while let Ok(event) = events.try_recv() {
        match event {
            CoreEvent::NetworkChanged(info) => {
                assert!(info.revision > revision);
                revision = info.revision;
                direct = info
                    .peers
                    .iter()
                    .filter(|p| {
                        p.phase == PeerPhase::Connected
                            && p.path == chat_core::transport::PeerPath::Direct
                    })
                    .count() as u32;
            }
            CoreEvent::PeerChanged {
                revision: peer_revision,
                ..
            } => assert_eq!(revision, peer_revision),
            CoreEvent::ConnectionState(snapshot) => assert_eq!(snapshot.direct_peers, direct),
            _ => {}
        }
    }
    assert!(!overflow.load(Ordering::SeqCst));
    b.execute(NetworkOp::Stop).await.unwrap();
}
#[tokio::test]
async fn preparation_deadline_closes_the_connection() {
    let (a, _, _) = network();
    online(&a).await;
    let raw = raw_endpoint(chat_core::transport::frame::ALPN).await;
    let net = a.clone();
    let target = raw_ticket(&raw);
    let dial = tokio::spawn(async move { net.execute(NetworkOp::Connect { target }).await });
    let connection = raw.accept().await.unwrap().await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(12), dial)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NetworkTimeout);
    tokio::time::timeout(Duration::from_secs(1), connection.closed())
        .await
        .expect("timed-out preparation left connection open");
    a.execute(NetworkOp::Stop).await.unwrap();
    raw.close().await;
}

#[tokio::test]
async fn stop_interrupts_a_dial_without_blocking_info() {
    let (a, _, _) = network();
    online(&a).await;
    let blackhole = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target = iroh_tickets::endpoint::EndpointTicket::new(iroh::EndpointAddr::from_parts(
        iroh::SecretKey::generate().public(),
        [iroh::TransportAddr::Ip(blackhole.local_addr().unwrap())],
    ))
    .to_string();
    let net = a.clone();
    let dial = tokio::spawn(async move { net.execute(NetworkOp::Connect { target }).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if !info(&a).await.peers.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let stopping = a.clone();
    let stop = tokio::spawn(async move { stopping.execute(NetworkOp::Stop).await });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), dial)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code,
        ErrorCode::NetworkNotStarted
    );
    // iroh close drains unacknowledged QUIC handshakes for up to ~3 seconds.
    tokio::time::timeout(Duration::from_secs(5), stop)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn lone_connection_from_larger_id_is_kept() {
    let (a, mut ar, _) = network();
    let (b, mut br, _) = network();
    let ai = online(&a).await;
    let bi = online(&b).await;
    let aid = ai
        .endpoint_id
        .clone()
        .unwrap()
        .parse::<iroh::EndpointId>()
        .unwrap();
    let bid = bi
        .endpoint_id
        .clone()
        .unwrap()
        .parse::<iroh::EndpointId>()
        .unwrap();
    let (larger, smaller, target, peer_id, receiver) = if aid > bid {
        (&a, &b, bi.ticket.unwrap(), bid.to_string(), &mut br)
    } else {
        (&b, &a, ai.ticket.unwrap(), aid.to_string(), &mut ar)
    };
    larger.execute(NetworkOp::Connect { target }).await.unwrap();
    larger
        .execute(NetworkOp::Send {
            peer_id,
            text: "단독 연결".into(),
        })
        .await
        .unwrap();
    received(receiver, "단독 연결").await;
    assert_eq!(info(smaller).await.peers[0].phase, PeerPhase::Connected);
    a.execute(NetworkOp::Stop).await.unwrap();
    b.execute(NetworkOp::Stop).await.unwrap();
}

#[tokio::test]
async fn incoming_preparation_is_bounded_and_stop_joins_idle_readers() {
    let (a, _, _) = network();
    let ai = online(&a).await;
    let target = ai
        .ticket
        .unwrap()
        .parse::<iroh_tickets::endpoint::EndpointTicket>()
        .unwrap()
        .endpoint_addr()
        .clone();
    let mut endpoints = vec![];
    let mut connections = vec![];
    for _ in 0..8 {
        let endpoint = raw_endpoint(chat_core::transport::frame::ALPN).await;
        connections.push(
            endpoint
                .connect(target.clone(), chat_core::transport::frame::ALPN)
                .await
                .unwrap(),
        );
        endpoints.push(endpoint);
    }
    let ninth = raw_endpoint(chat_core::transport::frame::ALPN).await;
    assert!(
        ninth
            .connect(target, chat_core::transport::frame::ALPN)
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(3), a.execute(NetworkOp::Stop))
        .await
        .unwrap()
        .unwrap();
    for connection in connections {
        tokio::time::timeout(Duration::from_secs(1), connection.closed())
            .await
            .unwrap();
    }
    for endpoint in endpoints {
        endpoint.close().await;
    }
    ninth.close().await;
}
#[tokio::test]
async fn repeated_stop_emits_offline_without_rebinding_or_new_revision() {
    let (a, mut events, _) = network();
    a.execute(NetworkOp::Stop).await.unwrap();
    assert_eq!(
        events.recv().await.unwrap(),
        CoreEvent::ConnectionState(chat_core::ConnectionSnapshot::OFFLINE)
    );
    let first = info(&a).await;
    a.execute(NetworkOp::Stop).await.unwrap();
    assert_eq!(
        events.recv().await.unwrap(),
        CoreEvent::ConnectionState(chat_core::ConnectionSnapshot::OFFLINE)
    );
    assert_eq!(info(&a).await, first);
    assert_eq!(first.endpoint_id, None);
}

#[tokio::test]
async fn session_loss_reconnects_without_replaying_failed_text() {
    let (a, _, _) = network();
    let (b, mut br, _) = network();
    let ai = online(&a).await;
    let bi = online(&b).await;
    let bid = bi.endpoint_id.unwrap();
    a.execute(NetworkOp::Connect {
        target: bi.ticket.unwrap(),
    })
    .await
    .unwrap();
    b.execute(NetworkOp::Disconnect {
        peer_id: ai.endpoint_id.unwrap(),
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if info(&a).await.peers[0].phase == PeerPhase::Reconnecting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        a.execute(NetworkOp::Send {
            peer_id: bid.clone(),
            text: "오프라인 글".into()
        })
        .await
        .unwrap_err()
        .code,
        ErrorCode::PeerNotConnected
    );
    wait_connected(&a, &bid).await;
    a.execute(NetworkOp::Send {
        peer_id: bid,
        text: "복귀".into(),
    })
    .await
    .unwrap();
    received(&mut br, "복귀").await;
    a.execute(NetworkOp::Stop).await.unwrap();
    b.execute(NetworkOp::Stop).await.unwrap();
}
#[tokio::test]
async fn reconnect_budget_cancels_the_third_inflight_preparation_at_thirty_seconds() {
    let (a, _, _) = network();
    online(&a).await;
    let raw = raw_endpoint(chat_core::transport::frame::ALPN).await;
    let net = a.clone();
    let target = raw_ticket(&raw);
    let dial = tokio::spawn(async move { net.execute(NetworkOp::Connect { target }).await });
    let first = raw.accept().await.unwrap().await.unwrap();
    let mut writer = first.open_uni().await.unwrap();
    writer.write_all(b"CPM2").await.unwrap();
    let mut reader = first.accept_uni().await.unwrap();
    let mut magic = [0; 4];
    reader.read_exact(&mut magic).await.unwrap();
    dial.await.unwrap().unwrap();
    first.close(0u32.into(), b"test reconnect");
    let start = tokio::time::Instant::now();
    let server = async {
        let mut attempts = vec![];
        for _ in 0..3 {
            let connection = raw.accept().await.unwrap().await.unwrap();
            attempts.push(start.elapsed());
            connection.closed().await;
        }
        attempts
    };
    let disconnected = async {
        loop {
            if info(&a).await.peers[0].phase == PeerPhase::Disconnected {
                return start.elapsed();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let (attempts, elapsed) = tokio::time::timeout(Duration::from_secs(34), async {
        tokio::join!(server, disconnected)
    })
    .await
    .unwrap();
    assert_eq!(attempts.len(), 3);
    assert!(attempts[2] >= Duration::from_secs(27));
    assert!(elapsed >= Duration::from_secs(30) && elapsed < Duration::from_secs(33));
    a.execute(NetworkOp::Stop).await.unwrap();
    raw.close().await;
}
