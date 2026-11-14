//! `/handle` and `/push` through the real router with `FakeUpstream`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use push_proxy::{AppState, Delivery, DeliveryKind, FakeUpstream, Limits, router, serve};
use push_wire::{
    BASE64URL_NOPAD, HANDLE_LIFETIME_MS, HandlePlain, HandleRequest, HandleResponse, MAX_BODY,
    MAX_ITEMS, MAX_TOKEN, PUSH_CIPHER_MAX, Platform, PushItem, PushItemKind, PushRequest,
    PushResponse, PushResult, Tag, seal_handle,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

const T0: u64 = 1_800_000_000_000;
const PEER: [u8; 4] = [127, 0, 0, 1];
const CRLF: &str = "\r\n";

fn test_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    getrandom::fill(&mut k).unwrap();
    k
}

struct Proxy {
    app: Router,
    fcm: Arc<FakeUpstream>,
    apns: Arc<FakeUpstream>,
    now: Arc<AtomicU64>,
    log: Arc<Mutex<Vec<String>>>,
}

struct Setup {
    keys: Vec<(u8, [u8; 32])>,
    current: u8,
    jitter: Duration,
    apns: bool,
    trust_proxy: bool,
    limits: Limits,
}

impl Setup {
    fn new(key: [u8; 32]) -> Self {
        Setup {
            keys: vec![(1, key)],
            current: 1,
            jitter: Duration::ZERO,
            apns: false,
            trust_proxy: false,
            limits: Limits::default(),
        }
    }

    fn start(self) -> Proxy {
        let fcm = Arc::new(FakeUpstream::new());
        let apns = Arc::new(FakeUpstream::new());
        let now = Arc::new(AtomicU64::new(T0));
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut state = AppState::new(self.keys, self.current, fcm.clone());
        if self.apns {
            state.apns = Some(apns.clone());
        }
        let clock = now.clone();
        state.clock = Arc::new(move || clock.load(Ordering::SeqCst));
        let jitter = self.jitter;
        state.jitter = Arc::new(move || jitter);
        state.trust_proxy = self.trust_proxy;
        state.limits = self.limits;
        let sink = log.clone();
        state.log = Arc::new(move |line: &str| sink.lock().unwrap().push(line.to_owned()));
        Proxy {
            app: router(state),
            fcm,
            apns,
            now,
            log,
        }
    }
}

impl Proxy {
    async fn call(&self, path: &str, body: Vec<u8>, xff: Option<&str>) -> (StatusCode, Vec<u8>) {
        let mut req = Request::post(path).header(header::CONTENT_TYPE, "application/json");
        if let Some(v) = xff {
            req = req.header("x-forwarded-for", v);
        }
        let mut req = req.body(Body::from(body)).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((PEER, 40_000))));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, bytes)
    }

    async fn handle_status(&self, req: &HandleRequest, xff: Option<&str>) -> StatusCode {
        self.call("/handle", serde_json::to_vec(req).unwrap(), xff)
            .await
            .0
    }

    async fn issue(&self, platform: Platform, token: &str, tag: Tag) -> String {
        let (status, body) = self
            .call(
                "/handle",
                serde_json::to_vec(&handle_req(platform, token, tag)).unwrap(),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let resp: HandleResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp.expires_at_ms, self.now() + HANDLE_LIFETIME_MS);
        resp.handle
    }

    async fn push(&self, items: Vec<PushItem>) -> (StatusCode, Vec<PushResult>) {
        let n = items.len();
        let (status, body) = self
            .call(
                "/push",
                serde_json::to_vec(&PushRequest { items }).unwrap(),
                None,
            )
            .await;
        if status != StatusCode::OK {
            return (status, Vec::new());
        }
        let resp: PushResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp.results.len(), n);
        (status, resp.results)
    }

    fn now(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    fn set_now(&self, ms: u64) {
        self.now.store(ms, Ordering::SeqCst);
    }

    fn log_text(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }
}

fn handle_req(platform: Platform, token: &str, tag: Tag) -> HandleRequest {
    HandleRequest {
        platform,
        token: token.to_owned(),
        tag: BASE64URL_NOPAD.encode(&tag),
    }
}

fn item(handle: &str, kind: PushItemKind) -> PushItem {
    PushItem {
        handle: handle.to_owned(),
        kind,
    }
}

fn cipher(bytes: &[u8]) -> PushItemKind {
    PushItemKind::Cipher {
        e: BASE64URL_NOPAD.encode(bytes),
    }
}

/// Waits until `fake` has `n` deliveries (5 s at most).
async fn deliveries(fake: &FakeUpstream, n: usize) -> Vec<Delivery> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let got = fake.deliveries();
        if got.len() >= n || Instant::now() > deadline {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn handle_then_push_reaches_upstream_with_tag() {
    let p = Setup::new(test_key()).start();
    let tag = [7u8; 16];
    let handle = p.issue(Platform::Fcm, "fcm-token-1", tag).await;
    let e = BASE64URL_NOPAD.encode(b"stripped mls ciphertext");

    let (status, results) = p
        .push(vec![item(&handle, PushItemKind::Cipher { e: e.clone() })])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(results, vec![PushResult::Ok]);

    let got = deliveries(&p.fcm, 1).await;
    assert_eq!(
        got,
        vec![Delivery {
            platform: Platform::Fcm,
            token: "fcm-token-1".to_owned(),
            tag,
            kind: DeliveryKind::Cipher(e),
        }]
    );
}

#[tokio::test]
async fn push_answers_before_jitter_elapses() {
    let mut setup = Setup::new(test_key());
    setup.jitter = Duration::from_secs(2);
    let p = setup.start();
    let handle = p.issue(Platform::Fcm, "tok", [1; 16]).await;

    let start = Instant::now();
    let (_, results) = p.push(vec![item(&handle, PushItemKind::Wake)]).await;
    let answered = start.elapsed();
    assert_eq!(results, vec![PushResult::Ok]);
    assert!(
        answered < Duration::from_millis(200),
        "answered after {answered:?}"
    );
    assert!(p.fcm.deliveries().is_empty(), "sent before the jitter");

    let got = deliveries(&p.fcm, 1).await;
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].kind, DeliveryKind::Wake);
    assert!(
        start.elapsed() >= Duration::from_secs(2),
        "sent after {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn expired_or_forged_handle_is_gone_and_not_sent() {
    let key = test_key();
    let p = Setup::new(key).start();
    let handle = p.issue(Platform::Fcm, "tok-old", [2; 16]).await;

    // Expired: 30 days and 1 ms after issue.
    p.set_now(T0 + HANDLE_LIFETIME_MS + 1);
    assert_eq!(
        p.push(vec![item(&handle, PushItemKind::Wake)]).await.1,
        vec![PushResult::Gone]
    );
    p.set_now(T0);

    // Forged: one flipped byte, a handle of another key, not base64url at all.
    let mut bytes = BASE64URL_NOPAD.decode(handle.as_bytes()).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let flipped = BASE64URL_NOPAD.encode(&bytes);
    let other = Setup::new(test_key()).start();
    let foreign = other.issue(Platform::Fcm, "tok-foreign", [3; 16]).await;
    let (_, results) = p
        .push(vec![
            item(&flipped, PushItemKind::Wake),
            item(&foreign, cipher(b"x")),
            item("!!not-base64!!", PushItemKind::Wake),
        ])
        .await;
    assert_eq!(results, vec![PushResult::Gone; 3]);

    // A valid push afterwards is the only thing upstream ever sees.
    let fresh = p.issue(Platform::Fcm, "tok-fresh", [4; 16]).await;
    assert_eq!(
        p.push(vec![item(&fresh, PushItemKind::Wake)]).await.1,
        vec![PushResult::Ok]
    );
    deliveries(&p.fcm, 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let got = p.fcm.deliveries();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].token, "tok-fresh");
}

#[tokio::test]
async fn per_handle_rate_limit() {
    let p = Setup::new(test_key()).start();
    let handle = p.issue(Platform::Fcm, "tok", [5; 16]).await;
    let other = p.issue(Platform::Fcm, "tok", [6; 16]).await;

    let items = vec![item(&handle, PushItemKind::Wake); 21];
    let (_, results) = p.push(items).await;
    let mut want = vec![PushResult::Ok; 20];
    want.push(PushResult::Limited);
    assert_eq!(results, want);
    // Another handle has its own bucket.
    assert_eq!(
        p.push(vec![item(&other, PushItemKind::Wake)]).await.1,
        vec![PushResult::Ok]
    );

    p.set_now(T0 + 60_000);
    assert_eq!(
        p.push(vec![item(&handle, PushItemKind::Wake)]).await.1,
        vec![PushResult::Ok]
    );
    assert_eq!(deliveries(&p.fcm, 22).await.len(), 22);
}

#[tokio::test]
async fn handle_issuance_limited_per_ip() {
    let p = Setup::new(test_key()).start();
    let req = handle_req(Platform::Fcm, "tok", [8; 16]);
    for i in 0..30 {
        assert_eq!(
            p.handle_status(&req, None).await,
            StatusCode::OK,
            "request {i}"
        );
    }
    assert_eq!(
        p.handle_status(&req, None).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Without PUSH_TRUST_PROXY a forwarded-for header changes nothing.
    assert_eq!(
        p.handle_status(&req, Some("203.0.113.9")).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    p.set_now(T0 + 2_000);
    assert_eq!(
        p.handle_status(&req, None).await,
        StatusCode::OK,
        "one token a 2 s"
    );

    // Behind a trusted reverse proxy the last forwarded-for entry is the client.
    let mut setup = Setup::new(test_key());
    setup.trust_proxy = true;
    let p = setup.start();
    for _ in 0..30 {
        assert_eq!(
            p.handle_status(&req, Some("203.0.113.9")).await,
            StatusCode::OK
        );
    }
    assert_eq!(
        p.handle_status(&req, Some("203.0.113.9")).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        p.handle_status(&req, Some("203.0.113.9, 198.51.100.7"))
            .await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn handle_request_validation() {
    let p = Setup::new(test_key()).start();
    let ok = handle_req(Platform::Fcm, "tok", [9; 16]);
    let mut short_tag = ok.clone();
    short_tag.tag = BASE64URL_NOPAD.encode(&[9; 15]);
    let mut long_tag = ok.clone();
    long_tag.tag = BASE64URL_NOPAD.encode(&[9; 17]);
    let mut not_b64 = ok.clone();
    not_b64.tag = "*".repeat(22);
    let mut empty_token = ok.clone();
    empty_token.token = String::new();
    let mut long_token = ok.clone();
    long_token.token = "t".repeat(MAX_TOKEN + 1);
    for (name, req) in [
        ("short tag", &short_tag),
        ("long tag", &long_tag),
        ("tag not base64url", &not_b64),
        ("empty token", &empty_token),
        ("long token", &long_token),
    ] {
        assert_eq!(
            p.handle_status(req, None).await,
            StatusCode::BAD_REQUEST,
            "{name}"
        );
    }
    assert_eq!(
        p.call("/handle", b"{not json".to_vec(), None).await.0,
        StatusCode::BAD_REQUEST
    );
    let mut max_token = ok.clone();
    max_token.token = "t".repeat(MAX_TOKEN);
    assert_eq!(p.handle_status(&max_token, None).await, StatusCode::OK);
}

#[tokio::test]
async fn body_and_item_limits() {
    let p = Setup::new(test_key()).start();
    let handle = p.issue(Platform::Fcm, "tok", [10; 16]).await;

    let pad = |len: usize| {
        let mut body = br#"{"items":[]}"#.to_vec();
        body.resize(len, b' ');
        body
    };
    assert_eq!(
        p.call("/push", pad(MAX_BODY + 1), None).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(p.call("/push", pad(MAX_BODY), None).await.0, StatusCode::OK);
    assert_eq!(
        p.call("/push", b"{\"items\":".to_vec(), None).await.0,
        StatusCode::BAD_REQUEST
    );

    let too_many = vec![item(&handle, PushItemKind::Wake); MAX_ITEMS + 1];
    assert_eq!(p.push(too_many).await.0, StatusCode::BAD_REQUEST);

    let too_long = "A".repeat(BASE64URL_NOPAD.encode_len(PUSH_CIPHER_MAX) + 1);
    let (status, results) = p
        .push(vec![
            item(&handle, PushItemKind::Cipher { e: too_long }),
            item(&handle, cipher(&[0xab; PUSH_CIPHER_MAX])),
            item(
                &handle,
                PushItemKind::Cipher {
                    e: "*bad*".to_owned(),
                },
            ),
            item(&handle, PushItemKind::Cipher { e: String::new() }),
        ])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        results,
        vec![
            PushResult::Bad,
            PushResult::Ok,
            PushResult::Bad,
            PushResult::Bad
        ]
    );
    assert_eq!(deliveries(&p.fcm, 1).await.len(), 1);
}

#[tokio::test]
async fn read_for_apns_is_bad() {
    let mut setup = Setup::new(test_key());
    setup.apns = true;
    let p = setup.start();
    let apns = p.issue(Platform::Apns, "apns-tok", [11; 16]).await;
    let fcm = p.issue(Platform::Fcm, "fcm-tok", [12; 16]).await;

    let (_, results) = p
        .push(vec![
            item(&apns, PushItemKind::Read),
            item(&apns, PushItemKind::Wake),
            item(&fcm, PushItemKind::Read),
        ])
        .await;
    assert_eq!(
        results,
        vec![PushResult::Bad, PushResult::Ok, PushResult::Ok]
    );

    let to_apns = deliveries(&p.apns, 1).await;
    assert_eq!(to_apns.len(), 1);
    assert_eq!(
        (to_apns[0].platform, &to_apns[0].kind),
        (Platform::Apns, &DeliveryKind::Wake)
    );
    let to_fcm = deliveries(&p.fcm, 1).await;
    assert_eq!(to_fcm.len(), 1);
    assert_eq!(to_fcm[0].kind, DeliveryKind::Read);
}

#[tokio::test]
async fn apns_handle_without_apns_config_is_bad() {
    let key = test_key();
    let p = Setup::new(key).start();
    assert_eq!(
        p.handle_status(&handle_req(Platform::Apns, "apns-tok", [13; 16]), None)
            .await,
        StatusCode::BAD_REQUEST
    );

    // A valid APNs handle (sealed by a proxy that had APNs) is still refused.
    let sealed = seal_handle(
        &key,
        1,
        &HandlePlain {
            platform: Platform::Apns,
            token: "apns-tok".to_owned(),
            tag: [13; 16],
            issued_at_ms: T0,
            expires_at_ms: T0 + HANDLE_LIFETIME_MS,
        },
    )
    .unwrap();
    let handle = BASE64URL_NOPAD.encode(&sealed);
    assert_eq!(
        p.push(vec![item(&handle, PushItemKind::Wake)]).await.1,
        vec![PushResult::Bad]
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(p.fcm.deliveries().is_empty());
    assert!(p.apns.deliveries().is_empty());
}

#[tokio::test]
async fn restart_opens_same_handle() {
    let key = test_key();
    let first = Setup::new(key).start();
    let handle = first.issue(Platform::Fcm, "tok-restart", [14; 16]).await;
    drop(first);

    let second = Setup::new(key).start();
    assert_eq!(
        second.push(vec![item(&handle, PushItemKind::Wake)]).await.1,
        vec![PushResult::Ok]
    );
    assert_eq!(deliveries(&second.fcm, 1).await[0].token, "tok-restart");

    // After rotation the old key still opens it; new handles use the new key.
    let new_key = test_key();
    let mut rotated = Setup::new(new_key);
    rotated.keys = vec![(2, new_key), (1, key)];
    rotated.current = 2;
    let third = rotated.start();
    assert_eq!(
        third.push(vec![item(&handle, PushItemKind::Wake)]).await.1,
        vec![PushResult::Ok]
    );
    let fresh = third.issue(Platform::Fcm, "tok-new", [15; 16]).await;
    assert_eq!(
        BASE64URL_NOPAD.decode(fresh.as_bytes()).unwrap()[1],
        2,
        "key id byte"
    );
    let old_only = Setup::new(key).start();
    assert_eq!(
        old_only
            .push(vec![item(&fresh, PushItemKind::Wake)])
            .await
            .1,
        vec![PushResult::Gone]
    );
}

#[tokio::test]
async fn logs_hold_no_tokens_tags_handles_or_ips() {
    let p = Setup::new(test_key()).start();
    let token = "fcm-secret-token-value";
    let tag = [0x5a; 16];
    let handle = p.issue(Platform::Fcm, token, tag).await;
    let e = BASE64URL_NOPAD.encode(b"secret-ciphertext-bytes");
    let (_, results) = p
        .push(vec![
            item(&handle, PushItemKind::Cipher { e: e.clone() }),
            item("gone-handle", PushItemKind::Wake),
        ])
        .await;
    assert_eq!(results, vec![PushResult::Ok, PushResult::Gone]);
    p.call("/nowhere/secret-path", Vec::new(), None).await;
    deliveries(&p.fcm, 1).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !p.log_text().contains("path=upstream") && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let log = p.log_text();
    for secret in [
        token,
        BASE64URL_NOPAD.encode(&tag).as_str(),
        &handle,
        &e,
        "gone-handle",
        "secret-path",
        "127.0.0.1",
    ] {
        assert!(!log.contains(secret), "{secret} in log:\n{log}");
    }
    // The upstream line may land anywhere after the /push one.
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 4, "{log}");
    for want in [
        format!("t={T0} path=/handle status=200 ms="),
        format!("t={T0} path=/push status=200 items=2 ok=1 gone=1 limited=0 bad=0 ms="),
        format!("t={T0} path=other status=404 ms="),
        format!("t={T0} path=upstream result=sent items=1 ms="),
    ] {
        let found = lines.iter().filter(|l| l.starts_with(&want)).count();
        assert_eq!(found, 1, "{want} in:\n{log}");
    }
}

#[tokio::test]
async fn in_flight_sends_are_capped() {
    let mut setup = Setup::new(test_key());
    setup.limits.max_sends = 2;
    setup.jitter = Duration::from_millis(300);
    let p = setup.start();
    let a = p.issue(Platform::Fcm, "tok-a", [16; 16]).await;
    let b = p.issue(Platform::Fcm, "tok-b", [17; 16]).await;
    let c = p.issue(Platform::Fcm, "tok-c", [18; 16]).await;

    let start = Instant::now();
    let (_, results) = p
        .push(vec![
            item(&a, PushItemKind::Wake),
            item(&b, PushItemKind::Wake),
            item(&c, PushItemKind::Wake),
        ])
        .await;
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "waited for a slot"
    );
    assert_eq!(
        results,
        vec![PushResult::Ok, PushResult::Ok, PushResult::Limited]
    );

    // Slots come back when the sends end.
    assert_eq!(deliveries(&p.fcm, 2).await.len(), 2);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        p.push(vec![item(&c, PushItemKind::Wake)]).await.1,
        vec![PushResult::Ok]
    );
    let got = deliveries(&p.fcm, 3).await;
    assert_eq!(got.len(), 3);
    assert_eq!(got[2].token, "tok-c");
}

#[tokio::test]
async fn stalled_request_body_times_out() {
    let mut setup = Setup::new(test_key());
    setup.limits.request_timeout = Duration::from_millis(200);
    let p = setup.start();
    let first = futures_util::stream::once(async {
        Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"{\"items\":"))
    });
    let body = Body::from_stream(first.chain(futures_util::stream::pending()));
    let mut req = Request::post("/push").body(body).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from((PEER, 40_000))));

    let start = Instant::now();
    let resp = tokio::time::timeout(Duration::from_secs(5), p.app.clone().oneshot(req))
        .await
        .expect("request not bounded")
        .unwrap();
    assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);
    assert!(start.elapsed() >= Duration::from_millis(200));
    assert!(
        p.log_text().contains("path=/push status=408"),
        "{}",
        p.log_text()
    );
}

#[tokio::test]
async fn stalled_headers_close_the_connection() {
    let p = Setup::new(test_key()).start();
    let listener = tokio::net::TcpListener::bind(SocketAddr::from((PEER, 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, p.app.clone(), Duration::from_millis(200)));

    // A complete request is served.
    let mut ok = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = br#"{"items":[]}"#;
    let head = format!(
        "POST /push HTTP/1.1{CRLF}Host: proxy{CRLF}Content-Length: {}{CRLF}Connection: close{CRLF}{CRLF}",
        body.len()
    );
    ok.write_all(head.as_bytes()).await.unwrap();
    ok.write_all(body).await.unwrap();
    let mut answer = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), ok.read_to_end(&mut answer))
        .await
        .unwrap()
        .unwrap();
    assert!(
        answer.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&answer)
    );

    // A head that never ends is cut off.
    let mut stalled = tokio::net::TcpStream::connect(addr).await.unwrap();
    let partial = format!("POST /push HTTP/1.1{CRLF}Host: proxy{CRLF}");
    stalled.write_all(partial.as_bytes()).await.unwrap();
    let start = Instant::now();
    let mut rest = Vec::new();
    let closed = tokio::time::timeout(Duration::from_secs(5), stalled.read_to_end(&mut rest)).await;
    assert!(closed.is_ok(), "connection still open after 5 s");
    assert!(
        start.elapsed() >= Duration::from_millis(150),
        "{:?}",
        start.elapsed()
    );

    // So is a connection that never sends a byte.
    let mut silent = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut rest = Vec::new();
    let closed = tokio::time::timeout(Duration::from_secs(5), silent.read_to_end(&mut rest)).await;
    assert!(closed.is_ok(), "silent connection still open after 5 s");
}
