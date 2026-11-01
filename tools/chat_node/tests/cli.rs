#![cfg(all(feature = "harness", debug_assertions))]

use std::{net::TcpListener, process::Stdio, time::Duration};
use tokio::process::Command;

fn node() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_chat_node"));
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP);
    command
}

async fn stop(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{
            CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent, GetConsoleProcessList,
        };
        let mut processes = [0; 8];
        assert_ne!(
            GetConsoleProcessList(processes.as_mut_ptr(), processes.len() as u32),
            0,
            "graceful signal generator must own a console"
        );
        assert_ne!(
            GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id().unwrap()),
            0,
            "targeted shutdown signal failed"
        );
    }
    #[cfg(unix)]
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().unwrap().to_string()])
            .status()
            .await
            .unwrap()
            .success()
    );
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "owned participant must exit cleanly");
}

type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;
struct Participant {
    socket: Socket,
    id: u64,
    seq: u64,
    instance: serde_json::Value,
    events: Vec<serde_json::Value>,
}
impl Participant {
    async fn accept(listener: &tokio::net::TcpListener, name: &str) -> Self {
        use futures_util::SinkExt;
        let stream = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap()
            .0;
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let hello = receive(&mut socket).await;
        assert_eq!(hello["type"], "hello");
        assert_eq!(hello["name"], name);
        socket
            .send(tokio_tungstenite::tungstenite::Message::text(
                serde_json::json!({"type":"config","relay":"http://127.0.0.1:3340"}).to_string(),
            ))
            .await
            .unwrap();
        Self {
            socket,
            id: 0,
            seq: 0,
            instance: hello["instance"].clone(),
            events: vec![],
        }
    }
    async fn next(&mut self) -> serde_json::Value {
        let record = receive(&mut self.socket).await;
        if record["type"] == "event" {
            self.seq += 1;
            assert_eq!(record["seq"], self.seq);
            assert_eq!(record["instance"], self.instance);
            self.events.push(record["event"].clone());
        }
        record
    }
    async fn rpc(&mut self, op: &str, args: serde_json::Value) -> serde_json::Value {
        use futures_util::SinkExt;
        self.id += 1;
        self.socket
            .send(tokio_tungstenite::tungstenite::Message::text(
                serde_json::json!({"type":"cmd","id":self.id,"op":op,"args":args}).to_string(),
            ))
            .await
            .unwrap();
        loop {
            let record = self.next().await;
            if record["type"] == "result" {
                assert_eq!(record["id"], self.id);
                assert_eq!(record["ok"], true, "RPC {op} must succeed");
                return record["value"].clone();
            }
        }
    }
    async fn online(&mut self) -> serde_json::Value {
        self.rpc("network_start", serde_json::json!({})).await;
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let info = self.rpc("network_info", serde_json::json!({})).await;
                if info["state"] == "Online" {
                    return info;
                }
                assert_ne!(info["state"], "Failed");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn body(&mut self, expected: &str) {
        while !self
            .events
            .iter()
            .any(|event| event["kind"] == "probe_text_received" && event["text"] == expected)
        {
            self.next().await;
        }
    }
}
async fn receive(socket: &mut Socket) -> serde_json::Value {
    use futures_util::StreamExt;
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(15), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
            return serde_json::from_str(&text).unwrap();
        }
    }
}
async fn relay(bind: &str) -> tokio::process::Child {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut child = node()
        .args(["relay", "--bind", bind])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap(),
        serde_json::json!({"type":"ready","role":"relay"})
    );
    child
}

#[tokio::test]
async fn relay_readiness_and_graceful_owned_shutdown() {
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reservation.local_addr().unwrap();
    drop(reservation);
    let mut child = relay(&addr.to_string()).await;
    assert!(tokio::net::TcpStream::connect(addr).await.is_ok());
    stop(&mut child).await;
    assert!(TcpListener::bind(addr).is_ok());
}

#[tokio::test]
async fn saved_address_without_relay_cannot_use_a_later_added_relay_hint() {
    let alpn = b"chatproject/m2/1";
    let receiver = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::empty()))
        .clear_ip_transports()
        .alpns(vec![alpn.to_vec()])
        .bind().await.unwrap();
    // Owned address snapshots prove route-hint consequences; serialization is
    // covered by the actual manager test, not this SDK-only characterization.
    let old = receiver.addr();
    assert!(old.addrs.is_empty());
    assert!(tokio::time::timeout(Duration::from_millis(30), receiver.online()).await.is_err());
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reservation.local_addr().unwrap();
    drop(reservation);
    let mut relay_child = relay(&addr.to_string()).await;
    let url: iroh::RelayUrl = format!("http://{addr}").parse().unwrap();
    let map = iroh::RelayMap::from_iter([url.clone()]);
    receiver.insert_relay(url.clone(), map.get(&url).unwrap()).await;
    tokio::time::timeout(Duration::from_secs(10), receiver.online()).await.unwrap();
    let current = receiver.addr();
    assert!(current.addrs.contains(&iroh::TransportAddr::Relay(url)));
    assert!(old.addrs.is_empty());
    let sender = || iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Custom(map.clone()))
        .clear_ip_transports();
    let old_sender = sender().bind().await.unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(2), old_sender.connect(old, alpn)).await.unwrap().is_err());
    old_sender.close().await;
    let fresh_sender = sender().bind().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        let (dialed, accepted) = tokio::join!(fresh_sender.connect(current, alpn), async {
            receiver.accept().await.unwrap().await.unwrap()
        });
        let dialed = dialed.unwrap();
        let mut send = dialed.open_uni().await.unwrap();
        send.write_all(b"CPM2").await.unwrap();
        let body = "fresh relay";
        send.write_all(&(body.len() as u32).to_be_bytes()).await.unwrap();
        send.write_all(body.as_bytes()).await.unwrap();
        let mut recv = accepted.accept_uni().await.unwrap();
        let mut magic = [0; 4];
        recv.read_exact(&mut magic).await.unwrap();
        assert_eq!(&magic, b"CPM2");
        let mut length = [0; 4];
        recv.read_exact(&mut length).await.unwrap();
        assert_eq!(u32::from_be_bytes(length), body.len() as u32);
        let mut received = vec![0; body.len()];
        recv.read_exact(&mut received).await.unwrap();
        assert_eq!(received, body.as_bytes());
        assert!(dialed.paths().iter().any(|path| path.is_selected() && matches!(path.remote_addr(), iroh::TransportAddr::Relay(_))));
    }).await.unwrap();
    fresh_sender.close().await;
    receiver.close().await;
    stop(&mut relay_child).await;
    assert!(TcpListener::bind(addr).is_ok());
}

#[tokio::test]
async fn hooked_pair_exchanges_utf8_and_actual_core_selects_relay_without_direct_peers() {
    use serde_json::json;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:47000")
        .await
        .unwrap();
    let mut relay_child = relay("127.0.0.1:3340").await;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    // A controller may never send config. Shutdown must still be owned and
    // graceful while hook startup is awaiting that response.
    let mut pending_child = node()
        .args([
            "puppet",
            "--control-url",
            "ws://127.0.0.1:47000",
            "--name",
            "pending",
            "--data-dir",
            ad.path().to_str().unwrap(),
        ])
        .spawn()
        .unwrap();
    let mut pending_socket = tokio_tungstenite::accept_async(listener.accept().await.unwrap().0)
        .await
        .unwrap();
    assert_eq!(receive(&mut pending_socket).await["type"], "hello");
    stop(&mut pending_child).await;
    let mut a_child = node()
        .args([
            "puppet",
            "--control-url",
            "ws://127.0.0.1:47000",
            "--name",
            "a",
            "--data-dir",
            ad.path().to_str().unwrap(),
        ])
        .spawn()
        .unwrap();
    let mut a = Participant::accept(&listener, "a").await;
    let mut b_child = node()
        .args([
            "puppet",
            "--control-url",
            "ws://127.0.0.1:47000",
            "--name",
            "b",
            "--data-dir",
            bd.path().to_str().unwrap(),
        ])
        .spawn()
        .unwrap();
    let mut b = Participant::accept(&listener, "b").await;
    let ai = a.online().await;
    let bi = b.online().await;
    a.rpc("connect_peer", json!({"target":bi["ticket"]})).await;
    a.rpc(
        "send_probe_text",
        json!({"peer_id":bi["endpoint_id"],"text":"안녕하세요 🦀"}),
    )
    .await;
    b.body("안녕하세요 🦀").await;
    b.rpc(
        "send_probe_text",
        json!({"peer_id":ai["endpoint_id"],"text":"반갑습니다 🌍"}),
    )
    .await;
    a.body("반갑습니다 🌍").await;
    a.rpc("disconnect_peer", json!({"peer_id":bi["endpoint_id"]}))
        .await;
    b.rpc("network_stop", json!({})).await;

    // A real pinned SDK peer disables IP transports. This makes selected Relay
    // deterministic without adding a product relay-only switch.
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from_iter([
            "http://127.0.0.1:3340".parse::<iroh::RelayUrl>().unwrap(),
        ])))
        .clear_ip_transports()
        .bind()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), endpoint.online())
        .await
        .unwrap();
    let target = iroh::EndpointAddr::from_parts(
        ai["endpoint_id"]
            .as_str()
            .unwrap()
            .parse::<iroh::EndpointId>()
            .unwrap(),
        [iroh::TransportAddr::Relay(
            "http://127.0.0.1:3340".parse().unwrap(),
        )],
    );
    let connection = endpoint.connect(target, b"chatproject/m2/1").await.unwrap();
    let mut send = connection.open_uni().await.unwrap();
    send.write_all(b"CPM2").await.unwrap();
    let mut recv = tokio::time::timeout(Duration::from_secs(10), connection.accept_uni())
        .await
        .unwrap()
        .unwrap();
    let mut magic = [0; 4];
    tokio::time::timeout(Duration::from_secs(10), recv.read_exact(&mut magic))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&magic, b"CPM2");
    let raw_id = endpoint.id().to_string();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let info = a.rpc("network_info", json!({})).await;
            if info["peers"].as_array().unwrap().iter().any(|peer| {
                peer["peer_id"] == raw_id && peer["phase"] == "Connected" && peer["path"] == "Relay"
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let body = "relay 전달";
    send.write_all(&(body.len() as u32).to_be_bytes())
        .await
        .unwrap();
    send.write_all(body.as_bytes()).await.unwrap();
    a.body(body).await;
    assert_eq!(
        a.events
            .iter()
            .rev()
            .find(|event| event["kind"] == "connection_state")
            .unwrap()["direct_peers"],
        0
    );
    a.rpc(
        "send_probe_text",
        json!({"peer_id":raw_id,"text":"relay 응답"}),
    )
    .await;
    let mut length = [0; 4];
    tokio::time::timeout(Duration::from_secs(10), recv.read_exact(&mut length))
        .await
        .unwrap()
        .unwrap();
    let mut body = vec![0; u32::from_be_bytes(length) as usize];
    tokio::time::timeout(Duration::from_secs(10), recv.read_exact(&mut body))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(String::from_utf8(body).unwrap(), "relay 응답");
    endpoint.close().await;
    a.rpc("network_stop", json!({})).await;
    stop(&mut a_child).await;
    stop(&mut b_child).await;
    stop(&mut relay_child).await;
}

#[tokio::test]
async fn help_succeeds_and_puppet_requires_name_and_absolute_data_dir() {
    assert!(
        node()
            .arg("--help")
            .output()
            .await
            .unwrap()
            .status
            .success()
    );
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec![
            "puppet",
            "--control-url",
            "ws://127.0.0.1:47000",
            "--data-dir",
            dir.path().to_str().unwrap(),
        ],
        vec![
            "puppet",
            "--control-url",
            "ws://127.0.0.1:47000",
            "--name",
            "node",
            "--data-dir",
            "relative",
        ],
    ] {
        let output = tokio::time::timeout(Duration::from_secs(5), node().args(args).output())
            .await
            .unwrap()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

#[tokio::test]
async fn occupied_relay_port_fails_without_readiness_and_keeps_holder_alive() {
    let holder = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = holder.local_addr().unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        node()
            .args(["relay", "--bind", &addr.to_string()])
            .stdout(Stdio::piped())
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(std::net::TcpStream::connect(addr).is_ok());
}
