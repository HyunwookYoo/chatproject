//! One bounded, typed writer. SQL and OS protection run only on blocking threads.
use super::migration::{self, binding, load};
use crate::message::store::{self as messages, Identity, MessageDbOp, MessageDbReply};
use crate::{
    CoreError, ErrorCode,
    account::storage::{KeyProtector, StoredPlaintext, ownership::Ownership, schema::AccountState},
};
use rusqlite::{Connection, params};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
type R<T> = Result<T, CoreError>;
#[cfg(all(feature = "test-hooks", debug_assertions))]
fn corrupt() -> CoreError {
    CoreError::new(ErrorCode::StorageCorrupt, "db_binding_invalid")
}
fn locked() -> CoreError {
    CoreError::new(ErrorCode::StorageLocked, "storage_suspended_or_stale")
}
fn uncertain() -> CoreError {
    CoreError::new(ErrorCode::StorageUncertain, "storage_commit_uncertain")
}
struct Fence {
    // Exact last accepted M3 network lifecycle ticket. Never minted here.
    ticket: u64,
    blocked: bool,
    stale_ticket: bool,
    // Local request cancellation has no network authority and cannot wrap.
    cancellation: CancellationToken,
}
#[derive(Default)]
pub(crate) struct Counts {
    pub live: std::sync::atomic::AtomicUsize,
    pub opens: std::sync::atomic::AtomicUsize,
}
impl Counts {
    fn opened(&self) {
        self.live.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.opens.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn closed(&self) {
        self.live.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
enum State {
    Closed,
    Open(Connection),
    Uncertain,
}
struct Worker {
    ownership: Ownership,
    protector: Arc<dyn KeyProtector>,
    id: [u8; 16],
    state: State,
    pending_message: Option<messages::PendingMessage>,
    counts: Arc<Counts>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    hook: Option<(usize, CommitAction)>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    message_fault: Option<CommitFault>,
    #[cfg(all(feature="test-hooks",debug_assertions))]
    message_reply_pause: Option<(oneshot::Sender<()>,std::sync::mpsc::Receiver<()>)>,
    #[cfg(all(feature="test-hooks",debug_assertions))]
    message_clock: Option<(Arc<crate::account::clock::FakeClock>,u64)>,
}
impl Worker {
    fn message(&mut self, identity: Identity, op: MessageDbOp) -> R<MessageDbReply> {
        let key = op.pending(identity)?;
        let db_id = self.id;
        let pending = self.pending_message;
        let mut reconciled = false;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let fault = if key.is_some() {
            self.message_fault.take()
        } else {
            None
        };
        #[cfg(all(feature="test-hooks",debug_assertions))]
        let clock=if key.is_some() {self.message_clock.take()} else {None};
        let result = self.operation(|c| {
            let stored = load(c, &db_id)?.ok_or_else(|| {
                CoreError::new(ErrorCode::AccountRestricted, "active_account_required")
            })?;
            let AccountState::Active(local) = AccountState::decode(&stored)? else {
                return Err(CoreError::new(
                    ErrorCode::AccountRestricted,
                    "active_account_required",
                ));
            };
            if local.keys.peer_id() != identity.peer
                || local.device.installation_id() != identity.installation
            {
                return Err(CoreError::new(
                    ErrorCode::AccountRestricted,
                    "message_identity_mismatch",
                ));
            }
            if let Some(key) = pending {
                messages::reconcile(c, key)?;
            }
            reconciled = true;
            messages::authorize(c,&local,&op)?;
            messages::execute(
                c,
                identity,
                op,
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                fault,
            )
        });
        #[cfg(all(feature="test-hooks",debug_assertions))]
        if key.is_some() && result.is_ok() && let Some((entered,release))=self.message_reply_pause.take() {let _=entered.send(());let _=release.recv();}
        #[cfg(all(feature="test-hooks",debug_assertions))]
        if result.is_ok() && let Some((clock,seconds))=clock {clock.set_unix_seconds(seconds);}
        if result
            .as_ref()
            .is_err_and(|e| e.code == ErrorCode::StorageUncertain)
        {
            self.pending_message = if reconciled { key } else { pending.or(key) };
            self.close(true);
        } else if result.is_ok() {
            self.pending_message = None;
        }
        result
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    fn next_commit_action(&mut self) -> Option<CommitAction> {
        match self.hook.as_mut() {
            Some((remaining, _)) if *remaining > 1 => {
                *remaining -= 1;
                None
            }
            Some(_) => self.hook.take().map(|(_, action)| action),
            None => None,
        }
    }
    fn discard_pending(&mut self) -> R<()> {
        let id = self.id;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let action = self.next_commit_action();
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let fault = match &action {
            Some(CommitAction::Fault(fault)) => Some(*fault),
            _ => None,
        };
        let result = self.operation(|conn| {
            if let Some(stored) = load(conn, &id)? {
                if matches!(AccountState::decode(&stored)?, AccountState::Active(_)) {
                    return Err(CoreError::new(
                        ErrorCode::AccountExists,
                        "active_binding_immutable",
                    ));
                }
            }
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            if fault == Some(CommitFault::WriteDenied) {
                conn.pragma_update(None, "query_only", true)
                    .map_err(|_| uncertain())?;
            }
            let tx = conn.transaction().map_err(|_| uncertain())?;
            tx.execute("DELETE FROM account_state WHERE singleton=1", [])
                .map_err(|_| uncertain())?;
            tx.execute(
                "UPDATE storage_meta SET binding=?1 WHERE singleton=1",
                params![binding(None)?],
            )
            .map_err(|_| uncertain())?;
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            if fault == Some(CommitFault::BeforeCommit) {
                return Err(uncertain());
            }
            tx.commit().map_err(|_| uncertain())?;
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            if fault == Some(CommitFault::AfterCommit) {
                return Err(uncertain());
            }
            Ok(())
        });
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if result.is_ok()
            && let Some(CommitAction::Clock(clock, seconds)) = action
        {
            clock.set_unix_seconds(seconds);
        }
        if result
            .as_ref()
            .is_err_and(|e| e.code == ErrorCode::StorageUncertain)
        {
            self.close(true);
        }
        result
    }
    fn close(&mut self, uncertain: bool) {
        let previous = std::mem::replace(
            &mut self.state,
            if uncertain {
                State::Uncertain
            } else {
                State::Closed
            },
        );
        if let State::Open(c) = previous {
            drop(c);
            self.counts.closed();
        }
    }
    fn operation<T>(&mut self, action: impl FnOnce(&mut Connection) -> R<T>) -> R<T> {
        let Self {
            ownership,
            protector,
            id,
            state,
            counts,
            ..
        } = self;
        ownership.with_lock(|root| {
            if !matches!(state, State::Open(_)) {
                // Re-key, read binding and requery account after every uncertain result.
                let conn = migration::reopen(root, &**protector, id)?;
                counts.opened();
                *state = State::Open(conn);
            }
            let State::Open(conn) = state else {
                unreachable!()
            };
            let result = action(conn);
            // iOS never retains a connection after releasing its operation lock.
            if cfg!(target_os = "ios") {
                let old = std::mem::replace(state, State::Closed);
                drop(old);
                counts.closed();
            }
            result
        })
    }
    fn commit(&mut self, state: StoredPlaintext) -> R<()> {
        if state.payload.len() > 4 * 1024 * 1024 {
            return Err(CoreError::new(
                ErrorCode::StorageFull,
                "account_snapshot_too_large",
            ));
        }
        let new_binding = binding(Some(&state))?;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let action = self.next_commit_action();
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let fault = match &action {
            Some(CommitAction::Fault(fault)) => Some(*fault),
            _ => None,
        };
        let id = self.id;
        let result = self.operation(|conn| {
            if load(conn, &id)?.is_some() {
                // load just validated this binding against the full old snapshot.
                // Same owned connection: no await or intervening write/other writer.
                let previous: Vec<u8> = conn
                    .query_row(
                        "SELECT binding FROM storage_meta WHERE singleton=1",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|_| CoreError::new(ErrorCode::StorageCorrupt, "db_binding_invalid"))?;
                // Active identity/installation cannot be replaced with Candidate or another account.
                if previous.get(2) == Some(&2) && previous != new_binding {
                    return Err(CoreError::new(
                        ErrorCode::AccountExists,
                        "active_binding_immutable",
                    ));
                }
            }
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            if fault == Some(CommitFault::WriteDenied) {
                conn.pragma_update(None, "query_only", true)
                    .map_err(|_| uncertain())?;
            }
            let tx = conn.transaction().map_err(|_| uncertain())?;
            tx.execute(
                "INSERT INTO account_state VALUES(1,?1,?2) ON CONFLICT(singleton) \
                 DO UPDATE SET installation=excluded.installation,payload=excluded.payload",
                params![state.installation.as_slice(), state.payload.as_slice()],
            )
            .map_err(|_| uncertain())?;
            tx.execute(
                "UPDATE storage_meta SET binding=?1 WHERE singleton=1",
                params![new_binding],
            )
            .map_err(|_| uncertain())?;
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            if fault == Some(CommitFault::BeforeCommit) {
                return Err(uncertain());
            }
            tx.commit().map_err(|_| uncertain())?;
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            if fault == Some(CommitFault::AfterCommit) {
                return Err(uncertain());
            }
            Ok(())
        });
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if result.is_ok()
            && let Some(CommitAction::Clock(clock, seconds)) = action
        {
            clock.set_unix_seconds(seconds);
        }
        if result
            .as_ref()
            .is_err_and(|e| e.code == ErrorCode::StorageUncertain)
        {
            self.close(true);
        }
        result
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.close(false);
    }
}
struct Request {
    ticket: u64,
    cancellation: CancellationToken,
    op: DbOp,
}
struct Pending<T> {
    reply: oneshot::Receiver<R<T>>,
    ticket: u64,
    cancellation: CancellationToken,
}
enum DbOp {
    Message(Identity, MessageDbOp, oneshot::Sender<R<MessageDbReply>>),
    Load(oneshot::Sender<R<Option<StoredPlaintext>>>),
    Commit(StoredPlaintext, oneshot::Sender<R<()>>),
    DiscardPending(oneshot::Sender<R<()>>),
    Suspend(oneshot::Sender<R<()>>),
    Resume(oneshot::Sender<R<()>>),
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    Test(TestOp),
}
// Private persistence capability shared by the account actor and its lifecycle gate.
#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct StorageHandle {
    sender: mpsc::Sender<Request>,
    fence: Arc<Mutex<Fence>>,
    pub(crate) counts: Arc<Counts>,
}
#[allow(dead_code)]
impl StorageHandle {
    pub(crate) async fn message(&self, identity: Identity, op: MessageDbOp) -> R<MessageDbReply> {
        self.receive(self.send(|reply| DbOp::Message(identity, op, reply), false)?)
            .await
    }
    /// Caller must use a blocking thread/pool; initialization does SQL, OS key work and fsync.
    pub(crate) fn open_blocking(
        root: PathBuf,
        protector: Arc<dyn KeyProtector>,
    ) -> R<(Self, Option<StoredPlaintext>)> {
        let migration::OpenedStore {
            connection: conn,
            secrets,
            ownership,
            initial,
        } = migration::open_or_migrate(&root, &*protector)?;
        let id = secrets.db_id;
        let counts = Arc::new(Counts::default());
        if conn.is_some() {
            counts.opened();
        }
        let worker = Worker {
            ownership,
            protector,
            id,
            state: conn.map(State::Open).unwrap_or(State::Closed),
            pending_message: None,
            counts: counts.clone(),
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            hook: None,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            message_fault: None,
            #[cfg(all(feature="test-hooks",debug_assertions))]
            message_clock: None,
            #[cfg(all(feature="test-hooks",debug_assertions))]
            message_reply_pause: None,
        };
        let (sender, receiver) = mpsc::channel(32);
        let fence = Arc::new(Mutex::new(Fence {
            ticket: 0,
            blocked: false,
            stale_ticket: false,
            cancellation: CancellationToken::new(),
        }));
        let shared = fence.clone();
        std::thread::Builder::new()
            .name("chat-storage-writer".into())
            .spawn(move || run(worker, receiver, shared))
            .map_err(|_| CoreError::new(ErrorCode::NotAvailable, "storage_worker_unavailable"))?;
        Ok((
            Self {
                sender,
                fence,
                counts,
            },
            initial,
        ))
    }
    fn send<T>(
        &self,
        op: impl FnOnce(oneshot::Sender<R<T>>) -> DbOp,
        lifecycle: bool,
    ) -> R<Pending<T>> {
        let mut guard = self.fence.lock().map_err(|_| locked())?;
        if guard.blocked && !lifecycle {
            return Err(locked());
        }
        if lifecycle {
            // Both transitions supersede prior operations and pending completions.
            // Fence, capture and enqueue under one lock; never change network authority.
            guard.cancellation.cancel();
            guard.cancellation = CancellationToken::new();
            guard.blocked = true;
        }
        let (reply, rx) = oneshot::channel();
        self.sender
            .try_send(Request {
                ticket: guard.ticket,
                cancellation: guard.cancellation.clone(),
                op: op(reply),
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => {
                    CoreError::new(ErrorCode::Busy, "storage_queue_full")
                }
                mpsc::error::TrySendError::Closed(_) => locked(),
            })?;
        Ok(Pending {
            reply: rx,
            ticket: guard.ticket,
            cancellation: guard.cancellation.clone(),
        })
    }
    async fn receive<T>(&self, pending: Pending<T>) -> R<T> {
        let result = pending.reply.await.map_err(|_| locked())?;
        let f = self.fence.lock().map_err(|_| locked())?;
        if pending.cancellation.is_cancelled() || f.ticket != pending.ticket {
            return Err(locked());
        }
        result
    }
    pub(crate) async fn load_account(&self) -> R<Option<StoredPlaintext>> {
        self.receive(self.send(DbOp::Load, false)?).await
    }
    pub(crate) async fn commit_account(&self, state: StoredPlaintext) -> R<()> {
        self.receive(self.send(|reply| DbOp::Commit(state, reply), false)?)
            .await
    }
    /// Only never-active state may be cleared. SQL identity/keys/mode remain.
    pub(crate) async fn discard_pending(&self) -> R<()> {
        self.receive(self.send(DbOp::DiscardPending, false)?).await
    }
    pub(crate) fn invalidate(&self, ticket: u64) {
        if let Ok(mut f) = self.fence.lock() {
            f.cancellation.cancel();
            f.cancellation = CancellationToken::new();
            f.blocked = true;
            f.stale_ticket = ticket < f.ticket;
            if !f.stale_ticket {
                f.ticket = ticket;
            }
        }
    }
    pub(crate) async fn suspend(&self) -> R<()> {
        self.receive(self.send(DbOp::Suspend, true)?).await
    }
    pub(crate) async fn resume(&self) -> R<()> {
        self.receive(self.send(DbOp::Resume, true)?).await
    }
}
fn run(mut w: Worker, mut rx: mpsc::Receiver<Request>, fence: Arc<Mutex<Fence>>) {
    while let Some(request) = rx.blocking_recv() {
        let valid = || {
            !request.cancellation.is_cancelled()
                && fence.lock().is_ok_and(|f| f.ticket == request.ticket)
        };
        match request.op {
            DbOp::Message(identity, op, reply) => {
                let result = if valid() && fence.lock().is_ok_and(|f| !f.blocked) {
                    w.message(identity, op)
                } else {
                    Err(locked())
                };
                let _ = reply.send(if valid() { result } else { Err(locked()) });
            }
            DbOp::Load(reply) => {
                let id = w.id;
                let result = if valid() {
                    w.operation(|c| load(c, &id))
                } else {
                    Err(locked())
                };
                let _ = reply.send(if valid() { result } else { Err(locked()) });
            }
            DbOp::Commit(state, reply) => {
                let allowed = fence.lock().is_ok_and(|f| {
                    f.ticket == request.ticket && !request.cancellation.is_cancelled() && !f.blocked
                });
                let result = if allowed {
                    w.commit(state)
                } else {
                    Err(locked())
                };
                let _ = reply.send(if valid() { result } else { Err(locked()) });
            }
            DbOp::DiscardPending(reply) => {
                let allowed = fence.lock().is_ok_and(|f| {
                    f.ticket == request.ticket && !request.cancellation.is_cancelled() && !f.blocked
                });
                let result = if allowed {
                    w.discard_pending()
                } else {
                    Err(locked())
                };
                let _ = reply.send(if valid() { result } else { Err(locked()) });
            }
            DbOp::Suspend(reply) => {
                w.close(false);
                w.ownership.suspend();
                let _ = reply.send(if valid() { Ok(()) } else { Err(locked()) });
            }
            DbOp::Resume(reply) => {
                w.close(false);
                let result = if valid() && fence.lock().is_ok_and(|f| !f.stale_ticket) {
                    w.ownership.resume().and_then(|()| w.operation(|_| Ok(())))
                } else {
                    Err(locked())
                };
                let result = match fence.lock() {
                    Ok(mut f)
                        if f.ticket == request.ticket
                            && !request.cancellation.is_cancelled()
                            && !f.stale_ticket =>
                    {
                        if result.is_ok() {
                            f.blocked = false;
                        }
                        result
                    }
                    _ => Err(locked()),
                };
                if result.is_err() {
                    w.close(false);
                    w.ownership.suspend();
                }
                let _ = reply.send(result);
            }
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            DbOp::Test(op) => match op {
                #[cfg(test)]
                TestOp::MessageCounts(reply) => {
                    let result = w.operation(|c| {
                        c.query_row(
                            "SELECT (SELECT count(*) FROM conversation), \
                             (SELECT count(*) FROM message), (SELECT count(*) FROM outbox), \
                             message_revision FROM storage_meta WHERE singleton=1",
                            [],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                        )
                        .map_err(|_| corrupt())
                    });
                    let _ = reply.send(result);
                }
                TestOp::Message(op) => {
                    let _ = w.operation(|c| {
                        messages::fixture(c, op);
                        Ok(())
                    });
                }
                TestOp::MessageReplyPause(entered,release,reply)=>{w.message_reply_pause=Some((entered,release));let _=reply.send(());}
                TestOp::MessageClock(clock,seconds,reply)=>{w.message_clock=Some((clock,seconds));let _=reply.send(());}
                TestOp::MessageFault(fault, reply) => {
                    w.message_fault = Some(fault);
                    let _ = reply.send(());
                }
                TestOp::Pause(entered, release) => {
                    let _ = entered.send(());
                    let _ = release.recv();
                }
                TestOp::Fault(fault, reply) => {
                    w.hook = Some((1, CommitAction::Fault(fault)));
                    let _ = reply.send(());
                }
                TestOp::CommitAction(after, action, reply) => {
                    assert!(after > 0);
                    w.hook = Some((after, action));
                    let _ = reply.send(());
                }
                TestOp::Rows(reply) => {
                    let result = w.operation(|c| {
                        c.query_row("SELECT count(*) FROM account_state", [], |r| {
                            r.get::<_, i64>(0)
                        })
                        .map(|n| n as usize)
                        .map_err(|_| corrupt())
                    });
                    let _ = reply.send(result);
                }
                TestOp::Stop(reply) => {
                    w.close(false);
                    drop(w);
                    let _ = reply.send(());
                    return;
                }
            },
        }
    }
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommitFault {
    WriteDenied,
    BeforeCommit,
    AfterCommit,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(crate) enum CommitAction {
    Fault(CommitFault),
    Clock(Arc<crate::account::clock::FakeClock>, u64),
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(crate) enum TestOp {
    #[cfg(test)]
    MessageCounts(oneshot::Sender<R<(i64, i64, i64, i64)>>),
    Message(messages::FixtureOp),
    MessageFault(CommitFault, oneshot::Sender<()>),
    MessageReplyPause(oneshot::Sender<()>,std::sync::mpsc::Receiver<()>,oneshot::Sender<()>),
    MessageClock(Arc<crate::account::clock::FakeClock>,u64,oneshot::Sender<()>),
    Pause(oneshot::Sender<()>, std::sync::mpsc::Receiver<()>),
    Fault(CommitFault, oneshot::Sender<()>),
    CommitAction(usize, CommitAction, oneshot::Sender<()>),
    Rows(oneshot::Sender<R<usize>>),
    Stop(oneshot::Sender<()>),
}

#[cfg(all(test, feature = "test-hooks", debug_assertions))]
mod tests {
    use super::*;
    use crate::account::{PeerId, Secret32, StoreId, derive_account, derive_device};
    use crate::message::types::MessageId;

    struct Protector;
    impl KeyProtector for Protector {
        fn protect(&self, id: StoreId, secret: &Secret32) -> R<Vec<u8>> {
            let mut blob = id.0.to_vec();
            blob.extend(secret.as_bytes().iter().map(|b| b ^ 0xa7));
            Ok(blob)
        }
        fn unprotect(&self, id: StoreId, blob: &[u8]) -> R<Secret32> {
            if blob.len() != 48 || blob[..16] != id.0 {
                return Err(CoreError::new(
                    ErrorCode::StorageMissing,
                    "fixture_key_missing",
                ));
            }
            Ok(Secret32::from(std::array::from_fn(|i| blob[16 + i] ^ 0xa7)))
        }
        fn delete(&self, _: StoreId) -> R<()> {
            Ok(())
        }
    }

    struct OwnedStore {
        handle: StorageHandle,
        _root: tempfile::TempDir,
    }
    impl Drop for OwnedStore {
        fn drop(&mut self) {
            let handle = self.handle.clone();
            // Join real worker teardown before the owned root, even on assertion unwind.
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(async move {
                        let (tx, rx) = oneshot::channel();
                        handle.test(TestOp::Stop(tx)).await;
                        rx.await.unwrap();
                    });
            })
            .join()
            .unwrap();
        }
    }
    impl OwnedStore {
        async fn open() -> Self {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().to_owned();
            let (handle, initial) = tokio::task::spawn_blocking(move || {
                StorageHandle::open_blocking(path, Arc::new(Protector))
            })
            .await
            .unwrap()
            .unwrap();
            let owned = Self {
                handle,
                _root: root,
            };
            assert!(initial.is_none());
            owned
        }
        async fn counts(&self) -> (i64, i64, i64, i64) {
            let (tx, rx) = oneshot::channel();
            self.handle.test(TestOp::MessageCounts(tx)).await;
            rx.await.unwrap().unwrap()
        }
    }

    enum SqlBinding {
        Unconfigured,
        WrongPeer,
        WrongInstallation,
    }
    async fn check_restricted_binding(binding: SqlBinding) {
        let store = OwnedStore::open().await;
        let stored = crate::account::storage::migration_fixtures::migration_states()
            .pop()
            .unwrap()
            .unwrap();
        let AccountState::Active(local) = AccountState::decode(&stored).unwrap() else {
            unreachable!()
        };
        let mut captured = Identity {
            peer: local.keys.peer_id(),
            installation: local.device.installation_id(),
        };
        match binding {
            SqlBinding::Unconfigured => {}
            SqlBinding::WrongPeer => {
                store.handle.commit_account(stored).await.unwrap();
                captured.peer = derive_account(&Secret32::from([91; 32])).unwrap().peer_id();
                assert_ne!(captured.peer, local.keys.peer_id());
            }
            SqlBinding::WrongInstallation => {
                store.handle.commit_account(stored).await.unwrap();
                captured.installation = derive_device(Secret32::from([92; 32]))
                    .unwrap()
                    .installation_id();
                assert_ne!(captured.installation, local.device.installation_id());
            }
        }
        let before = store.counts().await;
        let result = store
            .handle
            .message(
                captured,
                MessageDbOp::Enqueue(
                    PeerId([2; 32]),
                    MessageId([1; 16]),
                    "unreturned fixture body".into(),
                    1,
                ),
            )
            .await;
        let private_result = result.is_ok();
        let code = result.err().map(|e| e.code);
        let after = store.counts().await;
        assert!(
            !private_result,
            "restricted worker returned a private message result"
        );
        assert_eq!(code, Some(ErrorCode::AccountRestricted));
        assert_eq!(before, (0, 0, 0, 0));
        assert_eq!(
            after, before,
            "restricted worker changed conversation/message/outbox/revision"
        );
    }
    #[tokio::test]
    async fn message_identity_unconfigured_sql_rejects_without_mutation() {
        check_restricted_binding(SqlBinding::Unconfigured).await;
    }
    #[tokio::test]
    async fn message_identity_wrong_peer_rejects_without_mutation() {
        check_restricted_binding(SqlBinding::WrongPeer).await;
    }
    #[tokio::test]
    async fn message_identity_wrong_installation_rejects_without_mutation() {
        check_restricted_binding(SqlBinding::WrongInstallation).await;
    }
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
impl StorageHandle {
    pub(crate) async fn test(&self, op: TestOp) {
        let (ticket, cancellation) = {
            let f = self.fence.lock().unwrap();
            (f.ticket, f.cancellation.clone())
        };
        self.sender
            .send(Request {
                ticket,
                cancellation,
                op: DbOp::Test(op),
            })
            .await
            .unwrap_or_else(|_| panic!("fixture_worker_closed"));
    }
    pub(crate) fn accepted_ticket(&self) -> u64 {
        self.fence.lock().unwrap().ticket
    }
    pub(crate) fn capacity(&self) -> usize {
        self.sender.capacity()
    }
}
