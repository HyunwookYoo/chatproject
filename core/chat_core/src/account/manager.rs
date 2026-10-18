//! Single owner of protected account state. A5 extends commands and AccountState.
use super::{
    InstallationId, derive_device, random_secret,
    storage::{KeyProtector, Store, schema::AccountState},
};
use crate::{CoreError, ErrorCode, NetworkHandle};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateInfo {
    pub installation_id: InstallationId,
    pub endpoint_id: String,
}
struct Prepare(oneshot::Sender<Result<CandidateInfo, CoreError>>);
#[derive(Clone, Copy)]
struct Request {
    ticket: u64,
    suspend: bool,
}
struct Shared {
    network: NetworkHandle,
    requests: watch::Sender<Request>,
    // ticket << 2 | 0 pending / 1 complete / 2 failed. Monotonic revisions
    // prevent an obsolete completion from reporting success for a newer request.
    status: AtomicU64,
}
#[derive(Clone)]
pub struct AccountHandle {
    commands: mpsc::Sender<Prepare>,
    shared: Arc<Shared>,
}
fn unavailable(code: ErrorCode) -> CoreError {
    CoreError::new(code, "Account operation unavailable")
}

impl AccountHandle {
    pub(crate) fn open(
        runtime: &tokio::runtime::Handle,
        root: &Path,
        protector: Arc<dyn KeyProtector>,
        network: NetworkHandle,
    ) -> Result<Self, CoreError> {
        network.begin_account();
        let mut store = Store::open(root, protector)?;
        let state = store
            .load()?
            .as_ref()
            .map(AccountState::decode)
            .transpose()?;
        let ticket = network.block();
        let (requests, received) = watch::channel(Request {
            ticket,
            suspend: network.desired_suspend(),
        });
        let shared = Arc::new(Shared {
            network,
            requests,
            status: AtomicU64::new(ticket << 2),
        });
        let (commands, input) = mpsc::channel(8);
        runtime.spawn(
            Manager {
                store,
                state,
                shared: shared.clone(),
            }
            .run(input, received),
        );
        Ok(Self { commands, shared })
    }
    /// Persists the inactive installation before binding its Endpoint. No account
    /// membership, identity, certificate, or QR authorization is implied.
    pub async fn prepare_candidate(&self) -> Result<CandidateInfo, CoreError> {
        if self.shared.requests.borrow().suspend || self.lifecycle_status() == 2 {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        let (reply, result) = oneshot::channel();
        self.commands
            .try_send(Prepare(reply))
            .map_err(|_| unavailable(ErrorCode::Busy))?;
        result
            .await
            .map_err(|_| unavailable(ErrorCode::NotAvailable))?
    }
    /// ABI status: 0 completed, 1 pending, 2 failed. Never waits for file IO.
    pub fn lifecycle_status(&self) -> i32 {
        match self.shared.status.load(Ordering::Acquire) & 3 {
            1 => 0,
            2 => 2,
            _ => 1,
        }
    }
    pub fn request_suspend(&self) -> i32 {
        self.request(true)
    }
    pub fn request_resume(&self) -> i32 {
        self.request(false)
    }
    fn request(&self, suspend: bool) -> i32 {
        self.shared.network.lifecycle_gate(suspend);
        let current = *self.shared.requests.borrow();
        if current.suspend == suspend {
            return self.lifecycle_status();
        }
        let ticket = self.shared.network.block();
        self.shared.status.fetch_max(ticket << 2, Ordering::AcqRel);
        // Bounded latest-request mailbox; concurrent/rapid scene callbacks cannot
        // overwrite a newer request with an older one or fill an IO work queue.
        self.shared.requests.send_if_modified(|request| {
            if request.ticket >= ticket {
                return false;
            }
            *request = Request { ticket, suspend };
            true
        });
        1
    }
}
struct Manager {
    store: Store,
    state: Option<AccountState>,
    shared: Arc<Shared>,
}
impl Manager {
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Prepare>,
        mut requests: watch::Receiver<Request>,
    ) {
        let initial = *requests.borrow_and_update();
        let result = self.lifecycle(initial).await;
        self.complete(initial, result);
        loop {
            tokio::select! { biased;
                changed = requests.changed() => {
                    if changed.is_err() { break; }
                    let request = *requests.borrow_and_update();
                    let result = self.lifecycle(request).await;
                    self.complete(request, result);
                }
                command = commands.recv() => match command {
                    Some(Prepare(reply)) => { let result = self.prepare().await; let _ = reply.send(result); }
                    None => break,
                }
            }
        }
        let _ = self.store.suspend();
    }
    fn current(&self, request: Request) -> bool {
        self.shared.requests.borrow().ticket == request.ticket
    }
    fn complete(&self, request: Request, result: Result<(), CoreError>) {
        let status = if result.is_ok() { 1 } else { 2 };
        self.shared
            .status
            .fetch_max((request.ticket << 2) | status, Ordering::AcqRel);
        if result.is_ok()
            && !request.suspend
            && !self.shared.network.desired_suspend()
            && self.current(request)
        {
            self.shared.network.release(request.ticket);
        }
    }
    async fn restore(&mut self, request: Request) -> Result<(), CoreError> {
        if let Some(state) = &self.state {
            let key = iroh::SecretKey::from_bytes(state.device().transport().as_bytes());
            tokio::time::timeout(
                Duration::from_secs(2),
                self.shared.network.install_key(key, false, request.ticket),
            )
            .await
            .map_err(|_| unavailable(ErrorCode::NetworkTimeout))??;
        }
        Ok(())
    }
    async fn lifecycle(&mut self, request: Request) -> Result<(), CoreError> {
        // The caller already gated all new operations. Drain only for a bounded
        // time; pending/failure stays closed. Store locks never cross an await.
        let stopped =
            tokio::time::timeout(Duration::from_secs(2), self.shared.network.quiesce()).await;
        self.store.suspend()?;
        if request.suspend {
            return stopped.map_err(|_| unavailable(ErrorCode::NetworkTimeout))?;
        }
        stopped.map_err(|_| unavailable(ErrorCode::NetworkTimeout))??;
        if !self.current(request) {
            return Err(unavailable(ErrorCode::Busy));
        }
        self.store.resume()?;
        let loaded = self
            .store
            .load()
            .and_then(|state| state.as_ref().map(AccountState::decode).transpose());
        match loaded {
            Ok(state) => self.state = state,
            Err(error) => {
                self.store.suspend()?;
                return Err(error);
            }
        }
        let result = self.restore(request).await;
        if result.is_err() {
            self.store.suspend()?;
        }
        result
    }
    async fn prepare(&mut self) -> Result<CandidateInfo, CoreError> {
        let request = *self.shared.requests.borrow();
        if request.suspend
            || self.shared.network.desired_suspend()
            || self.shared.status.load(Ordering::Acquire) != (request.ticket << 2) | 1
        {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        if self.state.is_none() {
            let state = AccountState::Candidate(derive_device(random_secret()?)?);
            self.store.commit(&state.encode())?;
            self.state = Some(state);
        }
        if !self.current(request) {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        let device = self.state.as_ref().unwrap().device();
        let candidate = CandidateInfo {
            installation_id: device.installation_id(),
            endpoint_id: iroh::SecretKey::from_bytes(device.transport().as_bytes())
                .public()
                .to_string(),
        };
        let key = iroh::SecretKey::from_bytes(device.transport().as_bytes());
        self.shared.network.activate_key(key).await?;
        if !self.current(request) {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        Ok(candidate)
    }
}
