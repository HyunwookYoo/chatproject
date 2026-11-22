//! Push handles of the account actor (M7 spec 5.1, 5.2, 8): issuing and renewing this device's
//! handles at the proxy, announcing them to the conversation peers over MLS, withdrawing them
//! when push goes off, and delivering the sealed frames to each current peer device.
//!
//! * While push is on and the device has a platform and a token, every conversation with a
//!   group has a tag of its own; a conversation without a handle, or with one issued
//!   `RENEW_AFTER_MS` ago, gets a new one from `/handle`. So does the own-device handle of an
//!   Android device with another own device (it rides the kind 18 `Announce`). Checked right
//!   after `push_configure` and `push_set_token`, on `push_renew_due`, and every
//!   `RENEW_CHECK_MS` while the core runs.
//! * The `/handle` requests run in a spawned task, one after another, and stop at the first
//!   failure; nothing waits on them inside a transaction or an actor turn. Their result is
//!   stored by one short transaction, and only if push did not go off and the token did not
//!   change meanwhile (the generation counter here, and the stored platform and token).
//! * A conversation whose handle (or withdrawal) is not announced yet is sealed as
//!   `Content::PushHandle` with a frame time above any it was sent with before (Ruling R11) and
//!   queued in `push_delivery` for every current device of the peer account; a newer frame
//!   replaces what is still queued. The queue is sent like `mls_delivery`: over a ready
//!   session, one frame per recipient at a time, a recipient without one dialed at most every
//!   `DIAL_RETRY_MS`, back-off 1 s doubling to 5 min. A mailbox handoff (M6 spec 7) carries the
//!   queue too; its `Delivered` note empties the rows of that frame. The `QueueRef` frames of
//!   this device's server queues (M8, `manager/queue.rs`) are rows of their own kind in the same
//!   table, sent, acknowledged and handed off the same way.
//! * Push off: every conversation that had a handle gets a withdrawal (an empty handle), the
//!   token and handles are dropped, and no proxy request is made until push is on again.
//! * What revoked or unlisted devices announced, and what is queued for them, goes when the
//!   records change.
//! * Sending (spec 6): a text's first attempt that has a result decides, in the transaction
//!   recording that result, on one `/push` with an item for each current device of the peer
//!   that announced a handle and was neither reached nor sent a copy (`push_store::FirstAttempt`);
//!   retries, mailbox handoffs, receipts, Commits and PushHandles never push. A `/push` runs in a
//!   spawned task, bounded by `PUSH_TIMEOUT`; only its `gone` results come back, withdrawing
//!   those handles. Read clearing (spec 7): reads here push one `read`, `READ_CLEAR_MS` after the
//!   first of them, to my devices with an own-device handle and no session. A device it skipped
//!   for its session gets that `read` once if the session goes within `READ_RECHECK_MS`.
//! * Receiving (spec 5.3, 5.4, 7): a push's tag names the conversation of one of this device's
//!   current handles; an unknown one shows nothing. A `cipher` gets its group id back and is
//!   opened like a received frame (`Via::Push`: no answer is sent), so a copy that also comes
//!   directly is the same ciphertext and is taken once. A text stored just now alerts, one stored
//!   already and unread updates its notification silently, one read already shows nothing; what
//!   cannot be opened alerts as "새 메시지". A `wake` alerts as "새 메시지" first, then dials the
//!   sender's devices and syncs with my devices until the caller's deadline, or until a
//!   notification of the conversation shows a text in the generic one's place. Ruling R13: once
//!   a generic notification alerted, the conversation's next text (by any path, also after a
//!   restart: the mark is `push_tag.generic`) takes its place without alerting (`silenced`,
//!   shown also where the generic one was dismissed) and ends it; so does a read here or a
//!   synced read of a text not here yet, once nothing is unread.
//!   A read clear syncs with my devices until the deadline. Both start the
//!   network only if it was off, and stop only what they started; the waits happen in the
//!   caller's task, never in an actor turn.
use super::*;
use super::mls::{Received, Via};
use crate::account::connection::{Input, Session};
use crate::message::{
    mls_group::Delivery,
    push_store::{
        FirstAttempt, FrameKind, Issued, PushBatch, PushDbOp, PushDbReply, PushDue, RenewPlan,
        Tagged,
    },
    store::{MessageDbOp, MessageDbReply},
    types::{ConversationId, MessageId, MessageKey},
};
use crate::mls::{
    group::MAX_MLS_MESSAGE_BYTES,
    push_ct::{PushCtError, restore_group_id},
    wire::MlsReply,
};
use crate::push::{
    FETCH_MAX_MS, HandleRequest, Platform, PushConfig, PushError, PushOutcome, PushProxy,
    PushResult, PushStatus, ReceivedPush, RenewSummary, Tag,
};
use crate::mailbox::wire::HandoffEntry;
use crate::{NetworkOp, NetworkReply, NetworkState};
use push_wire::BASE64URL_NOPAD;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// How often the handles are checked for renewal while the core runs.
const RENEW_CHECK_MS: u64 = 3_600_000;
/// Outer bound of one `/handle` request, whatever the proxy implementation bounds itself to.
const HANDLE_TIMEOUT: Duration = Duration::from_secs(20);
/// A recipient of a queued frame without a session is dialed at most this often.
const DIAL_RETRY_MS: u64 = 30_000;
/// The queue is looked through for recipients to dial at most this often.
const DIAL_CHECK_MS: u64 = 1_000;
/// Outer bound of one `/push` request, whatever the proxy implementation bounds itself to.
const PUSH_TIMEOUT: Duration = Duration::from_secs(20);
/// Reads within this long of the first one clear notifications with one push (spec 7).
const READ_CLEAR_MS: u64 = 1_000;
/// A device a read clear skipped for its ready session gets the `read` if that session goes
/// within this long of the decision (spec 7, amended 2026-11-15).
const READ_RECHECK_MS: u64 = 600_000;
/// While a fetch for a received push waits for the network to come up, it looks this often.
const NETWORK_LOOK: Duration = Duration::from_millis(50);
/// A fetch that got what it waited for keeps the network this much longer: the answer to the
/// frame that brought it (the sender's `Ack`, so it shows delivered) is still on its way.
const FETCH_LINGER: Duration = Duration::from_millis(1_000);
/// While `push_messages_deleted` waits for the network to start, it looks this often.
const FULL_SYNC_LOOK_MS: u64 = 1_000;
/// Longest a background renewal holds the network for the frames it sealed.
const RENEW_DELIVERY: Duration = Duration::from_secs(10);

/// What `AccountHandle` shares with the actor for push.
#[derive(Default)]
pub(super) struct Shared {
    /// The latest `push_configure`.
    config: ConfigSlot,
    /// The network as received pushes use it.
    network: tokio::sync::Mutex<NetworkLease>,
    /// Each fetch of a received push (`wake`, read clear): whether the network was online in
    /// time, and whether what it waited for came in time.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    fetches: std::sync::Mutex<Vec<(bool, bool)>>,
}

/// Received pushes fetching now, and whether they started the network: the last of them to end
/// stops it, unless a start was asked for since by anyone else (the app came to the foreground,
/// pairing): `starts` is the network's start count right after the push's own start.
#[derive(Default)]
struct NetworkLease {
    users: u32,
    started: bool,
    starts: u64,
}

/// The latest `push_configure` and its version, for the actor to take.
#[derive(Default)]
pub(super) struct ConfigSlot(std::sync::Mutex<(u64, Option<PushConfig>)>);
impl ConfigSlot {
    fn set(&self, config: PushConfig) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        *slot = (slot.0.wrapping_add(1), Some(config));
    }
    /// The configuration if it is newer than version `seen`.
    fn newer(&self, seen: u64) -> Option<(u64, PushConfig)> {
        let slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match &slot.1 {
            Some(config) if slot.0 != seen => Some((slot.0, config.clone())),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(crate) struct Runtime {
    /// Version of the configuration taken last.
    config_seen: u64,
    /// The proxy of the configured address (`None`: no address, no request of any kind).
    proxy: Option<Arc<dyn PushProxy>>,
    /// Moves on when push goes off, the token or the platform changes: a renewal of an older
    /// generation stores nothing and sends no further request.
    generation: Arc<std::sync::atomic::AtomicU64>,
    /// A renewal's `/handle` requests are running.
    renewing: bool,
    /// The wall time the running renewal looks at.
    running_wall: u64,
    /// Another renewal is wanted once the running one ends (a later time was asked for, or the
    /// running one was made stale), at this wall time at the latest.
    again: Option<u64>,
    /// `push_renew_due` callers waiting for the running (or next) renewal.
    waiters: Vec<oneshot::Sender<Result<RenewSummary, CoreError>>>,
    /// Scheduler time of the next renewal check.
    check_at: u64,
    /// Some conversation may have a state not sealed yet.
    seal_due: bool,
    /// `push_local` was read (`own_handle` is what it holds).
    loaded: bool,
    /// The own-device handle the `Announce` carries.
    own_handle: Option<Vec<u8>>,
    /// Frames being sent, one per recipient of a conversation at a time.
    inflight: BTreeSet<(ConversationId, InstallationId)>,
    /// Scheduler time the queue is read next (`None`: at the next poll).
    delivery_at: Option<u64>,
    /// Ready sessions to other accounts' devices seen by the last read of the queue.
    sessions: Vec<InstallationId>,
    /// Due frames are left (some recipient had no session).
    left: bool,
    /// When each recipient without a session was last dialed, within `DIAL_RETRY_MS`.
    dials: BTreeMap<InstallationId, u64>,
    dial_check_at: u64,
    /// Hash of the device lists the stored handles were last pruned against.
    fingerprint: Option<[u8; 32]>,
    /// Scheduler time the pending read clear is sent at.
    read_clear_at: Option<u64>,
    /// My devices the latest read clear skipped for their ready session, with the scheduler
    /// time until which that session going still sends them the `read` (one entry each).
    read_recheck: BTreeMap<InstallationId, u64>,
    /// Wake pushes fetching now, per conversation, with the unread count their generic
    /// notification showed: told when a notification of the conversation shows a text in its
    /// place. One whose caller stopped waiting (closed) is over.
    waking: BTreeMap<ConversationId, (oneshot::Sender<()>, u64)>,
    /// Read clear pushes waiting for a sync with one of my devices to end.
    syncs: Vec<oneshot::Sender<()>>,
    /// `push_messages_deleted`: once the network is up, my devices are synced with and every
    /// contact's devices dialed.
    full_sync: bool,
    /// Each read clear decided: my other devices with a ready session, `read` items pushed.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) read_clear_log: Vec<(u32, u32)>,
    /// The network is being looked at for `full_sync`, and when it is looked at next.
    probing: bool,
    probe_at: u64,
}

/// A spawned push request came back.
pub(crate) enum PushEvent {
    Issued {
        ticket: u64,
        generation: u64,
        issued: Box<Issued>,
    },
    Delivered {
        ticket: u64,
        conversation: ConversationId,
        recipient: InstallationId,
        kind: FrameKind,
        msg_id: MessageId,
        outcome: Delivery,
    },
    /// The proxy answered `gone` for these handles of a `/push` (see `PushBatch`).
    Gone {
        ticket: u64,
        conversation: Option<ConversationId>,
        gone: Vec<(InstallationId, Vec<u8>)>,
    },
    /// Whether the network is up, looked at for `push_messages_deleted`.
    Network { ticket: u64, online: bool },
}

/// A wake push's conversation peer, and what tells its fetch is done.
type Waking = (PeerId, oneshot::Receiver<()>);

/// What a received push asks of the actor (`AccountHandle::push_received` and the rest).
pub(super) enum PushCommand {
    Cipher {
        tag: Tag,
        ciphertext: Vec<u8>,
        reply: oneshot::Sender<Result<PushOutcome, CoreError>>,
    },
    /// Shows the generic notification of a wake push's conversation; replies its peer and what
    /// tells the fetch is done (`None`: the tag is unknown).
    Wake {
        tag: Tag,
        reply: oneshot::Sender<Result<Option<Waking>, CoreError>>,
    },
    /// Replies what tells a sync with one of my devices ended (a read clear push; `None`: `tag`
    /// is not this device's current own-device tag).
    ReadClear {
        tag: Tag,
        reply: oneshot::Sender<Result<Option<oneshot::Receiver<()>>, CoreError>>,
    },
    /// Dials the devices of `peer` without a session, and syncs with my devices.
    Fetch {
        peer: Option<PeerId>,
        reply: oneshot::Sender<Result<(), CoreError>>,
    },
    Ingest {
        entries: Vec<(Tag, Vec<u8>)>,
        reply: oneshot::Sender<Result<u32, CoreError>>,
    },
    MessagesDeleted {
        reply: oneshot::Sender<Result<(), CoreError>>,
    },
    ConversationOf {
        tag: Tag,
        reply: oneshot::Sender<Result<Option<ConversationId>, CoreError>>,
    },
}

impl PushCommand {
    pub(super) fn reject(self, error: CoreError) {
        match self {
            Self::Cipher { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::Wake { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::ReadClear { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::Fetch { reply, .. } | Self::MessagesDeleted { reply } => {
                let _ = reply.send(Err(error));
            }
            Self::Ingest { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::ConversationOf { reply, .. } => {
                let _ = reply.send(Err(error));
            }
        }
    }
}

/// How a received ciphertext opened.
enum PushOpened {
    /// A text, stored just now or not.
    Text(MessageKey, bool),
    /// Something that is not a text (nothing to show).
    Other,
    /// It could not be opened or taken.
    Failed,
}

impl AccountHandle {
    /// Tells the core the push proxy address, this device's platform and the iOS snapshot
    /// directory (at every start; spec 5.1). Release builds take an `https://` address only.
    /// The actor takes it at its next poll and checks the handles right after.
    pub fn push_configure(&self, config: PushConfig) -> Result<(), CoreError> {
        if let Some(url) = &config.proxy_url {
            crate::push::check_proxy_url(url)?;
        }
        self.shared.push.config.set(config);
        Ok(())
    }
    /// A push this device received (spec 5.3, 5.4, 7), processed within `deadline` (at most
    /// `FETCH_MAX_MS`). Shown through `CoreEvent::Notify`; an `Err` (the account cannot be used
    /// now) shows nothing, and the platform shows its own "새 메시지".
    pub async fn push_received(
        &self,
        push: ReceivedPush,
        deadline: Duration,
    ) -> Result<PushOutcome, CoreError> {
        let until = tokio::time::Instant::now()
            + deadline.min(Duration::from_millis(FETCH_MAX_MS));
        match push {
            ReceivedPush::Cipher { tag, ciphertext } => {
                self.send(|reply| {
                    Command::Push(PushCommand::Cipher {
                        tag,
                        ciphertext,
                        reply,
                    })
                })
                .await
            }
            ReceivedPush::Wake { tag } => {
                // The generic notification first: the fetch may not end in time.
                let Some((peer, fetched)) = self
                    .send(|reply| Command::Push(PushCommand::Wake { tag, reply }))
                    .await?
                else {
                    return Ok(PushOutcome::UnknownTag);
                };
                self.push_fetch(Some(peer), fetched, until).await;
                Ok(PushOutcome::Notified)
            }
            ReceivedPush::ReadClear { tag } => {
                let Some(synced) = self
                    .send(|reply| Command::Push(PushCommand::ReadClear { tag, reply }))
                    .await?
                else {
                    return Ok(PushOutcome::UnknownTag);
                };
                Ok(if self.push_fetch(None, synced, until).await {
                    PushOutcome::Updated
                } else {
                    PushOutcome::Nothing
                })
            }
        }
    }
    /// FCM dropped pushes for this device (`onDeletedMessages`): once the network is up, this
    /// device syncs with my devices and dials every contact's devices.
    pub async fn push_messages_deleted(&self) -> Result<(), CoreError> {
        self.send(|reply| Command::Push(PushCommand::MessagesDeleted { reply }))
            .await
    }
    /// The ciphertexts the iOS notification extension kept (spec 5.6), in order: each is taken
    /// like a `cipher` push, without a notification (the extension showed one). Returns how
    /// many texts were stored; ones stored already, and unknown tags, are skipped.
    pub async fn push_ingest_inbox(&self, entries: Vec<(Tag, Vec<u8>)>) -> Result<u32, CoreError> {
        self.send(|reply| Command::Push(PushCommand::Ingest { entries, reply }))
            .await
    }
    /// The conversation `tag` names, if it is one of this device's current handles.
    pub async fn push_conversation_of(&self, tag: Tag) -> Result<Option<ConversationId>, CoreError> {
        self.send(|reply| Command::Push(PushCommand::ConversationOf { tag, reply }))
            .await
    }

    /// Fetches for a received push until `done` is told or `until`: starts the network if it is
    /// off, dials `peer`'s devices and syncs with my devices, and stops the network again if it
    /// started it (and no other push still fetches). True when `done` was told in time; the
    /// network then stays up `FETCH_LINGER` longer (within `until`) for the answers on their way.
    async fn push_fetch(
        &self,
        peer: Option<PeerId>,
        done: oneshot::Receiver<()>,
        until: tokio::time::Instant,
    ) -> bool {
        let lease = NetworkUse::acquire(self.shared.clone()).await;
        let online = lease.online(until).await;
        if online {
            let _ = self
                .send(|reply| Command::Push(PushCommand::Fetch { peer, reply }))
                .await;
        }
        let fetched = matches!(tokio::time::timeout_at(until, done).await, Ok(Ok(())));
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.shared
            .push
            .fetches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((online, fetched));
        #[cfg(not(all(feature = "test-hooks", debug_assertions)))]
        let _ = online;
        if fetched {
            let linger = tokio::time::Instant::now() + FETCH_LINGER;
            tokio::time::sleep_until(linger.min(until)).await;
        }
        lease.release().await;
        fetched
    }
    /// A new FCM/APNs token: taken while push is on, and every handle is issued again under it.
    pub async fn push_set_token(&self, platform: Platform, token: String) -> Result<(), CoreError> {
        self.send(|reply| Command::PushSetToken(platform, token, reply))
            .await
    }
    /// Push on, or off (spec 8): off withdraws every handle at the conversation peers, drops
    /// the token, and sends the proxy nothing until push is on again.
    pub async fn push_set_enabled(&self, enabled: bool) -> Result<(), CoreError> {
        self.send(|reply| Command::PushSetEnabled(enabled, reply))
            .await
    }
    pub async fn push_status(&self) -> Result<PushStatus, CoreError> {
        self.send(Command::PushStatus).await
    }
    /// Each fetch of a received push so far, in order: whether the network was online in time,
    /// and whether what it waited for (the sync, the woken text) came in time.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub fn push_test_fetches(&self) -> Vec<(bool, bool)> {
        self.shared
            .push
            .fetches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Issues the handles due at `now_ms` (wall time) and seals them for the peers; returns
    /// once that renewal ended (the background renewal of Android and iOS).
    pub async fn push_renew_due(&self, now_ms: u64) -> Result<RenewSummary, CoreError> {
        self.send(|reply| Command::PushRenewDue(now_ms, reply))
            .await
    }
    /// The background renewal (Android job, iOS BGAppRefresh): `push_renew_due`, then, when it
    /// sealed frames for peers, the network is held through the push lease (started if it is
    /// off) for at most `RENEW_DELIVERY` so they are sent, ending by `budget` after the call
    /// began. A start by the app or a pairing meanwhile keeps the network on at the end.
    pub async fn push_renew_background(
        &self,
        now_ms: u64,
        budget: Duration,
    ) -> Result<RenewSummary, CoreError> {
        let until = tokio::time::Instant::now() + budget;
        let summary = self.push_renew_due(now_ms).await?;
        let deliver_until = until.min(tokio::time::Instant::now() + RENEW_DELIVERY);
        if summary.announced + summary.withdrawn > 0 && tokio::time::Instant::now() < deliver_until
        {
            let lease = NetworkUse::acquire(self.shared.clone()).await;
            tokio::time::sleep_until(deliver_until).await;
            lease.release().await;
        }
        Ok(summary)
    }
}

/// One `/handle` request: the handle bytes, or why there is none.
async fn issue(
    proxy: &dyn PushProxy,
    platform: Platform,
    token: &str,
    tag: Tag,
) -> Result<Vec<u8>, PushError> {
    let request = HandleRequest {
        platform,
        token: token.to_owned(),
        tag: BASE64URL_NOPAD.encode(&tag),
    };
    let response = tokio::time::timeout(HANDLE_TIMEOUT, proxy.handle(request))
        .await
        .map_err(|_| PushError::Timeout)??;
    BASE64URL_NOPAD
        .decode(response.handle.as_bytes())
        .map_err(|_| PushError::Malformed)
}

#[cfg(feature = "push-http")]
fn http_proxy(url: &str) -> Option<Arc<dyn PushProxy>> {
    let url = crate::push::Url::parse(url).ok()?;
    let proxy = crate::push::HttpPushProxy::new(url).ok()?;
    Some(Arc::new(proxy))
}
#[cfg(not(feature = "push-http"))]
fn http_proxy(_url: &str) -> Option<Arc<dyn PushProxy>> {
    None
}

impl Manager {
    async fn push_db(&mut self, op: PushDbOp, request: Request) -> Result<PushDbReply, CoreError> {
        match self.message_db(MessageDbOp::Push(op), request).await? {
            MessageDbReply::Push(reply) => Ok(reply),
            _ => Err(unavailable(ErrorCode::InvalidMessage)),
        }
    }

    /// What the next `Announce` to my other devices carries (spec 5.2).
    pub(super) fn push_own_handle(&self) -> Option<Vec<u8>> {
        self.messages.push.own_handle.clone()
    }

    /// `push_local` was read, so `push_own_handle` is what is stored.
    pub(super) fn push_loaded(&self) -> bool {
        self.messages.push.loaded
    }

    /// The hash of the device lists this poll pruned against (`None` before the first).
    pub(super) fn push_devices(&self) -> Option<[u8; 32]> {
        self.messages.push.fingerprint
    }

    /// New frames were queued in `push_delivery` (the queues' `QueueRef`s): read at the next
    /// poll.
    pub(super) fn push_deliver_soon(&mut self) {
        self.messages.push.delivery_at = None;
    }

    /// Reads `push_local` again. A changed own-device handle reaches my other devices with the
    /// next poll's `Announce` (own-device sync tells each one until it took what holds now).
    async fn push_reload(&mut self, request: Request) -> Result<(), CoreError> {
        let PushDbReply::Local(local) = self.push_db(PushDbOp::Local, request).await? else {
            return Err(unavailable(ErrorCode::InvalidMessage));
        };
        let push = &mut self.messages.push;
        push.own_handle = local.own_handle;
        push.loaded = true;
        Ok(())
    }

    /// One tick of push upkeep, with the message scheduler's poll.
    pub(super) async fn poll_push(&mut self) {
        let Ok(request) = self.ready() else { return };
        if self.local().is_err() {
            return;
        }
        if !self.messages.push.loaded && self.push_reload(request).await.is_err() {
            return;
        }
        if self.push_take_config(request).await.is_err() {
            return;
        }
        if self.push_prune(request).await.is_err() {
            return;
        }
        let now = self.schedule_now();
        let push = &self.messages.push;
        if !push.renewing && now >= push.check_at && push.proxy.is_some() {
            let wall = self.services.clock.wall_ms();
            self.start_renewal(wall, request).await;
        }
        if self.messages.push.seal_due {
            self.push_seal(request).await;
        }
        match self.messages.push.read_clear_at {
            Some(at) if now >= at => self.push_read_clears(request).await,
            None if !self.messages.push.read_recheck.is_empty() => {
                self.push_read_recheck(now, request).await;
            }
            _ => {}
        }
        self.push_probe(now, request);
        self.push_deliver(now, request).await;
    }

    /// What the push decision of a text's first attempt needs (spec 6.1), `reached` being the
    /// devices the attempt reached; `None` without a proxy (no request of any kind then).
    pub(super) fn push_first(
        &self,
        peer: PeerId,
        reached: Vec<InstallationId>,
    ) -> Option<Box<FirstAttempt>> {
        self.messages.push.proxy.as_ref()?;
        Some(Box::new(FirstAttempt {
            devices: self.current_installations(peer),
            reached,
            batch: None,
        }))
    }

    /// `push_first` for a text no device was reached for, sealed first: the push carries its
    /// ciphertext. `None` while it cannot be sealed (no group yet); a later attempt decides.
    pub(super) async fn push_first_unreached(
        &mut self,
        key: MessageKey,
        peer: PeerId,
        request: Request,
    ) -> Option<Box<FirstAttempt>> {
        let first = self.push_first(peer, Vec::new())?;
        let seal = MessageDbOp::Seal(key, self.services.clock.wall_ms());
        match self.message_db(seal, request).await {
            Ok(MessageDbReply::Sealed { .. }) => Some(first),
            _ => None,
        }
    }

    /// Records a text attempt's outcome `op`, with its push decision in the same transaction
    /// when `first` is given, and sends the push decided on.
    pub(super) async fn outcome_db(
        &mut self,
        op: MessageDbOp,
        first: Option<Box<FirstAttempt>>,
        request: Request,
    ) -> Result<MessageDbReply, CoreError> {
        let Some(first) = first else {
            return self.message_db(op, request).await;
        };
        let reply = self
            .message_db(MessageDbOp::FirstAttempt(Box::new(op), first), request)
            .await?;
        if let MessageDbReply::Push(PushDbReply::Batch(Some(batch))) = reply {
            self.push_send(batch, request.ticket);
        }
        Ok(MessageDbReply::Done)
    }

    /// The own texts of `entries` go to a mailbox now: those whose push is not decided yet (no
    /// attempt had a result) are decided as reached by no device, and pushed (spec 6.1). Best
    /// effort: a failure leaves them to their next attempt. Nothing without a proxy.
    pub(super) async fn push_handed_off(&mut self, entries: &[HandoffEntry], request: Request) {
        if self.messages.push.proxy.is_none() || entries.is_empty() {
            return;
        }
        let Ok(local) = self.local() else {
            return;
        };
        let (sender, installation) = (local.keys.peer_id(), local.device.installation_id());
        let mut devices: BTreeMap<PeerId, Vec<InstallationId>> = BTreeMap::new();
        let texts = entries
            .iter()
            .map(|entry| {
                let key = MessageKey {
                    conversation: entry.conversation,
                    sender,
                    installation,
                    id: entry.msg_id,
                };
                let current = devices
                    .entry(entry.peer)
                    .or_insert_with(|| self.current_installations(entry.peer))
                    .clone();
                (key, current)
            })
            .collect();
        if let Ok(PushDbReply::Batches(batches)) = self
            .push_db(PushDbOp::HandedOff { texts }, request)
            .await
        {
            for batch in batches {
                self.push_send(batch, request.ticket);
            }
        }
    }

    /// Messages were read here just now (spec 7): the read clear goes `READ_CLEAR_MS` after the
    /// first read, for every read until then. Nothing without a proxy.
    pub(super) fn push_read_clear(&mut self) {
        if self.messages.push.proxy.is_none() {
            return;
        }
        let at = self.schedule_now().saturating_add(READ_CLEAR_MS);
        self.messages.push.read_clear_at.get_or_insert(at);
    }

    /// One `read` for each of my other current devices without a session now that announced an
    /// own-device handle (only Android devices do). Those with a session are kept for
    /// `push_read_recheck`.
    async fn push_read_clears(&mut self, request: Request) {
        self.messages.push.read_clear_at = None;
        let Some((current, connected)) = self.own_devices() else {
            return;
        };
        let (offline, skipped): (Vec<_>, Vec<_>) =
            current.into_iter().partition(|i| !connected.contains(i));
        // Spec 7 (amended): the devices skipped now get this read if their session goes soon;
        // the latest decision is the one that counts.
        let until = self.schedule_now().saturating_add(READ_RECHECK_MS);
        self.messages.push.read_recheck = skipped.into_iter().map(|i| (i, until)).collect();
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let sessions = u32::try_from(connected.len()).unwrap_or(u32::MAX);
        self.push_read_items(
            offline,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            sessions,
            request,
        )
        .await;
    }

    /// My other current devices, and those of them with a ready session; `None` without an
    /// account.
    fn own_devices(&self) -> Option<(Vec<InstallationId>, Vec<InstallationId>)> {
        let local = self.local().ok()?;
        let (me, own) = (local.device.installation_id(), local.keys.peer_id());
        let connected: Vec<InstallationId> = self
            .mls_sessions()
            .into_iter()
            .filter(|s| s.grant.peer == own)
            .map(|s| s.grant.installation)
            .collect();
        let current = self
            .current_installations(own)
            .into_iter()
            .filter(|i| *i != me)
            .collect();
        Some((current, connected))
    }

    /// Spec 7 (amended): a device the latest read clear skipped for its session, whose session
    /// went before `READ_RECHECK_MS` passed, gets that read clear now, once. While another read
    /// clear is pending it decides for them all (`poll_push` comes here only without one).
    async fn push_read_recheck(&mut self, now: u64, request: Request) {
        let Some((current, connected)) = self.own_devices() else {
            return;
        };
        let recheck = &mut self.messages.push.read_recheck;
        recheck.retain(|i, until| now < *until && current.contains(i));
        let gone: Vec<InstallationId> = recheck
            .keys()
            .filter(|i| !connected.contains(i))
            .copied()
            .collect();
        if gone.is_empty() {
            return;
        }
        recheck.retain(|i, _| !gone.contains(i));
        self.push_read_items(
            gone,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            0,
            request,
        )
        .await;
    }

    /// One `read` for each of `offline` that announced an own-device handle (none with push
    /// off), pushed in a spawned task.
    async fn push_read_items(
        &mut self,
        offline: Vec<InstallationId>,
        #[cfg(all(feature = "test-hooks", debug_assertions))] sessions: u32,
        request: Request,
    ) {
        let reply = self.push_db(PushDbOp::ReadClear { offline }, request).await;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            let items = match &reply {
                Ok(PushDbReply::Batch(Some(batch))) => batch.targets.len(),
                _ => 0,
            };
            let items = u32::try_from(items).unwrap_or(u32::MAX);
            self.messages.push.read_clear_log.push((sessions, items));
        }
        if let Ok(PushDbReply::Batch(Some(batch))) = reply {
            self.push_send(batch, request.ticket);
        }
    }

    /// Sends one `/push` in a spawned task, bounded by `PUSH_TIMEOUT`; nothing waits for it. A
    /// failure or `limited` is dropped, never sent again (spec 6.2); `gone` results come back.
    fn push_send(&self, batch: PushBatch, ticket: u64) {
        let Some(proxy) = self.messages.push.proxy.clone() else {
            return;
        };
        let mailbox = self.auth.send.clone();
        tokio::spawn(async move {
            let PushBatch {
                conversation,
                targets,
                request,
            } = batch;
            let Ok(Ok(response)) = tokio::time::timeout(PUSH_TIMEOUT, proxy.push(request)).await
            else {
                return;
            };
            // An answer that does not match the items says nothing about any of them.
            if response.results.len() != targets.len() {
                return;
            }
            let gone: Vec<_> = targets
                .into_iter()
                .zip(response.results)
                .filter(|(_, result)| *result == PushResult::Gone)
                .map(|(target, _)| target)
                .collect();
            if !gone.is_empty() {
                let _ = mailbox
                    .send(Input::Push(PushEvent::Gone {
                        ticket,
                        conversation,
                        gone,
                    }))
                    .await;
            }
        });
    }

    /// Takes a newer `push_configure`: the proxy for its address, and its platform into
    /// `push_local` (a change drops the token and every handle). The handles are checked next.
    async fn push_take_config(&mut self, request: Request) -> Result<(), CoreError> {
        let Some((version, config)) = self.shared.push.config.newer(self.messages.push.config_seen)
        else {
            return Ok(());
        };
        let PushDbReply::Count(changed) = self
            .push_db(PushDbOp::Configure(config.platform), request)
            .await?
        else {
            return Err(unavailable(ErrorCode::InvalidMessage));
        };
        // The notification extension's state files (spec 5.6): an APNs device with a folder.
        let push_dir = match (config.platform, &config.snapshot_dir) {
            (Some(Platform::Apns), Some(dir)) => Some(dir.join(crate::push::ios_state::PUSH_DIR)),
            _ => None,
        };
        self.ensure_ready(request)?;
        self.store.push_dir(push_dir).await?;
        let proxy = config
            .proxy_url
            .as_deref()
            .and_then(|url| self.services.push.clone().or_else(|| http_proxy(url)));
        let push = &mut self.messages.push;
        push.config_seen = version;
        push.proxy = proxy;
        push.check_at = 0;
        if changed > 0 {
            push.generation.fetch_add(1, Ordering::SeqCst);
            self.push_reload(request).await?;
        }
        Ok(())
    }

    pub(super) async fn push_set_token(
        &mut self,
        platform: Platform,
        token: String,
    ) -> Result<(), CoreError> {
        let request = self.ready()?;
        // A `push_configure` made just before counts (the poll may not have taken it yet).
        self.push_take_config(request).await?;
        let PushDbReply::Count(changed) = self
            .push_db(PushDbOp::SetToken { platform, token }, request)
            .await?
        else {
            return Err(unavailable(ErrorCode::InvalidMessage));
        };
        if changed > 0 {
            // A renewal under the old token stores nothing and asks for nothing more.
            self.messages.push.generation.fetch_add(1, Ordering::SeqCst);
            self.push_reload(request).await?;
        }
        let wall = self.services.clock.wall_ms();
        self.start_renewal(wall, request).await;
        Ok(())
    }

    pub(super) async fn push_set_enabled(&mut self, enabled: bool) -> Result<(), CoreError> {
        let request = self.ready()?;
        self.push_take_config(request).await?;
        let now = self.services.clock.wall_ms();
        self.push_db(PushDbOp::SetEnabled { enabled, now }, request)
            .await?;
        if enabled {
            self.messages.push.check_at = 0;
        } else {
            // A `/handle` reply still on its way is dropped, and no request follows it.
            self.messages.push.generation.fetch_add(1, Ordering::SeqCst);
            self.push_reload(request).await?;
            self.push_seal(request).await;
        }
        Ok(())
    }

    pub(super) async fn push_status(&mut self) -> Result<PushStatus, CoreError> {
        let request = self.ready()?;
        match self.push_db(PushDbOp::Status, request).await? {
            PushDbReply::Status(status) => Ok(status),
            _ => Err(unavailable(ErrorCode::InvalidMessage)),
        }
    }

    pub(super) async fn push_renew_due(
        &mut self,
        now_ms: u64,
        reply: oneshot::Sender<Result<RenewSummary, CoreError>>,
    ) {
        // A `push_configure` made just before counts: the background renewal of a cold start
        // (spec 3) configures and asks at once, and must not get an empty answer.
        let request = match self.ready() {
            Ok(request) => match self.push_take_config(request).await {
                Ok(()) => request,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            },
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        self.messages.push.waiters.push(reply);
        self.start_renewal(now_ms, request).await;
    }

    fn finish_renewal(&mut self, result: Result<RenewSummary, CoreError>) {
        for waiter in std::mem::take(&mut self.messages.push.waiters) {
            let _ = waiter.send(result.clone());
        }
    }

    /// Issues what is due at `wall` (wall time), or, while a renewal runs, once it ended.
    async fn start_renewal(&mut self, wall: u64, request: Request) {
        let push = &mut self.messages.push;
        if push.renewing {
            // The running renewal answers a request for the same time or an earlier one; a
            // stale one (token change, push off) is done again when it ends (`push_issued`).
            if wall > push.running_wall {
                push.again = Some(push.again.map_or(wall, |at| at.max(wall)));
            }
            return;
        }
        let next = self.schedule_now().saturating_add(RENEW_CHECK_MS);
        self.messages.push.check_at = next;
        let Some(proxy) = self.messages.push.proxy.clone() else {
            let (announced, withdrawn) = self.push_seal(request).await;
            self.finish_renewal(Ok(RenewSummary {
                issued: 0,
                announced,
                withdrawn,
            }));
            return;
        };
        let own = self.has_other_own_device();
        let plan = match self.push_db(PushDbOp::Renew { now: wall, own }, request).await {
            Ok(PushDbReply::Plan(plan)) => plan,
            Ok(_) => None,
            Err(error) => {
                self.finish_renewal(Err(error));
                return;
            }
        };
        let Some(plan) = plan.filter(|p| !p.due.is_empty() || p.own.is_some()) else {
            let (announced, withdrawn) = self.push_seal(request).await;
            self.finish_renewal(Ok(RenewSummary {
                issued: 0,
                announced,
                withdrawn,
            }));
            return;
        };
        let push = &mut self.messages.push;
        push.renewing = true;
        push.running_wall = wall;
        let current = push.generation.clone();
        let generation = current.load(Ordering::SeqCst);
        let mailbox = self.auth.send.clone();
        let ticket = request.ticket;
        tokio::spawn(async move {
            let RenewPlan {
                platform,
                token,
                due,
                own,
            } = plan;
            let live = || current.load(Ordering::SeqCst) == generation;
            let mut handles = Vec::new();
            let mut failed = false;
            for (conversation, tag) in due {
                if !live() {
                    failed = true;
                    break;
                }
                match issue(&*proxy, platform, &token, tag).await {
                    Ok(handle) => handles.push((conversation, tag, handle)),
                    // The rest stays due for the next check.
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            let mut own_handle = None;
            if let Some(tag) = own
                && !failed
                && live()
                && let Ok(handle) = issue(&*proxy, platform, &token, tag).await
            {
                own_handle = Some((tag, handle));
            }
            let issued = Box::new(Issued {
                platform,
                token,
                now: wall,
                handles,
                own: own_handle,
            });
            let _ = mailbox
                .send(Input::Push(PushEvent::Issued {
                    ticket,
                    generation,
                    issued,
                }))
                .await;
        });
    }

    /// Whether this account has a current device besides this one.
    fn has_other_own_device(&self) -> bool {
        let Ok(local) = self.local() else {
            return false;
        };
        let me = local.device.installation_id();
        self.current_installations(local.keys.peer_id())
            .into_iter()
            .any(|i| i != me)
    }

    pub(super) async fn push_event(&mut self, event: PushEvent) {
        match event {
            PushEvent::Issued {
                ticket,
                generation,
                issued,
            } => self.push_issued(ticket, generation, issued).await,
            PushEvent::Delivered {
                ticket,
                conversation,
                recipient,
                kind,
                msg_id,
                outcome,
            } => {
                let push = &mut self.messages.push;
                push.inflight.remove(&(conversation, recipient));
                push.delivery_at = None;
                let Some(request) = self.ready().ok().filter(|r| r.ticket == ticket) else {
                    return;
                };
                let op = PushDbOp::Delivered {
                    conversation,
                    recipient,
                    kind,
                    msg_id,
                    outcome,
                    now: self.schedule_now(),
                };
                let _ = self.push_db(op, request).await;
            }
            PushEvent::Gone {
                ticket,
                conversation,
                gone,
            } => {
                let Some(request) = self.ready().ok().filter(|r| r.ticket == ticket) else {
                    return;
                };
                let _ = self
                    .push_db(PushDbOp::Gone { conversation, gone }, request)
                    .await;
            }
            PushEvent::Network { ticket, online } => {
                self.messages.push.probing = false;
                let Some(request) = self.ready().ok().filter(|r| r.ticket == ticket) else {
                    return;
                };
                if online && self.messages.push.full_sync {
                    self.messages.push.full_sync = false;
                    self.push_full_sync(request).await;
                }
            }
        }
    }

    async fn push_issued(&mut self, ticket: u64, generation: u64, issued: Box<Issued>) {
        self.messages.push.renewing = false;
        let Some(request) = self.ready().ok().filter(|r| r.ticket == ticket) else {
            self.finish_renewal(Err(unavailable(ErrorCode::StorageLocked)));
            return;
        };
        let mut summary = RenewSummary::default();
        let current = self.messages.push.generation.load(Ordering::SeqCst) == generation;
        if !current {
            // Asked under an old token, or push went off: done again under what holds now
            // (with push off that asks the proxy nothing).
            let push = &mut self.messages.push;
            push.again = Some(push.again.map_or(issued.now, |at| at.max(issued.now)));
        }
        if current && (!issued.handles.is_empty() || issued.own.is_some()) {
            match self.push_db(PushDbOp::StoreIssued(issued), request).await {
                Ok(PushDbReply::Count(stored)) => {
                    summary.issued = u32::try_from(stored).unwrap_or(u32::MAX);
                }
                Ok(_) => {}
                Err(error) => {
                    self.finish_renewal(Err(error));
                    return;
                }
            }
            if self.push_reload(request).await.is_err() {
                return;
            }
        }
        let (announced, withdrawn) = self.push_seal(request).await;
        summary.announced = announced;
        summary.withdrawn = withdrawn;
        if let Some(wall) = self.messages.push.again.take() {
            // The waiters get the result of the renewal that follows.
            self.start_renewal(wall, request).await;
            return;
        }
        self.finish_renewal(Ok(summary));
    }

    /// Seals every conversation whose handle or withdrawal is not announced yet, for each
    /// current device of its peer. Returns `(announced, withdrawn)`.
    async fn push_seal(&mut self, request: Request) -> (u32, u32) {
        let Ok(local) = self.local() else {
            return (0, 0);
        };
        let peers: Vec<PeerId> = local.remote.iter().map(|r| r.peer).collect();
        let devices = peers
            .into_iter()
            .map(|peer| (peer, self.current_installations(peer)))
            .collect();
        let op = PushDbOp::Seal {
            devices,
            wall: self.services.clock.wall_ms(),
            now: self.schedule_now(),
        };
        match self.push_db(op, request).await {
            Ok(PushDbReply::Sealed {
                announced,
                withdrawn,
            }) => {
                let push = &mut self.messages.push;
                push.seal_due = false;
                if announced + withdrawn > 0 {
                    push.delivery_at = None;
                }
                (announced, withdrawn)
            }
            _ => {
                self.messages.push.seal_due = true;
                (0, 0)
            }
        }
    }

    /// Hash of this account's device list (without revoked devices) and the peers' records.
    fn push_fingerprint(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        if let Ok(local) = self.local() {
            if let Some(accepted) = &local.records.accepted {
                hash.update(accepted.hash);
            }
            for tombstone in &local.records.tombstones {
                hash.update(tombstone.target_installation.0);
            }
            for remote in &local.remote {
                hash.update(remote.peer.0);
                hash.update(remote.accepted.hash);
            }
        }
        hash.finalize().into()
    }

    /// When a device list changed: drops what revoked or unlisted devices announced and what
    /// is queued for them (spec 5.2), and looks at the handles and unsealed states again.
    async fn push_prune(&mut self, request: Request) -> Result<(), CoreError> {
        let fingerprint = self.push_fingerprint();
        if self.messages.push.fingerprint == Some(fingerprint) {
            return Ok(());
        }
        let local = self.local()?;
        let me = local.device.installation_id();
        let own_peer = local.keys.peer_id();
        let peers: Vec<PeerId> = local.remote.iter().map(|r| r.peer).collect();
        let own = self
            .current_installations(own_peer)
            .into_iter()
            .filter(|i| *i != me)
            .collect();
        let peers = peers
            .into_iter()
            .map(|peer| (peer, self.current_installations(peer)))
            .collect();
        let PushDbReply::Pruned { unsealed } = self
            .push_db(PushDbOp::Prune { own, peers }, request)
            .await?
        else {
            return Err(unavailable(ErrorCode::InvalidMessage));
        };
        let push = &mut self.messages.push;
        push.fingerprint = Some(fingerprint);
        push.seal_due |= unsealed;
        push.check_at = 0;
        Ok(())
    }

    /// Sends due frames over ready sessions with other accounts' devices (one per recipient of
    /// a conversation at a time), and dials one recipient without a session per check.
    async fn push_deliver(&mut self, now: u64, request: Request) {
        let Ok(own) = self.local().map(|local| local.keys.peer_id()) else {
            return;
        };
        let sessions: Vec<Session> = self
            .mls_sessions()
            .into_iter()
            .filter(|s| s.grant.peer != own)
            .collect();
        let ids: Vec<InstallationId> = sessions.iter().map(|s| s.grant.installation).collect();
        let push = &mut self.messages.push;
        if push.sessions != ids {
            push.sessions = ids.clone();
            push.delivery_at = None;
        }
        if push.delivery_at.is_none_or(|at| now >= at) {
            let op = PushDbOp::DueDeliveries {
                now,
                recipients: ids.clone(),
            };
            let Ok(PushDbReply::Due { rows, next, left }) = self.push_db(op, request).await else {
                return;
            };
            let push = &mut self.messages.push;
            push.delivery_at = Some(next.unwrap_or(u64::MAX));
            push.left = left;
            for row in rows {
                if push.inflight.contains(&(row.conversation, row.recipient)) {
                    continue;
                }
                let Some(session) = sessions
                    .iter()
                    .find(|s| s.grant.installation == row.recipient)
                else {
                    continue;
                };
                push.inflight.insert((row.conversation, row.recipient));
                push_launch(session.clone(), row, self.auth.send.clone(), request.ticket);
            }
        }
        self.push_dial(now, &ids, request).await;
    }

    async fn push_dial(&mut self, now: u64, reachable: &[InstallationId], request: Request) {
        let push = &mut self.messages.push;
        if !push.left || now < push.dial_check_at {
            return;
        }
        push.dial_check_at = now.saturating_add(DIAL_CHECK_MS);
        push.dials
            .retain(|_, at| now < at.saturating_add(DIAL_RETRY_MS));
        let Ok(PushDbReply::Unreachable(rows)) = self
            .push_db(PushDbOp::UnreachableDue { now }, request)
            .await
        else {
            return;
        };
        for (_, recipient, peer) in rows {
            if reachable.contains(&recipient) || self.messages.push.dials.contains_key(&recipient)
            {
                continue;
            }
            self.messages.push.dials.insert(recipient, now);
            if self.start_dial(peer, recipient, request) {
                return;
            }
        }
    }
}

/// Receiving pushes (spec 5.3, 5.4, 5.6, 7).
impl Manager {
    pub(super) async fn push_command(&mut self, command: PushCommand) {
        match command {
            PushCommand::Cipher {
                tag,
                ciphertext,
                reply,
            } => {
                let result = self.push_cipher(tag, ciphertext).await;
                let _ = reply.send(result);
            }
            PushCommand::Wake { tag, reply } => {
                let result = self.push_wake(tag).await;
                let _ = reply.send(result);
            }
            PushCommand::ReadClear { tag, reply } => {
                let result = self.push_read_clear_received(tag).await;
                let _ = reply.send(result);
            }
            PushCommand::Fetch { peer, reply } => {
                let result = self.push_fetch_now(peer).await;
                let _ = reply.send(result);
            }
            PushCommand::Ingest { entries, reply } => {
                let result = self.push_ingest(entries).await;
                let _ = reply.send(result);
            }
            PushCommand::MessagesDeleted { reply } => {
                let result = self.ready().map(|_| {
                    self.messages.push.full_sync = true;
                });
                let _ = reply.send(result);
            }
            PushCommand::ConversationOf { tag, reply } => {
                let result = match self.ready() {
                    Ok(request) => self
                        .push_tagged(tag, request)
                        .await
                        .map(|tagged| tagged.map(|t| t.conversation)),
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
            }
        }
    }

    /// The conversation of a received push's tag: one of this device's current handles, in a
    /// conversation with a contact.
    async fn push_tagged(&mut self, tag: Tag, request: Request) -> Result<Option<Tagged>, CoreError> {
        let PushDbReply::Tagged(tagged) = self.push_db(PushDbOp::Tagged(tag), request).await?
        else {
            return Err(unavailable(ErrorCode::InvalidMessage));
        };
        Ok(tagged.filter(|t| self.conversation_peer(t.conversation, t.peer).is_some()))
    }

    /// Opens a pushed ciphertext of `tagged`'s conversation like a received frame (`Via::Push`),
    /// its group id given back first so a copy that came directly is the same ciphertext.
    async fn push_open(
        &mut self,
        tagged: &Tagged,
        ciphertext: Vec<u8>,
        request: Request,
    ) -> Result<PushOpened, CoreError> {
        if ciphertext.len() > MAX_MLS_MESSAGE_BYTES {
            return Ok(PushOpened::Failed);
        }
        let ciphertext = match restore_group_id(&ciphertext, &tagged.group_id) {
            Ok(restored) => restored,
            // Sent with its group id: opened as it is.
            Err(PushCtError::NotStripped) => ciphertext,
            Err(PushCtError::NotPrivate | PushCtError::Malformed) => {
                return Ok(PushOpened::Failed);
            }
        };
        let opened = self
            .open_received(
                tagged.peer,
                None,
                tagged.conversation,
                ciphertext,
                &Via::Push,
                request,
            )
            .await;
        match opened {
            Ok(Received {
                text: Some((key, new)),
                ..
            }) => Ok(PushOpened::Text(key, new)),
            Ok(Received {
                reply: Some(MlsReply::Ack { .. }),
                ..
            }) => Ok(PushOpened::Other),
            Ok(_) => Ok(PushOpened::Failed),
            Err(error)
                if matches!(
                    error.code,
                    ErrorCode::AccountRestricted | ErrorCode::StorageLocked
                ) =>
            {
                Err(error)
            }
            Err(_) => Ok(PushOpened::Failed),
        }
    }

    /// A `cipher` push: a text stored just now alerts, one stored already and unread updates the
    /// notification silently, one read already (or anything not a text) shows nothing, and
    /// what cannot be opened alerts as "새 메시지".
    async fn push_cipher(&mut self, tag: Tag, ciphertext: Vec<u8>) -> Result<PushOutcome, CoreError> {
        let request = self.ready()?;
        let Some(tagged) = self.push_tagged(tag, request).await? else {
            return Ok(PushOutcome::UnknownTag);
        };
        let shown = match self.push_open(&tagged, ciphertext, request).await {
            Ok(PushOpened::Text(key, new)) => {
                self.push_text_shown(tagged.conversation, key, new, request)
                    .await
            }
            Ok(PushOpened::Other) => Some(PushOutcome::Nothing),
            Ok(PushOpened::Failed) | Err(_) => None,
        };
        if let Some(outcome) = shown {
            return Ok(outcome);
        }
        let (_, alerted) = self
            .push_generic(tagged.conversation, tagged.peer, request)
            .await;
        Ok(if alerted {
            PushOutcome::Notified
        } else {
            PushOutcome::Updated
        })
    }

    /// Shows a pushed text (spec 5.3): `None` when its notification could not be made.
    async fn push_text_shown(
        &mut self,
        conversation: ConversationId,
        key: MessageKey,
        new: bool,
        request: Request,
    ) -> Option<PushOutcome> {
        let seen = match self.push_db(PushDbOp::Seen(key), request).await {
            Ok(PushDbReply::Seen(seen)) => seen,
            _ => None,
        };
        match (seen, new) {
            // Read on another device already: no notification (M6 user decision, spec 5.3).
            // Stored just now, it still ends a generic notification of the conversation, which
            // is then cleared or updated (Ruling R13).
            (Some(true), new) => {
                if new {
                    self.push_notify(vec![key], Vec::new(), request).await;
                }
                Some(PushOutcome::Nothing)
            }
            // Silent when a generic notification of the conversation alerted already (R13).
            (Some(false), true) => self
                .push_notify(vec![key], Vec::new(), request)
                .await
                .map(|alerted| {
                    if alerted {
                        PushOutcome::Notified
                    } else {
                        PushOutcome::Updated
                    }
                }),
            // It alerted when it came first: never a second time.
            (_, false) => Some(
                match self.push_notify(Vec::new(), vec![conversation], request).await {
                    Some(_) => PushOutcome::Updated,
                    None => PushOutcome::Nothing,
                },
            ),
            (None, true) => None,
        }
    }

    /// Notifies the texts `keys` and the conversations `read`: whether that alerted, `None`
    /// when nothing was shown.
    async fn push_notify(
        &mut self,
        keys: Vec<MessageKey>,
        read: Vec<ConversationId>,
        request: Request,
    ) -> Option<bool> {
        match self
            .message_db(MessageDbOp::NotifyItems(keys, read), request)
            .await
        {
            Ok(MessageDbReply::Notify(items)) if !items.is_empty() => {
                let alerted = items.iter().any(|item| item.new_messages > 0);
                self.publish_notify(items);
                Some(alerted)
            }
            _ => None,
        }
    }

    /// The "새 메시지" notification of a push whose text cannot be shown: one new message more
    /// than the conversation's unread ones, without a preview. It alerts unless one alerted for
    /// the conversation already and has not ended since (then it is `silenced`: shown, also
    /// where the user dismissed the first); the conversation's next text takes its place
    /// without alerting (Ruling R13, marked in `push_tag`). Returns the unread count it shows
    /// and whether it alerted.
    async fn push_generic(
        &mut self,
        conversation: ConversationId,
        peer: PeerId,
        request: Request,
    ) -> (u64, bool) {
        let (unread, pending) = match self
            .push_db(PushDbOp::Generic(conversation), request)
            .await
        {
            Ok(PushDbReply::Generic { unread, pending }) => (unread, pending),
            _ => (0, false),
        };
        let unread = unread.saturating_add(1);
        self.publish_notify(vec![crate::event::NotifyItem {
            conversation,
            peer,
            new_messages: u32::from(!pending),
            unread,
            title: String::new(),
            preview: None,
            silenced: pending,
        }]);
        (unread, !pending)
    }

    /// A `wake` push: its conversation's generic notification now; the fetch that follows
    /// (`push_fetch`) is told once a notification of the conversation takes its place.
    async fn push_wake(
        &mut self,
        tag: Tag,
    ) -> Result<Option<Waking>, CoreError> {
        let request = self.ready()?;
        let Some(Tagged {
            conversation, peer, ..
        }) = self.push_tagged(tag, request).await?
        else {
            return Ok(None);
        };
        let (unread, _) = self.push_generic(conversation, peer, request).await;
        let (tell, told) = oneshot::channel();
        self.messages.push.waking.insert(conversation, (tell, unread));
        Ok(Some((peer, told)))
    }

    /// Dials `peer`'s devices without a session and syncs with my devices; the dials run in
    /// spawned tasks.
    async fn push_fetch_now(&mut self, peer: Option<PeerId>) -> Result<(), CoreError> {
        let request = self.ready()?;
        if let Some(peer) = peer {
            self.push_dial_devices(peer, request);
        }
        self.mailbox_sync_now().await
    }

    fn push_dial_devices(&mut self, peer: PeerId, request: Request) {
        for installation in self.current_installations(peer) {
            if !self.ready_session(peer, installation) {
                self.start_dial(peer, installation, request);
            }
        }
    }

    /// The network is up after `push_messages_deleted`: syncs with my devices and dials every
    /// contact's devices (spec 5.4, design 8.3).
    async fn push_full_sync(&mut self, request: Request) {
        let Ok(local) = self.local() else {
            return;
        };
        let peers: Vec<PeerId> = local.contacts.iter().map(|c| c.peer).collect();
        for peer in peers {
            self.push_dial_devices(peer, request);
        }
        let _ = self.mailbox_sync_now().await;
    }

    /// While `push_messages_deleted` waits, looks whether the network is up, in a spawned task.
    fn push_probe(&mut self, now: u64, request: Request) {
        let push = &mut self.messages.push;
        if !push.full_sync || push.probing || now < push.probe_at {
            return;
        }
        push.probing = true;
        push.probe_at = now.saturating_add(FULL_SYNC_LOOK_MS);
        let network = self.shared.network.clone();
        let mailbox = self.auth.send.clone();
        let ticket = request.ticket;
        tokio::spawn(async move {
            let online = matches!(
                network.execute(NetworkOp::Info).await,
                Ok(NetworkReply::Info(info)) if info.state == NetworkState::Online
            );
            let _ = mailbox
                .send(Input::Push(PushEvent::Network { ticket, online }))
                .await;
        });
    }

    /// A read clear push: what tells a sync with one of my devices ended, if `tag` is this
    /// device's current own-device tag (`None` otherwise: nothing happens, as for a withdrawn
    /// conversation tag).
    async fn push_read_clear_received(
        &mut self,
        tag: Tag,
    ) -> Result<Option<oneshot::Receiver<()>>, CoreError> {
        let request = self.ready()?;
        let PushDbReply::Own(true) = self.push_db(PushDbOp::OwnTag(tag), request).await? else {
            return Ok(None);
        };
        let (tell, told) = oneshot::channel();
        let syncs = &mut self.messages.push.syncs;
        syncs.retain(|sync| !sync.is_closed());
        syncs.push(tell);
        Ok(Some(told))
    }

    /// The iOS extension's ciphertexts, taken without a notification; how many were stored.
    async fn push_ingest(&mut self, entries: Vec<(Tag, Vec<u8>)>) -> Result<u32, CoreError> {
        let request = self.ready()?;
        let mut stored = 0u32;
        for (tag, ciphertext) in entries {
            let Some(tagged) = self.push_tagged(tag, request).await? else {
                continue;
            };
            if let PushOpened::Text(_, true) = self.push_open(&tagged, ciphertext, request).await? {
                stored = stored.saturating_add(1);
            }
        }
        Ok(stored)
    }

    /// Fills in each item's title (the contact's alias), and tells a wake push's fetch once a
    /// notification of its conversation shows a text in the generic one's place (or clears it).
    /// That notification does not alert again: `notify_items` keeps it silent (Ruling R13).
    pub(super) fn push_notify_items(&mut self, items: &mut [crate::event::NotifyItem]) {
        if let Ok(local) = self.local() {
            for item in items.iter_mut() {
                item.title = local
                    .contacts
                    .iter()
                    .find(|c| c.peer == item.peer)
                    .map(|c| c.alias.clone())
                    .unwrap_or_default();
            }
        }
        let waking = &mut self.messages.push.waking;
        waking.retain(|_, (fetched, _)| !fetched.is_closed());
        for item in items.iter() {
            // An item of older messages read elsewhere (fewer unread) says nothing of the fetch.
            let shown = |unread: u64| {
                item.unread == 0 || (item.preview.is_some() && item.unread >= unread)
            };
            if waking
                .get(&item.conversation)
                .is_some_and(|(_, unread)| shown(*unread))
                && let Some((fetched, _)) = waking.remove(&item.conversation)
            {
                let _ = fetched.send(());
            }
        }
    }

    /// A sync with one of my devices ended: the read clear pushes waiting are told.
    pub(super) fn push_pull_done(&mut self) {
        for sync in std::mem::take(&mut self.messages.push.syncs) {
            let _ = sync.send(());
        }
    }
}

/// One received push's use of the network: it starts the network if it is off and nobody else
/// uses it, and the last use to end stops what a push started, unless another start was asked
/// for meanwhile (the network is the app's then). A use dropped before it ended (its task
/// cancelled) ends in a task of its own.
struct NetworkUse {
    shared: Option<Arc<super::Shared>>,
}

impl NetworkUse {
    async fn acquire(shared: Arc<super::Shared>) -> Self {
        {
            let mut lease = shared.push.network.lock().await;
            if lease.users == 0 {
                let off = matches!(
                    shared.network.execute(NetworkOp::Info).await,
                    Ok(NetworkReply::Info(info))
                        if matches!(info.state, NetworkState::Stopped | NetworkState::Failed)
                );
                lease.started = false;
                if off {
                    let before = shared.network.starts();
                    let started = shared.network.execute(NetworkOp::Start).await.is_ok();
                    lease.starts = shared.network.starts();
                    // Another start at the same moment: the network is not only the push's.
                    lease.started = started && lease.starts == before + 1;
                }
            }
            lease.users = lease.users.saturating_add(1);
        }
        Self {
            shared: Some(shared),
        }
    }

    /// Whether the network is up by `until` (looked at every `NETWORK_LOOK`).
    async fn online(&self, until: tokio::time::Instant) -> bool {
        let Some(shared) = &self.shared else {
            return false;
        };
        loop {
            match shared.network.execute(NetworkOp::Info).await {
                Ok(NetworkReply::Info(info)) => match info.state {
                    NetworkState::Online => return true,
                    NetworkState::Starting => {}
                    NetworkState::Stopped | NetworkState::Failed => return false,
                },
                _ => return false,
            }
            if tokio::time::Instant::now() + NETWORK_LOOK > until {
                return false;
            }
            tokio::time::sleep(NETWORK_LOOK).await;
        }
    }

    async fn release(mut self) {
        if let Some(shared) = self.shared.take() {
            end_network_use(&shared).await;
        }
    }
}

impl Drop for NetworkUse {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move { end_network_use(&shared).await });
        }
    }
}

async fn end_network_use(shared: &super::Shared) {
    let mut lease = shared.push.network.lock().await;
    lease.users = lease.users.saturating_sub(1);
    if lease.users == 0 && lease.started {
        lease.started = false;
        if shared.network.starts() == lease.starts {
            let _ = shared.network.execute(NetworkOp::Stop).await;
        }
    }
}

/// Sends one queued frame (a kind 17 `App`) and reports how it ended.
fn push_launch(
    session: Session,
    row: PushDue,
    mailbox: tokio::sync::mpsc::Sender<Input>,
    ticket: u64,
) {
    tokio::spawn(async move {
        let outcome = super::mls::delivery_outcome(&super::mls::exchange(&session, &row.wire).await);
        let _ = mailbox
            .send(Input::Push(PushEvent::Delivered {
                ticket,
                conversation: row.conversation,
                recipient: row.recipient,
                kind: row.kind,
                msg_id: row.msg_id,
                outcome,
            }))
            .await;
    });
}
