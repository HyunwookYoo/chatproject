//! Task 18: a local network with a relay connects through a ticket that names only the relay,
//! like the local e2e relay (no QUIC on the relay, as `chat_node relay` runs it).
use chat_core::EventBus;
use chat_core::transport::{
    NetworkHandle, NetworkInfo, NetworkOp, NetworkProfile, NetworkReply, NetworkState, PeerPhase,
};
use std::time::Duration;

async fn online(net: &NetworkHandle) -> NetworkInfo {
    net.execute(NetworkOp::Start).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let NetworkReply::Info(info) = net.execute(NetworkOp::Info).await.unwrap() else {
                panic!()
            };
            if info.state == NetworkState::Online {
                return info;
            }
            assert_ne!(info.state, NetworkState::Failed);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_only_ticket_connects_within_a_second() {
    let mut config = iroh_relay::server::ServerConfig::default();
    config.relay = Some(iroh_relay::server::RelayConfig::new(
        "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap(),
    ));
    config.quic = None;
    config.metrics_addr = None;
    let relay = iroh_relay::server::Server::spawn(config).await.unwrap();
    let url: iroh::RelayUrl = format!("http://{}", relay.http_addr().unwrap()).parse().unwrap();
    let spawn = || {
        NetworkHandle::spawn(
            &tokio::runtime::Handle::current(),
            NetworkProfile::Local { relay: Some(url.clone()) },
            EventBus::new(),
        )
    };
    let (a, b) = (spawn(), spawn());
    online(&a).await;
    let ticket = online(&b).await.ticket.unwrap();
    let NetworkReply::Peer(peer) = tokio::time::timeout(
        Duration::from_secs(1),
        a.execute(NetworkOp::Connect { target: ticket }),
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!()
    };
    assert_eq!(peer.phase, PeerPhase::Connected);
    a.execute(NetworkOp::Stop).await.unwrap();
    b.execute(NetworkOp::Stop).await.unwrap();
}
