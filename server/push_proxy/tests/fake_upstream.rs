//! The debug harness upstream (`fake-upstream`, Ruling R6): one JSON POST per delivery to a
//! loopback URL, the HTTP status as the result, and no other address accepted.

use push_proxy::{Delivery, DeliveryKind, HarnessUpstream, Upstream, UpstreamResult};
use push_wire::Platform;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn delivery(kind: DeliveryKind) -> Delivery {
    Delivery {
        platform: Platform::Fcm,
        token: "secret-token-never-forwarded".into(),
        tag: [7; 16],
        kind,
    }
}

/// Accepts one connection, answers `status`, and returns the request line and JSON body.
async fn one_request(listener: TcpListener, status: &'static str) -> (String, Value) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut request = Vec::new();
    let mut chunk = [0u8; 1024];
    let body_at = loop {
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0, "request ended early");
        request.extend_from_slice(&chunk[..n]);
        if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8(request[..body_at].to_vec()).unwrap();
    let length: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .unwrap()
        .parse()
        .unwrap();
    while request.len() < body_at + length {
        let n = stream.read(&mut chunk).await.unwrap();
        request.extend_from_slice(&chunk[..n]);
    }
    stream
        .write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let line = head.lines().next().unwrap().to_owned();
    (line, serde_json::from_slice(&request[body_at..]).unwrap())
}

#[tokio::test]
async fn each_kind_is_posted_as_json_without_the_token() {
    for (kind, expected) in [
        (
            DeliveryKind::Cipher("Y2lwaGVy".into()),
            json!({"platform":"fcm","t":"BwcHBwcHBwcHBwcHBwcHBw","kind":"cipher","e":"Y2lwaGVy"}),
        ),
        (
            DeliveryKind::Wake,
            json!({"platform":"fcm","t":"BwcHBwcHBwcHBwcHBwcHBw","kind":"wake"}),
        ),
        (
            DeliveryKind::Read,
            json!({"platform":"fcm","t":"BwcHBwcHBwcHBwcHBwcHBw","kind":"read"}),
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let harness = HarnessUpstream::new(&format!("http://127.0.0.1:{port}/push")).unwrap();
        let server = tokio::spawn(one_request(listener, "200 OK"));
        assert_eq!(harness.send(delivery(kind)).await, UpstreamResult::Sent);
        let (line, body) = server.await.unwrap();
        assert_eq!(line, "POST /push HTTP/1.1");
        assert_eq!(body, expected);
        assert!(!body.to_string().contains("secret-token"));
    }
}

#[tokio::test]
async fn a_refusal_or_no_harness_is_failed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let harness = HarnessUpstream::new(&format!("http://127.0.0.1:{port}/push")).unwrap();
    let server = tokio::spawn(one_request(listener, "500 Internal Server Error"));
    assert_eq!(
        harness.send(delivery(DeliveryKind::Wake)).await,
        UpstreamResult::Failed
    );
    server.await.unwrap();
    // Nothing listens on that port any more.
    assert_eq!(
        harness.send(delivery(DeliveryKind::Wake)).await,
        UpstreamResult::Failed
    );
}

#[test]
fn only_a_plain_loopback_url_is_taken() {
    assert!(HarnessUpstream::new("http://127.0.0.1:47100/push").is_ok());
    assert!(HarnessUpstream::new("http://[::1]:47100/push").is_ok());
    for bad in [
        "https://127.0.0.1:47100/push",
        "http://10.0.2.2:47100/push",
        "http://example.com:47100/push",
        "http://localhost:47100/push",
        "http://127.0.0.1/push",
        "http://127.0.0.1:47100/push?x=1",
        "not a url",
    ] {
        assert!(HarnessUpstream::new(bad).is_err(), "{bad}");
    }
}
