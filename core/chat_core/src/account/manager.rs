//! Single owner of protected account state. A5 extends commands and AccountState.
use super::{
    AccountServices, FlowId, InstallationCert, InstallationId, PeerId, PublicationState,
    PublicationView, RecoveryDisplay,
    creation::Creation,
    derive_device,
    publication::{Action, PacketStatus, RecordState},
    random_secret, recovery,
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
    pub publication: PublicationView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateInfo {
    pub installation_id: InstallationId,
    pub endpoint_id: String,
}
enum Command {
    Connect(PeerId, oneshot::Sender<Result<(), CoreError>>),
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
    Refresh(PeerId, oneshot::Sender<Result<(), CoreError>>),
    Revoke(InstallationId, oneshot::Sender<Result<(), CoreError>>),
    Resolve([u8; 32], oneshot::Sender<Result<(), CoreError>>),
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
            Self::Confirm(_, _, reply)
            | Self::Cancel(_, reply)
            | Self::Recover(_, _, reply)
            | Self::Connect(_, reply)
            | Self::Refresh(_, reply)
            | Self::Revoke(_, reply)
            | Self::Resolve(_, reply) => {
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
        mut services: AccountServices,
        bus: crate::EventBus,
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
        let auth = super::connection::Runtime::default();
        shared.network.register_account(auth.send.clone())?;
        if services.notifier.is_none() {
            services.notifier = Some(Arc::new(super::connection::Notifier {
                registry: auth.registry.clone(),
                clock: services.clock.clone(),
            }));
        }
        runtime.spawn(
            Manager {
                auth,
                bus,
                store,
                state,
                creation: None,
                revision,
                shared: shared.clone(),
                services,
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
    pub async fn connect_account(&self, peer: PeerId) -> Result<(), CoreError> {
        self.send(|reply| Command::Connect(peer, reply)).await
    }
    pub async fn refresh_record(&self, peer: PeerId) -> Result<(), CoreError> {
        self.send(|reply| Command::Refresh(peer, reply)).await
    }
    pub async fn revoke_device(&self, target: InstallationId) -> Result<(), CoreError> {
        self.send(|reply| Command::Revoke(target, reply)).await
    }
    pub async fn confirm_record_conflict(&self, base_hash: [u8; 32]) -> Result<(), CoreError> {
        self.send(|reply| Command::Resolve(base_hash, reply)).await
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
    auth: super::connection::Runtime,
    bus: crate::EventBus,
    store: Store,
    state: Option<AccountState>,
    creation: Option<Creation>,
    revision: u64,
    shared: Arc<Shared>,
    services: AccountServices,
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
        self.auto_refresh().await;
        let mut expiry = tokio::time::interval(Duration::from_millis(100));
        let mut refresh = tokio::time::interval(Duration::from_secs(60));
        refresh.tick().await;
        loop {
            tokio::select! { biased;
                changed = requests.changed() => {
                    if changed.is_err() { break; }
                    let request = *requests.borrow_and_update();
                    let result = self.lifecycle(request).await;
                    self.complete(request, result);
                    self.auto_refresh().await;
                }
                _ = expiry.tick() => { self.restrict_grants(); }
                Some(input) = self.auth.input.recv() => { self.auth_input(input).await; }
                _ = refresh.tick() => { self.auto_refresh().await; }
                command = commands.recv() => {
                    let Some(queued) = command else { break; };
                    if !matches!(queued.command, Command::Snapshot(_)) && queued.ticket != self.shared.requests.borrow().ticket {
                        queued.command.reject(unavailable(ErrorCode::StorageLocked));
                        continue;
                    }
                    match queued.command {
                        Command::Connect(peer,reply) => {let result=self.connect(peer).await;let _=reply.send(result);}
                        Command::Prepare(reply) => { let result = self.prepare().await; let _ = reply.send(result); }
                        Command::Begin(reply) => { let result = self.begin(); let _ = reply.send(result); }
                        Command::Confirm(flow, answers, reply) => { let result = self.confirm(flow, &answers).await; let _ = reply.send(result); }
                        Command::Cancel(flow, reply) => { let result = self.cancel(flow); let _ = reply.send(result); }
                        Command::Recover(words, confirmed, reply) => { let result = self.recover(&words, confirmed).await; let _ = reply.send(result); }
                        Command::Snapshot(reply) => { let _ = reply.send(Ok(self.view())); }
                        Command::Refresh(peer, reply) => {
                            let result = if self.local().is_ok_and(|local| local.keys.peer_id() == peer) {
                                self.publication(Action::Refresh).await
                            } else { self.remote_record(peer).await.map(|_|()) };
                            let _ = reply.send(result);
                        }
                        Command::Revoke(target, reply) => { let result = self.publication(Action::Revoke(target)).await; let _ = reply.send(result); }
                        Command::Resolve(hash, reply) => { let result = self.publication(Action::Confirm(hash)).await; let _ = reply.send(result); }
                    }
                }
            }
        }
        self.auth.cancel.cancel();
        self.close_auth();
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
        self.auth.cancel.cancel();
        self.close_auth();
        self.auth.cancel = tokio_util::sync::CancellationToken::new();
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
        self.ensure_ready(request)?;
        self.auto_refresh().await;
        Ok(())
    }
    fn persist(&mut self, state: &AccountState, request: Request) -> Result<(), CoreError> {
        self.ensure_ready(request)?;
        if let Err(error) = self.store.commit(&state.encode()) {
            // An error can follow atomic replacement. Do not trust the old
            // in-memory state or permit another setup to overwrite the new one.
            // A new suspend/resume cycle must reload and validate the actual disk.
            self.close_auth();
            self.shared.network.block();
            self.shared
                .status
                .fetch_max((request.ticket << 2) | 2, Ordering::AcqRel);
            return Err(error);
        }
        Ok(())
    }
    fn local(&self) -> Result<&super::storage::schema::LocalAccount, CoreError> {
        match &self.state {
            Some(AccountState::Active(local)) => Ok(local),
            _ => Err(unavailable(ErrorCode::AccountRestricted)),
        }
    }
    async fn auto_refresh(&mut self) {
        if self.ready().is_ok() && self.local().is_ok() {
            let _ = self.publication(Action::Refresh).await;
        }
    }
    async fn service<T>(
        &self,
        request: Request,
        future: super::RecordFuture<'_, T>,
    ) -> Result<T, CoreError> {
        let mut lifecycle = self.shared.requests.subscribe();
        self.ensure_ready(request)?;
        tokio::select! { biased;
            _ = lifecycle.changed() => Err(unavailable(ErrorCode::StorageLocked)),
            result = tokio::time::timeout(Duration::from_secs(10), future) => {
                self.ensure_ready(request)?;
                result.map_err(|_| unavailable(ErrorCode::RecordUnavailable))?
            }
        }
    }
    fn commit_records(&mut self, records: RecordState, request: Request) -> Result<(), CoreError> {
        self.ensure_ready(request)?;
        let old_accepted = self.local()?.records.accepted.as_ref().map(|r| r.hash);
        if self.local()?.records.encode() == records.encode() {
            return Ok(());
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| unavailable(ErrorCode::StorageFull))?;
        let mut state = self.state.take().unwrap();
        let AccountState::Active(local) = &mut state else {
            unreachable!()
        };
        local.records = records;
        local.revision = revision;
        let result = self.persist(&state, request);
        self.state = Some(state);
        if result.is_ok() {
            self.revision = revision;
        }
        self.restrict_grants();
        if result.is_ok()
            && self.local()?.records.conflict.is_none()
            && let Some(record) = &self.local()?.records.accepted
            && Some(record.hash) != old_accepted
        {
            self.auth.registry.sync_record(
                self.local()?.keys.peer_id(),
                record,
                self.auth.cancel.child_token(),
            );
        }
        result
    }
    fn ensure_packet_time(
        &mut self,
        signed: &super::publication::SignedRecord,
        request: Request,
    ) -> Result<(), CoreError> {
        self.ensure_ready(request)?;
        let now = self.services.clock.unix_seconds();
        if !signed.current(now) || signed.record.is_expired(now) {
            let mut records = self.local()?.records.clone();
            records.state = PublicationState::LocalBlocked;
            self.commit_records(records, request)?;
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        Ok(())
    }
    async fn publication(&mut self, action: Action) -> Result<(), CoreError> {
        let request = self.ready()?;
        let local = self.local()?;
        if matches!(action, Action::Revoke(target) if target == local.device.installation_id()) {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        if matches!(action, Action::Confirm(hash) if local.records.conflict.as_ref().map(|r| r.hash) != Some(hash))
        {
            return Err(unavailable(ErrorCode::RecordConflict));
        }
        let peer = local.keys.peer_id();
        let backend = self.services.backend()?;
        let lookup = self.service(request, backend.lookup(peer)).await;
        self.ensure_ready(request)?;
        let mut records = self.local()?.records.clone();
        let result = match lookup {
            Ok(lookup) => records.reconcile(
                lookup,
                self.local()?,
                self.services.clock.unix_seconds(),
                action,
            ),
            Err(error) => {
                if records.conflict.is_none() {
                    records.state = PublicationState::LocalBlocked;
                }
                Err(error)
            }
        };
        // Capture once, in the same durable transaction as the new tombstone.
        // Existing/uncaptured intents never resample the live registry on retry.
        for intent in &mut records.pending {
            if let Some(nonce) = intent.revoke_nonce
                && !self
                    .local()?
                    .records
                    .tombstones
                    .iter()
                    .any(|t| t.nonce == nonce)
            {
                let revoke = records
                    .tombstones
                    .iter()
                    .find(|t| t.nonce == nonce)
                    .unwrap();
                intent.recipients = Some(
                    self.auth
                        .registry
                        .capture(revoke, self.services.clock.unix_seconds())?,
                );
            }
        }
        // Includes observed floors and conflicting signed evidence on Err.
        self.commit_records(records, request)?;
        result?;
        let pending = self.local()?.records.pending.clone();
        for intent in pending {
            if intent.status == PacketStatus::Held {
                continue;
            }
            if intent.status == PacketStatus::AwaitingNotification {
                let Some(notifier) = self.services.notifier.clone() else {
                    continue;
                };
                self.ensure_packet_time(&intent.signed, request)?;
                let revoke = self
                    .local()?
                    .records
                    .tombstones
                    .iter()
                    .find(|t| Some(t.nonce) == intent.revoke_nonce)
                    .cloned()
                    .ok_or_else(|| unavailable(ErrorCode::StorageCorrupt))?;
                let recipients = intent
                    .recipients
                    .clone()
                    .ok_or_else(|| unavailable(ErrorCode::RecordUnavailable))?;
                self.auth
                    .registry
                    .1
                    .lock()
                    .unwrap()
                    .insert(revoke.nonce, recipients);
                self.service(request, notifier.notify(&revoke)).await?;
                let mut records = self.local()?.records.clone();
                records
                    .pending
                    .iter_mut()
                    .find(|p| p.signed.hash == intent.signed.hash)
                    .unwrap()
                    .status = PacketStatus::Ready;
                self.commit_records(records, request)?;
                self.auth.registry.1.lock().unwrap().remove(&revoke.nonce);
            }
            // Only the exact committed packet can be retried at its sequence.
            self.ensure_packet_time(&intent.signed, request)?;
            if let Err(error) = self
                .service(request, backend.publish(&intent.signed.packet))
                .await
            {
                if error.code == ErrorCode::RecordConflict {
                    let mut records = self.local()?.records.clone();
                    for pending in &mut records.pending {
                        pending.status = PacketStatus::Held;
                    }
                    records.state = PublicationState::Conflict;
                    self.commit_records(records, request)?;
                }
                return Err(error);
            }
        }
        // A publish ACK is deliberately not an observation or completion.
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
            publication: PublicationView::default(),
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
                view.publication = local.records.view();
                match local.records.state {
                    PublicationState::Observed => view.phase = AccountPhase::Active,
                    PublicationState::Conflict => view.phase = AccountPhase::Conflict,
                    PublicationState::Pending => view.phase = AccountPhase::PublishPending,
                    PublicationState::LocalBlocked => {}
                }
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

    fn offline_services() -> AccountServices {
        struct Offline;
        impl crate::account::RecordBackend for Offline {
            fn lookup(
                &self,
                _: PeerId,
            ) -> crate::account::RecordFuture<'_, crate::account::Lookup> {
                Box::pin(async { Err(unavailable(ErrorCode::RecordUnavailable)) })
            }
            fn publish<'a>(
                &'a self,
                _: &'a pkarr::SignedPacket,
            ) -> crate::account::RecordFuture<'a, ()> {
                Box::pin(async { panic!("offline lookup must not publish") })
            }
        }
        AccountServices::new(
            Arc::new(Offline),
            Arc::new(crate::account::clock::SystemClock::default()),
        )
    }

    #[tokio::test]
    async fn completed_packet_replace_error_never_sends_and_reload_retries_exact_packet() {
        use crate::account::{Lookup, PeerId, RecordBackend, RecordFuture, clock::FakeClock};
        use std::sync::atomic::AtomicBool;
        struct Backend {
            absent: AtomicBool,
            sent: std::sync::Mutex<Vec<pkarr::SignedPacket>>,
        }
        impl RecordBackend for Backend {
            fn lookup(&self, _: PeerId) -> RecordFuture<'_, Lookup> {
                Box::pin(async {
                    if self.absent.load(Ordering::SeqCst) {
                        Ok(Lookup::CompletedAbsent)
                    } else {
                        Err(unavailable(ErrorCode::RecordUnavailable))
                    }
                })
            }
            fn publish<'a>(&'a self, packet: &'a pkarr::SignedPacket) -> RecordFuture<'a, ()> {
                Box::pin(async {
                    self.sent.lock().unwrap().push(packet.clone());
                    Ok(())
                })
            }
        }
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
        let backend = Arc::new(Backend {
            absent: AtomicBool::new(false),
            sent: std::sync::Mutex::new(vec![]),
        });
        let mut manager = Manager {
            auth: super::super::connection::Runtime::default(),
            bus: EventBus::new(),
            store: Store::open(dir.path(), Arc::new(Protector)).unwrap(),
            state: None,
            creation: None,
            revision: 0,
            shared,
            services: AccountServices::new(backend.clone(), Arc::new(FakeClock::new(1000, 0))),
        };
        let restored = manager.lifecycle(request).await;
        manager.complete(request, restored);
        manager
            .recover(
                &super::super::encode_recovery(&Secret32::from([0; 32])).unwrap(),
                true,
            )
            .await
            .unwrap();
        backend.absent.store(true, Ordering::SeqCst);
        use futures_util::FutureExt;
        let result = super::super::storage::with_after_replace_failure(|| {
            manager.publication(Action::Refresh).now_or_never().unwrap()
        });
        assert_eq!(result.unwrap_err().code, ErrorCode::StorageFull);
        assert_eq!(manager.view().phase, AccountPhase::Locked);
        assert!(backend.sent.lock().unwrap().is_empty());
        let AccountState::Active(saved) =
            AccountState::decode(&manager.store.load().unwrap().unwrap()).unwrap()
        else {
            panic!()
        };
        assert_eq!(saved.records.allocated_seq, 1_000_000_000);
        assert_eq!(saved.records.pending.len(), 1);
        let packet = saved.records.pending[0].signed.packet.clone();
        assert_eq!(
            manager.publication(Action::Refresh).await.unwrap_err().code,
            ErrorCode::StorageLocked
        );
        for suspend in [true, false] {
            let ticket = network.block();
            let request = Request { ticket, suspend };
            manager.shared.requests.send_replace(request);
            manager.shared.status.store(ticket << 2, Ordering::Release);
            let result = manager.lifecycle(request).await;
            manager.complete(request, result);
        }
        manager.publication(Action::Refresh).await.unwrap();
        assert_eq!(
            backend.sent.lock().unwrap()[0].as_bytes(),
            packet.as_bytes()
        );
        network.execute(NetworkOp::Stop).await.unwrap();
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
            auth: super::super::connection::Runtime::default(),
            bus: EventBus::new(),
            store: Store::open(dir.path(), Arc::new(Protector)).unwrap(),
            state: None,
            creation: None,
            revision: 0,
            shared,
            services: offline_services(),
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
            auth: super::super::connection::Runtime::default(),
            bus: EventBus::new(),
            store: Store::open(dir.path(), Arc::new(Protector)).unwrap(),
            state: None,
            creation: None,
            revision: 0,
            shared,
            services: offline_services(),
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

impl Manager {
    fn local_authorized(&self) -> bool {
        let Ok(local) = self.local() else {
            return false;
        };
        let Some(record) = &local.records.accepted else {
            return false;
        };
        let now = self.services.clock.unix_seconds();
        self.ready().is_ok()
            && local.records.conflict.is_none()
            && record.seq >= local.records.observed_seq
            && record.record.generation == local.records.generation
            && record.current(now)
            && !record.record.is_expired(now)
            && record.record.devices.iter().any(|d| {
                d.installation_id == local.device.installation_id()
                    && d.transport_public_key == local.device.transport_public_key()
            })
            && !local.records.tombstones.iter().any(|t| {
                t.target_installation == local.device.installation_id()
                    || t.target_transport == local.device.transport_public_key()
            })
    }
    fn grant_authorized(&self, grant: &super::auth::AuthGrant) -> bool {
        if !self.local_authorized() {
            return false;
        }
        let local = self.local().unwrap();
        let now = self.services.clock.unix_seconds();
        if grant.peer == local.keys.peer_id() {
            return local
                .records
                .accepted
                .as_ref()
                .is_some_and(|r| grant.valid(r, &local.records.tombstones, now));
        }
        local
            .remote
            .iter()
            .find(|r| r.peer == grant.peer)
            .is_some_and(|r| {
                r.conflict.is_none()
                    && r.accepted.seq >= r.floor
                    && grant.valid(&r.accepted, &[], now)
            })
    }
    fn must_close(&self, grant: &super::auth::AuthGrant) -> bool {
        let Ok(local) = self.local() else { return true };
        let Some(own) = local.records.accepted.as_ref() else {
            return true;
        };
        if self.ready().is_err()
            || local.records.conflict.is_some()
            || own.record.generation != local.records.generation
            || !own.record.devices.iter().any(|d| {
                d.installation_id == local.device.installation_id()
                    && d.transport_public_key == local.device.transport_public_key()
            })
            || local
                .records
                .tombstones
                .iter()
                .any(|t| t.target_installation == local.device.installation_id())
        {
            return true;
        }
        let record = if grant.peer == local.keys.peer_id() {
            if local.records.tombstones.iter().any(|t| {
                t.target_installation == grant.installation || t.target_transport == grant.transport
            }) {
                return true;
            }
            own
        } else {
            let Some(remote) = local.remote.iter().find(|r| r.peer == grant.peer) else {
                return true;
            };
            if remote.conflict.is_some() || remote.accepted.seq < remote.floor {
                return true;
            }
            &remote.accepted
        };
        record.record.generation != grant.generation
            || !record.record.devices.iter().any(|d| {
                d.installation_id == grant.installation && d.transport_public_key == grant.transport
            })
    }
    fn close_auth(&self) {
        for session in self.auth.registry.0.lock().unwrap().drain(..) {
            session.conn.close(0u32.into(), b"account stopped");
            if session.ready {
                self.bus.publish(crate::CoreEvent::AccountAuthChanged {
                    peer_id: session.grant.peer.to_string(),
                    installation_id: session.grant.installation.to_string(),
                    ready: false,
                });
            }
        }
    }
    fn restrict_grants(&self) {
        for grant in self
            .auth
            .registry
            .apply(|grant| (self.grant_authorized(grant), self.must_close(grant)))
        {
            self.bus.publish(crate::CoreEvent::AccountAuthChanged {
                peer_id: grant.peer.to_string(),
                installation_id: grant.installation.to_string(),
                ready: false,
            });
        }
    }
    fn commit_remote(
        &mut self,
        remote: Vec<super::connection::RemoteRecord>,
        request: Request,
    ) -> Result<(), CoreError> {
        if super::connection::encode_cache(&self.local()?.remote)
            == super::connection::encode_cache(&remote)
        {
            return Ok(());
        }
        self.ensure_ready(request)?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| unavailable(ErrorCode::StorageFull))?;
        let mut state = self.state.take().unwrap();
        let AccountState::Active(local) = &mut state else {
            unreachable!()
        };
        local.remote = remote;
        local.revision = revision;
        let result = self.persist(&state, request);
        self.state = Some(state);
        if result.is_ok() {
            self.revision = revision;
        }
        self.restrict_grants();
        result
    }
    async fn remote_record(
        &mut self,
        peer: PeerId,
    ) -> Result<super::publication::SignedRecord, CoreError> {
        let request = self.ready()?;
        if self.local()?.keys.peer_id() == peer {
            if !self.local_authorized() {
                return Err(unavailable(ErrorCode::AccountRestricted));
            }
            return Ok(self.local()?.records.accepted.clone().unwrap());
        }
        let backend = self.services.backend()?;
        let lookup = self.service(request, backend.lookup(peer)).await;
        self.ensure_ready(request)?;
        let mut remote = self.local()?.remote.clone();
        let mut cached = false;
        match lookup {
            Ok(super::Lookup::Found(packet)) => {
                let signed = super::publication::SignedRecord::new(packet, peer)?;
                let result = if let Some(existing) = remote.iter_mut().find(|r| r.peer == peer) {
                    existing.observe(signed, self.services.clock.unix_seconds())
                } else {
                    if remote.len() >= 1000 {
                        return Err(unavailable(ErrorCode::StorageFull));
                    }
                    let conflict = if signed.current(self.services.clock.unix_seconds()) {
                        None
                    } else {
                        Some(signed.clone())
                    };
                    remote.push(super::connection::RemoteRecord {
                        peer,
                        floor: signed.seq,
                        accepted: signed,
                        conflict,
                    });
                    remote.sort_by_key(|r| r.peer);
                    Ok(())
                };
                self.commit_remote(remote, request)?;
                result?;
            }
            Ok(super::Lookup::CompletedAbsent) | Err(_) => {
                cached = true;
            }
        }
        let accepted = self
            .local()?
            .remote
            .iter()
            .find(|r| r.peer == peer)
            .ok_or_else(|| unavailable(ErrorCode::RecordUnavailable))?;
        if accepted.conflict.is_some()
            || accepted.accepted.seq < accepted.floor
            || !accepted
                .accepted
                .current(self.services.clock.unix_seconds())
            || accepted
                .accepted
                .record
                .is_expired(self.services.clock.unix_seconds())
        {
            self.restrict_grants();
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        super::parse_record(
            &accepted.accepted.packet,
            peer,
            self.services.clock.unix_seconds(),
        )?;
        let record = accepted.accepted.clone();
        if cached {
            self.bus.publish(crate::CoreEvent::AccountCacheUsed {
                peer_id: peer.to_string(),
            });
        }
        Ok(record)
    }
    async fn connect(&mut self, peer: PeerId) -> Result<(), CoreError> {
        self.restrict_grants();
        if !self.local_authorized() {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        let request = self.ready()?;
        let record = self.remote_record(peer).await?;
        let local_id = self.local()?.device.transport_public_key();
        let targets: Vec<_> = record
            .record
            .devices
            .iter()
            .filter(|d| d.transport_public_key != local_id)
            .cloned()
            .collect();
        let mut last = unavailable(ErrorCode::AccountRestricted);
        for device in targets {
            if peer == self.local()?.keys.peer_id()
                && self
                    .local()?
                    .records
                    .tombstones
                    .iter()
                    .any(|t| t.target_installation == device.installation_id)
            {
                continue;
            }
            let id = iroh::EndpointId::from_bytes(&device.transport_public_key)
                .map_err(|_| unavailable(ErrorCode::AuthFailed))?;
            let conn = match self
                .service(
                    request,
                    Box::pin(
                        self.shared
                            .network
                            .dial_account(iroh::EndpointAddr::from(id)),
                    ),
                )
                .await
            {
                Ok(conn) => conn,
                Err(error) => {
                    last = error;
                    continue;
                }
            };
            let result = self
                .service(
                    request,
                    Box::pin(super::auth::authenticate(
                        conn.clone(),
                        peer,
                        self.local()?,
                        &record,
                        &*self.services.clock,
                    )),
                )
                .await;
            match result {
                Ok(grant) => return self.install_grant(conn, grant),
                Err(error) => {
                    conn.close(0u32.into(), b"account authentication failed");
                    last = error;
                }
            }
        }
        Err(last)
    }
    fn install_grant(
        &mut self,
        conn: iroh::endpoint::Connection,
        grant: super::auth::AuthGrant,
    ) -> Result<(), CoreError> {
        if !self.grant_authorized(&grant) {
            conn.close(0u32.into(), b"account restricted");
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        let mut sessions = self.auth.registry.0.lock().unwrap();
        sessions.retain(|s| {
            if s.grant.peer == grant.peer && s.grant.installation == grant.installation {
                s.conn.close(0u32.into(), b"new account proof");
                false
            } else {
                true
            }
        });
        if sessions.len() >= 32 {
            conn.close(0u32.into(), b"account capacity");
            return Err(unavailable(ErrorCode::Busy));
        }
        sessions.push(super::connection::Session {
            conn: conn.clone(),
            grant: grant.clone(),
            ready: true,
        });
        drop(sessions);
        self.bus.publish(crate::CoreEvent::AccountAuthChanged {
            peer_id: grant.peer.to_string(),
            installation_id: grant.installation.to_string(),
            ready: true,
        });
        super::connection::reader(conn, self.auth.send.clone(), self.auth.cancel.child_token());
        Ok(())
    }
    async fn auth_input(&mut self, input: super::connection::Input) {
        use super::connection::Input;
        self.restrict_grants();
        match input {
            Input::Incoming(conn) => {
                let result = async {
                    let request = self.ready()?;
                    if !self.local_authorized() {
                        return Err(unavailable(ErrorCode::AccountRestricted));
                    }
                    let preface = self
                        .service(request, Box::pin(super::auth::receive_preface(&conn)))
                        .await?;
                    let record = self.remote_record(preface.hello.peer).await?;
                    let grant = self
                        .service(
                            request,
                            Box::pin(super::auth::authenticate_incoming(
                                &conn,
                                self.local()?,
                                &record,
                                &*self.services.clock,
                                preface,
                            )),
                        )
                        .await?;
                    self.install_grant(conn.clone(), grant)
                }
                .await;
                if result.is_err() {
                    conn.close(0u32.into(), b"account authentication failed");
                }
            }
            Input::Control {
                connection,
                body,
                reply,
            } => {
                let result = self.receive_control(&connection, &body);
                let _ = reply.send(result);
            }
            Input::Closed(id) => {
                let mut sessions = self.auth.registry.0.lock().unwrap();
                sessions.retain(|s| {
                    if s.conn.stable_id() == id {
                        self.bus.publish(crate::CoreEvent::AccountAuthChanged {
                            peer_id: s.grant.peer.to_string(),
                            installation_id: s.grant.installation.to_string(),
                            ready: false,
                        });
                        false
                    } else {
                        true
                    }
                });
            }
        }
    }
    fn apply_record_change(
        &mut self,
        signed: &super::publication::SignedRecord,
        request: Request,
    ) -> Result<(), CoreError> {
        let mut records = self.local()?.records.clone();
        let previous = records
            .accepted
            .as_ref()
            .ok_or_else(|| unavailable(ErrorCode::AccountRestricted))?;
        let current = signed.current(self.services.clock.unix_seconds());
        let exact = records
            .pending
            .iter()
            .position(|p| p.signed.hash == signed.hash && p.status == PacketStatus::Ready);
        let renewal = records.pending.is_empty()
            && signed.seq > records.observed_seq
            && current
            && signed.record.generation == previous.record.generation
            && signed.record.devices
                == previous
                    .record
                    .devices
                    .iter()
                    .filter(|d| {
                        !records.tombstones.iter().any(|t| {
                            t.target_installation == d.installation_id
                                || t.target_transport == d.transport_public_key
                        })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
        let result = if records.conflict.is_none() && signed.hash == previous.hash {
            Ok(())
        } else if records.conflict.is_none()
            && signed.seq >= records.observed_seq
            && (exact.is_some() || renewal)
            && current
        {
            records.observed_seq = signed.seq;
            records.accepted = Some(signed.clone());
            if let Some(index) = exact {
                records.pending.remove(index);
            }
            records.state = if records.pending.is_empty() {
                PublicationState::Observed
            } else {
                PublicationState::Pending
            };
            Ok(())
        } else {
            records.observed_seq = records.observed_seq.max(signed.seq);
            records.conflict = Some(signed.clone());
            records.state = PublicationState::Conflict;
            for pending in &mut records.pending {
                pending.status = PacketStatus::Held;
            }
            Err(unavailable(ErrorCode::RecordConflict))
        };
        self.commit_records(records, request)?;
        result
    }
    fn receive_control(
        &mut self,
        conn: &iroh::endpoint::Connection,
        body: &[u8],
    ) -> Result<Vec<u8>, CoreError> {
        let request = self.ready()?;
        let (grant, ready) = self
            .auth
            .registry
            .0
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.conn.stable_id() == conn.stable_id())
            .map(|s| (s.grant.clone(), s.ready))
            .ok_or_else(|| unavailable(ErrorCode::AuthFailed))?;
        if let Ok(bytes) = super::auth::payload(body, 2) {
            if self.must_close(&grant) || grant.peer != self.local()?.keys.peer_id() {
                return Err(unavailable(ErrorCode::AccountRestricted));
            }
            let signed = super::publication::SignedRecord::decode(&bytes, grant.peer)?;
            super::parse_record(
                &signed.packet,
                grant.peer,
                self.services.clock.unix_seconds(),
            )?;
            self.apply_record_change(&signed, request)?;
            return Ok(super::auth::envelope(
                2,
                self.local()?
                    .records
                    .accepted
                    .as_ref()
                    .unwrap()
                    .packet
                    .as_bytes(),
            ));
        }
        if body == [0x83, 1, 4, 0xf6] {
            conn.close(0u32.into(), b"fresh account proof required");
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        if !ready || !self.grant_authorized(&grant) {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(body);
        let intent = super::RevokeIntent::from_frame(&frame)?;
        let local = self.local()?;
        let peer = local.keys.peer_id();
        if grant.peer != peer {
            return Err(unavailable(ErrorCode::AuthFailed));
        }
        intent.verify(peer)?;
        let record = local
            .records
            .accepted
            .as_ref()
            .ok_or_else(|| unavailable(ErrorCode::AccountRestricted))?;
        if intent.generation != record.record.generation
            || intent.base_hash != record.hash
            || intent.issued_at < record.record.issued_at
            || intent.issued_at > self.services.clock.unix_seconds().saturating_add(300)
            || !record.record.devices.iter().any(|d| {
                d.installation_id == intent.target_installation
                    && d.transport_public_key == intent.target_transport
            })
        {
            return Err(unavailable(ErrorCode::AuthFailed));
        }
        let mut records = local.records.clone();
        if !records.tombstones.iter().any(|t| t == &intent) {
            if records.tombstones.iter().any(|t| {
                t.nonce == intent.nonce || t.target_installation == intent.target_installation
            }) {
                return Err(unavailable(ErrorCode::AuthFailed));
            }
            if records.tombstones.len() >= 256 {
                return Err(unavailable(ErrorCode::StorageFull));
            }
            records.tombstones.push(intent);
            self.commit_records(records, request)?;
        }
        self.restrict_grants();
        if !self.grant_authorized(&grant) || conn.close_reason().is_some() {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        Ok(body.to_vec())
    }
}
