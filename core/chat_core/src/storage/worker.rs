//! One bounded, typed writer. SQL and OS protection run only on blocking threads.
use super::migration::{self, binding, load};
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
    counts: Arc<Counts>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    fault: Option<CommitFault>,
}
impl Worker {
    fn discard_pending(&mut self) -> R<()> {
        let id = self.id;
        let result = self.operation(|conn| {
            if let Some(stored) = load(conn, &id)? {
                if matches!(AccountState::decode(&stored)?, AccountState::Active(_)) {
                    return Err(CoreError::new(
                        ErrorCode::AccountExists,
                        "active_binding_immutable",
                    ));
                }
            }
            let tx = conn.transaction().map_err(|_| uncertain())?;
            tx.execute("DELETE FROM account_state WHERE singleton=1", [])
                .map_err(|_| uncertain())?;
            tx.execute(
                "UPDATE storage_meta SET binding=?1 WHERE singleton=1",
                params![binding(None)?],
            )
            .map_err(|_| uncertain())?;
            tx.commit().map_err(|_| uncertain())
        });
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
        let fault = self.fault.take();
        let id = self.id;
        let result = self.operation(|conn| {
            let old = load(conn, &id)?;
            if let Some(old) = old {
                let previous = binding(Some(&old))?;
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
    Load(oneshot::Sender<R<Option<StoredPlaintext>>>),
    Commit(StoredPlaintext, oneshot::Sender<R<()>>),
    DiscardPending(oneshot::Sender<R<()>>),
    Suspend(oneshot::Sender<R<()>>),
    Resume(oneshot::Sender<R<()>>),
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    Test(TestOp),
}
// Interim until T4 wires the private capability into the account actor.
#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct StorageHandle {
    sender: mpsc::Sender<Request>,
    fence: Arc<Mutex<Fence>>,
    pub(crate) counts: Arc<Counts>,
}
#[allow(dead_code)]
impl StorageHandle {
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
            counts: counts.clone(),
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            fault: None,
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
                TestOp::Pause(entered, release) => {
                    let _ = entered.send(());
                    let _ = release.recv();
                }
                TestOp::Fault(fault, reply) => {
                    w.fault = Some(fault);
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
pub(crate) enum TestOp {
    Pause(oneshot::Sender<()>, std::sync::mpsc::Receiver<()>),
    Fault(CommitFault, oneshot::Sender<()>),
    Rows(oneshot::Sender<R<usize>>),
    Stop(oneshot::Sender<()>),
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
