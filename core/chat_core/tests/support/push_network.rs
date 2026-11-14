// M7 push handles between real cores: the own-device sync harness, with one in-memory push
// proxy every core reaches. `FakePushProxy` runs the real `push_proxy` router (Ruling R2), so
// it issues and opens handles as the proxy does, and it records what it was asked.
include!("mailbox_network.rs");

use axum::body::Body;
use axum::http::{Request, header};
use chat_core::push::{
    HandleRequest, HandleResponse, Platform, PushConfig, PushError, PushFuture, PushProxy,
    PushRequest, PushResponse, RenewSummary,
};
use chat_core::storage_fixture::{PushPeerRow, PushState};
use http_body_util::BodyExt;
use push_wire::{BASE64URL_NOPAD, HandlePlain, RENEW_AFTER_MS, open_handle};
use tower::ServiceExt;

/// The proxy address the cores are configured with; the fake answers in memory.
const PROXY_URL: &str = "https://push.test";

struct FakePushProxy {
    app: axum::Router,
    /// The one handle key, made for this run.
    key: [u8; 32],
    handles: Mutex<Vec<HandleRequest>>,
    pushes: Mutex<Vec<PushRequest>>,
    /// While true, `/handle` requests wait before they reach the router.
    hold: tokio::sync::watch::Sender<bool>,
}

impl FakePushProxy {
    fn new() -> Arc<Self> {
        let mut key = [0; 32];
        getrandom::fill(&mut key).unwrap();
        let mut state = push_proxy::AppState::new(
            vec![(1, key)],
            1,
            Arc::new(push_proxy::FakeUpstream::new()),
        );
        state.jitter = Arc::new(|| Duration::ZERO);
        state.log = Arc::new(|_: &str| {});
        Arc::new(Self {
            app: push_proxy::router(state),
            key,
            handles: Mutex::new(Vec::new()),
            pushes: Mutex::new(Vec::new()),
            hold: tokio::sync::watch::channel(false).0,
        })
    }
    /// Every `/handle` request so far, in order.
    fn handle_requests(&self) -> Vec<HandleRequest> {
        self.handles.lock().unwrap().clone()
    }
    fn push_requests(&self) -> Vec<PushRequest> {
        self.pushes.lock().unwrap().clone()
    }
    fn hold(&self, held: bool) {
        self.hold.send_replace(held);
    }
    /// What a handle this proxy issued says.
    fn open(&self, handle: &[u8]) -> HandlePlain {
        open_handle(&[(1, self.key)], handle).unwrap()
    }
}

async fn call<T: serde::Serialize, U: serde::de::DeserializeOwned>(
    app: axum::Router,
    path: &str,
    body: &T,
) -> Result<U, PushError> {
    let request = Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    let response = app.oneshot(request).await.map_err(|_| PushError::Network)?;
    let status = response.status().as_u16();
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|_| PushError::Network)?
        .to_bytes();
    if status != 200 {
        return Err(PushError::Status(status));
    }
    serde_json::from_slice(&body).map_err(|_| PushError::Malformed)
}

impl PushProxy for FakePushProxy {
    fn handle(&self, request: HandleRequest) -> PushFuture<HandleResponse> {
        self.handles.lock().unwrap().push(request.clone());
        let app = self.app.clone();
        let mut hold = self.hold.subscribe();
        Box::pin(async move {
            let _ = hold.wait_for(|held| !*held).await;
            call(app, "/handle", &request).await
        })
    }
    fn push(&self, request: PushRequest) -> PushFuture<PushResponse> {
        self.pushes.lock().unwrap().push(request.clone());
        let app = self.app.clone();
        Box::pin(async move { call(app, "/push", &request).await })
    }
}

/// The proxy every core on `net` reaches from now on.
fn proxy(net: &Arc<Backend>) -> Arc<FakePushProxy> {
    let proxy = FakePushProxy::new();
    *net.push.lock().unwrap() = Some(proxy.clone());
    proxy
}

fn config(platform: Option<Platform>) -> PushConfig {
    PushConfig {
        proxy_url: Some(PROXY_URL.into()),
        platform,
        snapshot_dir: None,
    }
}

/// `f` starts as an Android device with push on and gets its first FCM token.
async fn android(f: &Fixture, token: &str) {
    f.account.push_configure(config(Some(Platform::Fcm))).unwrap();
    f.account
        .push_set_token(Platform::Fcm, token.into())
        .await
        .unwrap();
}

async fn push_state(f: &Fixture) -> PushState {
    f.account.storage_test_fixture().push_state().await.unwrap()
}

async fn peer_rows(f: &Fixture) -> Vec<PushPeerRow> {
    f.account
        .storage_test_fixture()
        .push_peer_state()
        .await
        .unwrap()
        .peers
}

/// The `push_peer` row `f` keeps for `from`'s device in the conversation: `None` without one,
/// `Some(None)` for a withdrawal.
async fn peer_handle(
    f: &Fixture,
    conv: ConversationId,
    from: chat_core::account::InstallationId,
) -> Option<Option<Vec<u8>>> {
    peer_rows(f)
        .await
        .into_iter()
        .find(|r| r.conversation == conv && r.installation == from)
        .map(|r| r.handle)
}

/// The handle `f` holds for the conversation.
async fn own_tag_handle(f: &Fixture, conv: ConversationId) -> Option<Vec<u8>> {
    push_state(f)
        .await
        .tags
        .into_iter()
        .find(|t| t.conversation == conv)
        .and_then(|t| t.handle)
}

/// `/handle` requests for `tag`.
fn asked_for(proxy: &FakePushProxy, tag: &[u8; 16]) -> usize {
    let tag = BASE64URL_NOPAD.encode(tag);
    proxy
        .handle_requests()
        .iter()
        .filter(|r| r.tag == tag)
        .count()
}
