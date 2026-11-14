//! Entry points for native code that runs without Flutter in this process (design 12.3): the
//! Android push receiver, its renewal job and its notifications (Kotlin, spec 5.4, 5.5), and the
//! iOS background renewal (`cp_push_renew`, spec 5.6). The iOS notification extension runs in
//! its own process and links `core/chat_nse` instead.
//!
//! Every call here blocks: Kotlin and Swift call from their own background threads. Core calls
//! run on the core's runtime and are waited for with a bound; nothing here blocks a thread of
//! a tokio runtime.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chat_core::account::storage::KeyProtector;
use chat_core::push::{PushConfig, PushOutcome, ReceivedPush, Tag, FETCH_MAX_MS};
use chat_core::{CoreConfig, CoreError, CoreEvent, CoreSlot, ErrorCode, EventBus, EventSink};
use chat_core::{NetworkOp, NetworkReply, NetworkState};
use push_wire::{Platform, BASE64URL_NOPAD, PUSH_CIPHER_MAX};

use crate::global::{BUS, CORE};

/// Version of the core in this library.
#[uniffi::export]
pub fn native_core_version() -> String {
    chat_core::CORE_VERSION.to_string()
}

/// Instance id of the core running in this process, or `None` before the first start.
/// Equals the id Dart got from `core_start` when both run in one process.
#[uniffi::export]
pub fn native_instance_id() -> Option<String> {
    CORE.info().map(|info| info.instance_id)
}

/// Why a native call failed. Never carries a token, handle, tag or payload.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Error)]
pub enum NativeError {
    /// The push's data is not one the push proxy builds (plan 2 payload table).
    BadPayload,
    /// A setting the core refuses: a storage root that is not absolute, or a push proxy address
    /// this build may not use (release builds take `https://` only).
    InvalidConfig,
    /// The call did not end within its bound.
    Timeout,
    /// The core could not do it now; `code` names the core's `ErrorCode` (e.g. `CoreNotStarted`).
    Core { code: String },
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadPayload => f.write_str("push data not understood"),
            Self::InvalidConfig => f.write_str("invalid configuration"),
            Self::Timeout => f.write_str("timed out"),
            Self::Core { code } => write!(f, "core error {code}"),
        }
    }
}

impl std::error::Error for NativeError {}

impl From<CoreError> for NativeError {
    fn from(error: CoreError) -> Self {
        // Only the code crosses: a detail may name paths or values.
        match error.code {
            ErrorCode::InvalidConfig => Self::InvalidConfig,
            code => Self::core(code),
        }
    }
}

impl NativeError {
    fn core(code: ErrorCode) -> Self {
        Self::Core { code: format!("{code:?}") }
    }
}

/// The core this call reached.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeInfo {
    pub version: String,
    /// Same as Dart's `CoreInfo.instance_id` when both reached one core.
    pub instance_id: String,
}

/// What processing a received push showed (`chat_core::push::PushOutcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativePushOutcome {
    /// A notification that alerts went to the `NativeNotifier`.
    Notified,
    /// A notification was updated or cleared without alerting.
    Updated,
    /// Nothing to show.
    Nothing,
    /// The push names no current handle of this device: show nothing.
    UnknownTag,
}

impl From<PushOutcome> for NativePushOutcome {
    fn from(outcome: PushOutcome) -> Self {
        match outcome {
            PushOutcome::Notified => Self::Notified,
            PushOutcome::Updated => Self::Updated,
            PushOutcome::Nothing => Self::Nothing,
            PushOutcome::UnknownTag => Self::UnknownTag,
        }
    }
}

/// Longest wait for one core call that does not reach the network.
const CALL_WAIT: Duration = Duration::from_secs(15);
/// What a received push may take past its fetch deadline: opening it and giving the network up.
const RECEIVE_MARGIN: Duration = Duration::from_secs(2);
/// Bound of `native_push_renew` (the Android renewal job): the renewal's `/handle` requests and
/// the delivery window after it.
const RENEW_WAIT: Duration = Duration::from_secs(60);
/// Longest the network stays up after a renewal sealed frames, for the peers to get them.
const DELIVERY_WINDOW: Duration = Duration::from_secs(10);
/// Kept free at the end of a renewal's budget for stopping the network again.
const STOP_MARGIN: Duration = Duration::from_secs(1);

/// The push proxy address: the caller's, else this build's (`CHATPROJECT_PUSH_URL` at build
/// time, Global Constraints). `None` turns push off.
pub(crate) fn push_url(given: Option<String>) -> Option<String> {
    given.or_else(|| option_env!("CHATPROJECT_PUSH_URL").map(str::to_owned))
}

/// Runs `call` on the core's runtime and waits on this thread for at most `bound`; a call that
/// does not end in time is dropped. Refused on a thread inside a tokio runtime, where waiting
/// could stall the runtime the call needs.
fn wait_on<T, F>(slot: &CoreSlot, bound: Duration, call: F) -> Result<T, NativeError>
where
    T: Send + 'static,
    F: Future<Output = Result<T, CoreError>> + Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(NativeError::core(ErrorCode::Busy));
    }
    let runtime = slot.runtime()?;
    let (done, result) = mpsc::sync_channel(1);
    let task = runtime.spawn(async move {
        let _ = done.send(call.await);
    });
    match result.recv_timeout(bound) {
        Ok(result) => result.map_err(NativeError::from),
        Err(RecvTimeoutError::Timeout) => {
            task.abort();
            Err(NativeError::Timeout)
        }
        // The task ended without an answer: the runtime is shutting down.
        Err(RecvTimeoutError::Disconnected) => Err(NativeError::core(ErrorCode::NotAvailable)),
    }
}

/// The OS key protection for a data folder (`platform_keys::provider`; a fake in tests).
type Provider = fn(&Path) -> Result<Arc<dyn KeyProtector>, CoreError>;

/// Starts the core (or reaches the running one for the same folder) and opens the account.
/// Push is configured for `platform` when this call opened the account or names a proxy
/// address; an account Dart opened keeps Dart's push configuration.
fn start_in(
    slot: &CoreSlot,
    bus: &EventBus,
    storage_root: &str,
    given_url: Option<String>,
    platform: Platform,
    provider: Provider,
) -> Result<NativeInfo, NativeError> {
    let explicit = given_url.is_some();
    let proxy_url = push_url(given_url);
    if let Some(url) = &proxy_url {
        chat_core::push::check_proxy_url(url)?;
    }
    let info = slot.start(CoreConfig { data_dir: PathBuf::from(storage_root) }, bus)?;
    let (account, opened) = match slot.account() {
        Ok(account) => (account, false),
        Err(_) => (slot.open_account(provider(&info.data_dir)?)?, true),
    };
    if opened || explicit {
        account.push_configure(PushConfig { proxy_url, platform: Some(platform), snapshot_dir: None })?;
    }
    Ok(NativeInfo { version: info.version, instance_id: info.instance_id })
}

/// Starts the core for the Android push receiver, its renewal job or `Application`, or reaches
/// the one Dart started: the same `instance_id` either way. `push_url` replaces the build's
/// proxy address (debug builds); `None` keeps it.
#[uniffi::export]
pub fn native_start(storage_root: String, push_url: Option<String>) -> Result<NativeInfo, NativeError> {
    start_in(&CORE, &BUS, &storage_root, push_url, Platform::Fcm, crate::platform_keys::provider)
}

/// A new FCM token (`onNewToken`): every handle is issued again under it.
#[uniffi::export]
pub fn native_push_token(token: String) -> Result<(), NativeError> {
    let account = CORE.account()?;
    wait_on(&CORE, CALL_WAIT, async move { account.push_set_token(Platform::Fcm, token).await })
}

fn tag_of(value: &str) -> Result<Tag, NativeError> {
    BASE64URL_NOPAD
        .decode(value.as_bytes())
        .ok()
        .and_then(|bytes| Tag::try_from(bytes.as_slice()).ok())
        .ok_or(NativeError::BadPayload)
}

/// Reads FCM `data` as the proxy builds it (plan 2): `t`+`e` cipher, `t`+`w:"1"` wake,
/// `t`+`r:"1"` read clear, base64url without padding. Anything else is `BadPayload`.
fn parse_push(data: &HashMap<String, String>) -> Result<ReceivedPush, NativeError> {
    let tag = tag_of(data.get("t").ok_or(NativeError::BadPayload)?)?;
    let field = |key: &str| data.get(key).map(String::as_str);
    match (data.len(), field("e"), field("w"), field("r")) {
        (2, Some(e), None, None) => {
            let ciphertext = BASE64URL_NOPAD
                .decode(e.as_bytes())
                .map_err(|_| NativeError::BadPayload)?;
            if ciphertext.len() > PUSH_CIPHER_MAX {
                return Err(NativeError::BadPayload);
            }
            Ok(ReceivedPush::Cipher { tag, ciphertext })
        }
        (2, None, Some("1"), None) => Ok(ReceivedPush::Wake { tag }),
        (2, None, None, Some("1")) => Ok(ReceivedPush::ReadClear),
        _ => Err(NativeError::BadPayload),
    }
}

/// The fetch deadline a received push gets: the caller's, at most `FETCH_MAX_MS`.
fn receive_deadline(deadline_ms: u32) -> Duration {
    Duration::from_millis(u64::from(deadline_ms).min(FETCH_MAX_MS))
}

fn received_in(
    slot: &CoreSlot,
    data: &HashMap<String, String>,
    deadline_ms: u32,
) -> Result<NativePushOutcome, NativeError> {
    let push = parse_push(data)?;
    let deadline = receive_deadline(deadline_ms);
    let account = slot.account()?;
    wait_on(slot, deadline + RECEIVE_MARGIN, async move { account.push_received(push, deadline).await })
        .map(NativePushOutcome::from)
}

/// A received FCM data message (`onMessageReceived`), processed within `deadline_ms` (at most
/// 20,000 ms) plus a 2-second margin. Notifications go to the `NativeNotifier`. On `Err` the
/// receiver shows its own "새 메시지", except for `BadPayload` (not a push of this app).
#[uniffi::export]
pub fn native_push_received(
    data: HashMap<String, String>,
    deadline_ms: u32,
) -> Result<NativePushOutcome, NativeError> {
    received_in(&CORE, &data, deadline_ms)
}

/// FCM dropped pushes for this device (`onDeletedMessages`): the core syncs with my devices and
/// every contact once the network is up.
#[uniffi::export]
pub fn native_push_deleted() -> Result<(), NativeError> {
    let account = CORE.account()?;
    wait_on(&CORE, CALL_WAIT, async move { account.push_messages_deleted().await })
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Issues the handles due now. When that sealed frames for peers, the network is kept up for a
/// while (at most `DELIVERY_WINDOW`, within `budget`) so they are sent, and stopped again only
/// if this call started it. Returns the handles issued.
fn renew_in(slot: &CoreSlot, budget: Duration) -> Result<u32, NativeError> {
    let started = Instant::now();
    let account = slot.account()?;
    let summary = wait_on(slot, budget, async move { account.push_renew_due(wall_ms()).await })?;
    let window = budget
        .saturating_sub(started.elapsed())
        .saturating_sub(STOP_MARGIN)
        .min(DELIVERY_WINDOW);
    if summary.announced + summary.withdrawn > 0 && !window.is_zero() {
        let network = slot.network()?;
        wait_on(slot, window + STOP_MARGIN, async move {
            let off = matches!(
                network.execute(NetworkOp::Info).await,
                Ok(NetworkReply::Info(info))
                    if matches!(info.state, NetworkState::Stopped | NetworkState::Failed)
            );
            let ours = off && network.execute(NetworkOp::Start).await.is_ok();
            tokio::time::sleep(window).await;
            if ours {
                network.execute(NetworkOp::Stop).await?;
            }
            Ok(())
        })?;
    }
    Ok(summary.issued)
}

/// The Android renewal job (every 24 hours): issues the handles due now, gives the peers the
/// new ones, and returns how many were issued. Call `native_start` first.
#[uniffi::export]
pub fn native_push_renew() -> Result<u32, NativeError> {
    renew_in(&CORE, RENEW_WAIT)
}

/// iOS background renewal (`cp_push_renew`): starts the core for `data_dir` (or reaches the
/// running one), opens the account and renews within `timeout_ms`. 0 done; negative codes:
/// -1 bad argument, -2 invalid configuration, -3 timed out, -4 the core could not do it.
#[cfg_attr(not(target_os = "ios"), allow(dead_code))]
pub(crate) fn background_renew(data_dir: &str, timeout_ms: u32, provider: Provider) -> i32 {
    renew_status(background_renew_in(&CORE, &BUS, data_dir, timeout_ms, provider))
}

fn background_renew_in(
    slot: &CoreSlot,
    bus: &EventBus,
    data_dir: &str,
    timeout_ms: u32,
    provider: Provider,
) -> Result<u32, NativeError> {
    let started = Instant::now();
    start_in(slot, bus, data_dir, None, Platform::Apns, provider)?;
    let budget = Duration::from_millis(u64::from(timeout_ms)).saturating_sub(started.elapsed());
    renew_in(slot, budget)
}

/// `cp_push_renew`'s status for a renewal result.
pub(crate) fn renew_status(result: Result<u32, NativeError>) -> i32 {
    match result {
        Ok(_) => 0,
        Err(NativeError::BadPayload) => -1,
        Err(NativeError::InvalidConfig) => -2,
        Err(NativeError::Timeout) => -3,
        Err(NativeError::Core { .. }) => -4,
    }
}

/// One conversation's notification as Kotlin shows it (spec 5.5; M6 channel and id rules).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeNotifyItem {
    /// The conversation's first 4 bytes as a signed big-endian i32 (Dart `notificationId`).
    pub id: i32,
    /// The contact's alias, or "새 메시지".
    pub title: String,
    /// The start of the last unread text (with " 외 N개"), or "새 메시지 N개".
    pub body: String,
    /// Update the shown notification without alerting (no new message).
    pub update: bool,
    /// Remove the notification (nothing unread); `title` and `body` are empty.
    pub cancel: bool,
    pub conversation: Vec<u8>,
}

/// Shows notifications for Kotlin. Called on a thread of its own, one batch at a time.
#[uniffi::export(callback_interface)]
pub trait NativeNotifier: Send + Sync {
    fn notify(&self, items: Vec<NativeNotifyItem>);
}

/// A conversation's notification id: its first 4 bytes as a signed big-endian i32.
fn notification_id(conversation: &[u8; 32]) -> i32 {
    i32::from_be_bytes([conversation[0], conversation[1], conversation[2], conversation[3]])
}

/// The notifications of a `Notify`; the open conversation's are left out unless they clear it.
fn native_items(items: Vec<chat_core::NotifyItem>, open: Option<i32>) -> Vec<NativeNotifyItem> {
    items
        .into_iter()
        .filter_map(|item| {
            let id = notification_id(&item.conversation.0);
            let cancel = item.unread == 0;
            if !cancel && open == Some(id) {
                return None;
            }
            let (title, body) = if cancel {
                (String::new(), String::new())
            } else {
                let title = if item.title.is_empty() { "새 메시지".to_owned() } else { item.title };
                let body = item.preview.unwrap_or_else(|| format!("새 메시지 {}개", item.unread));
                (title, body)
            };
            Some(NativeNotifyItem {
                id,
                title,
                body,
                update: !cancel && item.new_messages == 0,
                cancel,
                conversation: item.conversation.0.to_vec(),
            })
        })
        .collect()
}

/// Batches waiting for the notifier thread; more are dropped rather than stall the core.
const NOTIFY_QUEUE: usize = 64;

type Target = Arc<Mutex<Option<Arc<dyn NativeNotifier>>>>;

/// Hands `Notify` events to the `NativeNotifier` on a thread of its own: the bus calls `send`
/// under its lock, so a slow or failing callback must never run there.
struct Forwarder {
    open: Mutex<Option<i32>>,
    target: Target,
    /// `None` when the notifier thread could not be started.
    queue: Option<SyncSender<Vec<NativeNotifyItem>>>,
}

impl Forwarder {
    fn new() -> Self {
        let target: Target = Arc::default();
        let (queue, batches) = mpsc::sync_channel::<Vec<NativeNotifyItem>>(NOTIFY_QUEUE);
        let notifier = target.clone();
        let spawned = std::thread::Builder::new()
            .name("native-notifier".into())
            .spawn(move || {
                for items in batches {
                    let current = notifier.lock().ok().and_then(|slot| slot.clone());
                    if let Some(current) = current {
                        // A callback that fails loses this batch only.
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            current.notify(items)
                        }));
                    }
                }
            });
        Self { open: Mutex::new(None), target, queue: spawned.ok().map(|_| queue) }
    }

    fn set_notifier(&self, notifier: Box<dyn NativeNotifier>) {
        if let Ok(mut slot) = self.target.lock() {
            *slot = Some(Arc::from(notifier));
        }
    }

    fn set_open(&self, id: Option<i32>) {
        if let Ok(mut open) = self.open.lock() {
            *open = id;
        }
    }

    /// Queues the items to show; false once the notifier thread is gone.
    fn forward(&self, items: Vec<chat_core::NotifyItem>) -> bool {
        let Some(queue) = &self.queue else {
            return false;
        };
        let open = self.open.lock().map_or(None, |open| *open);
        let items = native_items(items, open);
        if items.is_empty() {
            return true;
        }
        match queue.try_send(items) {
            Ok(()) | Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

impl EventSink for Forwarder {
    fn send(&self, event: CoreEvent) -> bool {
        match event {
            CoreEvent::Notify { items } => self.forward(items),
            _ => true,
        }
    }
}

static FORWARDER: LazyLock<Arc<Forwarder>> = LazyLock::new(|| {
    let forwarder = Arc::new(Forwarder::new());
    // Android only: there Kotlin shows every notification (spec 5.5). Windows has Dart's
    // notifier and iOS the notification extension.
    #[cfg(target_os = "android")]
    BUS.subscribe(forwarder.clone());
    forwarder
});

/// Sets where `Notify` events go (Android; replaces an earlier notifier).
#[uniffi::export]
pub fn native_set_notifier(notifier: Box<dyn NativeNotifier>) {
    FORWARDER.set_notifier(notifier);
}

/// The notification id of the conversation on screen while the app is in the foreground, or
/// `None`: that conversation's notifications are not shown, only cleared.
#[uniffi::export]
pub fn native_set_open_conversation(id: Option<i32>) {
    FORWARDER.set_open(id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat_core::account::{PeerId, Secret32, StoreId};
    use chat_core::message::ConversationId;

    // This test exclusively owns starting the process-global core; other tests use local CoreSlots.
    #[tokio::test]
    async fn instance_id_is_none_before_start_and_matches_after() {
        use crate::api::chat;
        assert_eq!(native_instance_id(), None);
        assert_eq!(chat::network_info().await.unwrap_err().code, chat::ErrorCode::CoreNotStarted);
        assert_eq!(chat::network_start().await.unwrap_err().code, chat::ErrorCode::CoreNotStarted);
        assert_eq!(chat::network_stop().await.unwrap_err().code, chat::ErrorCode::CoreNotStarted);
        assert_eq!(chat::connect_peer("unused".into()).await.unwrap_err().code, chat::ErrorCode::CoreNotStarted);
        assert_eq!(chat::send_probe_text("unused".into(), "hello".into()).await.unwrap_err().code, chat::ErrorCode::CoreNotStarted);
        assert_eq!(chat::disconnect_peer("unused".into()).await.unwrap_err().code, chat::ErrorCode::CoreNotStarted);
        let tmp = tempfile::tempdir().unwrap();
        let info = crate::api::chat::core_start(tmp.path().to_string_lossy().into_owned()).unwrap();
        assert_eq!(native_instance_id(), Some(info.instance_id.clone()));
        assert_eq!(native_core_version(), crate::api::chat::core_version());
        let stopped = chat::network_info().await.unwrap();
        assert_eq!(stopped.state, chat::NetworkState::Stopped);
        assert_eq!(stopped.endpoint_id, None);
        assert_eq!(stopped.ticket, None);
        assert!(stopped.peers.is_empty());
        assert_eq!(chat::connect_peer("unused".into()).await.unwrap_err().code, chat::ErrorCode::NetworkNotStarted);
        assert_eq!(chat::send_probe_text("unused".into(), "hello".into()).await.unwrap_err().code, chat::ErrorCode::NetworkNotStarted);
        chat::disconnect_peer("unused".into()).await.unwrap();
        chat::network_stop().await.unwrap();
        chat::network_stop().await.unwrap();
        assert_eq!(chat::network_info().await.unwrap(), stopped);
        assert_eq!(native_instance_id(), Some(info.instance_id));
    }

    struct XorProtector;
    impl KeyProtector for XorProtector {
        fn protect(&self, _: StoreId, key: &Secret32) -> Result<Vec<u8>, CoreError> {
            Ok(key.as_bytes().iter().map(|b| b ^ 0x5a).collect())
        }
        fn unprotect(&self, _: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
            if blob.len() != 32 {
                return Err(CoreError::new(ErrorCode::StorageCorrupt, "test blob"));
            }
            Ok(Secret32::from(std::array::from_fn(|i| blob[i] ^ 0x5a)))
        }
        fn delete(&self, _: StoreId) -> Result<(), CoreError> {
            Ok(())
        }
    }
    fn test_keys(_: &Path) -> Result<Arc<dyn KeyProtector>, CoreError> {
        Ok(Arc::new(XorProtector))
    }
    fn root(dir: &tempfile::TempDir) -> String {
        dir.path().to_string_lossy().into_owned()
    }

    #[test]
    fn native_start_twice_same_instance_and_matches_core_start() {
        let dir = tempfile::tempdir().unwrap();
        let (slot, bus) = (CoreSlot::new(), EventBus::new());
        // Dart started first (`core_start` is `CORE.start`).
        let dart = slot.start(CoreConfig { data_dir: dir.path().into() }, &bus).unwrap();
        let first = start_in(&slot, &bus, &root(&dir), None, Platform::Fcm, test_keys).unwrap();
        let second = start_in(&slot, &bus, &root(&dir), None, Platform::Fcm, test_keys).unwrap();
        assert_eq!(first.instance_id, dart.instance_id);
        assert_eq!(second, first);
        assert_eq!(first.version, chat_core::CORE_VERSION);
        assert!(slot.account().is_ok());

        // Kotlin first: Dart's later start reaches the same core.
        let other = tempfile::tempdir().unwrap();
        let (slot, bus) = (CoreSlot::new(), EventBus::new());
        let native = start_in(&slot, &bus, &root(&other), None, Platform::Fcm, test_keys).unwrap();
        let dart = slot.start(CoreConfig { data_dir: other.path().into() }, &bus).unwrap();
        assert_eq!(dart.instance_id, native.instance_id);
        // Another folder in the same process is refused, as for Dart.
        let elsewhere = tempfile::tempdir().unwrap();
        assert_eq!(
            start_in(&slot, &bus, &root(&elsewhere), None, Platform::Fcm, test_keys),
            Err(NativeError::Core { code: "AlreadyStartedDifferentConfig".into() })
        );
        assert_eq!(
            start_in(&CoreSlot::new(), &bus, "relative/dir", None, Platform::Fcm, test_keys),
            Err(NativeError::InvalidConfig)
        );
    }

    #[test]
    fn push_url_http_refused_in_release() {
        let dir = tempfile::tempdir().unwrap();
        let (slot, bus) = (CoreSlot::new(), EventBus::new());
        let http = Some("http://127.0.0.1:9".to_owned());
        let result = start_in(&slot, &bus, &root(&dir), http, Platform::Fcm, test_keys);
        #[cfg(debug_assertions)]
        assert!(result.is_ok(), "{result:?}");
        #[cfg(not(debug_assertions))]
        {
            assert_eq!(result, Err(NativeError::InvalidConfig));
            // Refused before anything started.
            assert!(slot.info().is_none());
        }
        // Not a URL this app knows, in any build.
        let ftp = Some("ftp://push.example".to_owned());
        assert_eq!(
            start_in(&slot, &bus, &root(&dir), ftp, Platform::Fcm, test_keys),
            Err(NativeError::InvalidConfig)
        );
        let https = Some("https://push.example".to_owned());
        assert!(start_in(&slot, &bus, &root(&dir), https, Platform::Fcm, test_keys).is_ok());
    }

    fn data(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn native_push_received_parses_payload_kinds() {
        let tag: Tag = std::array::from_fn(|i| i as u8 + 1);
        let t = BASE64URL_NOPAD.encode(&tag);
        let e = BASE64URL_NOPAD.encode(b"cipher bytes");
        assert_eq!(
            parse_push(&data(&[("t", &t), ("e", &e)])),
            Ok(ReceivedPush::Cipher { tag, ciphertext: b"cipher bytes".to_vec() })
        );
        assert_eq!(parse_push(&data(&[("t", &t), ("w", "1")])), Ok(ReceivedPush::Wake { tag }));
        assert_eq!(parse_push(&data(&[("t", &t), ("r", "1")])), Ok(ReceivedPush::ReadClear));
        let largest = BASE64URL_NOPAD.encode(&vec![7u8; PUSH_CIPHER_MAX]);
        assert!(parse_push(&data(&[("t", &t), ("e", &largest)])).is_ok());

        let short = BASE64URL_NOPAD.encode(&tag[..15]);
        let long = BASE64URL_NOPAD.encode(&[0u8; 17]);
        let padded = format!("{}=", BASE64URL_NOPAD.encode(b"cipher"));
        let standard = "ab+/cd".to_owned();
        let too_big = BASE64URL_NOPAD.encode(&vec![7u8; PUSH_CIPHER_MAX + 1]);
        let bad: Vec<HashMap<String, String>> = vec![
            data(&[]),
            data(&[("e", &e)]),
            data(&[("w", "1")]),
            data(&[("r", "1")]),
            data(&[("t", &t)]),
            data(&[("t", &short), ("w", "1")]),
            data(&[("t", &long), ("w", "1")]),
            data(&[("t", "not base64!"), ("w", "1")]),
            data(&[("t", &t), ("e", &padded)]),
            data(&[("t", &t), ("e", &standard)]),
            data(&[("t", &t), ("e", &too_big)]),
            data(&[("t", &t), ("w", "0")]),
            data(&[("t", &t), ("r", "true")]),
            data(&[("t", &t), ("e", &e), ("w", "1")]),
            data(&[("t", &t), ("w", "1"), ("r", "1")]),
            data(&[("t", &t), ("w", "1"), ("x", "1")]),
            data(&[("t", &t), ("x", "1")]),
            data(&[("t", "\u{1F600}"), ("w", "1")]),
        ];
        for payload in &bad {
            assert_eq!(parse_push(payload), Err(NativeError::BadPayload), "{:?}", payload.keys());
        }
        // Refused before the core is reached: a core that is not even started is not asked.
        assert_eq!(received_in(&CoreSlot::new(), &bad[13], 5_000), Err(NativeError::BadPayload));
        assert_eq!(
            received_in(&CoreSlot::new(), &data(&[("t", &t), ("w", "1")]), 5_000),
            Err(NativeError::Core { code: "CoreNotStarted".into() })
        );
        assert_eq!(receive_deadline(60_000), Duration::from_millis(FETCH_MAX_MS));
        assert_eq!(receive_deadline(5_000), Duration::from_millis(5_000));
        assert_eq!(receive_deadline(u32::MAX), Duration::from_millis(FETCH_MAX_MS));
    }

    struct Recorder(mpsc::Sender<Vec<NativeNotifyItem>>);
    impl NativeNotifier for Recorder {
        fn notify(&self, items: Vec<NativeNotifyItem>) {
            let _ = self.0.send(items);
        }
    }

    fn conversation(first: [u8; 4]) -> ConversationId {
        let mut id = [9u8; 32];
        id[..4].copy_from_slice(&first);
        ConversationId(id)
    }
    fn item(first: [u8; 4], new_messages: u32, unread: u64, title: &str, preview: Option<&str>) -> chat_core::NotifyItem {
        chat_core::NotifyItem {
            conversation: conversation(first),
            peer: PeerId([3; 32]),
            new_messages,
            unread,
            title: title.into(),
            preview: preview.map(str::to_owned),
        }
    }
    fn recording() -> (Forwarder, mpsc::Receiver<Vec<NativeNotifyItem>>) {
        let forwarder = Forwarder::new();
        let (sender, received) = mpsc::channel();
        forwarder.set_notifier(Box::new(Recorder(sender)));
        (forwarder, received)
    }
    fn next(received: &mpsc::Receiver<Vec<NativeNotifyItem>>) -> Vec<NativeNotifyItem> {
        received.recv_timeout(Duration::from_secs(5)).expect("a notify batch")
    }

    #[test]
    fn notifier_gets_notify_items_with_ids() {
        let (forwarder, received) = recording();
        assert!(forwarder.send(CoreEvent::ConnectionState(chat_core::ConnectionSnapshot::OFFLINE)));
        assert!(forwarder.send(CoreEvent::Notify {
            items: vec![
                item([0x00, 0x00, 0x01, 0x02], 1, 1, "Alice", Some("hello")),
                item([0xff, 0xff, 0xff, 0xfe], 0, 3, "", None),
                item([0x80, 0x00, 0x00, 0x00], 0, 0, "Bob", None),
            ],
        }));
        let items = next(&received);
        assert_eq!(
            items,
            vec![
                NativeNotifyItem {
                    id: 0x0102,
                    title: "Alice".into(),
                    body: "hello".into(),
                    update: false,
                    cancel: false,
                    conversation: conversation([0, 0, 1, 2]).0.to_vec(),
                },
                NativeNotifyItem {
                    id: -2,
                    title: "새 메시지".into(),
                    body: "새 메시지 3개".into(),
                    update: true,
                    cancel: false,
                    conversation: conversation([0xff, 0xff, 0xff, 0xfe]).0.to_vec(),
                },
                NativeNotifyItem {
                    id: i32::MIN,
                    title: String::new(),
                    body: String::new(),
                    update: false,
                    cancel: true,
                    conversation: conversation([0x80, 0, 0, 0]).0.to_vec(),
                },
            ]
        );
        // Only `Notify` reaches the notifier.
        assert!(received.recv_timeout(Duration::from_millis(200)).is_err());

        // A callback that panics, or one that is slow, does not hold the core's publisher.
        struct Slow(mpsc::Sender<Vec<NativeNotifyItem>>);
        impl NativeNotifier for Slow {
            fn notify(&self, items: Vec<NativeNotifyItem>) {
                if items[0].body == "panic" {
                    panic!("callback failed");
                }
                std::thread::sleep(Duration::from_millis(300));
                let _ = self.0.send(items);
            }
        }
        let (sender, slow) = mpsc::channel();
        forwarder.set_notifier(Box::new(Slow(sender)));
        let begin = Instant::now();
        for body in ["panic", "a", "b"] {
            assert!(forwarder.send(CoreEvent::Notify { items: vec![item([1, 2, 3, 4], 1, 1, "", Some(body))] }));
        }
        assert!(begin.elapsed() < Duration::from_millis(250), "send waited on the callback");
        assert_eq!(next(&slow)[0].body, "a");
        assert_eq!(next(&slow)[0].body, "b");
        // A full queue drops batches instead of waiting.
        let begin = Instant::now();
        for _ in 0..NOTIFY_QUEUE * 2 {
            assert!(forwarder.send(CoreEvent::Notify { items: vec![item([1, 2, 3, 4], 1, 1, "", Some("x"))] }));
        }
        assert!(begin.elapsed() < Duration::from_millis(250), "a full queue blocked send");
    }

    #[test]
    fn notifier_skips_open_conversation() {
        let (forwarder, received) = recording();
        let open = [0x12, 0x34, 0x56, 0x78];
        forwarder.set_open(Some(0x1234_5678));
        assert!(forwarder.send(CoreEvent::Notify {
            items: vec![item(open, 1, 1, "Alice", Some("on screen")), item([1, 1, 1, 1], 1, 1, "Bob", Some("elsewhere"))],
        }));
        let items = next(&received);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].body, "elsewhere");
        // Nothing at all for the open conversation alone; its clearing still goes through.
        assert!(forwarder.send(CoreEvent::Notify { items: vec![item(open, 0, 2, "Alice", Some("update"))] }));
        assert!(forwarder.send(CoreEvent::Notify { items: vec![item(open, 0, 0, "Alice", None)] }));
        let items = next(&received);
        assert_eq!((items.len(), items[0].id, items[0].cancel), (1, 0x1234_5678, true));
        // Left the conversation (or the app went to the background): notified again.
        forwarder.set_open(None);
        assert!(forwarder.send(CoreEvent::Notify { items: vec![item(open, 1, 1, "Alice", Some("back"))] }));
        assert_eq!(next(&received)[0].body, "back");
    }

    #[test]
    fn renew_status_codes_and_a_core_not_started() {
        assert_eq!(renew_status(Ok(3)), 0);
        assert_eq!(renew_status(Err(NativeError::BadPayload)), -1);
        assert_eq!(renew_status(Err(NativeError::InvalidConfig)), -2);
        assert_eq!(renew_status(Err(NativeError::Timeout)), -3);
        assert_eq!(renew_status(Err(NativeError::core(ErrorCode::NotAvailable))), -4);
        assert_eq!(renew_in(&CoreSlot::new(), RENEW_WAIT), Err(NativeError::Core { code: "CoreNotStarted".into() }));
        // A fresh account has nothing to renew: the background renewal still starts and opens.
        let dir = tempfile::tempdir().unwrap();
        let (slot, bus) = (CoreSlot::new(), EventBus::new());
        let result = background_renew_in(&slot, &bus, &root(&dir), 10_000, test_keys);
        assert!(slot.account().is_ok());
        assert!(matches!(result, Ok(0) | Err(NativeError::Core { .. })), "{result:?}");
    }

    #[tokio::test]
    async fn waiting_on_a_runtime_thread_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (slot, bus) = (CoreSlot::new(), EventBus::new());
        slot.start(CoreConfig { data_dir: dir.path().into() }, &bus).unwrap();
        let result = wait_on(&slot, CALL_WAIT, async { Ok(()) });
        assert_eq!(result, Err(NativeError::Core { code: "Busy".into() }));
        // A thread of its own (as Kotlin's and Swift's are) waits.
        std::thread::spawn(move || {
            assert_eq!(wait_on(&slot, CALL_WAIT, async { Ok(7) }), Ok(7));
            let slow = wait_on(&slot, Duration::from_millis(50), async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(())
            });
            assert_eq!(slow, Err(NativeError::Timeout));
        })
        .join()
        .unwrap();
    }
}
