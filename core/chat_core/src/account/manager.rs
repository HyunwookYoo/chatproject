//! Single owner of protected account state. A5 extends commands and AccountState.
use super::{
    FlowId, InstallationCert, InstallationId, PeerId, RecoveryDisplay,
    creation::Creation,
    derive_device, random_secret, recovery,
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
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPhase {
    Unconfigured,
    Candidate,
    PendingImport,
    LocalReady,
    PublishPending,
    Active,
    Conflict,
    Locked,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishIntent {
    LocalCreate,
    Replacement,
}
/// Public data only. No recovery display or private keys can enter this view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountView {
    pub revision: u64,
    pub phase: AccountPhase,
    pub public_peer_id: Option<PeerId>,
    pub installation: Option<InstallationId>,
    pub devices: Vec<InstallationCert>,
    pub contacts: Vec<PeerId>,
    pub pair: Option<FlowId>,
    pub publish_intents: Vec<PublishIntent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateInfo {
    pub installation_id: InstallationId,
    pub endpoint_id: String,
}
enum Command {
    Prepare(oneshot::Sender<Result<CandidateInfo, CoreError>>),
    Begin(oneshot::Sender<Result<RecoveryDisplay, CoreError>>),
    Confirm(
        FlowId,
        Zeroizing<[String; 3]>,
        oneshot::Sender<Result<(), CoreError>>,
    ),
    Cancel(FlowId, oneshot::Sender<Result<(), CoreError>>),
    Recover(
        Zeroizing<String>,
        bool,
        oneshot::Sender<Result<(), CoreError>>,
    ),
    Snapshot(oneshot::Sender<Result<AccountView, CoreError>>),
}
struct Queued {
    ticket: u64,
    command: Command,
}
impl Command {
    fn reject(self, error: CoreError) {
        match self {
            Self::Prepare(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Begin(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Confirm(_, _, reply) | Self::Cancel(_, reply) | Self::Recover(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Snapshot(reply) => {
                let _ = reply.send(Err(error));
            }
        }
    }
}
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
    commands: mpsc::Sender<Queued>,
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
        let revision = match &state {
            Some(AccountState::Active(local)) => local.revision,
            Some(AccountState::Candidate(_)) => 1,
            None => 0,
        };
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
                creation: None,
                revision,
                shared: shared.clone(),
            }
            .run(input, received),
        );
        Ok(Self { commands, shared })
    }
    /// Persists the inactive installation before binding its Endpoint. No account
    /// membership, identity, certificate, or QR authorization is implied.
    pub async fn prepare_candidate(&self) -> Result<CandidateInfo, CoreError> {
        self.send(Command::Prepare).await
    }
    async fn send<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T, CoreError>>) -> Command,
    ) -> Result<T, CoreError> {
        let (reply, result) = oneshot::channel();
        let ticket = self.shared.requests.borrow().ticket;
        self.commands
            .try_send(Queued {
                ticket,
                command: command(reply),
            })
            .map_err(|_| unavailable(ErrorCode::Busy))?;
        result
            .await
            .map_err(|_| unavailable(ErrorCode::NotAvailable))?
    }
    pub async fn begin_create(&self) -> Result<RecoveryDisplay, CoreError> {
        self.send(Command::Begin).await
    }
    pub async fn confirm_create(
        &self,
        flow: FlowId,
        answers: [String; 3],
    ) -> Result<(), CoreError> {
        let answers = Zeroizing::new(answers);
        self.send(|reply| Command::Confirm(flow, answers, reply))
            .await
    }
    pub async fn cancel_create(&self, flow: FlowId) -> Result<(), CoreError> {
        self.send(|reply| Command::Cancel(flow, reply)).await
    }
    pub async fn recover(&self, words: String, confirm_replacement: bool) -> Result<(), CoreError> {
        let words = Zeroizing::new(words);
        self.send(|reply| Command::Recover(words, confirm_replacement, reply))
            .await
    }
    pub async fn snapshot(&self) -> Result<AccountView, CoreError> {
        self.send(Command::Snapshot).await
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
    creation: Option<Creation>,
    revision: u64,
    shared: Arc<Shared>,
}
impl Manager {
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Queued>,
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
                command = commands.recv() => {
                    let Some(queued) = command else { break; };
                    if !matches!(queued.command, Command::Snapshot(_)) && queued.ticket != self.shared.requests.borrow().ticket {
                        queued.command.reject(unavailable(ErrorCode::StorageLocked));
                        continue;
                    }
                    match queued.command {
                        Command::Prepare(reply) => { let result = self.prepare().await; let _ = reply.send(result); }
                        Command::Begin(reply) => { let result = self.begin(); let _ = reply.send(result); }
                        Command::Confirm(flow, answers, reply) => { let result = self.confirm(flow, &answers).await; let _ = reply.send(result); }
                        Command::Cancel(flow, reply) => { let result = self.cancel(flow); let _ = reply.send(result); }
                        Command::Recover(words, confirmed, reply) => { let result = self.recover(&words, confirmed).await; let _ = reply.send(result); }
                        Command::Snapshot(reply) => { let _ = reply.send(Ok(self.view())); }
                    }
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
        if self.creation.take().is_some() {
            self.revision += 1;
        }
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
            Ok(state) => {
                if let Some(AccountState::Active(local)) = &state {
                    self.revision = self.revision.max(local.revision);
                }
                self.state = state;
            }
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
        let request = self.ready()?;
        self.allow_setup()?;
        if self.creation.is_some() {
            return Err(unavailable(ErrorCode::Busy));
        }
        if self.state.is_none() {
            let state = AccountState::Candidate(derive_device(random_secret()?)?);
            self.ensure_ready(request)?;
            self.persist(&state, request)?;
            self.state = Some(state);
            self.revision += 1;
        }
        self.ensure_ready(request)?;
        let device = self.state.as_ref().unwrap().device();
        let candidate = CandidateInfo {
            installation_id: device.installation_id(),
            endpoint_id: iroh::SecretKey::from_bytes(device.transport().as_bytes())
                .public()
                .to_string(),
        };
        let key = iroh::SecretKey::from_bytes(device.transport().as_bytes());
        self.shared.network.activate_key(key).await?;
        self.ensure_ready(request)?;
        Ok(candidate)
    }
    fn ready(&self) -> Result<Request, CoreError> {
        let request = *self.shared.requests.borrow();
        if request.suspend
            || self.shared.network.desired_suspend()
            || self.shared.status.load(Ordering::Acquire) != (request.ticket << 2) | 1
        {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        Ok(request)
    }
    fn ensure_ready(&self, request: Request) -> Result<(), CoreError> {
        if self.ready()?.ticket != request.ticket {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        Ok(())
    }
    fn allow_setup(&self) -> Result<(), CoreError> {
        if matches!(self.state, Some(AccountState::Active(_))) {
            return Err(CoreError::new(
                ErrorCode::AccountExists,
                "Account already exists; use a separate empty storage folder for recovery",
            ));
        }
        Ok(())
    }
    fn begin(&mut self) -> Result<RecoveryDisplay, CoreError> {
        let request = self.ready()?;
        self.allow_setup()?;
        let (creation, display) = Creation::begin()?;
        self.ensure_ready(request)?;
        self.creation = Some(creation);
        self.revision += 1;
        Ok(display)
    }
    fn cancel(&mut self, flow: FlowId) -> Result<(), CoreError> {
        self.ready()?;
        self.allow_setup()?;
        if self.creation.as_ref().map(|c| c.flow) != Some(flow) {
            return Err(unavailable(ErrorCode::InvalidRecovery));
        }
        self.creation = None;
        self.revision += 1;
        Ok(())
    }
    async fn confirm(&mut self, flow: FlowId, answers: &[String; 3]) -> Result<(), CoreError> {
        let request = self.ready()?;
        self.allow_setup()?;
        if !self
            .creation
            .as_ref()
            .is_some_and(|c| c.matches(flow, answers))
        {
            return Err(unavailable(ErrorCode::InvalidRecovery));
        }
        let creation = self.creation.take().unwrap();
        let local =
            recovery::local_account(creation.keys, PublishIntent::LocalCreate, self.revision + 1)?;
        self.establish(local, request).await
    }
    async fn recover(&mut self, words: &str, confirmed: bool) -> Result<(), CoreError> {
        let request = self.ready()?;
        self.allow_setup()?;
        if !confirmed {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        let local = recovery::recover(words, self.revision + 1)?;
        self.establish(local, request).await
    }
    async fn establish(
        &mut self,
        local: super::storage::schema::LocalAccount,
        request: Request,
    ) -> Result<(), CoreError> {
        self.ensure_ready(request)?;
        let state = AccountState::Active(Box::new(local));
        self.persist(&state, request)?;
        let AccountState::Active(local) = &state else {
            unreachable!()
        };
        self.revision = local.revision;
        self.state = Some(state);
        self.creation = None;
        self.ensure_ready(request)?;
        let device = self.state.as_ref().unwrap().device();
        self.shared
            .network
            .activate_key(iroh::SecretKey::from_bytes(device.transport().as_bytes()))
            .await?;
        self.ensure_ready(request)
    }
    fn persist(&mut self, state: &AccountState, request: Request) -> Result<(), CoreError> {
        self.ensure_ready(request)?;
        if let Err(error) = self.store.commit(&state.encode()) {
            // An error can follow atomic replacement. Do not trust the old
            // in-memory state or permit another setup to overwrite the new one.
            // A new suspend/resume cycle must reload and validate the actual disk.
            self.shared.network.block();
            self.shared
                .status
                .fetch_max((request.ticket << 2) | 2, Ordering::AcqRel);
            return Err(error);
        }
        Ok(())
    }
    fn view(&self) -> AccountView {
        let mut view = AccountView {
            revision: self.revision,
            phase: AccountPhase::Unconfigured,
            public_peer_id: None,
            installation: None,
            devices: vec![],
            contacts: vec![],
            pair: None,
            publish_intents: vec![],
        };
        if self.ready().is_err() {
            view.phase = AccountPhase::Locked;
            return view;
        }
        match &self.state {
            Some(AccountState::Candidate(device)) => {
                view.phase = AccountPhase::Candidate;
                view.installation = Some(device.installation_id());
            }
            Some(AccountState::Active(local)) => {
                view.revision = local.revision;
                view.phase = match local.intent {
                    PublishIntent::LocalCreate => AccountPhase::LocalReady,
                    PublishIntent::Replacement => AccountPhase::PublishPending,
                };
                view.public_peer_id = Some(local.keys.peer_id());
                view.installation = Some(local.device.installation_id());
                view.devices.push(local.cert.clone());
                view.publish_intents.push(local.intent);
            }
            None if self.creation.is_some() => view.phase = AccountPhase::Candidate,
            None => {}
        }
        view
    }
}

#[cfg(all(test, feature = "test-hooks", debug_assertions))]
mod tests {
    use super::*;
    use crate::account::{Secret32, StoreId};
    use crate::{CoreConfig, CoreSlot, EventBus, NetworkOp, NetworkReply, NetworkState};

    struct Protector;
    impl KeyProtector for Protector {
        fn protect(&self, _: StoreId, secret: &Secret32) -> Result<Vec<u8>, CoreError> {
            Ok(secret.as_bytes().iter().map(|byte| byte ^ 0xa7).collect())
        }
        fn unprotect(&self, _: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
            assert_eq!(blob.len(), 32);
            Ok(Secret32::from(std::array::from_fn(|i| blob[i] ^ 0xa7)))
        }
        fn delete(&self, _: StoreId) -> Result<(), CoreError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn completed_replace_error_cannot_overwrite_established_account() {
        let dir = tempfile::tempdir().unwrap();
        let slot = CoreSlot::new();
        slot.start(
            CoreConfig {
                data_dir: dir.path().into(),
            },
            &EventBus::new(),
        )
        .unwrap();
        let network = slot.network().unwrap();
        network.configure_local(None).await.unwrap();
        let ticket = network.begin_account();
        let request = Request {
            ticket,
            suspend: false,
        };
        let (requests, _) = watch::channel(request);
        let shared = Arc::new(Shared {
            network: network.clone(),
            requests,
            status: AtomicU64::new(ticket << 2),
        });
        let mut manager = Manager {
            store: Store::open(dir.path(), Arc::new(Protector)).unwrap(),
            state: None,
            creation: None,
            revision: 0,
            shared,
        };
        let restored = manager.lifecycle(request).await;
        manager.complete(request, restored);
        manager.prepare().await.unwrap();
        let local = recovery::recover(
            &super::super::encode_recovery(&Secret32::from([0; 32])).unwrap(),
            2,
        )
        .unwrap();
        let peer = local.keys.peer_id();
        let state = AccountState::Active(Box::new(local));
        let result =
            super::super::storage::with_after_replace_failure(|| manager.persist(&state, request));
        assert_eq!(result.unwrap_err().code, ErrorCode::StorageFull);
        assert_eq!(manager.view().phase, AccountPhase::Locked);
        assert_eq!(
            manager.begin().err().unwrap().code,
            ErrorCode::StorageLocked
        );
        assert_eq!(
            manager.recover("invalid", true).await.unwrap_err().code,
            ErrorCode::StorageLocked
        );
        let bytes = std::fs::read(dir.path().join("account.bin")).unwrap();
        // The call returned Err, but atomic replacement really committed Active.
        let AccountState::Active(saved) =
            AccountState::decode(&manager.store.load().unwrap().unwrap()).unwrap()
        else {
            panic!()
        };
        assert_eq!(saved.keys.peer_id(), peer);
        for suspend in [true, false] {
            let ticket = network.block();
            let request = Request { ticket, suspend };
            manager.shared.requests.send_replace(request);
            manager.shared.status.store(ticket << 2, Ordering::Release);
            let result = manager.lifecycle(request).await;
            manager.complete(request, result);
        }
        assert_eq!(
            manager.begin().err().unwrap().code,
            ErrorCode::AccountExists
        );
        assert_eq!(
            manager.recover("invalid", true).await.unwrap_err().code,
            ErrorCode::AccountExists
        );
        assert_eq!(
            std::fs::read(dir.path().join("account.bin")).unwrap(),
            bytes
        );
        assert_eq!(manager.view().public_peer_id, Some(peer));
        network.execute(NetworkOp::Stop).await.unwrap();
    }

    #[tokio::test]
    async fn persisted_never_active_candidate_can_change_key_in_same_core() {
        let dir = tempfile::tempdir().unwrap();
        let slot = CoreSlot::new();
        slot.start(
            CoreConfig {
                data_dir: dir.path().into(),
            },
            &EventBus::new(),
        )
        .unwrap();
        let network = slot.network().unwrap();
        network.configure_local(None).await.unwrap();
        let ticket = network.begin_account();
        let request = Request {
            ticket,
            suspend: false,
        };
        let (requests, _) = watch::channel(request);
        let shared = Arc::new(Shared {
            network: network.clone(),
            requests,
            status: AtomicU64::new(ticket << 2),
        });
        let mut manager = Manager {
            store: Store::open(dir.path(), Arc::new(Protector)).unwrap(),
            state: None,
            creation: None,
            revision: 0,
            shared,
        };
        let restored = manager.lifecycle(request).await;
        manager.complete(request, restored);
        let first = manager.prepare().await.unwrap();
        let persisted = manager.store.load().unwrap().unwrap();
        assert_eq!(
            AccountState::decode(&persisted)
                .unwrap()
                .device()
                .installation_id(),
            first.installation_id
        );

        let remote = NetworkHandle::spawn(
            &tokio::runtime::Handle::current(),
            crate::transport::NetworkProfile::Local { relay: None },
            EventBus::new(),
        );
        remote.execute(NetworkOp::Start).await.unwrap();
        let remote_info = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let NetworkReply::Info(info) = remote.execute(NetworkOp::Info).await.unwrap()
                else {
                    panic!()
                };
                if info.state == NetworkState::Online {
                    break info;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        network
            .execute(NetworkOp::Connect {
                target: remote_info.ticket.unwrap(),
            })
            .await
            .unwrap();
        let instance = slot.info().unwrap().instance_id;

        // Fixture represents only the future actor's authorized never-active
        // decision. A5 must test actual recovery policy and fresh DeviceSeed.
        // Use the same real Store/handler/network; no second writer or public reset.
        let replacement = AccountState::Candidate(derive_device(random_secret().unwrap()).unwrap());
        manager.store.commit(&replacement.encode()).unwrap();
        manager.state = Some(replacement);
        let second = manager.prepare().await.unwrap();
        assert_ne!(second.endpoint_id, first.endpoint_id);
        assert_ne!(second.installation_id, first.installation_id);
        assert_eq!(slot.info().unwrap().instance_id, instance);
        let persisted = manager.store.load().unwrap().unwrap();
        assert_eq!(
            AccountState::decode(&persisted)
                .unwrap()
                .device()
                .installation_id(),
            second.installation_id
        );
        let NetworkReply::Info(info) = network.execute(NetworkOp::Info).await.unwrap() else {
            panic!()
        };
        assert_eq!(info.state, NetworkState::Online);
        assert_eq!(info.endpoint_id, Some(second.endpoint_id));
        assert!(info.peers.is_empty());
        assert!(
            network
                .execute(NetworkOp::Send {
                    peer_id: remote_info.endpoint_id.unwrap(),
                    text: "old session".into()
                })
                .await
                .is_err()
        );
        network.execute(NetworkOp::Stop).await.unwrap();
        remote.execute(NetworkOp::Stop).await.unwrap();
    }
}
