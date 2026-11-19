//! The real FCM and APNs senders against an in-process TLS HTTP/2 fake of
//! FCM, Google OAuth and APNs. Every key here is made at run time.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use axum::body::Bytes;
use axum::http::{HeaderMap, Request, Response, Version};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use push_proxy::{
    ApnsUpstream, Delivery, DeliveryKind, FcmUpstream, SenderError, Upstream, UpstreamResult, Url,
};
use push_wire::{BASE64URL_NOPAD, MAX_PAYLOAD, PUSH_CIPHER_MAX, Platform, Tag};
use ring::signature::{ECDSA_P256_SHA256_FIXED, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
use rsa::pkcs1::EncodeRsaPublicKey;
use rsa::pkcs8::{EncodePrivateKey, LineEnding};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

const T0: u64 = 1_800_000_000_000;
const TAG: Tag = [7; 16];
const FCM_TOKEN: &str = "fcm-device-token";
const APNS_TOKEN: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
const PROJECT: &str = "proj-test";
const CLIENT_EMAIL: &str = "proxy@proj-test.iam.gserviceaccount.com";
const KEY_ID: &str = "ABC123DEFG";
const TEAM_ID: &str = "DEF123GHIJ";
const TOPIC: &str = "dev.chatproject.chatapp";
/// FCM and APNs keep an undelivered push this long (plan 2).
const STORE_SECS: u64 = 2_419_200;

// ---- fake server ---------------------------------------------------------

#[derive(Clone)]
struct Seen {
    version: Version,
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Seen {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    fn header(&self, name: &str) -> &str {
        self.headers.get(name).unwrap().to_str().unwrap()
    }
}

type Reply = Arc<dyn Fn(&Seen) -> (u16, String) + Send + Sync>;

struct FakeServer {
    addr: SocketAddr,
    root: CertificateDer<'static>,
    seen: Arc<Mutex<Vec<Seen>>>,
    connections: Arc<AtomicUsize>,
}

impl FakeServer {
    /// TLS on 127.0.0.1 offering `h2` and `http/1.1`, answering with `reply`.
    async fn start(reply: Reply) -> Self {
        let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).unwrap();
        let root = certified.cert.der().clone();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            certified.signing_key.serialize_der(),
        ));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![root.clone()], key)
            .unwrap();
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let (seen2, connections2) = (seen.clone(), connections.clone());
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                connections2.fetch_add(1, Ordering::SeqCst);
                let (acceptor, seen, reply) = (acceptor.clone(), seen2.clone(), reply.clone());
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let service = service_fn(move |req: Request<Incoming>| {
                        let (seen, reply) = (seen.clone(), reply.clone());
                        async move {
                            let (parts, body) = req.into_parts();
                            let body = body.collect().await.unwrap().to_bytes().to_vec();
                            let request = Seen {
                                version: parts.version,
                                path: parts.uri.path().to_owned(),
                                headers: parts.headers,
                                body,
                            };
                            let (status, text) = reply(&request);
                            seen.lock().unwrap().push(request);
                            Ok::<_, Infallible>(
                                Response::builder()
                                    .status(status)
                                    .body(Full::new(Bytes::from(text)))
                                    .unwrap(),
                            )
                        }
                    });
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(tls), service)
                        .await;
                });
            }
        });
        FakeServer {
            addr,
            root,
            seen,
            connections,
        }
    }

    fn url(&self, path: &str) -> Url {
        Url::parse(&format!("https://{}{path}", self.addr)).unwrap()
    }

    fn seen(&self, path: &str) -> Vec<Seen> {
        let seen = self.seen.lock().unwrap();
        seen.iter().filter(|s| s.path == path).cloned().collect()
    }
}

/// Replies `(status, body)` for every path but the token endpoint, which
/// hands out `access-1`, `access-2`, ... valid for an hour.
fn google(send: Arc<Mutex<(u16, String)>>) -> Reply {
    let issued = Arc::new(AtomicUsize::new(0));
    Arc::new(move |seen: &Seen| {
        if seen.path == "/token" {
            let n = issued.fetch_add(1, Ordering::SeqCst) + 1;
            let body = json!({"access_token": format!("access-{n}"), "expires_in": 3600, "token_type": "Bearer"});
            (200, body.to_string())
        } else {
            send.lock().unwrap().clone()
        }
    })
}

fn ok_send() -> Arc<Mutex<(u16, String)>> {
    Arc::new(Mutex::new((
        200,
        r#"{"name":"projects/proj-test/messages/1"}"#.to_owned(),
    )))
}

// ---- keys and senders ----------------------------------------------------

struct RsaKey {
    pem: String,
    public_pkcs1: Vec<u8>,
}

/// One 2048-bit key per test binary: generating it is the slow part.
static RSA_KEY: LazyLock<RsaKey> = LazyLock::new(|| {
    let mut rng = rsa::rand_core::OsRng;
    let key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
    RsaKey {
        pem: key.to_pkcs8_pem(LineEnding::LF).unwrap().to_string(),
        public_pkcs1: key
            .to_public_key()
            .to_pkcs1_der()
            .unwrap()
            .as_bytes()
            .to_vec(),
    }
});

fn service_account(token_url: &Url) -> String {
    json!({
        "type": "service_account",
        "project_id": PROJECT,
        "private_key_id": "sa-key-1",
        "private_key": RSA_KEY.pem,
        "client_email": CLIENT_EMAIL,
        "token_uri": token_url.as_str(),
    })
    .to_string()
}

fn clock_at(now: &Arc<AtomicU64>) -> Arc<dyn Fn() -> u64 + Send + Sync> {
    let now = now.clone();
    Arc::new(move || now.load(Ordering::SeqCst))
}

fn fcm(server: &FakeServer, now: &Arc<AtomicU64>) -> FcmUpstream {
    let token_url = server.url("/token");
    FcmUpstream::with_root(
        &service_account(&token_url),
        server.url("/"),
        token_url,
        &server.root,
    )
    .unwrap()
    .with_clock(clock_at(now))
}

struct EcKey {
    pem: String,
    public: Vec<u8>,
}

fn ec_key() -> EcKey {
    let key = rcgen::KeyPair::generate().unwrap();
    EcKey {
        pem: key.serialize_pem(),
        public: key.public_key_raw().to_vec(),
    }
}

fn apns(server: &FakeServer, key: &EcKey, now: &Arc<AtomicU64>) -> ApnsUpstream {
    ApnsUpstream::with_root(
        &key.pem,
        KEY_ID,
        TEAM_ID,
        TOPIC,
        server.url(""),
        &server.root,
    )
    .unwrap()
    .with_clock(clock_at(now))
}

fn delivery(platform: Platform, kind: DeliveryKind) -> Delivery {
    Delivery {
        platform,
        token: match platform {
            Platform::Fcm => FCM_TOKEN,
            Platform::Apns => APNS_TOKEN,
        }
        .to_owned(),
        tag: TAG,
        kind,
    }
}

fn cipher(bytes: &[u8]) -> DeliveryKind {
    DeliveryKind::Cipher(BASE64URL_NOPAD.encode(bytes))
}

fn t() -> String {
    BASE64URL_NOPAD.encode(&TAG)
}

fn b64(part: &str) -> Vec<u8> {
    BASE64URL_NOPAD.decode(part.as_bytes()).unwrap()
}

/// Header and claims of a compact JWT whose signature `verify` accepts.
fn open_jwt(jwt: &str, verify: impl Fn(&[u8], &[u8])) -> (Value, Value) {
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3, "compact JWT");
    let signed = format!("{}.{}", parts[0], parts[1]);
    verify(signed.as_bytes(), &b64(parts[2]));
    (
        serde_json::from_slice(&b64(parts[0])).unwrap(),
        serde_json::from_slice(&b64(parts[1])).unwrap(),
    )
}

fn fcm_send_path() -> String {
    format!("/v1/projects/{PROJECT}/messages:send")
}

// ---- FCM -----------------------------------------------------------------

#[tokio::test]
async fn fcm_cipher_request_shape() {
    let server = FakeServer::start(google(ok_send())).await;
    let now = Arc::new(AtomicU64::new(T0));
    let sender = fcm(&server, &now);

    let result = sender
        .send(delivery(Platform::Fcm, cipher(&[1, 2, 3, 250])))
        .await;
    assert_eq!(result, UpstreamResult::Sent);

    // The OAuth assertion: RS256 from the service account, for the FCM scope.
    let token = server.seen("/token");
    assert_eq!(token.len(), 1);
    assert_eq!(
        token[0].header("content-type"),
        "application/x-www-form-urlencoded"
    );
    let form = String::from_utf8(token[0].body.clone()).unwrap();
    let assertion = form
        .strip_prefix("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion=")
        .expect("jwt-bearer grant");
    let (header, claims) = open_jwt(assertion, |msg, sig| {
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &RSA_KEY.public_pkcs1)
            .verify(msg, sig)
            .expect("RS256 signature");
    });
    assert_eq!(header["alg"], "RS256");
    assert_eq!(header["kid"], "sa-key-1");
    assert_eq!(claims["iss"], CLIENT_EMAIL);
    assert_eq!(
        claims["scope"],
        "https://www.googleapis.com/auth/firebase.messaging"
    );
    assert_eq!(claims["aud"], server.url("/token").as_str());
    assert_eq!(claims["iat"], T0 / 1000);
    assert_eq!(claims["exp"], T0 / 1000 + 3600);

    let sent = server.seen(&fcm_send_path());
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].header("authorization"), "Bearer access-1");
    assert_eq!(sent[0].header("content-type"), "application/json");
    assert_eq!(
        sent[0].json(),
        json!({"message": {
            "token": FCM_TOKEN,
            "android": {"priority": "HIGH", "ttl": format!("{STORE_SECS}s")},
            "data": {"t": t(), "e": BASE64URL_NOPAD.encode(&[1, 2, 3, 250])},
        }})
    );
}

#[tokio::test]
async fn fcm_read_is_normal_priority() {
    let server = FakeServer::start(google(ok_send())).await;
    let now = Arc::new(AtomicU64::new(T0));
    let sender = fcm(&server, &now);

    let result = sender
        .send(delivery(Platform::Fcm, DeliveryKind::Read))
        .await;
    assert_eq!(result, UpstreamResult::Sent);
    let message = server.seen(&fcm_send_path())[0].json()["message"].clone();
    assert_eq!(message["android"]["priority"], "NORMAL");
    assert_eq!(message["data"], json!({"t": t(), "r": "1"}));
}

#[tokio::test]
async fn fcm_wake_has_w() {
    let server = FakeServer::start(google(ok_send())).await;
    let now = Arc::new(AtomicU64::new(T0));
    let sender = fcm(&server, &now);

    let result = sender
        .send(delivery(Platform::Fcm, DeliveryKind::Wake))
        .await;
    assert_eq!(result, UpstreamResult::Sent);
    let message = server.seen(&fcm_send_path())[0].json()["message"].clone();
    assert_eq!(message["android"]["priority"], "HIGH");
    assert_eq!(message["data"], json!({"t": t(), "w": "1"}));
}

#[tokio::test]
async fn fcm_oauth_token_cached_until_near_expiry() {
    let server = FakeServer::start(google(ok_send())).await;
    let now = Arc::new(AtomicU64::new(T0));
    let sender = fcm(&server, &now);
    let wake = || delivery(Platform::Fcm, DeliveryKind::Wake);

    assert_eq!(sender.send(wake()).await, UpstreamResult::Sent);
    // The token lives 3,600 s; it is reused until 60 s before that.
    now.store(T0 + 3_540_000 - 1, Ordering::SeqCst);
    assert_eq!(sender.send(wake()).await, UpstreamResult::Sent);
    assert_eq!(server.seen("/token").len(), 1);

    now.store(T0 + 3_540_000, Ordering::SeqCst);
    assert_eq!(sender.send(wake()).await, UpstreamResult::Sent);
    assert_eq!(server.seen("/token").len(), 2);
    let auths: Vec<String> = server
        .seen(&fcm_send_path())
        .iter()
        .map(|s| s.header("authorization").to_owned())
        .collect();
    assert_eq!(
        auths,
        ["Bearer access-1", "Bearer access-1", "Bearer access-2"]
    );
}

#[tokio::test]
async fn fcm_concurrent_sends_fetch_one_token() {
    let server = FakeServer::start(google(ok_send())).await;
    let now = Arc::new(AtomicU64::new(T0));
    let sender = fcm(&server, &now);

    let sends: Vec<_> = (0..20)
        .map(|_| sender.send(delivery(Platform::Fcm, DeliveryKind::Wake)))
        .collect();
    let results = futures_util::future::join_all(sends).await;
    assert!(results.iter().all(|r| *r == UpstreamResult::Sent));
    assert_eq!(server.seen("/token").len(), 1);
    assert_eq!(server.seen(&fcm_send_path()).len(), 20);
}

#[tokio::test]
async fn fcm_unregistered_is_token_gone() {
    let send = ok_send();
    let server = FakeServer::start(google(send.clone())).await;
    let now = Arc::new(AtomicU64::new(T0));
    let sender = fcm(&server, &now);
    let fcm_error = |code: u16, status: &str, error_code: &str| {
        json!({"error": {"code": code, "message": "x", "status": status, "details": [
            {"@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError", "errorCode": error_code}
        ]}})
        .to_string()
    };

    let cases = [
        (
            404,
            fcm_error(404, "NOT_FOUND", "UNREGISTERED"),
            UpstreamResult::TokenGone,
        ),
        (
            404,
            fcm_error(404, "NOT_FOUND", "SENDER_ID_MISMATCH"),
            UpstreamResult::Failed,
        ),
        (404, "not json".to_owned(), UpstreamResult::Failed),
        (
            400,
            fcm_error(400, "INVALID_ARGUMENT", "INVALID_ARGUMENT"),
            UpstreamResult::Failed,
        ),
        (
            403,
            fcm_error(403, "PERMISSION_DENIED", "SENDER_ID_MISMATCH"),
            UpstreamResult::Failed,
        ),
        (
            429,
            fcm_error(429, "RESOURCE_EXHAUSTED", "QUOTA_EXCEEDED"),
            UpstreamResult::Failed,
        ),
        (
            500,
            fcm_error(500, "INTERNAL", "INTERNAL"),
            UpstreamResult::Failed,
        ),
        (503, String::new(), UpstreamResult::Failed),
    ];
    for (status, body, expected) in cases {
        *send.lock().unwrap() = (status, body);
        let result = sender
            .send(delivery(Platform::Fcm, DeliveryKind::Wake))
            .await;
        assert_eq!(result, expected, "status {status}");
    }
}

#[tokio::test]
async fn fcm_token_endpoint_failure_is_failed_and_not_cached() {
    let fail_token = Arc::new(AtomicUsize::new(1));
    let fail = fail_token.clone();
    let inner = google(ok_send());
    let reply: Reply = Arc::new(move |seen: &Seen| {
        if seen.path == "/token" && fail.load(Ordering::SeqCst) > 0 {
            fail.fetch_sub(1, Ordering::SeqCst);
            return (500, String::new());
        }
        inner(seen)
    });
    let server = FakeServer::start(reply).await;
    let now = Arc::new(AtomicU64::new(T0));
    let sender = fcm(&server, &now);

    let wake = || delivery(Platform::Fcm, DeliveryKind::Wake);
    assert_eq!(sender.send(wake()).await, UpstreamResult::Failed);
    assert!(server.seen(&fcm_send_path()).is_empty());
    assert_eq!(sender.send(wake()).await, UpstreamResult::Sent);
    assert_eq!(server.seen("/token").len(), 2);
}

// ---- APNs ----------------------------------------------------------------

fn apns_reply(status: Arc<Mutex<(u16, String)>>) -> Reply {
    Arc::new(move |_: &Seen| status.lock().unwrap().clone())
}

fn apns_path() -> String {
    format!("/3/device/{APNS_TOKEN}")
}

fn jwt_of(seen: &Seen, key: &EcKey) -> (Value, Value) {
    let auth = seen.header("authorization");
    let jwt = auth.strip_prefix("bearer ").expect("bearer scheme");
    open_jwt(jwt, |msg, sig| {
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &key.public)
            .verify(msg, sig)
            .expect("ES256 signature");
    })
}

#[tokio::test]
async fn apns_request_shape() {
    let server = FakeServer::start(apns_reply(Arc::new(Mutex::new((200, String::new()))))).await;
    let key = ec_key();
    let now = Arc::new(AtomicU64::new(T0));
    let sender = apns(&server, &key, &now);

    let ct = [9u8, 8, 7];
    let result = sender.send(delivery(Platform::Apns, cipher(&ct))).await;
    assert_eq!(result, UpstreamResult::Sent);
    let result = sender
        .send(delivery(Platform::Apns, DeliveryKind::Wake))
        .await;
    assert_eq!(result, UpstreamResult::Sent);

    let seen = server.seen(&apns_path());
    assert_eq!(seen.len(), 2);
    for s in &seen {
        assert_eq!(s.version, Version::HTTP_2);
        let (header, claims) = jwt_of(s, &key);
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], KEY_ID);
        assert_eq!(claims, json!({"iss": TEAM_ID, "iat": T0 / 1000}));
        assert_eq!(s.header("apns-push-type"), "alert");
        assert_eq!(s.header("apns-priority"), "10");
        assert_eq!(s.header("apns-topic"), TOPIC);
        assert_eq!(
            s.header("apns-expiration"),
            (T0 / 1000 + STORE_SECS).to_string()
        );
    }
    let aps = json!({"mutable-content": 1, "alert": {"loc-key": "NEW_MSG"}});
    assert_eq!(
        seen[0].json(),
        json!({"aps": aps, "t": t(), "e": BASE64URL_NOPAD.encode(&ct)})
    );
    assert_eq!(seen[1].json(), json!({"aps": aps, "t": t()}));
    // One HTTP/2 connection carries both.
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn apns_jwt_reused_for_50_minutes() {
    let server = FakeServer::start(apns_reply(Arc::new(Mutex::new((200, String::new()))))).await;
    let key = ec_key();
    let now = Arc::new(AtomicU64::new(T0));
    let sender = apns(&server, &key, &now);
    let wake = || delivery(Platform::Apns, DeliveryKind::Wake);

    assert_eq!(sender.send(wake()).await, UpstreamResult::Sent);
    now.store(T0 + 3_000_000 - 1, Ordering::SeqCst);
    assert_eq!(sender.send(wake()).await, UpstreamResult::Sent);
    now.store(T0 + 3_000_000, Ordering::SeqCst);
    assert_eq!(sender.send(wake()).await, UpstreamResult::Sent);

    let seen = server.seen(&apns_path());
    let auths: Vec<&str> = seen.iter().map(|s| s.header("authorization")).collect();
    assert_eq!(auths[0], auths[1], "reused within 50 minutes");
    assert_ne!(auths[1], auths[2], "minted again at 50 minutes");
    assert_eq!(jwt_of(&seen[2], &key).1["iat"], (T0 + 3_000_000) / 1000);
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn apns_410_is_token_gone() {
    let status = Arc::new(Mutex::new((200, String::new())));
    let server = FakeServer::start(apns_reply(status.clone())).await;
    let key = ec_key();
    let now = Arc::new(AtomicU64::new(T0));
    let sender = apns(&server, &key, &now);

    let cases = [
        (
            410,
            r#"{"reason":"Unregistered","timestamp":1800000000000}"#,
            UpstreamResult::TokenGone,
        ),
        (
            400,
            r#"{"reason":"BadDeviceToken"}"#,
            UpstreamResult::Failed,
        ),
        (
            403,
            r#"{"reason":"ExpiredProviderToken"}"#,
            UpstreamResult::Failed,
        ),
        (
            413,
            r#"{"reason":"PayloadTooLarge"}"#,
            UpstreamResult::Failed,
        ),
        (
            429,
            r#"{"reason":"TooManyRequests"}"#,
            UpstreamResult::Failed,
        ),
        (
            500,
            r#"{"reason":"InternalServerError"}"#,
            UpstreamResult::Failed,
        ),
        (
            503,
            r#"{"reason":"ServiceUnavailable"}"#,
            UpstreamResult::Failed,
        ),
    ];
    for (code, body, expected) in cases {
        *status.lock().unwrap() = (code, body.to_owned());
        let result = sender
            .send(delivery(Platform::Apns, DeliveryKind::Wake))
            .await;
        assert_eq!(result, expected, "status {code}");
    }
}

#[tokio::test]
async fn apns_never_sends_read() {
    let server = FakeServer::start(apns_reply(Arc::new(Mutex::new((200, String::new()))))).await;
    let key = ec_key();
    let now = Arc::new(AtomicU64::new(T0));
    let sender = apns(&server, &key, &now);

    let result = sender
        .send(delivery(Platform::Apns, DeliveryKind::Read))
        .await;
    assert_eq!(result, UpstreamResult::Failed);
    assert!(server.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn apns_token_cannot_leave_the_device_path() {
    let server = FakeServer::start(apns_reply(Arc::new(Mutex::new((200, String::new()))))).await;
    let key = ec_key();
    let now = Arc::new(AtomicU64::new(T0));
    let sender = apns(&server, &key, &now);

    let mut d = delivery(Platform::Apns, DeliveryKind::Wake);
    d.token = "../../x?y#z".to_owned();
    assert_eq!(sender.send(d).await, UpstreamResult::Sent);
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].path.starts_with("/3/device/") && seen[0].path.matches('/').count() == 3,
        "token escaped its path segment"
    );
}

// ---- both ----------------------------------------------------------------

#[tokio::test]
async fn payload_with_max_cipher_fits_4096() {
    let fcm_server = FakeServer::start(google(ok_send())).await;
    let apns_server =
        FakeServer::start(apns_reply(Arc::new(Mutex::new((200, String::new()))))).await;
    let key = ec_key();
    let now = Arc::new(AtomicU64::new(T0));
    let fcm_sender = fcm(&fcm_server, &now);
    let apns_sender = apns(&apns_server, &key, &now);

    let ct: Vec<u8> = (0..PUSH_CIPHER_MAX).map(|i| (i % 251) as u8).collect();
    let mut fcm_delivery = delivery(Platform::Fcm, cipher(&ct));
    // The longest token a handle can carry.
    fcm_delivery.token = "x".repeat(push_wire::MAX_TOKEN);
    assert_eq!(fcm_sender.send(fcm_delivery).await, UpstreamResult::Sent);
    assert_eq!(
        apns_sender
            .send(delivery(Platform::Apns, cipher(&ct)))
            .await,
        UpstreamResult::Sent
    );

    // FCM counts the data payload against its limit; APNs the whole body.
    let fcm_body = fcm_server.seen(&fcm_send_path())[0].json();
    let data = serde_json::to_vec(&fcm_body["message"]["data"]).unwrap();
    let apns_seen = apns_server.seen(&apns_path());
    let apns_body = &apns_seen[0].body;
    assert!(data.len() <= MAX_PAYLOAD, "FCM data {} B", data.len());
    assert!(
        apns_body.len() <= MAX_PAYLOAD,
        "APNs body {} B",
        apns_body.len()
    );

    // The ciphertext and tag come back byte for byte.
    for v in [&fcm_body["message"]["data"], &apns_seen[0].json()] {
        assert_eq!(b64(v["e"].as_str().unwrap()), ct);
        assert_eq!(b64(v["t"].as_str().unwrap()), TAG);
    }
}

#[tokio::test]
async fn oversize_payload_is_failed_not_sent() {
    let fcm_server = FakeServer::start(google(ok_send())).await;
    let apns_server =
        FakeServer::start(apns_reply(Arc::new(Mutex::new((200, String::new()))))).await;
    let key = ec_key();
    let now = Arc::new(AtomicU64::new(T0));

    // Past what the route lets through: about 4,100 B once encoded.
    let ct = vec![0u8; MAX_PAYLOAD * 3 / 4 + 1];
    let fcm_result = fcm(&fcm_server, &now)
        .send(delivery(Platform::Fcm, cipher(&ct)))
        .await;
    let apns_result = apns(&apns_server, &key, &now)
        .send(delivery(Platform::Apns, cipher(&ct)))
        .await;
    assert_eq!(fcm_result, UpstreamResult::Failed);
    assert_eq!(apns_result, UpstreamResult::Failed);
    assert!(fcm_server.seen(&fcm_send_path()).is_empty());
    assert!(apns_server.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn bad_secrets_are_refused_without_echo() {
    let url = Url::parse("https://127.0.0.1:1/").unwrap();
    let token_url = url.join("token").unwrap();
    let mut sa: Value = serde_json::from_str(&service_account(&token_url)).unwrap();

    sa["private_key"] = json!("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n");
    let err = FcmUpstream::new(&sa.to_string(), url.clone(), token_url.clone())
        .err()
        .unwrap();
    assert_eq!(err, SenderError::Invalid("private_key"));
    assert!(!err.to_string().contains("AAAA"));

    for field in ["project_id", "client_email", "private_key"] {
        let mut missing: Value = serde_json::from_str(&service_account(&token_url)).unwrap();
        missing.as_object_mut().unwrap().remove(field);
        let err = FcmUpstream::new(&missing.to_string(), url.clone(), token_url.clone())
            .err()
            .unwrap();
        assert_eq!(err, SenderError::Invalid(field));
    }
    assert_eq!(
        FcmUpstream::new("{", url.clone(), token_url.clone())
            .err()
            .unwrap(),
        SenderError::Invalid("service account JSON")
    );

    // An RSA key where APNs needs P-256, and a topic that is no header value.
    let err = ApnsUpstream::new(&RSA_KEY.pem, KEY_ID, TEAM_ID, TOPIC, url.clone())
        .err()
        .unwrap();
    assert_eq!(err, SenderError::Invalid("APNs key"));
    assert!(!err.to_string().contains("PRIVATE"));
    let key = ec_key();
    let err = ApnsUpstream::new(&key.pem, KEY_ID, TEAM_ID, "bad\ntopic", url)
        .err()
        .unwrap();
    assert_eq!(err, SenderError::Invalid("APNs topic"));
}
