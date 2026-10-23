//! M4 message authorization and one bounded network attempt; scheduling remains separate.
use super::*;
use crate::account::{
    auth,
    connection::{Registry, Session},
};
use crate::message::{
    store::{MessageDbOp, MessageDbReply, MessageRepo, conversation_id},
    types::{BlockReason, MessageKey},
    wire::{self, Frame, ReceiptFrame, RejectReason},
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(crate) struct Dispatch {
    pub key: MessageKey,
    pub peer: PeerId,
    pub wire: Zeroizing<Vec<u8>>,
    pub ticket: u64,
}
#[derive(Clone)]
pub(crate) struct SessionLease {
    pub session: Session,
    pub ticket: u64,
}
pub(crate) enum AttemptOutcome {
    Written,
    Receipt(ReceiptFrame),
    ReceiptApplied(ReceiptFrame),
    Transient(ErrorCode),
    Blocked(BlockReason),
    Permanent(ErrorCode),
}
pub(crate) struct AttemptResult {
    pub key: MessageKey,
    pub lease: SessionLease,
    pub outcome: AttemptOutcome,
}
struct Pending {
    peer: PeerId,
    lease: SessionLease,
    receipt: Option<ReceiptFrame>,
    reply: oneshot::Sender<Result<(), CoreError>>,
}
#[derive(Default)]
pub(super) struct Runtime {
    pending: BTreeMap<MessageKey, Pending>,
    unlaunched: BTreeMap<MessageKey, u64>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    writes: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    replies: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    prelaunch_pause: Option<TestPause>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    reply_pause: Option<TestPause>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    write_pause: Option<TestPause>,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
struct TestPause {
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
pub struct MessagePause {
    entered: Option<oneshot::Receiver<()>>,
    release: Option<oneshot::Sender<()>>,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
impl MessagePause {
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
}
impl AccountHandle {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub async fn message_test_write_starts(&self) -> Result<(usize, usize), CoreError> {
        self.send(Command::MessageWriteStarts).await
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
}
impl NetworkAttempt {
    async fn dispatch_once(self, job: Dispatch, lease: SessionLease) -> AttemptResult {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let mut pause = self.pause;
        let mut written = false;
        let work = async {
            if job.ticket != lease.ticket
                || job.peer != lease.session.grant.peer
                || !self.authority.permits(&lease)
            {
                return Err(ErrorCode::AccountRestricted);
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
            self.writes.fetch_add(1, Ordering::SeqCst);
            auth::write_frame(&mut send, &wire)
                .await
                .map_err(|_| ErrorCode::TransportFailed)?;
            written = true;
            send.finish().map_err(|_| ErrorCode::TransportFailed)?;
            let body = auth::read_frame(&mut recv, 61440)
                .await
                .map_err(|_| ErrorCode::TransportFailed)?;
            if !self.authority.permits(&lease) {
                return Err(ErrorCode::AccountRestricted);
            }
            let inner = auth::payload(&body, 16).map_err(|_| ErrorCode::InvalidMessage)?;
            match wire::decode(&inner).map_err(|_| ErrorCode::InvalidMessage)? {
                Frame::Receipt(r) => Ok(AttemptOutcome::Receipt(r)),
                Frame::ReceiptApplied(r) => Ok(AttemptOutcome::ReceiptApplied(r)),
                Frame::Reject(r) => Ok(match r {
                    RejectReason::ContactRequired => {
                        AttemptOutcome::Blocked(BlockReason::ContactRequired)
                    }
                    RejectReason::StorageUnavailable => {
                        AttemptOutcome::Transient(ErrorCode::StorageFull)
                    }
                    RejectReason::UnsupportedVersion => {
                        AttemptOutcome::Permanent(ErrorCode::UnsupportedMessageVersion)
                    }
                    RejectReason::InvalidMessage => {
                        AttemptOutcome::Permanent(ErrorCode::InvalidMessage)
                    }
                }),
                _ => Err(ErrorCode::InvalidMessage),
            }
        };
        let outcome = match tokio::time::timeout(Duration::from_secs(5), work).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(ErrorCode::AccountRestricted)) => AttemptOutcome::Blocked(BlockReason::Account),
            Ok(Err(_)) | Err(_) if written => AttemptOutcome::Written,
            Ok(Err(code)) => AttemptOutcome::Transient(code),
            Err(_) => AttemptOutcome::Transient(ErrorCode::NetworkTimeout),
        };
        AttemptResult {
            key: job.key,
            lease,
            outcome,
        }
    }
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
fn mutate(wire: Zeroizing<Vec<u8>>, mutation: MessageMutation) -> Zeroizing<Vec<u8>> {
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
    pub(super) fn message_write_starts(&self) -> (usize, usize) {
        (
            self.messages.writes.load(Ordering::SeqCst),
            self.messages.replies.load(Ordering::SeqCst),
        )
    }

    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) fn message_pause(&mut self, response: MessagePoint) -> MessagePause {
        let (entered, rx) = oneshot::channel();
        let (release, tx) = oneshot::channel();
        let pause = Some(TestPause {
            entered,
            release: tx,
        });
        match response {
            MessagePoint::Response => self.messages.reply_pause = pause,
            MessagePoint::FirstByte => self.messages.write_pause = pause,
            MessagePoint::Prelaunch => self.messages.prelaunch_pause = pause,
        }
        MessagePause {
            entered: Some(rx),
            release: Some(release),
        }
    }
    pub(super) async fn reset_unlaunched(&mut self, request: Request) -> Result<(), CoreError> {
        let keys: Vec<_> = self.messages.unlaunched.keys().copied().collect();
        for key in keys {
            if !self.current(request) {
                return Err(unavailable(ErrorCode::StorageLocked));
            }
            self.message_repo()?
                .operation(MessageDbOp::ResetUnlaunched(key))
                .await?;
            if !self.current(request) {
                return Err(unavailable(ErrorCode::StorageLocked));
            }
            self.messages.unlaunched.remove(&key);
        }
        Ok(())
    }
    fn message_repo(&self) -> Result<MessageRepo, CoreError> {
        Ok(MessageRepo::new(self.store.clone(), self.local()?))
    }
    fn message_session(
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
    ) -> Result<ResponseGate, CoreError> {
        let request = self.ready()?;
        let session = self.message_session(conn, request)?;
        let authority = self.message_authority()?;
        let lease = SessionLease {
            session,
            ticket: request.ticket,
        };
        Ok(ResponseGate {
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            writes: self.messages.replies.clone(),
            authority,
            lease,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            pause: self.messages.reply_pause.take(),
        })
    }
    async fn message_db(
        &mut self,
        op: MessageDbOp,
        request: Request,
    ) -> Result<MessageDbReply, CoreError> {
        self.ensure_ready(request)?;
        let result = self.message_repo()?.operation(op).await;
        if let Err(e) = &result {
            if e.code == ErrorCode::StorageUncertain && self.current(request) {
                self.close_auth();
                let ticket = self.shared.network.block();
                self.store.invalidate(ticket);
                self.lock_error = Some(e.code);
                self.shared
                    .status
                    .fetch_max((request.ticket << 2) | 2, Ordering::AcqRel);
            }
        }
        match result {
            Ok(reply) => {
                self.ensure_ready(request)?;
                Ok(reply)
            }
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
                            self.services.clock.unix_seconds().saturating_mul(1000),
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
                                self.services.clock.unix_seconds().saturating_mul(1000),
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
            if self.messages.pending.len() + self.messages.unlaunched.len() >= 4
                || self.messages.pending.contains_key(&key)
                || self.messages.unlaunched.contains_key(&key)
            {
                return Err(unavailable(ErrorCode::Busy));
            }
            let MessageDbReply::Peer(peer) = self
                .message_db(MessageDbOp::Target(key, receipt), request)
                .await?
            else {
                unreachable!()
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
            {
                return Err(unavailable(ErrorCode::Busy));
            }
            let session = self
                .auth
                .registry
                .0
                .lock()
                .unwrap()
                .iter()
                .find(|s| {
                    s.grant.peer == peer
                        && (!receipt || s.grant.installation == key.installation)
                        && s.ready
                        && self.grant_authorized(&s.grant)
                        && s.conn.close_reason().is_none()
                })
                .cloned()
                .ok_or_else(|| unavailable(ErrorCode::AccountRestricted))?;
            if !receipt {
                self.messages.unlaunched.insert(key, request.ticket);
            }
            let MessageDbReply::Prepared(wire, receipt) = self
                .message_db(
                    MessageDbOp::Prepare(
                        key,
                        session.grant.installation,
                        receipt,
                        self.services.clock.unix_seconds().saturating_mul(1000),
                    ),
                    request,
                )
                .await?
            else {
                unreachable!()
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
                pause: self.messages.write_pause.take(),
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                mutation,
            };
            Ok((
                peer,
                receipt,
                Dispatch {
                    key,
                    peer,
                    wire,
                    ticket: request.ticket,
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
                    && self.messages.unlaunched.get(&key) == Some(&request.ticket)
                {
                    if self
                        .message_db(MessageDbOp::ResetUnlaunched(key), request)
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
                        reply,
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
    pub(super) async fn message_attempt_result(&mut self, result: AttemptResult) {
        let Some(pending) = self.messages.pending.get(&result.key) else {
            return;
        };
        if pending.lease.ticket != result.lease.ticket
            || pending.lease.session.conn.stable_id() != result.lease.session.conn.stable_id()
        {
            return;
        }
        let pending = self.messages.pending.remove(&result.key).unwrap();
        let outcome = async {
            let request = self.ready()?;
            if request.ticket != result.lease.ticket {
                return Err(unavailable(ErrorCode::AccountRestricted));
            }
            if let Err(error) = self.message_session(&result.lease.session.conn, request) {
                // No receipt is accepted from a closed/restricted grant. The known
                // completed attempt may still release its retained retry job.
                if pending.receipt.is_none() {
                    self.message_db(
                        MessageDbOp::AttemptOutcome(
                            result.key,
                            Some(BlockReason::Account),
                            None,
                            false,
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
                    MessageDbOp::ApplyReceipt(
                        r,
                        pending.peer,
                        self.services.clock.unix_seconds().saturating_mul(1000),
                    )
                }
                AttemptOutcome::ReceiptApplied(r) if pending.receipt.as_ref() == Some(&r) => {
                    MessageDbOp::Applied(r, pending.peer)
                }
                AttemptOutcome::Receipt(_) | AttemptOutcome::ReceiptApplied(_) => {
                    return Err(unavailable(ErrorCode::InvalidMessage));
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
        let _ = pending.reply.send(outcome);
    }
}
