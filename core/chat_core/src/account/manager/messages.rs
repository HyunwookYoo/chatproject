//! M4 message authorization and one bounded network attempt; scheduling remains separate.
use super::*;
use crate::account::{
    auth,
    connection::{Registry, Session},
};
use crate::message::{
    mls_group::{Delivery, DueDelivery},
    store::{MessageDbOp, MessageDbReply, MessageRepo, PrepareToken, conversation_id},
    types::{BlockReason, ConversationId, MessageKey},
    wire::{self, Frame, ReceiptFrame, RejectReason},
};
use crate::mls::{
    group::{CLASS_FUTURE_EPOCH, CLASS_NO_GROUP},
    wire::{MlsFrame, MlsReply},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use crate::message::scheduler::Dispatch;
#[derive(Clone)]
pub(crate) struct SessionLease {
    pub session: Session,
    pub ticket: u64,
}
pub(crate) enum AttemptOutcome {
    Written,
    Receipt(ReceiptFrame),
    ReceiptApplied(ReceiptFrame),
    /// The recipient stored the sealed `App` frame: a kind 17 `Ack` of exactly the ciphertext
    /// sent (the M5 counterpart of an M4 `Receipt`).
    Acked,
    /// The recipient could not open the `App` frame: failure class and its epoch.
    DecryptFailed(u8, u64),
    /// The recipient answered the kind 17 frame with the M4 `Reject(UnsupportedVersion)`: it
    /// has not updated yet. The message waits (`waiting_peer_update`), it does not fail.
    PeerUpdateRequired,
    /// The `App` frame was written and the stream or connection ended without an answer: what a
    /// released M4 app does with kind 17 (it closes the connection), or a lost answer.
    NoReply,
    Transient(ErrorCode),
    Blocked(BlockReason),
    Permanent(ErrorCode),
}
pub(crate) struct AttemptResult {
    pub key: MessageKey,
    pub lease: SessionLease,
    pub outcome: AttemptOutcome,
    /// The Commits/Welcomes sent ahead of the `App` frame, and how each ended.
    pub ahead: Vec<(DueDelivery, Delivery)>,
    /// The `(conversation, recipient)` this attempt reserved for those deliveries.
    pub reserved: Option<(ConversationId, InstallationId)>,
}
struct Pending {
    peer: PeerId,
    lease: SessionLease,
    receipt: Option<ReceiptFrame>,
    reply: Option<oneshot::Sender<Result<(), CoreError>>>,
    completed: bool,
}
#[derive(Default)]
pub(super) struct Runtime {
    pending: BTreeMap<MessageKey, Pending>,
    unlaunched: BTreeMap<MessageKey, (u64, PrepareToken, PeerId)>,
    acquiring: BTreeMap<MessageKey, Acquisition>,
    scheduler: Option<crate::message::scheduler::Scheduler>,
    recovered: bool,
    /// MLS group lifecycle state (`manager/mls.rs`).
    pub(super) mls: super::mls::Runtime,
    /// Own-device sync state (`manager/mailbox.rs`).
    pub(super) mailbox: super::mailbox::Runtime,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    writes: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    replies: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    prelaunch_pause: Option<TestPause>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    proof_pause: Option<TestPause>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    signature_pause: Option<TestPause>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    proof_result_pause: Option<ProofPause>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    proof_writes: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    reply_pause: Option<TestPause>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    write_pause: Option<TestPause>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    scheduler_held: bool,
    /// Messages whose re-encryption found an attempt in flight: re-encrypted once it settles
    /// (`true`: counted against the re-encryption budget).
    reencrypt_after: BTreeMap<MessageKey, bool>,
    /// Messages a recipient could not open because it moved to a newer epoch this device has
    /// not reached yet (C1): sealed again, uncounted, once the group is at that epoch.
    epoch_waits: BTreeMap<MessageKey, (PeerId, u64)>,
    /// The epoch each message was last sealed again in without counting (at most once per
    /// epoch, so a recipient cannot make this device re-encrypt without end).
    epoch_resealed: BTreeMap<MessageKey, u64>,
    /// The recipient epoch each message was last answered class 2 from behind (M2): a repeat at
    /// the same epoch means the Commit carried ahead did not help, and it is counted.
    behind: BTreeMap<MessageKey, u64>,
    /// Receipts whose last attempt to the message's sender went unanswered, and the connection
    /// it went out on: while that session lasts their attempts do not prefer it (Ruling 23).
    unanswered: BTreeMap<MessageKey, usize>,
    /// The text the `Ack` about to be written stores (keys the response checkpoint).
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) ack_key: Option<MessageKey>,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
struct TestPause {
    key: Option<MessageKey>,
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
impl TestPause {
    async fn wait(self) {
        let _ = self.entered.send(());
        let _ = self.release.await;
    }
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
struct ProofPause {
    pause: TestPause,
    queued: oneshot::Sender<()>,
    connection: oneshot::Sender<iroh::endpoint::Connection>,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub struct MessagePause {
    entered: Option<oneshot::Receiver<()>>,
    release: Option<oneshot::Sender<()>>,
    queued: Option<oneshot::Receiver<()>>,
    connection: Option<oneshot::Receiver<iroh::endpoint::Connection>>,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
impl MessagePause {
    pub(crate) async fn wait_entered_checked(&mut self) -> Result<(), CoreError> {
        self.entered.take().ok_or_else(|| unavailable(ErrorCode::NotAvailable))?
            .await.map_err(|_| unavailable(ErrorCode::NotAvailable))
    }

    pub async fn release_and_wait_queued(&mut self) {
        let _ = self.release.take().unwrap().send(());
        self.queued.take().unwrap().await.unwrap();
    }
    pub async fn wait_connection_closed(&mut self) {
        self.connection
            .take()
            .unwrap()
            .await
            .unwrap()
            .closed()
            .await;
    }
    pub async fn wait_entered(&mut self) {
        self.entered.take().unwrap().await.unwrap();
    }
    pub fn release(mut self) {
        let _ = self.release.take().unwrap().send(());
    }
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Clone, Copy)]
pub enum MessagePoint {
    Response,
    FirstByte,
    Prelaunch,
    Proof,
    Signature,
    ProofResult,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Clone, Copy, Default)]
pub enum MessageMutation {
    #[default]
    None,
    Sender,
    Conversation,
    ReceiptRecipient,
    ReceiptHash,
    UnsupportedVersion,
    Trailing,
    ConflictingText,
    ReceiptAsApplied,
    ReadReceipt,
}
impl AccountHandle {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub async fn message_test_proof_write_starts(&self) -> Result<usize, CoreError> {
        self.send(Command::MessageProofWriteStarts).await
    }

    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub async fn message_test_write_starts(&self) -> Result<(usize, usize), CoreError> {
        self.send(Command::MessageWriteStarts).await
    }

    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub async fn message_ack_pause(&self, key: MessageKey) -> Result<(MessagePause, MessagePause), CoreError> {
        self.send(|reply| Command::MessageAckPause(key, reply)).await
    }

    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub async fn message_test_pause(
        &self,
        response: MessagePoint,
    ) -> Result<MessagePause, CoreError> {
        self.send(|reply| Command::MessagePause(response, reply))
            .await
    }

    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub async fn message_test_attempt(
        &self,
        key: MessageKey,
        receipt: bool,
        mutation: MessageMutation,
    ) -> Result<(), CoreError> {
        self.send(|reply| Command::MessageAttempt(key, receipt, mutation, reply))
            .await
    }

    /// Stops automatic delivery from starting attempts, so a test that drives
    /// message_test_attempt by hand does not race the 100 ms scheduler poll.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub async fn message_test_hold_scheduler(&self, hold: bool) -> Result<(), CoreError> {
        self.send(|reply| Command::MessageHoldScheduler(hold, reply))
            .await
    }
}
struct Authority {
    shared: Arc<Shared>,
    registry: Registry,
    clock: Arc<dyn crate::account::clock::Clock>,
    own_expires: u64,
}
impl Authority {
    fn permits(&self, lease: &SessionLease) -> bool {
        let request = *self.shared.requests.borrow();
        request.ticket == lease.ticket
            && !request.suspend
            && !self.shared.network.desired_suspend()
            && self.shared.status.load(Ordering::Acquire) == (lease.ticket << 2) | 1
            && self.clock.unix_seconds() < self.own_expires
            && self.clock.unix_seconds() < lease.session.grant.expires_at
            && lease.session.conn.close_reason().is_none()
            && self.registry.0.lock().unwrap().iter().any(|s| {
                s.conn.stable_id() == lease.session.conn.stable_id()
                    && s.ready
                    && s.grant.record_hash == lease.session.grant.record_hash
            })
    }
}
pub(crate) struct ResponseGate {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    writes: Arc<std::sync::atomic::AtomicUsize>,
    authority: Authority,
    lease: SessionLease,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pause: Option<TestPause>,
}
impl ResponseGate {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(crate) fn write_started(&self) {
        self.writes.fetch_add(1, Ordering::SeqCst);
    }

    pub(crate) fn permits(&self) -> bool {
        self.authority.permits(&self.lease)
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(crate) async fn checkpoint(&mut self) {
        if let Some(pause) = self.pause.take() {
            pause.wait().await;
        }
    }
}
struct NetworkAttempt {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    writes: Arc<std::sync::atomic::AtomicUsize>,
    authority: Authority,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pause: Option<TestPause>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    mutation: MessageMutation,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    frames: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    reserved: Option<(ConversationId, InstallationId)>,
    /// Sessions that get a copy of an `App` frame the recipient acknowledged (spec 5.3).
    copies: Vec<Session>,
}
/// SHA-256 of the MLS ciphertext of a kind 17 `App` frame; `None` for any other frame (an M4
/// receipt, or the M4-peer fixture's text).
fn app_ciphertext_hash(wire: &[u8]) -> Option<[u8; 32]> {
    let inner = auth::payload(wire, 17).ok()?;
    match MlsFrame::decode(&inner).ok()? {
        MlsFrame::App { ciphertext, .. } => Some(Sha256::digest(&ciphertext).into()),
        _ => None,
    }
}
/// The answer to an `App` frame: kind 17, or the M4 kind 16 `Reject` (an M4 recipient, or a
/// text the recipient refuses the way M4 would).
fn app_outcome(body: &[u8], sent: [u8; 32]) -> Result<AttemptOutcome, ErrorCode> {
    if let Ok(inner) = auth::payload(body, 17) {
        return Ok(
            match MlsReply::decode(&inner).map_err(|_| ErrorCode::InvalidMessage)? {
                MlsReply::Ack { ct_hash } if ct_hash == sent => AttemptOutcome::Acked,
                MlsReply::DecryptFailed { class, epoch } => {
                    AttemptOutcome::DecryptFailed(class, epoch)
                }
                MlsReply::Busy => AttemptOutcome::Transient(ErrorCode::Busy),
                MlsReply::ContactRequired => AttemptOutcome::Blocked(BlockReason::ContactRequired),
                _ => return Err(ErrorCode::InvalidMessage),
            },
        );
    }
    let inner = auth::payload(body, 16).map_err(|_| ErrorCode::InvalidMessage)?;
    match wire::decode(&inner).map_err(|_| ErrorCode::InvalidMessage)? {
        Frame::Reject(RejectReason::UnsupportedVersion) => Ok(AttemptOutcome::PeerUpdateRequired),
        Frame::Reject(reason) => Ok(reject_outcome(reason)),
        _ => Err(ErrorCode::InvalidMessage),
    }
}
/// The session an application message goes out on: to `prefer` if it is a candidate that has
/// not refused kind 17, else the first to an installation that holds a leaf in the
/// conversation's group and has not refused, else the first that has not refused, else the
/// first.
fn choose_session(
    candidates: Vec<Session>,
    members: &[InstallationId],
    prefer: Option<InstallationId>,
    refused: impl Fn(InstallationId) -> bool,
) -> Option<Session> {
    let rank = |s: &Session| {
        let installation = s.grant.installation;
        match (
            refused(installation),
            prefer == Some(installation),
            members.contains(&installation),
        ) {
            (false, true, _) => 0,
            (false, false, true) => 1,
            (false, false, false) => 2,
            (true, _, _) => 3,
        }
    };
    candidates.into_iter().min_by_key(rank)
}
fn reject_outcome(reason: RejectReason) -> AttemptOutcome {
    match reason {
        RejectReason::ContactRequired => AttemptOutcome::Blocked(BlockReason::ContactRequired),
        RejectReason::StorageUnavailable => AttemptOutcome::Transient(ErrorCode::StorageFull),
        RejectReason::UnsupportedVersion => {
            AttemptOutcome::Permanent(ErrorCode::UnsupportedMessageVersion)
        }
        RejectReason::InvalidMessage => AttemptOutcome::Permanent(ErrorCode::InvalidMessage),
    }
}
impl NetworkAttempt {
    async fn dispatch_once(self, job: Dispatch, lease: SessionLease) -> AttemptResult {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let mut pause = self.pause;
        let mut written = false;
        let mut ahead = Vec::new();
        let sent_hash = app_ciphertext_hash(&job.wire);
        let copy = (sent_hash.is_some() && !self.copies.is_empty()).then(|| job.wire.to_vec());
        let work = async {
            if job.ticket != lease.ticket
                || job.peer != lease.session.grant.peer
                || !self.authority.permits(&lease)
            {
                return Err(ErrorCode::AccountRestricted);
            }
            // The group state the recipient needs comes first, over this same session, each
            // delivery answered before the next; the message waits for all of them.
            for delivery in job.ahead {
                if !self.authority.permits(&lease) {
                    return Err(ErrorCode::AccountRestricted);
                }
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                self.frames.lock().unwrap().push(delivery.body.clone());
                let outcome = super::mls::delivery_outcome(
                    &super::mls::exchange(&lease.session, &delivery.body).await,
                );
                ahead.push((delivery, outcome));
                if outcome == Delivery::Retry {
                    return Err(ErrorCode::Busy);
                }
            }
            let (mut send, mut recv) = lease
                .session
                .conn
                .open_bi()
                .await
                .map_err(|_| ErrorCode::TransportFailed)?;
            if !self.authority.permits(&lease) {
                return Err(ErrorCode::AccountRestricted);
            }
            let wire = job.wire;
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            let wire = mutate(wire, self.mutation);
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            if let Some(pause) = pause.take() {
                pause.wait().await;
            }
            // Receiver fact committed before launch; recheck after opening the stream, before its first byte.
            if !self.authority.permits(&lease) {
                return Err(ErrorCode::AccountRestricted);
            }
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            {
                self.writes.fetch_add(1, Ordering::SeqCst);
                self.frames.lock().unwrap().push(wire.to_vec());
            }
            auth::write_frame(&mut send, &wire)
                .await
                .map_err(|_| ErrorCode::TransportFailed)?;
            written = true;
            send.finish().map_err(|_| ErrorCode::TransportFailed)?;
            let body = auth::read_frame(&mut recv, 61440)
                .await
                .map_err(|_| ErrorCode::PeerNotConnected)?;
            if !self.authority.permits(&lease) {
                return Err(ErrorCode::AccountRestricted);
            }
            if let Some(sent) = sent_hash {
                return app_outcome(&body, sent);
            }
            let inner = auth::payload(&body, 16).map_err(|_| ErrorCode::InvalidMessage)?;
            match wire::decode(&inner).map_err(|_| ErrorCode::InvalidMessage)? {
                Frame::Receipt(r) => Ok(AttemptOutcome::Receipt(r)),
                Frame::ReceiptApplied(r) => Ok(AttemptOutcome::ReceiptApplied(r)),
                Frame::Reject(r) => Ok(reject_outcome(r)),
                _ => Err(ErrorCode::InvalidMessage),
            }
        };
        let outcome = match tokio::time::timeout(Duration::from_secs(5), work).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(ErrorCode::AccountRestricted)) => AttemptOutcome::Blocked(BlockReason::Account),
            // An `App` frame nobody answered: decided by the actor (`NoReply`).
            Ok(Err(ErrorCode::PeerNotConnected)) if written && sent_hash.is_some() => {
                AttemptOutcome::NoReply
            }
            Ok(Err(_)) | Err(_) if written => AttemptOutcome::Written,
            Ok(Err(code)) => AttemptOutcome::Transient(code),
            Err(_) => AttemptOutcome::Transient(ErrorCode::NetworkTimeout),
        };
        // The recipient has it: copies go out on their own and nobody waits for them.
        if let (AttemptOutcome::Acked, Some(wire)) = (&outcome, copy) {
            let sessions: Vec<Session> = self
                .copies
                .into_iter()
                .filter(|session| {
                    self.authority.permits(&SessionLease {
                        session: session.clone(),
                        ticket: lease.ticket,
                    })
                })
                .collect();
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            for _ in &sessions {
                self.frames.lock().unwrap().push(wire.clone());
            }
            tokio::spawn(async move {
                for session in sessions {
                    let _ = super::mls::exchange(&session, &wire).await;
                }
            });
        }
        AttemptResult {
            key: job.key,
            lease,
            outcome,
            ahead,
            reserved: self.reserved,
        }
    }
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
fn mutate(wire: Zeroizing<Vec<u8>>, mutation: MessageMutation) -> Zeroizing<Vec<u8>> {
    // Mutations rewrite kind 16 frames; anything else (an MLS `App` frame) goes out unchanged.
    if matches!(mutation, MessageMutation::None) {
        return wire;
    }
    let mut inner = Zeroizing::new(auth::payload(&wire, 16).unwrap());
    match mutation {
        MessageMutation::None => return wire,
        MessageMutation::UnsupportedVersion => inner[1] = 2,
        MessageMutation::Trailing => inner.push(0),
        _ => {
            let mut frame = wire::decode(&inner).unwrap();
            match (&mut frame, mutation) {
                (Frame::Text(t), MessageMutation::Sender) => t.key.sender = PeerId([0; 32]),
                (Frame::Text(t), MessageMutation::ConflictingText) => t.body.push('!'),
                (Frame::Text(t), MessageMutation::Conversation) => t.key.conversation.0 = [0; 32],
                (Frame::Receipt(r), MessageMutation::ReceiptRecipient) => {
                    r.recipient = InstallationId([0; 32])
                }
                (Frame::Receipt(r), MessageMutation::ReceiptHash) => r.text_hash = [0; 32],
                (Frame::Receipt(r), MessageMutation::ReadReceipt) => {
                    r.kind = crate::message::wire::ReceiptKind::Read
                }
                _ => {}
            }
            if matches!(mutation, MessageMutation::ReceiptAsApplied)
                && let Frame::Receipt(r) = frame
            {
                frame = Frame::ReceiptApplied(r);
            }
            inner = frame.encode();
        }
    }
    Zeroizing::new(auth::envelope(16, &inner))
}
impl Manager {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) fn message_proof_write_starts(&self) -> usize {
        self.messages.proof_writes.load(Ordering::SeqCst)
    }

    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) fn message_hold_scheduler(&mut self, hold: bool) {
        self.messages.scheduler_held = hold;
    }

    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) fn message_write_starts(&self) -> (usize, usize) {
        (
            self.messages.writes.load(Ordering::SeqCst),
            self.messages.replies.load(Ordering::SeqCst),
        )
    }

    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) fn message_ack_pause(&mut self, key: MessageKey) -> Result<(MessagePause, MessagePause), CoreError> {
        self.ready()?;
        self.local()?;
        if self.messages.reply_pause.is_some() || self.messages.write_pause.is_some() {
            return Err(unavailable(ErrorCode::Busy));
        }
        let response = self.message_pause(MessagePoint::Response);
        self.messages.reply_pause.as_mut().unwrap().key = Some(key);
        let receipt = self.message_pause(MessagePoint::FirstByte);
        self.messages.write_pause.as_mut().unwrap().key = Some(key);
        Ok((response, receipt))
    }

    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) fn message_pause(&mut self, response: MessagePoint) -> MessagePause {
        let (entered, rx) = oneshot::channel();
        let (release, tx) = oneshot::channel();
        let (queued, queued_rx) = oneshot::channel();
        let (connection, connection_rx) = oneshot::channel();
        let pause = TestPause {
            key: None,
            entered,
            release: tx,
        };
        match response {
            MessagePoint::Response => self.messages.reply_pause = Some(pause),
            MessagePoint::FirstByte => self.messages.write_pause = Some(pause),
            MessagePoint::Prelaunch => self.messages.prelaunch_pause = Some(pause),
            MessagePoint::Proof => self.messages.proof_pause = Some(pause),
            MessagePoint::Signature => self.messages.signature_pause = Some(pause),
            MessagePoint::ProofResult => {
                self.messages.proof_result_pause = Some(ProofPause {
                    pause,
                    queued,
                    connection,
                })
            }
        }
        MessagePause {
            entered: Some(rx),
            release: Some(release),
            queued: matches!(response, MessagePoint::ProofResult).then_some(queued_rx),
            connection: matches!(response, MessagePoint::ProofResult).then_some(connection_rx),
        }
    }
    pub(super) async fn reset_unlaunched(&mut self, request: Request) -> Result<(), CoreError> {
        let keys: Vec<_> = self
            .messages
            .unlaunched
            .iter()
            .map(|(k, (_, token, _))| (*k, token.clone()))
            .collect();
        for (key, token) in keys {
            if !self.current(request) {
                return Err(unavailable(ErrorCode::StorageLocked));
            }
            self.message_repo()?
                .operation(MessageDbOp::ResetUnlaunched(key, token))
                .await?;
            if !self.current(request) {
                return Err(unavailable(ErrorCode::StorageLocked));
            }
            self.messages.unlaunched.remove(&key);
        }
        Ok(())
    }
    pub(super) async fn release_message_peer(&mut self, peer: PeerId) -> Result<(), CoreError> {
        // Normal M3 supports sessions without contacts and self-device proofs.
        // Those have no releasable M4 text jobs and retain their M3 behavior.
        if peer == self.local()?.keys.peer_id() || self.message_contact(peer).is_err() {
            return Ok(());
        }
        let request = self.ready()?;
        self.message_db(
            MessageDbOp::ReleasePeer(peer, self.services.clock.wall_ms()),
            request,
        )
        .await?;
        Ok(())
    }
    pub(super) async fn message_request(
        &mut self,
        operation: crate::message::service::MessageRequest,
    ) -> Result<crate::message::service::MessageReply, CoreError> {
        use crate::message::service::{MessageReply as A, MessageRequest as Q};
        let request = self.ready()?;
        let local = self.local()?;
        // local() requires this owning actor to be Active; Candidate and PendingImport cannot enqueue.
        let op = match operation {
            Q::Open(peer) => MessageDbOp::Open(local.keys.peer_id(), peer),
            Q::Conversations(before, limit) => MessageDbOp::Conversations(before, limit),
            Q::Messages(id, before, limit) => MessageDbOp::Messages(id, before, limit),
            Q::Send(peer, id, body) => {
                MessageDbOp::Enqueue(peer, id, body, self.services.clock.wall_ms())
            }
            Q::Cancel(key) => MessageDbOp::Cancel(key, self.services.clock.wall_ms()),
            Q::Retry(key) => MessageDbOp::Retry(key, self.services.clock.wall_ms()),
            Q::Read(id, keys) => MessageDbOp::MarkRead(
                id,
                keys,
                self.messages
                    .scheduler
                    .as_ref()
                    .map_or_else(|| self.services.clock.wall_ms(), |s| s.now_ms()),
                self.services.clock.wall_ms(),
            ),
        };
        Ok(match self.message_db(op, request).await? {
            MessageDbReply::Conversation(v) => A::Conversation(v),
            MessageDbReply::Conversations(v) => A::Conversations(v),
            MessageDbReply::Messages(v) => A::Messages(v),
            MessageDbReply::Message(v) => A::Message(v),
            MessageDbReply::Done => A::Done,
            _ => unreachable!(),
        })
    }
    fn message_repo(&self) -> Result<MessageRepo, CoreError> {
        Ok(MessageRepo::new(self.store.clone(), self.local()?))
    }
    /// The scheduler's clock (wall time at start plus monotonic progress), for retry times.
    pub(super) fn schedule_now(&self) -> u64 {
        self.messages
            .scheduler
            .as_ref()
            .map_or_else(|| self.services.clock.wall_ms(), |s| s.now_ms())
    }
    pub(super) fn message_session(
        &self,
        conn: &iroh::endpoint::Connection,
        request: Request,
    ) -> Result<Session, CoreError> {
        self.ensure_ready(request)?;
        let session = self
            .auth
            .registry
            .0
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.conn.stable_id() == conn.stable_id())
            .cloned()
            .ok_or_else(|| unavailable(ErrorCode::AuthFailed))?;
        if !session.ready || !self.grant_authorized(&session.grant) || conn.close_reason().is_some()
        {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        Ok(session)
    }
    fn message_contact(&self, peer: PeerId) -> Result<(), CoreError> {
        if !self.local()?.contacts.iter().any(|c| c.peer == peer) {
            return Err(unavailable(ErrorCode::ContactRequired));
        }
        Ok(())
    }
    fn message_authority(&self) -> Result<Authority, CoreError> {
        Ok(Authority {
            shared: self.shared.clone(),
            registry: self.auth.registry.clone(),
            clock: self.services.clock.clone(),
            own_expires: self
                .local()?
                .records
                .accepted
                .as_ref()
                .ok_or_else(|| unavailable(ErrorCode::AccountRestricted))?
                .record
                .expires_at,
        })
    }
    pub(super) fn message_response_gate(
        &mut self,
        conn: &iroh::endpoint::Connection,
        #[cfg(all(feature = "test-hooks", debug_assertions))] response: &[u8],
    ) -> Result<ResponseGate, CoreError> {
        let request = self.ready()?;
        let session = self.message_session(conn, request)?;
        let authority = self.message_authority()?;
        let lease = SessionLease {
            session,
            ticket: request.ticket,
        };
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let pause = {
            // Only successful Text replies are Receipt (kind 16) or an `Ack` that stored a
            // text (kind 17); rejects and ReceiptApplied can never consume the keyed
            // durable-ack checkpoint.
            let stored = self.messages.ack_key.take();
            let key = auth::payload(response, 16)
                .ok()
                .and_then(|inner| match wire::decode(&inner).ok()? {
                    Frame::Receipt(receipt) => Some(receipt.key),
                    _ => None,
                })
                .or_else(|| {
                    let inner = auth::payload(response, 17).ok()?;
                    matches!(MlsReply::decode(&inner).ok()?, MlsReply::Ack { .. })
                        .then_some(stored)
                        .flatten()
                });
            if self.messages.reply_pause.as_ref().is_some_and(|pause| pause.key.is_none() || pause.key == key) {
                self.messages.reply_pause.take()
            } else { None }
        };
        Ok(ResponseGate {
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            writes: self.messages.replies.clone(),
            authority,
            lease,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            pause,
        })
    }
    fn message_storage_failure(&mut self, code: ErrorCode, request: Request) {
        if code == ErrorCode::StorageUncertain && self.current(request) {
            self.close_auth();
            let ticket = self.shared.network.block();
            self.store.invalidate(ticket);
            self.lock_error = Some(code);
            self.shared
                .status
                .fetch_max((request.ticket << 2) | 2, Ordering::AcqRel);
        }
    }
    pub(super) async fn message_db(
        &mut self,
        op: MessageDbOp,
        request: Request,
    ) -> Result<MessageDbReply, CoreError> {
        self.ensure_ready(request)?;
        let result = self.message_repo()?.operation(MessageDbOp::Observed(Box::new(op))).await;
        if let Err(e) = &result {
            self.message_storage_failure(e.code, request);
        }
        match result {
            Ok(MessageDbReply::Observed(reply, events, head)) => {
                let mailbox = &mut self.messages.mailbox;
                mailbox.head = mailbox.head.max(head);
                self.ensure_ready(request)?;
                for event in events { self.bus.publish(event); }
                Ok(*reply)
            }
            Ok(_) => unreachable!(),
            Err(e) => Err(e),
        }
    }
    pub(super) async fn receive_message(
        &mut self,
        conn: &iroh::endpoint::Connection,
        inner: &[u8],
        request: Request,
    ) -> Result<Vec<u8>, CoreError> {
        let session = self.message_session(conn, request)?;
        let result = async {
            self.message_contact(session.grant.peer)?;
            let frame = wire::decode(inner)?;
            let reply = match frame {
                Frame::Text(text) => {
                    if text.key.sender != session.grant.peer
                        || text.key.installation != session.grant.installation
                        || text.key.conversation
                            != conversation_id(self.local()?.keys.peer_id(), session.grant.peer)?
                    {
                        return Err(unavailable(ErrorCode::InvalidMessage));
                    }
                    self.message_db(
                        MessageDbOp::Receive(
                            text,
                            Sha256::digest(inner).into(),
                            self.services.clock.wall_ms(),
                            self.messages
                                .scheduler
                                .as_ref()
                                .map_or_else(|| self.services.clock.wall_ms(), |s| s.now_ms()),
                        ),
                        request,
                    )
                    .await?
                }
                Frame::Receipt(r) => {
                    if r.recipient != session.grant.installation {
                        return Err(unavailable(ErrorCode::InvalidMessage));
                    }
                    let reply = self
                        .message_db(
                            MessageDbOp::ApplyReceipt(
                                r,
                                session.grant.peer,
                                self.services.clock.wall_ms(),
                            ),
                            request,
                        )
                        .await?;
                    let MessageDbReply::Receipt(r) = reply else {
                        unreachable!()
                    };
                    self.message_session(conn, request)?;
                    return Ok(Frame::ReceiptApplied(r));
                }
                _ => return Err(unavailable(ErrorCode::InvalidMessage)),
            };
            self.message_session(conn, request)?;
            let MessageDbReply::Receipt(r) = reply else {
                unreachable!()
            };
            Ok(Frame::Receipt(r))
        }
        .await;
        // Authority failures close the session; valid application rejections stay on this RPC.
        self.message_session(conn, request)?;
        let frame = match result {
            Ok(frame) => frame,
            Err(e) => Frame::Reject(match e.code {
                ErrorCode::ContactRequired => RejectReason::ContactRequired,
                ErrorCode::UnsupportedMessageVersion => RejectReason::UnsupportedVersion,
                ErrorCode::StorageFull | ErrorCode::StorageUncertain => {
                    RejectReason::StorageUnavailable
                }
                ErrorCode::InvalidMessage
                | ErrorCode::MessageConflict
                | ErrorCode::MessageNotFound
                | ErrorCode::SelfConnection => RejectReason::InvalidMessage,
                _ => return Err(e),
            }),
        };
        Ok(frame.envelope().to_vec())
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) async fn message_attempt(
        &mut self,
        key: MessageKey,
        receipt: bool,
        mutation: MessageMutation,
        reply: oneshot::Sender<Result<(), CoreError>>,
    ) {
        self.start_message_attempt(key, receipt, reply, mutation)
            .await;
    }
    async fn start_message_attempt(
        &mut self,
        key: MessageKey,
        receipt: bool,
        reply: oneshot::Sender<Result<(), CoreError>>,
        #[cfg(all(feature = "test-hooks", debug_assertions))] mutation: MessageMutation,
    ) {
        let result = async {
            let request = self.ready()?;
            if self.messages.scheduler.is_none() {
                self.restore_message_scheduler(request).await?;
            }
            if self.messages.pending.len()
                + self.messages.unlaunched.len()
                + self.messages.acquiring.len()
                >= 4
                || self.messages.pending.contains_key(&key)
                || self.messages.unlaunched.contains_key(&key)
            {
                return Err(unavailable(ErrorCode::Busy));
            }
            // A receipt for a message that came over MLS may go to any device of its sender's
            // account in the group (spec 11: the sender is off, another of its devices takes it
            // and the sender gets it from that one's log); an M4 receipt only to the sender.
            let (peer, mls_receipt) = match self
                .message_db(MessageDbOp::Target(key, receipt), request)
                .await?
            {
                MessageDbReply::Peer(peer) => (peer, false),
                MessageDbReply::ReceiptPeer(peer, mls) => (peer, mls),
                _ => unreachable!(),
            };
            if let Err(error) = self.message_contact(peer) {
                if !receipt {
                    self.message_db(
                        MessageDbOp::AttemptOutcome(
                            key,
                            Some(BlockReason::ContactRequired),
                            None,
                            false,
                        ),
                        request,
                    )
                    .await?;
                }
                return Err(error);
            }
            if self.messages.pending.len() >= 4
                || self.messages.pending.contains_key(&key)
                || self.messages.pending.values().any(|p| p.peer == peer)
                || self.messages.acquiring.values().any(|p| p.peer == peer)
                || self
                    .messages
                    .unlaunched
                    .values()
                    .any(|(_, _, p)| *p == peer)
            {
                return Err(unavailable(ErrorCode::Busy));
            }
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            let m4 = self.messages.mls.m4_peer;
            #[cfg(not(all(feature = "test-hooks", debug_assertions)))]
            let m4 = false;
            // A text is sent sealed (spec 5.1): a row awaiting encryption is sealed first, in
            // the conversation's group; without one the group is asked for and the message
            // waits. There is no plaintext fallback. (The M4-peer fixture of test-hook builds
            // sends its plaintext frame instead, as a released M4 app does.)
            if !receipt {
                if m4 {
                    #[cfg(all(feature = "test-hooks", debug_assertions))]
                    self.message_db(MessageDbOp::M4Encode(key), request).await?;
                } else if let MessageDbReply::NeedGroup(conversation) = self
                    .seal(key, request)
                    .await?
                {
                    self.wait_for_group(key, false, conversation, peer, request)
                        .await?;
                    return Err(unavailable(ErrorCode::NotAvailable));
                }
            }
            let candidates: Vec<Session> = self
                .auth
                .registry
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|s| {
                    s.grant.peer == peer
                        && (!receipt || mls_receipt || s.grant.installation == key.installation)
                        && s.ready
                        && self.grant_authorized(&s.grant)
                        && s.conn.close_reason().is_none()
                })
                .cloned()
                .collect();
            // A text goes to an installation that holds a leaf in the group and has not refused
            // kind 17 (an M4 app among the peer's devices); failing that, to one that has not
            // refused. An MLS receipt goes to the message's sender when it is there, else to
            // another device of its account in the group; an M4 receipt to the sender.
            let members = if (receipt && !mls_receipt) || m4 {
                Vec::new()
            } else {
                self.group_members(key.conversation, request).await?
            };
            let session = if (receipt && !mls_receipt) || m4 {
                candidates.into_iter().next()
            } else if receipt {
                let candidates: Vec<Session> = candidates
                    .into_iter()
                    .filter(|s| {
                        s.grant.installation == key.installation
                            || members.contains(&s.grant.installation)
                    })
                    .collect();
                // Not again to a sender that left the last attempt unanswered on this session,
                // while another device is there.
                let unanswered = self.messages.unanswered.get(&key);
                let stale = |s: &Session| Some(&s.conn.stable_id()) == unanswered;
                let candidates = if candidates.iter().all(stale) {
                    candidates
                } else {
                    candidates.into_iter().filter(|s| !stale(s)).collect()
                };
                choose_session(candidates, &members, Some(key.installation), |i| {
                    self.outdated_now(i)
                })
            } else {
                choose_session(candidates, &members, None, |i| self.outdated_now(i))
            }
            .ok_or_else(|| unavailable(ErrorCode::AccountRestricted))?;
            // Commits/Welcomes this recipient still lacks go ahead of the message, over the
            // same session; while another delivery to it is under way the message waits.
            let recipient = session.grant.installation;
            // Spec 5.3: once the recipient has the text, the same ciphertext also goes, best
            // effort, to the other connected devices of the peer and of this account that hold
            // a leaf in the group and have not refused kind 17. Their answers change nothing.
            let own = self.local()?.keys.peer_id();
            let copies: Vec<Session> = self
                .mls_sessions()
                .into_iter()
                .filter(|s| {
                    !receipt
                        && (s.grant.peer == peer || s.grant.peer == own)
                        && s.grant.installation != recipient
                        && members.contains(&s.grant.installation)
                        && !self.outdated_now(s.grant.installation)
                })
                .collect();
            let ahead = if m4 {
                Vec::new()
            } else {
                if self
                    .messages
                    .mls
                    .inflight
                    .contains(&(key.conversation, recipient))
                {
                    return Err(unavailable(ErrorCode::Busy));
                }
                self.deliveries_ahead(key.conversation, recipient, request)
                    .await?
            };
            let (token, transition) = PrepareToken::new();
            if !receipt {
                self.messages
                    .unlaunched
                    .insert(key, (request.ticket, token, peer));
            }
            let prepared = self
                .message_db(
                    MessageDbOp::Prepare(
                        key,
                        session.grant.installation,
                        receipt,
                        self.services.clock.wall_ms(),
                        self.messages
                            .scheduler
                            .as_ref()
                            .map_or_else(|| self.services.clock.wall_ms(), |s| s.now_ms()),
                        transition,
                    ),
                    request,
                )
                .await?;
            let (wire, receipt) = match prepared {
                MessageDbReply::Prepared(wire, receipt) => (wire, receipt),
                // A receipt for a message received over MLS, and no group to seal it in.
                MessageDbReply::NeedGroup(conversation) => {
                    self.wait_for_group(key, true, conversation, peer, request)
                        .await?;
                    return Err(unavailable(ErrorCode::NotAvailable));
                }
                _ => unreachable!(),
            };
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            if let Some(pause) = self.messages.prelaunch_pause.take() {
                pause.wait().await;
            }
            self.message_session(&session.conn, request)?;
            let attempt = NetworkAttempt {
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                writes: self.messages.writes.clone(),
                authority: self.message_authority()?,
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                pause: if self.messages.write_pause.as_ref().is_some_and(|pause|
                    pause.key.is_none() || (pause.key == Some(key) && receipt.is_some())) {
                    self.messages.write_pause.take()
                } else { None },
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                mutation,
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                frames: self.messages.mls.frames.clone(),
                reserved: (!ahead.is_empty()).then_some((key.conversation, recipient)),
                copies,
            };
            // Nothing below can fail: the reservation is released by the attempt's result.
            if let Some(reserved) = attempt.reserved {
                self.messages.mls.inflight.insert(reserved);
            }
            Ok((
                peer,
                receipt,
                Dispatch {
                    key,
                    peer,
                    wire,
                    ticket: request.ticket,
                    ahead,
                },
                SessionLease {
                    session,
                    ticket: request.ticket,
                },
                attempt,
            ))
        }
        .await;
        match result {
            Err(e) => {
                if let Ok(request) = self.ready()
                    && let Some((ticket, token, _)) = self.messages.unlaunched.get(&key)
                    && *ticket == request.ticket
                {
                    let token = token.clone();
                    if self
                        .message_db(MessageDbOp::ResetUnlaunched(key, token), request)
                        .await
                        .is_ok()
                    {
                        self.messages.unlaunched.remove(&key);
                    }
                }
                let _ = reply.send(Err(e));
            }
            Ok((peer, receipt, job, lease, attempt)) => {
                self.messages.pending.insert(
                    key,
                    Pending {
                        peer,
                        lease: lease.clone(),
                        receipt,
                        reply: Some(reply),
                        completed: false,
                    },
                );
                let mailbox = self.auth.send.clone();
                tokio::spawn(async move {
                    let result = attempt.dispatch_once(job, lease).await;
                    let _ = mailbox
                        .send(crate::account::connection::Input::MessageAttempt(result))
                        .await;
                });
                self.messages.unlaunched.remove(&key);
            }
        }
    }
    pub(super) async fn message_attempt_result(&mut self, mut result: AttemptResult) {
        // The deliveries this attempt carried are no longer under way, whatever else holds.
        if let Some(reserved) = result.reserved.take() {
            self.messages.mls.inflight.remove(&reserved);
        }
        let ahead = std::mem::take(&mut result.ahead);
        if !ahead.is_empty()
            && let Ok(request) = self.ready()
        {
            let _ = self.settle_ahead(ahead, request).await;
        }
        let Some(pending) = self.messages.pending.get(&result.key) else {
            return;
        };
        if pending.lease.ticket != result.lease.ticket
            || pending.lease.session.conn.stable_id() != result.lease.session.conn.stable_id()
        {
            return;
        }
        let mut pending = self.messages.pending.remove(&result.key).unwrap();
        pending.completed = true;
        // Ruling 23: a receipt the message's sender did not answer is not preferred to it on
        // that session again; an answer from it, or the job done elsewhere, ends that.
        if pending.receipt.is_some() {
            let session = &result.lease.session;
            if session.grant.installation != result.key.installation {
                if matches!(
                    result.outcome,
                    AttemptOutcome::Acked | AttemptOutcome::ReceiptApplied(_)
                ) {
                    self.messages.unanswered.remove(&result.key);
                }
            } else if matches!(
                result.outcome,
                AttemptOutcome::Written
                    | AttemptOutcome::NoReply
                    | AttemptOutcome::Transient(
                        ErrorCode::NetworkTimeout
                            | ErrorCode::TransportFailed
                            | ErrorCode::PeerNotConnected
                    )
            ) {
                self.messages
                    .unanswered
                    .insert(result.key, session.conn.stable_id());
            } else {
                self.messages.unanswered.remove(&result.key);
            }
        }
        let outcome = async {
            let request = self.ready()?;
            if request.ticket != result.lease.ticket {
                return Err(unavailable(ErrorCode::AccountRestricted));
            }
            // Any kind 17 answer shows the installation speaks MLS. An unanswered `App` frame
            // from an installation that never did is the M4 app's refusal (it closes the
            // connection): the message waits for the update. Otherwise it was written and the
            // answer lost.
            let recipient = result.lease.session.grant.installation;
            match result.outcome {
                AttemptOutcome::Acked | AttemptOutcome::DecryptFailed(..) => {
                    self.speaks_mls(recipient);
                }
                AttemptOutcome::PeerUpdateRequired => self.mark_outdated(pending.peer, recipient),
                AttemptOutcome::NoReply => {
                    if self
                        .peer_speaks_mls(pending.peer, recipient, request)
                        .await?
                    {
                        result.outcome = AttemptOutcome::Written;
                    } else {
                        self.mark_outdated(pending.peer, recipient);
                        if pending.receipt.is_none() {
                            self.message_db(
                                MessageDbOp::MarkWaitingPeerUpdate(result.key, true),
                                request,
                            )
                            .await?;
                        }
                        result.outcome = AttemptOutcome::PeerUpdateRequired;
                    }
                }
                _ => {}
            }
            if let Err(error) = self.message_session(&result.lease.session.conn, request) {
                // No receipt is accepted from a closed/restricted grant. The known
                // completed attempt may still release its retained retry job.
                if pending.receipt.is_none() {
                    self.message_db(
                        MessageDbOp::AttemptOutcome(
                            result.key,
                            if self.grant_authorized(&result.lease.session.grant) {
                                None
                            } else {
                                Some(BlockReason::Account)
                            },
                            None,
                            matches!(
                                result.outcome,
                                AttemptOutcome::Written
                                    | AttemptOutcome::Receipt(_)
                                    | AttemptOutcome::ReceiptApplied(_)
                                    | AttemptOutcome::Acked
                            ),
                        ),
                        request,
                    )
                    .await?;
                }
                return Err(error);
            }
            let op = match result.outcome {
                AttemptOutcome::Receipt(r)
                    if pending.receipt.is_none()
                        && r.key == result.key
                        && r.recipient == result.lease.session.grant.installation =>
                {
                    MessageDbOp::ApplyReceipt(r, pending.peer, self.services.clock.wall_ms())
                }
                AttemptOutcome::ReceiptApplied(r) if pending.receipt.as_ref() == Some(&r) => {
                    MessageDbOp::Applied(r, pending.peer)
                }
                AttemptOutcome::Receipt(_) | AttemptOutcome::ReceiptApplied(_) => {
                    return Err(unavailable(ErrorCode::InvalidMessage));
                }
                // The recipient stored the sealed text: delivered.
                AttemptOutcome::Acked if pending.receipt.is_none() => {
                    self.messages.epoch_resealed.remove(&result.key);
                    self.messages.epoch_waits.remove(&result.key);
                    self.messages.behind.remove(&result.key);
                    MessageDbOp::MlsAcked(
                        result.key,
                        result.lease.session.grant.installation,
                        self.services.clock.wall_ms(),
                    )
                }
                // The sender of the message stored the sealed receipt: the job is done.
                AttemptOutcome::Acked => {
                    MessageDbOp::Applied(pending.receipt.clone().unwrap(), pending.peer)
                }
                AttemptOutcome::DecryptFailed(class, epoch) => {
                    if pending.receipt.is_none() {
                        self.message_db(
                            MessageDbOp::AttemptOutcome(result.key, None, None, false),
                            request,
                        )
                        .await?;
                        self.after_decrypt_failed(result.key, pending.peer, class, epoch, request)
                            .await?;
                    } else {
                        // The sealed receipt is not resent as is: its next attempt seals again.
                        self.message_db(MessageDbOp::ResealReceipt(result.key), request)
                            .await?;
                    }
                    return Err(unavailable(ErrorCode::NotAvailable));
                }
                AttemptOutcome::PeerUpdateRequired | AttemptOutcome::NoReply => {
                    if pending.receipt.is_none() {
                        self.message_db(
                            MessageDbOp::MarkWaitingPeerUpdate(result.key, true),
                            request,
                        )
                        .await?;
                        self.message_db(
                            MessageDbOp::AttemptOutcome(result.key, None, None, false),
                            request,
                        )
                        .await?;
                    }
                    return Err(unavailable(ErrorCode::UnsupportedMessageVersion));
                }
                AttemptOutcome::Blocked(reason) => {
                    if pending.receipt.is_none() {
                        self.message_db(
                            MessageDbOp::AttemptOutcome(result.key, Some(reason), None, false),
                            request,
                        )
                        .await?;
                    }
                    return Err(unavailable(if reason == BlockReason::ContactRequired {
                        ErrorCode::ContactRequired
                    } else {
                        ErrorCode::AccountRestricted
                    }));
                }
                AttemptOutcome::Permanent(code) => {
                    if pending.receipt.is_none() {
                        self.message_db(
                            MessageDbOp::AttemptOutcome(result.key, None, Some(code), false),
                            request,
                        )
                        .await?;
                    }
                    return Err(unavailable(code));
                }
                AttemptOutcome::Transient(code) => {
                    if pending.receipt.is_none() {
                        self.message_db(
                            MessageDbOp::AttemptOutcome(result.key, None, None, false),
                            request,
                        )
                        .await?;
                    }
                    return Err(unavailable(code));
                }
                AttemptOutcome::Written => {
                    if pending.receipt.is_none() {
                        self.message_db(
                            MessageDbOp::AttemptOutcome(result.key, None, None, true),
                            request,
                        )
                        .await?;
                    }
                    return Err(unavailable(ErrorCode::NetworkTimeout));
                }
            };
            self.message_db(op, request).await?;
            self.message_session(&result.lease.session.conn, request)?;
            Ok(())
        }
        .await;
        if outcome.as_ref().is_err_and(|e| {
            matches!(
                e.code,
                ErrorCode::InvalidMessage | ErrorCode::MessageNotFound
            )
        }) && pending.receipt.is_none()
            && let Ok(request) = self.ready()
            && request.ticket == result.lease.ticket
        {
            // A malformed/mismatched RPC response cannot strand an otherwise retained job.
            let _ = self
                .message_db(
                    MessageDbOp::AttemptOutcome(result.key, None, None, true),
                    request,
                )
                .await;
        }
        if let Some(reply) = pending.reply.take() {
            let _ = reply.send(outcome);
        }
        // Actual network completion is known; a stale lease supplies no receipt authority.
        // Keep ownership until a fresh current request durably settles this exact key.
        let settled = if let Ok(request) = self.ready() {
            self.message_db(MessageDbOp::SettleCompleted(result.key), request)
                .await
                .is_ok()
        } else {
            false
        };
        if !settled {
            self.messages.pending.insert(result.key, pending);
        } else if let Some(counted) = self.messages.reencrypt_after.remove(&result.key)
            && let Ok(request) = self.ready()
        {
            let _ = self
                .reencrypt(result.key, pending.peer, counted, request)
                .await;
        }
    }

    /// The recipient could not open a sent text (spec 6); `epoch` is the recipient's.
    ///
    /// * A recipient without the group (class 4) cannot open any ciphertext until its Welcome
    ///   arrives: the message keeps its back-off and the re-encryption budget (3) is not spent.
    /// * Class 2 from a recipient behind this device: it lacks a Commit of this device's group
    ///   state, which the next attempt carries ahead of the message over the same session; then
    ///   this very ciphertext opens. Nothing is re-encrypted (M2). A repeat at the same
    ///   recipient epoch means the recipient still lacks it (its delivery settled, or it cannot
    ///   rejoin): sealed again, counted, so the message can end Failed instead of Queued.
    /// * Class 2 from a recipient ahead of this device: it moved the group to a newer epoch (C1,
    ///   or any Commit not here yet). The message is sealed again, uncounted, once this device
    ///   has that epoch (`poll_epoch_waits`). Class 2 at this device's own epoch: the recipient
    ///   is there already and the ciphertext is older, so it is sealed again now, uncounted.
    ///   Each message is sealed again uncounted at most once per epoch.
    /// * Any other class from a recipient already past this device's epoch: this device lacks
    ///   a Commit; a new ciphertext cannot help yet (budget not spent).
    /// * Otherwise it is sealed again in the current epoch, counted, and is due at once.
    pub(super) async fn after_decrypt_failed(
        &mut self,
        key: MessageKey,
        peer: PeerId,
        class: u8,
        epoch: u64,
        request: Request,
    ) -> Result<(), CoreError> {
        if class == CLASS_NO_GROUP {
            return Ok(());
        }
        match self.group_epoch(key.conversation, request).await? {
            Some(own) if class == CLASS_FUTURE_EPOCH && epoch < own => {
                if self.messages.behind.insert(key, epoch) == Some(epoch) {
                    self.reencrypt(key, peer, true, request).await
                } else {
                    Ok(())
                }
            }
            Some(own) if class == CLASS_FUTURE_EPOCH && epoch > own => {
                self.messages.epoch_waits.insert(key, (peer, epoch));
                Ok(())
            }
            Some(own) if class == CLASS_FUTURE_EPOCH => {
                // Sealed again in this epoch here: a wait for it (or an older one) must not seal
                // (and count) again. A wait for a newer epoch of another device stays.
                if self
                    .messages
                    .epoch_waits
                    .get(&key)
                    .is_some_and(|(_, wait)| *wait <= own)
                {
                    self.messages.epoch_waits.remove(&key);
                }
                self.reseal_in_epoch(key, peer, own, request).await
            }
            Some(own) if own < epoch => Ok(()),
            _ => self.reencrypt(key, peer, true, request).await,
        }
    }

    /// Seals a message again because the epoch moved on under it (C1): uncounted the first time
    /// in `epoch`, counted after that (a recipient that keeps failing it in one epoch is a real
    /// failure).
    async fn reseal_in_epoch(
        &mut self,
        key: MessageKey,
        peer: PeerId,
        epoch: u64,
        request: Request,
    ) -> Result<(), CoreError> {
        let again = self.messages.epoch_resealed.insert(key, epoch) == Some(epoch);
        self.reencrypt(key, peer, again, request).await
    }

    /// Messages waiting for this device to reach a recipient's newer epoch (C1) are sealed again
    /// once the group is there; a group left meanwhile seals them afresh anyway.
    async fn poll_epoch_waits(&mut self) {
        if self.messages.epoch_waits.is_empty() {
            return;
        }
        let Ok(request) = self.ready() else { return };
        let waits: Vec<(MessageKey, PeerId, u64)> = self
            .messages
            .epoch_waits
            .iter()
            .map(|(key, (peer, epoch))| (*key, *peer, *epoch))
            .collect();
        let mut epochs: BTreeMap<ConversationId, Option<u64>> = BTreeMap::new();
        for (key, peer, epoch) in waits {
            let own = match epochs.get(&key.conversation) {
                Some(own) => *own,
                None => {
                    let Ok(own) = self.group_epoch(key.conversation, request).await else {
                        return;
                    };
                    epochs.insert(key.conversation, own);
                    own
                }
            };
            match own {
                Some(own) if own >= epoch => {
                    self.messages.epoch_waits.remove(&key);
                    let _ = self.reseal_in_epoch(key, peer, own, request).await;
                }
                Some(_) => {}
                None => {
                    self.messages.epoch_waits.remove(&key);
                }
            }
        }
    }

    /// `Seal` before an attempt. A queue too full to take the sealed frame (an upgraded M4
    /// backlog reserved nothing) backs the job off instead of retrying it every poll.
    async fn seal(
        &mut self,
        key: MessageKey,
        request: Request,
    ) -> Result<MessageDbReply, CoreError> {
        match self
            .message_db(
                MessageDbOp::Seal(key, self.services.clock.wall_ms()),
                request,
            )
            .await
        {
            Err(error) if error.code == ErrorCode::StorageFull => {
                let now = self.schedule_now();
                self.message_db(MessageDbOp::Defer(key, false, now), request)
                    .await?;
                Err(error)
            }
            other => other,
        }
    }

    /// `Reencrypt` (`counted` against the budget or not), or, while an attempt of the message
    /// is in flight, once it settles.
    async fn reencrypt(
        &mut self,
        key: MessageKey,
        peer: PeerId,
        counted: bool,
        request: Request,
    ) -> Result<(), CoreError> {
        let now = self.schedule_now();
        match self
            .message_db(MessageDbOp::Reencrypt(key, now, counted), request)
            .await
        {
            Ok(MessageDbReply::NeedGroup(conversation)) => {
                self.ensure_group(conversation, peer).await?;
                Ok(())
            }
            Ok(_) => Ok(()),
            Err(error) if error.code == ErrorCode::Busy => {
                self.messages.reencrypt_after.insert(key, counted);
                Ok(())
            }
            // Delivered or cancelled meanwhile: nothing to send again.
            Err(error) if error.code == ErrorCode::MessageNotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// A message (or an MLS receipt) cannot be sealed yet: the conversation has no group.
    /// `ensure_group` asks for one; the job backs off meanwhile. A peer known to run M4 still
    /// marks a text as waiting for its update.
    async fn wait_for_group(
        &mut self,
        key: MessageKey,
        receipt: bool,
        conversation: ConversationId,
        peer: PeerId,
        request: Request,
    ) -> Result<(), CoreError> {
        self.ensure_group(conversation, peer).await?;
        if !receipt && self.account_outdated(peer) {
            self.message_db(MessageDbOp::MarkWaitingPeerUpdate(key, true), request)
                .await?;
        }
        let now = self.schedule_now();
        self.message_db(MessageDbOp::Defer(key, receipt, now), request)
            .await?;
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AcquisitionPhase {
    Lookup,
    Proof(InstallationId),
    Completed,
}
struct Acquisition {
    peer: PeerId,
    ticket: u64,
    receipt: bool,
    phase: AcquisitionPhase,
    remaining: Vec<InstallationId>,
    cancel: tokio_util::sync::CancellationToken,
    snapshot: Option<ProofSnapshot>,
}
struct ProofSnapshot {
    own: [u8; 32],
    remote: [u8; 32],
    installation: InstallationId,
    transport: [u8; 32],
}
pub(crate) struct ProofGate {
    shared: Arc<Shared>,
    ticket: u64,
    cancel: tokio_util::sync::CancellationToken,
    clock: Arc<dyn crate::account::clock::Clock>,
    own_expires: u64,
    remote_expires: u64,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pause: Option<TestPause>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    writes: Arc<std::sync::atomic::AtomicUsize>,
}
impl ProofGate {
    pub(crate) fn check(&self) -> Result<(), CoreError> {
        if !acquisition_current(&self.shared, self.ticket) {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        if self.cancel.is_cancelled()
            || self.clock.unix_seconds() >= self.own_expires
            || self.clock.unix_seconds() >= self.remote_expires
        {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        Ok(())
    }
    pub(crate) async fn before_signature(&mut self) {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if let Some(pause) = self.pause.take() {
            pause.wait().await;
        }
    }
    pub(crate) fn signature_write(&self) {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.writes.fetch_add(1, Ordering::SeqCst);
    }
}
pub(crate) struct ProofResult {
    key: MessageKey,
    ticket: u64,
    installation: InstallationId,
    conn: Option<iroh::endpoint::Connection>,
    evidence: Option<crate::account::publication::SignedRecord>,
    result: Result<auth::AuthGrant, CoreError>,
}
impl Drop for ProofResult {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            conn.close(0u32.into(), b"unused message proof");
        }
    }
}
pub(super) fn acquisition_current(shared: &Shared, ticket: u64) -> bool {
    let request = *shared.requests.borrow();
    request.ticket == ticket
        && !request.suspend
        && !shared.network.desired_suspend()
        && shared.status.load(Ordering::Acquire) == (ticket << 2) | 1
}
impl Manager {
    pub(super) fn cancel_message_proofs(&self, peer: PeerId) {
        for a in self.messages.acquiring.values().filter(|a| a.peer == peer) {
            a.cancel.cancel();
        }
    }
    fn acquisition_snapshot_current(&self, a: &Acquisition, local: &LocalAccount) -> bool {
        let now = self.services.clock.unix_seconds();
        acquisition_current(&self.shared, a.ticket)
            && Self::local_authorized_at(local, now)
            && local.contacts.iter().any(|c| c.peer == a.peer)
            && a.snapshot.as_ref().is_none_or(|snapshot| {
                local
                    .records
                    .accepted
                    .as_ref()
                    .is_some_and(|r| r.hash == snapshot.own)
                    && local
                        .remote
                        .iter()
                        .find(|r| r.peer == a.peer)
                        .is_some_and(|r| {
                            r.conflict.is_none()
                                && r.accepted.seq >= r.floor
                                && r.accepted.hash == snapshot.remote
                                && r.accepted.current(now)
                                && !r.accepted.record.is_expired(now)
                                && r.accepted.record.devices.iter().any(|d| {
                                    d.installation_id == snapshot.installation
                                        && d.transport_public_key == snapshot.transport
                                })
                        })
            })
    }
    pub(super) fn restrict_message_acquisitions(&self, local: Option<&LocalAccount>) {
        for a in self.messages.acquiring.values() {
            if !local.is_some_and(|local| self.acquisition_snapshot_current(a, local)) {
                a.cancel.cancel();
            }
        }
    }
    // A completed task still owns its exact reservation until the actor consumes it.
    // A valid newer record requires a fresh proof, not an indefinitely blocked job.
    pub(super) fn check_message_proof(
        &self,
        owner: Option<(MessageKey, u64, InstallationId)>,
    ) -> Result<(), CoreError> {
        let Some((key, ticket, installation)) = owner else {
            return Ok(());
        };
        let a = self
            .messages
            .acquiring
            .get(&key)
            .ok_or_else(|| unavailable(ErrorCode::TransportFailed))?;
        if a.ticket != ticket
            || a.phase != AcquisitionPhase::Proof(installation)
            || !acquisition_current(&self.shared, ticket)
        {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        let local = self.local()?;
        let now = self.services.clock.unix_seconds();
        let current = Self::local_authorized_at(local, now)
            && local.contacts.iter().any(|c| c.peer == a.peer)
            && local
                .remote
                .iter()
                .find(|r| r.peer == a.peer)
                .is_some_and(|r| {
                    r.conflict.is_none()
                        && r.accepted.seq >= r.floor
                        && r.accepted.current(now)
                        && !r.accepted.record.is_expired(now)
                        && a.snapshot.as_ref().is_some_and(|snapshot| {
                            r.accepted.record.devices.iter().any(|d| {
                                d.installation_id == snapshot.installation
                                    && d.transport_public_key == snapshot.transport
                            })
                        })
                });
        if !current {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        if a.cancel.is_cancelled() || !self.acquisition_snapshot_current(a, local) {
            return Err(unavailable(ErrorCode::TransportFailed));
        }
        Ok(())
    }
    pub(super) async fn restore_message_scheduler(
        &mut self,
        request: Request,
    ) -> Result<(), CoreError> {
        if self.local().is_err() {
            return Ok(());
        }
        if !self.current(request) {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        let repo = self.message_repo()?;
        if !self.messages.recovered {
            repo.operation(MessageDbOp::RecoverScheduler(self.services.clock.wall_ms()))
                .await?;
            if !self.current(request) {
                return Err(unavailable(ErrorCode::StorageLocked));
            }
            self.messages.recovered = true;
            self.messages.scheduler = Some(crate::message::scheduler::Scheduler::new(
                repo.clone(),
                self.services.clock.clone(),
            ));
        }
        let completed: Vec<_> = self
            .messages
            .pending
            .iter()
            .filter(|(_, p)| p.completed)
            .map(|(k, _)| *k)
            .collect();
        for key in completed {
            repo.operation(MessageDbOp::SettleCompleted(key)).await?;
            if !self.current(request) {
                return Err(unavailable(ErrorCode::StorageLocked));
            }
            self.messages.pending.remove(&key);
        }
        let completed: Vec<_> = self
            .messages
            .acquiring
            .iter()
            .filter(|(_, p)| p.phase == AcquisitionPhase::Completed)
            .map(|(k, p)| (*k, p.receipt))
            .collect();
        for (key, receipt) in completed {
            let now = self.messages.scheduler.as_ref().unwrap().now_ms();
            repo.operation(MessageDbOp::Defer(key, receipt, now))
                .await?;
            if !self.current(request) {
                return Err(unavailable(ErrorCode::StorageLocked));
            }
            self.messages.acquiring.remove(&key);
        }
        Ok(())
    }
    pub(super) async fn poll_messages(&mut self) {
        // MLS group upkeep and its Commit/Welcome deliveries share this tick.
        self.poll_mls().await;
        self.poll_epoch_waits().await;
        // Own-device sync: device list, retention, pulls and `Notify`s (`manager/mailbox.rs`).
        self.poll_mailbox().await;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if self.messages.scheduler_held {
            return;
        }
        let Ok(request) = self.ready() else { return };
        if self.messages.scheduler.is_none() {
            if let Err(e) = self.restore_message_scheduler(request).await {
                self.message_storage_failure(e.code, request);
                return;
            }
            if self.messages.scheduler.is_none() {
                return;
            }
        }
        if self.messages.pending.len()
            + self.messages.unlaunched.len()
            + self.messages.acquiring.len()
            >= 4
        {
            return;
        }
        let mut excluded: Vec<_> = self.messages.pending.values().map(|p| p.peer).collect();
        excluded.extend(self.messages.unlaunched.values().map(|(_, _, peer)| *peer));
        excluded.extend(self.messages.acquiring.values().map(|p| p.peer));
        let scheduler = self.messages.scheduler.as_mut().unwrap();
        scheduler.ticket = request.ticket;
        scheduler.excluded = excluded;
        let now = scheduler.now_ms();
        let jobs = match scheduler.poll_due(now).await {
            Ok(jobs) => jobs,
            Err(e) => {
                self.message_storage_failure(e.code, request);
                return;
            }
        };
        for job in jobs {
            if self.ensure_ready(request).is_err() {
                break;
            }
            if self.messages.pending.len()
                + self.messages.unlaunched.len()
                + self.messages.acquiring.len()
                >= 4
            {
                break;
            }
            let receipt = job.key.sender != self.local().unwrap().keys.peer_id();
            if let Err(error) = self.message_contact(job.peer) {
                let _ = self
                    .scheduling_failure(job.key, receipt, error.code, request)
                    .await;
                continue;
            }
            if !self.local_authorized() {
                let _ = self
                    .scheduling_failure(job.key, receipt, ErrorCode::AccountRestricted, request)
                    .await;
                continue;
            }
            let ready = |original: bool| {
                self.auth.registry.0.lock().unwrap().iter().any(|s| {
                    s.grant.peer == job.peer
                        && (!receipt || !original || s.grant.installation == job.key.installation)
                        && s.ready
                        && self.grant_authorized(&s.grant)
                        && s.conn.close_reason().is_none()
                })
            };
            let (ready, other) = (ready(true), receipt && ready(false));
            // A receipt whose message's sender has no session may still go to another device
            // of its account (an MLS receipt, spec 11); if none takes it, the sender is looked up.
            if !ready && other {
                let (reply, _) = oneshot::channel();
                self.start_message_attempt(
                    job.key,
                    receipt,
                    reply,
                    #[cfg(all(feature = "test-hooks", debug_assertions))]
                    MessageMutation::None,
                )
                .await;
                if self.messages.pending.contains_key(&job.key) {
                    self.messages
                        .scheduler
                        .as_mut()
                        .unwrap()
                        .dispatched(receipt);
                    continue;
                }
            }
            if ready {
                let (reply, _) = oneshot::channel();
                self.start_message_attempt(
                    job.key,
                    receipt,
                    reply,
                    #[cfg(all(feature = "test-hooks", debug_assertions))]
                    MessageMutation::None,
                )
                .await;
                if self.messages.pending.contains_key(&job.key) {
                    self.messages
                        .scheduler
                        .as_mut()
                        .unwrap()
                        .dispatched(receipt);
                }
            } else {
                self.start_message_lookup(job.key, job.peer, receipt, request)
                    .await;
            }
        }
    }
    async fn scheduling_failure(
        &mut self,
        key: MessageKey,
        receipt: bool,
        code: ErrorCode,
        request: Request,
    ) -> Result<(), CoreError> {
        let block = match code {
            ErrorCode::ContactRequired => Some(BlockReason::ContactRequired),
            ErrorCode::AccountRestricted | ErrorCode::AuthFailed | ErrorCode::RecordConflict => {
                Some(BlockReason::Account)
            }
            ErrorCode::StorageLocked | ErrorCode::StorageUncertain => Some(BlockReason::Storage),
            _ => None,
        };
        if !receipt && block.is_some() {
            self.message_db(
                MessageDbOp::AttemptOutcome(key, block, None, false),
                request,
            )
            .await?;
        } else {
            let now = self.messages.scheduler.as_ref().unwrap().now_ms();
            self.message_db(MessageDbOp::Defer(key, receipt, now), request)
                .await?;
        }
        Ok(())
    }
    async fn start_message_lookup(
        &mut self,
        key: MessageKey,
        peer: PeerId,
        receipt: bool,
        request: Request,
    ) {
        let backend = match self.services.backend() {
            Ok(b) => b,
            Err(e) => {
                let _ = self.scheduling_failure(key, receipt, e.code, request).await;
                return;
            }
        };
        let cancel = self.auth.cancel.child_token();
        self.messages.acquiring.insert(
            key,
            Acquisition {
                peer,
                ticket: request.ticket,
                receipt,
                phase: AcquisitionPhase::Lookup,
                remaining: Vec::new(),
                cancel: cancel.clone(),
                snapshot: None,
            },
        );
        self.messages
            .scheduler
            .as_mut()
            .unwrap()
            .dispatched(receipt);
        let shared = self.shared.clone();
        let mailbox = self.auth.send.clone();
        tokio::spawn(async move {
            let result = tokio::select! {biased;
                _=cancel.cancelled()=>Err(unavailable(ErrorCode::StorageLocked)),
                result=tokio::time::timeout(Duration::from_secs(10),backend.lookup(peer))=>
                    result.unwrap_or_else(|_|Err(unavailable(ErrorCode::RecordUnavailable))),
            };
            let result = if acquisition_current(&shared, request.ticket) {
                result
            } else {
                Err(unavailable(ErrorCode::StorageLocked))
            };
            let _ = mailbox
                .send(crate::account::connection::Input::MessageLookup(
                    key,
                    request.ticket,
                    result,
                ))
                .await;
        });
    }
    pub(super) async fn message_lookup(
        &mut self,
        key: MessageKey,
        ticket: u64,
        result: Result<super::super::Lookup, CoreError>,
    ) {
        let Some(a) = self.messages.acquiring.get(&key) else {
            return;
        };
        if a.ticket != ticket || a.phase != AcquisitionPhase::Lookup {
            return;
        }
        let peer = a.peer;
        let result = async {
            let request = self.ready()?;
            if request.ticket != ticket {
                return Err(unavailable(ErrorCode::StorageLocked));
            }
            if !self.local_authorized() {
                return Err(unavailable(ErrorCode::AccountRestricted));
            }
            let record = self.finish_remote_record(peer, result, request).await?;
            self.ensure_ready(request)?;
            let a = self.messages.acquiring.get_mut(&key).unwrap();
            a.remaining = record
                .record
                .devices
                .iter()
                .filter(|d| !a.receipt || d.installation_id == key.installation)
                .map(|d| d.installation_id)
                .collect();
            self.start_message_proof(key, request)
        }
        .await;
        if let Err(e) = result {
            self.finish_acquisition(key, e.code).await;
        }
    }
    fn start_message_proof(&mut self, key: MessageKey, request: Request) -> Result<(), CoreError> {
        self.ensure_ready(request)?;
        if !self.local_authorized() {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        let a = self.messages.acquiring.get_mut(&key).unwrap();
        if a.cancel.is_cancelled() {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        if a.remaining.is_empty() {
            return Err(unavailable(ErrorCode::TransportFailed));
        }
        let installation = a.remaining.remove(0);
        let peer = a.peer;
        a.phase = AcquisitionPhase::Proof(installation);
        let Some(AccountState::Active(local)) = self.copy_state() else {
            return Err(unavailable(ErrorCode::AccountRestricted));
        };
        let remote = local
            .remote
            .iter()
            .find(|r| {
                r.peer == peer
                    && r.conflict.is_none()
                    && r.accepted.seq >= r.floor
                    && r.accepted.current(self.services.clock.unix_seconds())
            })
            .ok_or_else(|| unavailable(ErrorCode::AccountRestricted))?;
        let record = remote.accepted.clone();
        let device = record
            .record
            .devices
            .iter()
            .find(|d| d.installation_id == installation)
            .ok_or_else(|| unavailable(ErrorCode::AccountRestricted))?;
        let endpoint = iroh::EndpointId::from_bytes(&device.transport_public_key)
            .map_err(|_| unavailable(ErrorCode::AuthFailed))?;
        let shared = self.shared.clone();
        let clock = self.services.clock.clone();
        let mailbox = self.auth.send.clone();
        let a = self.messages.acquiring.get_mut(&key).unwrap();
        a.snapshot = Some(ProofSnapshot {
            own: local.records.accepted.as_ref().unwrap().hash,
            remote: record.hash,
            installation,
            transport: device.transport_public_key,
        });
        let cancel = a.cancel.clone();
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let pause = self.messages.proof_pause.take();
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let result_pause = self.messages.proof_result_pause.take();
        let mut gate = ProofGate {
            shared: shared.clone(),
            ticket: request.ticket,
            cancel: cancel.clone(),
            clock: clock.clone(),
            own_expires: local.records.accepted.as_ref().unwrap().record.expires_at,
            remote_expires: record.record.expires_at,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            pause: self.messages.signature_pause.take(),
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            writes: self.messages.proof_writes.clone(),
        };
        tokio::spawn(async move {
            let mut conn = None;
            let mut evidence = None;
            let work = async {
                if !acquisition_current(&shared, request.ticket) {
                    return Err(unavailable(ErrorCode::StorageLocked));
                }
                let connected = shared
                    .network
                    .dial_account(iroh::EndpointAddr::from(endpoint))
                    .await?;
                conn = Some(connected.clone());
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                if let Some(pause) = pause {
                    pause.wait().await;
                }
                if !acquisition_current(&shared, request.ticket) {
                    return Err(unavailable(ErrorCode::StorageLocked));
                }
                auth::authenticate(
                    connected,
                    peer,
                    &local,
                    &record,
                    &*clock,
                    &mut evidence,
                    Some(&mut gate),
                )
                .await
            };
            let result = tokio::select! {biased;
                _=cancel.cancelled()=>Err(unavailable(ErrorCode::AccountRestricted)),
                result=tokio::time::timeout(Duration::from_secs(10),work)=>result.unwrap_or_else(|_|Err(unavailable(ErrorCode::NetworkTimeout))),
            };
            let result = if acquisition_current(&shared, request.ticket) {
                result
            } else {
                Err(unavailable(ErrorCode::StorageLocked))
            };
            if result.is_err()
                && let Some(conn) = &conn
            {
                conn.close(0u32.into(), b"message proof failed");
            }
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            let queued = if result.is_ok()
                && let Some(pause) = result_pause
            {
                let _ = pause.connection.send(conn.as_ref().unwrap().clone());
                pause.pause.wait().await;
                Some(pause.queued)
            } else {
                None
            };
            let message = ProofResult {
                key,
                ticket: request.ticket,
                installation,
                conn,
                evidence,
                result,
            };
            // Drop closes an obtained connection on cancellation, stale/missing owner or failed mailbox.
            let sent = mailbox
                .send(crate::account::connection::Input::MessageProof(message))
                .await
                .is_ok();
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            if sent && let Some(queued) = queued {
                let _ = queued.send(());
            }
            #[cfg(not(all(feature = "test-hooks", debug_assertions)))]
            let _ = sent;
        });
        Ok(())
    }
    pub(super) async fn message_proof(&mut self, mut proof: ProofResult) {
        let Some(a) = self.messages.acquiring.get(&proof.key) else {
            return;
        };
        if a.ticket != proof.ticket || a.phase != AcquisitionPhase::Proof(proof.installation) {
            return;
        }
        let (peer, receipt) = (a.peer, a.receipt);
        let owner = Some((proof.key, proof.ticket, proof.installation));
        let result = async {
            let request = self.ready()?;
            if request.ticket != proof.ticket {
                return Err(unavailable(ErrorCode::StorageLocked));
            }
            if let Some(evidence) = proof.evidence.take() {
                self.observe_auth_record(peer, evidence, request).await?;
            }
            self.ensure_ready(request)?;
            self.check_message_proof(owner)?;
            match &proof.result {
                Ok(grant) => {
                    let conn = proof.conn.as_ref().unwrap().clone();
                    self.install_grant(conn, grant.clone(), owner).await?;
                    self.ensure_ready(request)?;
                    self.check_message_proof(owner)?;
                    proof.conn.take();
                    self.messages.acquiring.remove(&proof.key);
                    let (reply, _) = oneshot::channel();
                    self.start_message_attempt(
                        proof.key,
                        receipt,
                        reply,
                        #[cfg(all(feature = "test-hooks", debug_assertions))]
                        MessageMutation::None,
                    )
                    .await;
                    Ok(())
                }
                Err(e) => {
                    let code = e.code;
                    if self
                        .messages
                        .acquiring
                        .get(&proof.key)
                        .unwrap()
                        .remaining
                        .is_empty()
                    {
                        Err(unavailable(code))
                    } else {
                        self.start_message_proof(proof.key, request)
                    }
                }
            }
        }
        .await;
        if let Err(e) = result {
            self.finish_acquisition(proof.key, e.code).await;
        }
    }
    async fn finish_acquisition(&mut self, key: MessageKey, code: ErrorCode) {
        let Some(a) = self.messages.acquiring.get_mut(&key) else {
            return;
        };
        a.phase = AcquisitionPhase::Completed;
        let (receipt, ticket) = (a.receipt, a.ticket);
        if let Ok(request) = self.ready()
            && self
                .scheduling_failure(
                    key,
                    receipt,
                    if ticket != request.ticket {
                        ErrorCode::TransportFailed
                    } else {
                        code
                    },
                    request,
                )
                .await
                .is_ok()
        {
            self.messages.acquiring.remove(&key);
        }
    }
}
