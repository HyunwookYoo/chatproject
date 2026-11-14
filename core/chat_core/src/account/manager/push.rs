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
//!   queue too; its `Delivered` note empties the rows of that frame.
//! * Push off: every conversation that had a handle gets a withdrawal (an empty handle), the
//!   token and handles are dropped, and no proxy request is made until push is on again.
//! * What revoked or unlisted devices announced, and what is queued for them, goes when the
//!   records change.
use super::*;
use crate::account::connection::{Input, Session};
use crate::message::{
    mls_group::Delivery,
    push_store::{Issued, PushDbOp, PushDbReply, PushDue, RenewPlan},
    store::{MessageDbOp, MessageDbReply},
    types::{ConversationId, MessageId},
};
use crate::push::{
    HandleRequest, Platform, PushConfig, PushError, PushProxy, PushStatus, RenewSummary, Tag,
};
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
    /// The iOS snapshot directory, for the notification extension's state file (Task 11).
    #[allow(dead_code)]
    snapshot_dir: Option<std::path::PathBuf>,
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
        msg_id: MessageId,
        outcome: Delivery,
    },
}

impl AccountHandle {
    /// Tells the core the push proxy address, this device's platform and the iOS snapshot
    /// directory (at every start; spec 5.1). Release builds take an `https://` address only.
    /// The actor takes it at its next poll and checks the handles right after.
    pub fn push_configure(&self, config: PushConfig) -> Result<(), CoreError> {
        if let Some(url) = &config.proxy_url {
            crate::push::check_proxy_url(url)?;
        }
        self.shared.push.set(config);
        Ok(())
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
    /// Issues the handles due at `now_ms` (wall time) and seals them for the peers; returns
    /// once that renewal ended (the background renewal of Android and iOS).
    pub async fn push_renew_due(&self, now_ms: u64) -> Result<RenewSummary, CoreError> {
        self.send(|reply| Command::PushRenewDue(now_ms, reply))
            .await
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

    /// Reads `push_local` again; my other devices are told when the own-device handle changed.
    async fn push_reload(&mut self, request: Request) -> Result<(), CoreError> {
        let PushDbReply::Local(local) = self.push_db(PushDbOp::Local, request).await? else {
            return Err(unavailable(ErrorCode::InvalidMessage));
        };
        let push = &mut self.messages.push;
        let changed = push.loaded && push.own_handle != local.own_handle;
        push.own_handle = local.own_handle;
        push.loaded = true;
        if changed {
            self.announce_again();
        }
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
        self.push_deliver(now, request).await;
    }

    /// Takes a newer `push_configure`: the proxy for its address, and its platform into
    /// `push_local` (a change drops the token and every handle). The handles are checked next.
    async fn push_take_config(&mut self, request: Request) -> Result<(), CoreError> {
        let Some((version, config)) = self.shared.push.newer(self.messages.push.config_seen)
        else {
            return Ok(());
        };
        let PushDbReply::Count(changed) = self
            .push_db(PushDbOp::Configure(config.platform), request)
            .await?
        else {
            return Err(unavailable(ErrorCode::InvalidMessage));
        };
        let proxy = config
            .proxy_url
            .as_deref()
            .and_then(|url| self.services.push.clone().or_else(|| http_proxy(url)));
        let push = &mut self.messages.push;
        push.config_seen = version;
        push.proxy = proxy;
        push.snapshot_dir = config.snapshot_dir;
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
                    msg_id,
                    outcome,
                    now: self.schedule_now(),
                };
                let _ = self.push_db(op, request).await;
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
                msg_id: row.msg_id,
                outcome,
            }))
            .await;
    });
}
