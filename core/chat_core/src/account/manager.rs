//! Single owner of protected account state. A5 extends commands and AccountState.
use super::storage::schema::LocalAccount;
use super::{
    AccountServices, FlowId, InstallationId, PeerId, PublicationState,
    PublicationView, RecoveryDisplay,
    creation::Creation,
    derive_device,
    publication::{Action, PacketStatus, RecordState},
    random_secret, recovery,
    storage::{KeyProtector, schema::AccountState},
};
use crate::{CoreError, ErrorCode, NetworkHandle, storage::worker::StorageHandle};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PublishIntent {
    LocalCreate,
    Replacement,
}
/// Public data only. No recovery display or private keys can enter this view.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountView {
    pub disclosure_possible: bool,
    pub pending_pair_transactions: Vec<super::TxId>,
    pub pair_resumes: Vec<super::PairResumeView>,
    pub pending_import_requires_remote_revoke: bool,
    pub revision: u64,
    pub phase: AccountPhase,
    pub lock_error: Option<ErrorCode>,
    pub public_peer_id: Option<PeerId>,
    pub installation: Option<InstallationId>,
    pub devices: Vec<super::RecordDevice>,
    pub contacts: Vec<super::ContactView>,
    pub remote_records: Vec<super::RemoteRecordView>,
    pub pair: Option<super::PairView>,
    pub publish_intents: Vec<PublishIntent>,
    pub publication: PublicationView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateInfo {
    pub installation_id: InstallationId,
    pub endpoint_id: String,
}
enum Command {
    Message(crate::message::service::MessageRequest, oneshot::Sender<Result<crate::message::service::MessageReply, CoreError>>),
    #[cfg(all(feature="test-hooks",debug_assertions))]
    MessageProofWriteStarts(oneshot::Sender<Result<usize,CoreError>>),
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    MessageWriteStarts(oneshot::Sender<Result<(usize,usize),CoreError>>),
    #[cfg(all(feature="test-hooks",debug_assertions))]
    MessagePause(messages::MessagePoint,oneshot::Sender<Result<messages::MessagePause,CoreError>>),
    #[cfg(all(feature="test-hooks",debug_assertions))]
    MessageAckPause(crate::MessageKey,oneshot::Sender<Result<(messages::MessagePause,messages::MessagePause),CoreError>>),
    #[cfg(all(feature="test-hooks",debug_assertions))]
    MessageAttempt(crate::message::types::MessageKey,bool,MessageMutation,oneshot::Sender<Result<(),CoreError>>),
    Retry(oneshot::Sender<Result<u64, CoreError>>),
    #[cfg(all(feature="test-hooks",debug_assertions))]
    PairTestStop(super::PairTestStop,oneshot::Sender<Result<(),CoreError>>),
    #[cfg(all(feature="test-hooks",debug_assertions))]
    PairTestBytes(oneshot::Sender<Result<(usize,usize),CoreError>>),
    #[cfg(all(feature="test-hooks",debug_assertions))]
    PairTestReached(oneshot::Sender<Result<Option<super::PairTestStop>,CoreError>>),
    DiscardPending(oneshot::Sender<Result<(),CoreError>>),
    PairBegin(super::PairDirection,Option<super::TxId>,oneshot::Sender<Result<super::PairDisplay,CoreError>>),
    PairScan(Zeroizing<String>,oneshot::Sender<Result<FlowId,CoreError>>),
    PairConfirm(FlowId,bool,oneshot::Sender<Result<(),CoreError>>),
    PairApprove(FlowId,oneshot::Sender<Result<(),CoreError>>),
    PairCancel(FlowId,oneshot::Sender<Result<(),CoreError>>),
    PairView(oneshot::Sender<Result<Option<super::PairView>,CoreError>>),
    #[cfg(all(feature="test-hooks",debug_assertions))]
    PairFixtureSend(FlowId,Zeroizing<Vec<u8>>,oneshot::Sender<Result<usize,CoreError>>),
    #[cfg(all(feature="test-hooks",debug_assertions))]
    PairFixtureReceived(FlowId,oneshot::Sender<Result<Vec<u8>,CoreError>>),
    CreateInvite(Option<String>, oneshot::Sender<Result<String, CoreError>>),
    AcceptInvite(
        String,
        String,
        oneshot::Sender<Result<super::ContactView, CoreError>>,
    ),
    Contacts(oneshot::Sender<Result<Vec<super::ContactView>, CoreError>>),
    ConfirmSafety(PeerId, [u8; 32], oneshot::Sender<Result<(), CoreError>>),
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
    RepublishAccepted([u8; 32], oneshot::Sender<Result<(), CoreError>>),
    ResolveRemote(PeerId, [u8; 32], oneshot::Sender<Result<(), CoreError>>),
}
struct Queued {
    ticket: u64,
    command: Command,
}
impl Command {
    fn reject(self, error: CoreError) {
        match self {
            Self::Message(_, reply) => { let _ = reply.send(Err(error)); }
            #[cfg(all(feature="test-hooks",debug_assertions))]
            Self::MessageProofWriteStarts(reply)=>{let _=reply.send(Err(error));}
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            Self::MessageWriteStarts(reply)=>{let _=reply.send(Err(error));}
            #[cfg(all(feature="test-hooks",debug_assertions))]
            Self::MessagePause(_,reply)=>{let _=reply.send(Err(error));}
            #[cfg(all(feature="test-hooks",debug_assertions))]
            Self::MessageAckPause(_,reply)=>{let _=reply.send(Err(error));}
            #[cfg(all(feature="test-hooks",debug_assertions))]
            Self::MessageAttempt(_,_,_,reply)=>{let _=reply.send(Err(error));}
            Self::Retry(reply) => { let _ = reply.send(Err(error)); }
            #[cfg(all(feature="test-hooks",debug_assertions))]
            Self::PairTestStop(_,reply)=>{let _=reply.send(Err(error));}
            #[cfg(all(feature="test-hooks",debug_assertions))]
            Self::PairTestBytes(reply)=>{let _=reply.send(Err(error));}
            #[cfg(all(feature="test-hooks",debug_assertions))]
            Self::PairTestReached(reply)=>{let _=reply.send(Err(error));}
            Self::DiscardPending(reply)=>{let _=reply.send(Err(error));}
            Self::PairBegin(_,_,reply)=>{let _=reply.send(Err(error));}
            Self::PairView(reply)=>{let _=reply.send(Err(error));}
            #[cfg(all(feature="test-hooks",debug_assertions))]
            Self::PairFixtureSend(_,_,reply)=>{let _=reply.send(Err(error));}
            #[cfg(all(feature="test-hooks",debug_assertions))]
            Self::PairFixtureReceived(_,reply)=>{let _=reply.send(Err(error));}
            Self::PairScan(_,reply)=>{let _=reply.send(Err(error));}
            Self::PairConfirm(_,_,reply)|Self::PairApprove(_,reply)|Self::PairCancel(_,reply)=>{let _=reply.send(Err(error));}
            Self::CreateInvite(_, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::AcceptInvite(_, _, reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Contacts(reply) => {
                let _ = reply.send(Err(error));
            }
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
            | Self::ResolveRemote(_, _, reply)
            | Self::Resolve(_, reply)
            | Self::RepublishAccepted(_, reply)
            | Self::ConfirmSafety(_, _, reply) => {
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
    store: StorageHandle,
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
    pub(crate) async fn message(&self, request: crate::message::service::MessageRequest) -> Result<crate::message::service::MessageReply, CoreError> {
        let ticket = self.shared.requests.borrow().ticket;
        let value = self.send(|reply| Command::Message(request, reply)).await?;
        // A storage failure can lock this same request after its SQL reply was sent.
        if self.shared.status.load(Ordering::Acquire) != (ticket << 2) | 1 {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        Ok(value)
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub fn storage_test_fixture(&self) -> crate::storage_fixture::Fixture {
        crate::storage_fixture::Fixture::from_handle(self.shared.store.clone())
    }
    #[cfg(all(feature="test-hooks",debug_assertions))]
    pub async fn pair_test_stop(&self,stop:super::PairTestStop)->Result<(),CoreError>{self.send(|reply|Command::PairTestStop(stop,reply)).await}
    #[cfg(all(feature="test-hooks",debug_assertions))]
    pub async fn pair_test_bytes(&self)->Result<(usize,usize),CoreError>{self.send(Command::PairTestBytes).await}
    #[cfg(all(feature="test-hooks",debug_assertions))]
    pub async fn pair_test_reached(&self)->Result<Option<super::PairTestStop>,CoreError>{self.send(Command::PairTestReached).await}
    pub(crate) fn open(
        runtime: &tokio::runtime::Handle,
        root: &Path,
        protector: Arc<dyn KeyProtector>,
        network: NetworkHandle,
        mut services: AccountServices,
        bus: crate::EventBus,
    ) -> Result<Self, CoreError> {
        network.begin_account();
        let ticket = network.block();
        let (store, initial) = StorageHandle::open_blocking(root.to_owned(), protector)?;
        store.invalidate(ticket);
        let state = initial
            .as_ref()
            .map(AccountState::decode)
            .transpose()?;
        let revision = match &state {
            Some(AccountState::Active(local)) => local.revision,
            Some(AccountState::Candidate(_)) => 1,
            Some(AccountState::CandidatePinned(p)) => p.revision,
            Some(AccountState::PendingImport(p)) => p.revision,
            None => 0,
        };
        let (requests, received) = watch::channel(Request {
            ticket,
            suspend: network.desired_suspend(),
        });
        let shared = Arc::new(Shared {
            store: store.clone(),
            network,
            requests,
            status: AtomicU64::new(ticket << 2),
        });
        let (commands, input) = mpsc::channel(32);
        let auth = super::connection::Runtime::default();
        let pair=super::pair::session::Runtime::default();
        shared.network.register_pair(pair.send.clone())?;
        shared.network.register_account(auth.send.clone())?;
        if services.notifier.is_none() {
            services.notifier = Some(Arc::new(super::connection::Notifier {
                registry: auth.registry.clone(),
                clock: services.clock.clone(),
            }));
        }
        runtime.spawn(
            Manager {
                messages: Default::default(),
                pair,
                auth,
                bus,
                store,
                state,
                creation: None,
                cleanup_pending: false,
                revision,
                public_revision: 0,
                last_public_view: None,
                lock_error: None,
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
    /// Discard only a never-active candidate/import. Public membership requires
    /// separate revocation on an existing device; remote disclosure is retained.
    pub async fn discard_pending_import(&self) -> Result<(), CoreError> {
        self.send(Command::DiscardPending).await
    }
    async fn send<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<T, CoreError>>) -> Command,
    ) -> Result<T, CoreError> {
        let (reply, result) = oneshot::channel();
        let ticket = self.shared.requests.borrow().ticket;
        let command = command(reply);
        let snapshot = matches!(command, Command::Snapshot(_));
        self.commands
            .try_send(Queued {
                ticket,
                command,
            })
            .map_err(|_| unavailable(ErrorCode::Busy))?;
        let value = result
            .await
            .map_err(|_| unavailable(ErrorCode::NotAvailable))??;
        if self.shared.requests.borrow().ticket != ticket || (!snapshot && self.shared.network.desired_suspend()) {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        Ok(value)
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
    pub async fn retry_open(&self) -> Result<(), CoreError> {
        let (reply, result) = oneshot::channel();
        self.commands
            .try_send(Queued {
                ticket: self.shared.requests.borrow().ticket,
                command: Command::Retry(reply),
            })
            .map_err(|_| unavailable(ErrorCode::Busy))?;
        // Retry may advance the lifecycle ticket itself. Fence its acknowledgement
        // against that exact accepted ticket, including invalidation after send.
        let accepted = result.await.map_err(|_| unavailable(ErrorCode::NotAvailable))??;
        if self.shared.requests.borrow().ticket != accepted || self.shared.network.desired_suspend() {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        Ok(())
    }
    pub async fn snapshot(&self) -> Result<AccountView, CoreError> {
        self.send(Command::Snapshot).await
    }
    pub async fn create_invite(&self, name: Option<String>) -> Result<String, CoreError> {
        self.send(|reply| Command::CreateInvite(name, reply)).await
    }
    pub async fn accept_invite(
        &self,
        input: String,
        alias: String,
    ) -> Result<super::ContactView, CoreError> {
        self.send(|reply| Command::AcceptInvite(input, alias, reply))
            .await
    }
    pub async fn contacts(&self) -> Result<Vec<super::ContactView>, CoreError> {
        self.send(Command::Contacts).await
    }
    pub async fn confirm_safety(&self, peer: PeerId, digest: [u8; 32]) -> Result<(), CoreError> {
        self.send(|reply| Command::ConfirmSafety(peer, digest, reply))
            .await
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
    /// Resend the exact accepted packet without observing or resolving a conflict.
    pub async fn republish_accepted_record(&self, accepted_hash: [u8; 32]) -> Result<(), CoreError> {
        self.send(|reply| Command::RepublishAccepted(accepted_hash, reply))
            .await
    }
    pub async fn confirm_remote_record(&self, peer: PeerId, hash: [u8; 32]) -> Result<(), CoreError> {
        self.send(|reply| Command::ResolveRemote(peer, hash, reply)).await
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
        self.shared.store.invalidate(ticket);
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
    messages: messages::Runtime,
    pair:super::pair::session::Runtime,
    auth: super::connection::Runtime,
    bus: crate::EventBus,
    store: StorageHandle,
    state: Option<AccountState>,
    creation: Option<Creation>,
    cleanup_pending: bool,
    revision: u64,
    public_revision: u64,
    last_public_view: Option<AccountView>,
    lock_error: Option<ErrorCode>,
    shared: Arc<Shared>,
    services: AccountServices,
}
impl Manager {
    fn copy_state(&self) -> Option<AccountState> {
        self.state.as_ref().map(AccountState::copy_for_update)
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    fn pair_checkpoint(&mut self, at: super::PairTestStop) -> Result<(), CoreError> {
        if self.pair.stop == Some(at) {
            self.pair.stop = None;
            self.pair.reached = Some(at);
            self.pair.current = None;
            return Err(unavailable(ErrorCode::NotAvailable));
        }
        Ok(())
    }
    fn pending_pairs(&self) -> Vec<super::TxId> {
        use super::pair::transaction::Stage;
        match &self.state {
            Some(AccountState::CandidatePinned(p)) => vec![p.envelope.tx],
            Some(AccountState::PendingImport(p)) => vec![p.receipt.tx],
            Some(AccountState::Active(l)) => l
                .pairs
                .existing
                .iter()
                .filter(|t| t.stage != Stage::Observed)
                .map(|t| t.tx)
                .collect(),
            _ => vec![],
        }
    }
    fn pair_resumes(&self) -> Vec<super::PairResumeView> {
        use super::{PairResumeView, PairRole};
        let mut entries: Vec<_> = self.pending_pairs().into_iter().map(|transaction| PairResumeView {
            transaction,
            role: if matches!(self.state, Some(AccountState::Active(_))) { PairRole::Existing } else { PairRole::New },
        }).collect();
        if let Some(AccountState::Active(local)) = &self.state
            && let Some(origin) = &local.pairs.origin {
            entries.push(PairResumeView { transaction: origin.tx, role: PairRole::New });
        }
        entries
    }
    fn pair_view(&self) -> Option<super::PairView> {
        self.pair.current.as_ref().map(|s| s.view())
    }
    fn validate_pair_role(
        &self,
        role: super::pair::wire::Role,
        tx: Option<super::TxId>,
    ) -> Result<(), CoreError> {
        use super::pair::wire::Role;
        if let Some(tx) = tx {
            if super::pair::resume::role(self.state.as_ref().ok_or_else(super::wire::invalid)?, tx)?
                != role
            {
                return Err(super::wire::invalid());
            }
        } else if match role {
            Role::Existing => !matches!(self.state, Some(AccountState::Active(_))),
            Role::New => !matches!(self.state, None | Some(AccountState::Candidate(_))),
        } {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        Ok(())
    }
    async fn commit_pair_state(
        &mut self,
        state: AccountState,
        request: Request,
    ) -> Result<(), CoreError> {
        let result = self.persist(&state, request).await;
        if result.is_ok() {
            self.state = Some(state);
            self.revision += 1;
        }
        result
    }
    async fn discard_pending(&mut self) -> Result<(), CoreError> {
        let request = if self.cleanup_pending {
            *self.shared.requests.borrow()
        } else {
            self.ready()?
        };
        if request.suspend || self.shared.network.desired_suspend() {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        if !matches!(
            self.state,
            Some(
                AccountState::Candidate(_)
                    | AccountState::CandidatePinned(_)
                    | AccountState::PendingImport(_)
            )
        ) {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        self.cleanup_pending = true;
        self.pair.current = None;
        self.creation = None;
        self.shared.network.forget_pending_key().await?;
        if !self.current(request) || self.shared.network.desired_suspend() {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        self.store.discard_pending().await?;
        if !self.current(request) || self.shared.network.desired_suspend() {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        // SQL clears only the never-active state/binding; DB identity and keys remain.
        self.state = None;
        self.cleanup_pending = false;
        self.revision += 1;
        // An explicit completed cleanup can resolve this actor's earlier failed
        // cleanup/lifecycle ticket. Transport stays blocked until new setup.
        self.shared
            .status
            .store((request.ticket << 2) | 1, Ordering::Release);
        Ok(())
    }
    async fn start_transfer(&mut self, flow: FlowId) -> Result<(), CoreError> {
        use super::pair::{
            transaction::{ExistingPin, PairTransaction, Stage},
            transfer::{self, Payload},
        };
        let request = self.ready()?;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.pair_checkpoint(super::PairTestStop::EBeforeDisclosure)?;
        let s = self
            .pair
            .current
            .as_ref()
            .ok_or_else(super::wire::invalid)?;
        if s.transaction.is_some() {
            return Ok(());
        }
        let channel = s.channel.as_ref().ok_or_else(super::wire::invalid)?;
        let resume = s.qr.resume;
        let handshake = channel.hash;
        let local = self.local()?;
        let base = local
            .records
            .accepted
            .as_ref()
            .ok_or_else(super::wire::invalid)?;
        let (tx, payload) = if let Some(tx) = resume {
            let t = local
                .pairs
                .existing
                .iter()
                .find(|t| t.envelope().is_ok_and(|e| e.tx == tx))
                .ok_or_else(super::wire::invalid)?;
            let p = Payload::decode(&t.payload)?;
            let included = base.record.devices.iter().any(|d| {
                d.installation_id == p.cert.installation_id
                    && d.transport_public_key == p.cert.transport_public_key
                    && d.kind == p.cert.kind
            });
            if base.record.generation != p.generation
                || (!included && (base.hash != p.base || base.record.devices.len() >= 5))
            {
                return Err(unavailable(ErrorCode::RecordConflict));
            }
            (tx, Zeroizing::new(t.payload.to_vec()))
        } else {
            if base.record.devices.len() >= 5 {
                return Err(unavailable(ErrorCode::DeviceLimit));
            }
            local.pairs.admit_new(local.records.pending.len())?;
            let mut tx = super::TxId([0; 16]);
            getrandom::fill(&mut tx.0).map_err(|_| super::wire::invalid())?;
            let cert = super::certificate::sign_cert_public(
                &local.keys,
                channel.remote.installation,
                channel.remote.transport,
                channel.candidate_kind.ok_or_else(super::wire::invalid)?,
                self.services.clock.unix_seconds(),
            )?;
            (
                tx,
                Payload::encode_parts(
                    tx,
                    &local.keys,
                    &cert,
                    base.record.generation,
                    base.hash,
                    &local.contacts,
                ),
            )
        };
        let mut state = self.copy_state().ok_or_else(super::wire::invalid)?;
        let AccountState::Active(local) = &mut state else {
            unreachable!()
        };
        if let Some(t) = local
            .pairs
            .existing
            .iter_mut()
            .find(|t| t.envelope().is_ok_and(|e| e.tx == tx))
        {
            t.approved_flow = flow;
            t.approved_hash = handshake;
        } else {
            local.pairs.existing.push(PairTransaction {
                tx,
                pin: ExistingPin {
                    peer: local.keys.peer_id(),
                    installation: local.device.installation_id(),
                    transport: local.device.transport_public_key(),
                },
                payload: Zeroizing::new(payload.to_vec()),
                approved_flow: flow,
                approved_hash: handshake,
                stage: Stage::DisclosurePossible,
                receipt: None,
                publication: None,
            });
        }
        local.revision = self.revision + 1;
        // Any ambiguous Store error closes the actor; no permit or first frame exists yet.
        self.commit_pair_state(state, request).await?;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.pair_checkpoint(super::PairTestStop::EAfterDisclosure)?;
        if !self.local_authorized() {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        let t = self
            .local()?
            .pairs
            .existing
            .iter()
            .find(|t| t.envelope().is_ok_and(|e| e.tx == tx))
            .ok_or_else(super::wire::invalid)?;
        let permit = self
            .pair
            .current
            .as_ref()
            .ok_or_else(super::wire::invalid)?
            .committed_permit(
                t,
                self.local()?
                    .records
                    .accepted
                    .as_ref()
                    .ok_or_else(super::wire::invalid)?,
            )?;
        let envelope = t.envelope()?;
        let clock = self.services.clock.clone();
        let mut session = self.pair.current.take().ok_or_else(super::wire::invalid)?;
        session.transaction = Some(tx);
        session.disclosure_possible = true;
        session.phase = Some(super::PairPhase::DisclosurePossible);
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let stop_first = self.pair.stop == Some(super::PairTestStop::EAfterFirstChunk);
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if stop_first {
            self.pair.stop = None;
        }
        let mut sent = 0;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let mut reached_first = false;
        let result = self
            .service(
                request,
                Box::pin(async {
                    sent += session
                        .send_gated(&*clock, &permit, || Zeroizing::new(envelope.bytes()))
                        .await?;
                    for (index, bytes) in payload.chunks(transfer::CHUNK).enumerate() {
                        sent += session
                            .send_gated(&*clock, &permit, || {
                                transfer::chunk(tx, index as u64, bytes)
                            })
                            .await?;
                        #[cfg(all(feature = "test-hooks", debug_assertions))]
                        if stop_first {
                            reached_first = true;
                            return Err(unavailable(ErrorCode::NotAvailable));
                        }
                    }
                    Ok(())
                }),
            )
            .await;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            self.pair.bytes.0 += sent;
            if reached_first {
                self.pair.reached = Some(super::PairTestStop::EAfterFirstChunk);
            }
        }
        let _ = sent;
        if result.is_ok() && session.live(&*clock) {
            self.pair.current = Some(session);
        }
        result
    }
    async fn pair_plain(&mut self, flow: FlowId, plain: &[u8]) -> Result<(), CoreError> {
        use super::pair::{
            transaction::{CandidatePinned, PendingImport},
            transfer::{Envelope, Payload, Receipt, Receiving},
            wire::Role,
        };
        let request = self.ready()?;
        let s = self
            .pair
            .current
            .as_ref()
            .ok_or_else(super::wire::invalid)?;
        if s.role == Role::Existing {
            return self.accept_receipt(flow, plain).await;
        }
        let pin = super::pair::resume::existing(
            &s.channel.as_ref().ok_or_else(super::wire::invalid)?.remote,
        )?;
        if s.receiving.is_none() {
            let envelope = Envelope::decode(plain)?;
            super::pair::resume::envelope_matches(
                self.state.as_ref().ok_or_else(super::wire::invalid)?,
                pin,
                envelope,
            )?;
            if let Some(expected) = s.qr.resume
                && expected != envelope.tx
            {
                return Err(super::wire::invalid());
            }
            if matches!(self.state, Some(AccountState::Candidate(_))) {
                let Some(AccountState::Candidate(device)) = self.copy_state() else {
                    unreachable!()
                };
                self.commit_pair_state(
                    AccountState::CandidatePinned(Box::new(CandidatePinned {
                        device,
                        revision: self.revision + 1,
                        pin,
                        envelope,
                    })),
                    request,
                ).await?;
            }
            let s = self
                .pair
                .current
                .as_mut()
                .ok_or_else(super::wire::invalid)?;
            s.transaction = Some(envelope.tx);
            s.receiving = Some(Receiving::new(envelope));
            return Ok(());
        }
        let s = self
            .pair
            .current
            .as_mut()
            .ok_or_else(super::wire::invalid)?;
        let Some(payload) = s
            .receiving
            .as_mut()
            .ok_or_else(super::wire::invalid)?
            .push(plain)?
        else {
            return Ok(());
        };
        let parsed = Payload::decode(&payload)?;
        if Some(parsed.tx) != s.transaction {
            return Err(super::wire::invalid());
        }
        let device = self
            .state
            .as_ref()
            .ok_or_else(super::wire::invalid)?
            .device();
        let kind = if cfg!(any(target_os = "ios", target_os = "android")) {
            super::DeviceKind::Mobile
        } else {
            super::DeviceKind::Desktop
        };
        if parsed.keys.peer_id() != pin.peer
            || !parsed.matches_device(device)
            || parsed.cert.kind != kind
            || parsed.cert.created_at > self.services.clock.unix_seconds().saturating_add(300)
        {
            return Err(super::wire::invalid());
        }
        let channel = s.channel.as_ref().ok_or_else(super::wire::invalid)?;
        let receipt = Receipt::sign(
            parsed.tx,
            super::pair::transfer::hash(&payload),
            device,
            flow,
            channel.hash,
        );
        let accepted = channel
            .existing_record
            .clone()
            .ok_or_else(super::wire::invalid)?;
        if accepted.record.generation != parsed.generation {
            return Err(super::wire::invalid());
        }
        match self.state.as_ref().ok_or_else(super::wire::invalid)? {
            AccountState::PendingImport(p) if p.payload != payload => {
                return Err(super::wire::invalid());
            }
            AccountState::Active(l)
                if l.pairs
                    .origin
                    .as_ref()
                    .is_none_or(|o| o.hash != receipt.payload_hash || o.tx != receipt.tx) =>
            {
                return Err(super::wire::invalid());
            }
            _ => {}
        }
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.pair_checkpoint(super::PairTestStop::NBeforeImport)?;
        let state = self.copy_state().ok_or_else(super::wire::invalid)?;
        let state = match state {
            AccountState::CandidatePinned(p) => {
                let records = RecordState {
                    generation: parsed.generation,
                    observed_seq: accepted.seq,
                    accepted: Some(accepted),
                    conflict: None,
                    allocated_seq: 0,
                    tombstones: vec![],
                    pending: vec![],
                    state: PublicationState::LocalBlocked,
                };
                AccountState::PendingImport(Box::new(PendingImport {
                    device: p.device,
                    revision: self.revision + 1,
                    pin,
                    payload,
                    receipt: receipt.clone(),
                    records,
                }))
            }
            AccountState::PendingImport(mut p) => {
                p.receipt = receipt.clone();
                p.revision = self.revision + 1;
                AccountState::PendingImport(p)
            }
            AccountState::Active(mut l) => {
                l.pairs.origin.as_mut().unwrap().receipt = receipt.clone();
                l.revision = self.revision + 1;
                AccountState::Active(l)
            }
            _ => return Err(super::wire::invalid()),
        };
        // The exact import and newly channel-bound receipt become durable together.
        self.commit_pair_state(state, request).await?;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.pair_checkpoint(super::PairTestStop::NAfterImport)?;
        let clock = self.services.clock.clone();
        let mut session = self.pair.current.take().ok_or_else(super::wire::invalid)?;
        session.receiving = None;
        session.phase = Some(if matches!(self.state, Some(AccountState::Active(_))) {
            super::PairPhase::Active
        } else {
            super::PairPhase::ReceiptReady
        });
        let result = self
            .service(request, Box::pin(session.send_receipt(&*clock, &receipt)))
            .await;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if let Ok(bytes) = result {
            self.pair.bytes.1 += bytes;
        }
        if result.is_ok() && session.live(&*clock) {
            self.pair.current = Some(session);
        }
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.pair_checkpoint(super::PairTestStop::NAfterAck)?;
        result.map(|_| ())
    }
    async fn accept_receipt(&mut self, flow: FlowId, plain: &[u8]) -> Result<(), CoreError> {
        use super::pair::{
            transaction::Stage,
            transfer::{Payload, Receipt},
        };
        let request = self.ready()?;
        let r = Receipt::decode(plain)?;
        let s = self
            .pair
            .current
            .as_ref()
            .ok_or_else(super::wire::invalid)?;
        let c = s.channel.as_ref().ok_or_else(super::wire::invalid)?;
        if !s.approved
            || s.transaction != Some(r.tx)
            || r.flow != flow
            || r.handshake != c.hash
            || r.installation != c.remote.installation
        {
            return Err(super::wire::invalid());
        }
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.pair_checkpoint(super::PairTestStop::EBeforeReceipt)?;
        let index = self
            .local()?
            .pairs
            .existing
            .iter()
            .position(|t| {
                t.envelope()
                    .is_ok_and(|e| e.tx == r.tx && e.hash == r.payload_hash)
            })
            .ok_or_else(super::wire::invalid)?;
        let mut state = self.copy_state().ok_or_else(super::wire::invalid)?;
        let AccountState::Active(l) = &mut state else {
            unreachable!()
        };
        let t = &mut l.pairs.existing[index];
        t.receipt = Some(r);
        if t.stage == Stage::DisclosurePossible {
            t.stage = Stage::ReceiptVerified;
        }
        l.revision = self.revision + 1;
        self.commit_pair_state(state, request).await?;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.pair_checkpoint(super::PairTestStop::EAfterReceipt)?;
        if let Some(s) = &mut self.pair.current {
            s.phase = Some(super::PairPhase::ReceiptVerified);
        }
        // Fresh lookup after receipt commit rechecks E's membership/base/capacity.
        self.publication(Action::Refresh).await?;
        if !self.local_authorized() {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        let local = self.local()?;
        let t = &local.pairs.existing[index];
        let p = Payload::decode(&t.payload)?;
        let mut records = local.records.clone();
        let included = records.accepted.as_ref().is_some_and(|a| {
            a.record.generation == p.generation
                && a.record.devices.iter().any(|d| {
                    d.installation_id == p.cert.installation_id
                        && d.transport_public_key == p.cert.transport_public_key
                        && d.kind == p.cert.kind
                })
        });
        let packet = if included {
            records.accepted.as_ref().unwrap().hash
        } else if let Some(h) = t.publication {
            if !records
                .pending
                .iter()
                .any(|i| i.signed.hash == h && i.status == PacketStatus::Ready)
            {
                return Err(unavailable(ErrorCode::RecordConflict));
            }
            h
        } else {
            records.stage_add(
                local,
                &p.cert,
                p.base,
                p.generation,
                self.services.clock.unix_seconds(),
            )?
        };
        let mut state = self.copy_state().ok_or_else(super::wire::invalid)?;
        let AccountState::Active(l) = &mut state else {
            unreachable!()
        };
        l.records = records;
        l.pairs.existing[index].publication = Some(packet);
        l.pairs.existing[index].stage = if included {
            Stage::Observed
        } else {
            Stage::PublishPending
        };
        l.revision = self.revision + 1;
        self.commit_pair_state(state, request).await?;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.pair_checkpoint(super::PairTestStop::EBeforePublish)?;
        if !included {
            self.publication(Action::Refresh).await?;
            self.publication(Action::Refresh).await?;
        }
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.pair_checkpoint(super::PairTestStop::EAfterPublish)?;
        let observed = self
            .local()?
            .records
            .accepted
            .as_ref()
            .is_some_and(|a| a.hash == packet);
        if observed {
            let mut state = self.copy_state().ok_or_else(super::wire::invalid)?;
            let AccountState::Active(l) = &mut state else {
                unreachable!()
            };
            l.pairs.existing[index].stage = Stage::Observed;
            l.revision = self.revision + 1;
            self.commit_pair_state(state, request).await?;
        }
        if let Some(s) = &mut self.pair.current {
            s.phase = Some(if observed {
                super::PairPhase::Observed
            } else {
                super::PairPhase::PublishPending
            });
        }
        Ok(())
    }
    async fn refresh_import(&mut self) -> Result<(), CoreError> {
        use super::pair::{transaction::PairState, transfer::Payload};
        let request = self.ready()?;
        let Some(AccountState::PendingImport(p)) = &self.state else {
            return Err(unavailable(ErrorCode::AccountRestricted));
        };
        let pin = p.pin;
        let backend = self.services.backend()?;
        let super::Lookup::Found(packet) = self.service(request, backend.lookup(pin.peer)).await?
        else {
            return Err(unavailable(ErrorCode::RecordUnavailable));
        };
        let found = super::publication::SignedRecord::new(packet, pin.peer)?;
        let now = self.services.clock.unix_seconds();
        let Some(AccountState::PendingImport(p)) = &self.state else {
            unreachable!()
        };
        let payload = Payload::decode(&p.payload)?;
        let conflicting = found.seq < p.records.observed_seq
            || p.records
                .accepted
                .as_ref()
                .is_some_and(|a| a.seq == found.seq && a.hash != found.hash)
            || found.record.generation != payload.generation
            || p.records.conflict.is_some();
        let current = found.current(now) && !found.record.is_expired(now);
        let included = found.record.devices.iter().any(|d| {
            d.installation_id == payload.cert.installation_id
                && d.transport_public_key == payload.cert.transport_public_key
                && d.kind == payload.cert.kind
        });
        let existing = found.record.devices.iter().any(|d| {
            d.installation_id == pin.installation && d.transport_public_key == pin.transport
        });
        let Some(AccountState::PendingImport(mut p)) = self.copy_state() else {
            unreachable!()
        };
        p.records.observed_seq = p.records.observed_seq.max(found.seq);
        p.revision = self.revision + 1;
        if conflicting {
            p.records.conflict = Some(found);
            p.records.state = PublicationState::Conflict;
            self.commit_pair_state(AccountState::PendingImport(p), request).await?;
            return Err(unavailable(ErrorCode::RecordConflict));
        }
        p.records.accepted = Some(found);
        p.records.state = PublicationState::LocalBlocked;
        if !current || !included || !existing {
            self.commit_pair_state(AccountState::PendingImport(p), request).await?;
            if let Some(s) = &mut self.pair.current {
                s.phase = Some(super::PairPhase::PublishPending);
            }
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        let origin = p.origin()?;
        let mut payload = Payload::decode(&p.payload)?;
        let keys = super::AccountKeys::from_identity(super::Secret32::from(
            *payload.keys.identity().as_bytes(),
        ));
        p.records.state = PublicationState::Observed;
        let local = super::storage::schema::LocalAccount {
            keys,
            device: p.device,
            cert: payload.cert.clone(),
            revision: self.revision + 1,
            intent: PublishIntent::LocalCreate,
            records: p.records,
            remote: vec![],
            contacts: std::mem::take(&mut payload.contacts),
            pairs: PairState {
                existing: vec![],
                origin: Some(origin),
            },
        };
        self.commit_pair_state(AccountState::Active(Box::new(local)), request).await?;
        // Authority can expire while the SQL worker commits.
        // The committed Active snapshot is established/non-discardable, but
        // cannot be exposed as successful activation on stale authority.
        if !self.local_authorized() {
            self.pair.current = None;
            self.close_auth();
            self.shared.network.block();
            self.shared.status.fetch_max((request.ticket << 2) | 2, Ordering::AcqRel);
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        if let Some(s) = &mut self.pair.current {
            s.phase = Some(super::PairPhase::Active);
        }
        Ok(())
    }
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<Queued>,
        mut requests: watch::Receiver<Request>,
    ) {
        let initial = *requests.borrow_and_update();
        let result = self.lifecycle(initial).await;
        self.complete(initial, result);
        self.auto_refresh().await;
        self.publish_view();
        let mut expiry = tokio::time::interval(Duration::from_millis(100));
        let mut import_poll = tokio::time::interval(Duration::from_secs(1));
        let mut refresh = tokio::time::interval(Duration::from_secs(60));
        refresh.tick().await;
        let message_wake = self.services.clock.wake().unwrap_or_default();
        loop {
            tokio::select! { biased;
                changed = requests.changed() => {
                    if changed.is_err() { break; }
                    let request = *requests.borrow_and_update();
                    let result = self.lifecycle(request).await;
                    self.complete(request, result);
                    self.auto_refresh().await;
                }
                // Coalesce overdue maintenance: a slow SQL turn must leave commands a ready window.
                _ = expiry.tick() => { self.restrict_grants(); self.expire_pair(); self.poll_messages().await; expiry.reset(); }
                _ = message_wake.notified() => { self.restrict_grants(); self.expire_pair(); self.poll_messages().await; }
                _ = import_poll.tick(), if self.pair.current.is_some() && matches!(self.state,Some(AccountState::PendingImport(_))) => {let _=self.refresh_import().await;}
                Some(input)=self.pair.input.recv()=>{self.pair_input(input).await;}
                Some(input) = self.auth.input.recv() => { self.auth_input(input).await; }
                _ = refresh.tick() => { self.auto_refresh().await; }
                command = commands.recv() => {
                    let Some(queued) = command else { break; };
                    if !matches!(queued.command, Command::Snapshot(_)) && queued.ticket != self.shared.requests.borrow().ticket {
                        queued.command.reject(unavailable(ErrorCode::StorageLocked));
                        continue;
                    }
                    match queued.command {
                        Command::Message(operation, reply) => { let result = self.message_request(operation).await; let _ = reply.send(result); }
                        #[cfg(all(feature="test-hooks",debug_assertions))]
                        Command::MessageProofWriteStarts(reply)=>{let _=reply.send(Ok(self.message_proof_write_starts()));}
                        #[cfg(all(feature = "test-hooks", debug_assertions))]
                        Command::MessageWriteStarts(reply)=>{let _=reply.send(Ok(self.message_write_starts()));}
                        #[cfg(all(feature="test-hooks",debug_assertions))]
                        Command::MessagePause(response,reply)=>{let _=reply.send(Ok(self.message_pause(response)));}
                        #[cfg(all(feature="test-hooks",debug_assertions))]
                        Command::MessageAckPause(key,reply)=>{let _=reply.send(self.message_ack_pause(key));}
                        #[cfg(all(feature="test-hooks",debug_assertions))]
                        Command::MessageAttempt(key,receipt,mutation,reply)=>{self.message_attempt(key,receipt,mutation,reply).await;}
                        Command::Retry(reply) => { let result = self.retry_open(); let _ = reply.send(result); }
                        #[cfg(all(feature="test-hooks",debug_assertions))]
                        Command::PairTestStop(stop,reply)=>{self.pair.stop=Some(stop);self.pair.reached=None;let _=reply.send(Ok(()));}
                        #[cfg(all(feature="test-hooks",debug_assertions))]
                        Command::PairTestBytes(reply)=>{let _=reply.send(Ok(self.pair.bytes));}
                        #[cfg(all(feature="test-hooks",debug_assertions))]
                        Command::PairTestReached(reply)=>{let _=reply.send(Ok(self.pair.reached));}
                        Command::DiscardPending(reply)=>{let result=self.discard_pending().await;let _=reply.send(result);}
                        Command::PairBegin(direction,resume,reply)=>{let result=self.pair_begin(direction,resume).await;let _=reply.send(result);}
                        Command::PairScan(qr,reply)=>{let result=self.pair_scan(&qr).await;let _=reply.send(result);}
                        Command::PairConfirm(flow,matches,reply)=>{let result=self.pair_confirm(flow,matches).await;let _=reply.send(result);}
                        Command::PairApprove(flow,reply)=>{let result=self.pair_approve(flow).await;let _=reply.send(result);}
                        Command::PairCancel(flow,reply)=>{let result=self.pair_cancel(flow);let _=reply.send(result);}
                        Command::PairView(reply)=>{self.expire_pair();let result=self.ready().map(|_|self.pair_view());let _=reply.send(result);}
                        #[cfg(all(feature="test-hooks",debug_assertions))]
                        Command::PairFixtureSend(flow,bytes,reply)=>{let result=self.pair_fixture_send(flow,bytes).await;let _=reply.send(result);}
                        #[cfg(all(feature="test-hooks",debug_assertions))]
                        Command::PairFixtureReceived(flow,reply)=>{let result=self.pair_session(flow).map(|s|s.received.to_vec());let _=reply.send(result);}

                        Command::CreateInvite(name,reply) => {let result=self.create_invite(name);let _=reply.send(result);}
                        Command::AcceptInvite(input,alias,reply) => {let result=self.accept_invite(&input,alias).await;let _=reply.send(result);}
                        Command::Contacts(reply) => {let result=self.contact_views();let _=reply.send(result);}
                        Command::ConfirmSafety(peer,digest,reply) => {let result=self.confirm_safety(peer,digest).await;let _=reply.send(result);}
                        Command::Connect(peer,reply) => {let result=self.connect(peer).await;let _=reply.send(result);}
                        Command::Prepare(reply) => { let result = self.prepare().await; let _ = reply.send(result); }
                        Command::Begin(reply) => { let result = self.begin(); let _ = reply.send(result); }
                        Command::Confirm(flow, answers, reply) => { let result = self.confirm(flow, &answers).await; let _ = reply.send(result); }
                        Command::Cancel(flow, reply) => { let result = self.cancel(flow); let _ = reply.send(result); }
                        Command::Recover(words, confirmed, reply) => { let result = self.recover(&words, confirmed).await; let _ = reply.send(result); }
                        Command::Snapshot(reply) => { let _ = reply.send(Ok(self.snapshot_view())); }
                        Command::Refresh(peer, reply) => {
                            let result = if matches!(&self.state,Some(AccountState::PendingImport(p)) if p.pin.peer==peer) {
                                self.refresh_import().await
                            } else if self.local().is_ok_and(|local| local.keys.peer_id() == peer) {
                                self.publication(Action::Refresh).await
                            } else { self.remote_record(peer).await.map(|_|()) };
                            let _ = reply.send(result);
                        }
                        Command::Revoke(target, reply) => { let result = self.publication(Action::Revoke(target)).await; let _ = reply.send(result); }
                        Command::ResolveRemote(peer, hash, reply) => { let result = self.confirm_remote(peer, hash).await; let _ = reply.send(result); }
                        Command::RepublishAccepted(hash, reply) => { let result = self.republish_accepted_record(hash).await; let _ = reply.send(result); }
                        Command::Resolve(hash, reply) => { let result = self.publication(Action::Confirm(hash)).await; let _ = reply.send(result); }
                    }
                }
            }
            self.publish_view();
        }
        self.pair.current=None;
        self.auth.cancel.cancel();
        self.close_auth();
        let _ = self.store.suspend().await;
    }
    fn current(&self, request: Request) -> bool {
        self.shared.requests.borrow().ticket == request.ticket
    }
    fn complete(&mut self, request: Request, result: Result<(), CoreError>) {
        if self.current(request) {
            self.lock_error = result.as_ref().err().map(|error| error.code);
        }
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
        self.pair.current=None;
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
        if !self.current(request) { return Err(unavailable(ErrorCode::StorageLocked)); }
        self.store.suspend().await?;
        if !self.current(request) { return Err(unavailable(ErrorCode::StorageLocked)); }
        if request.suspend {
            return stopped.map_err(|_| unavailable(ErrorCode::NetworkTimeout))?;
        }
        stopped.map_err(|_| unavailable(ErrorCode::NetworkTimeout))??;
        if !self.current(request) {
            return Err(unavailable(ErrorCode::Busy));
        }
        self.store.resume().await?;
        if !self.current(request) { return Err(unavailable(ErrorCode::StorageLocked)); }
        if self.cleanup_pending{return Err(unavailable(ErrorCode::StorageLocked));}
        let loaded = self
            .store
            .load_account().await
            .and_then(|state| state.as_ref().map(AccountState::decode).transpose());
        match loaded {
            Ok(state) => {
                if !self.current(request) { return Err(unavailable(ErrorCode::StorageLocked)); }
                if let Some(AccountState::Active(local)) = &state {
                    self.revision = self.revision.max(local.revision);
                }
                self.state = state;
            }
            Err(error) => {
                if self.current(request) { self.store.suspend().await?; }
                return Err(error);
            }
        }
        self.reset_unlaunched(request).await?;
        self.restore_message_scheduler(request).await?;
        let result = self.restore(request).await;
        if result.is_err() && self.current(request) {
            self.store.suspend().await?;
        }
        result?;
        if !self.current(request) { return Err(unavailable(ErrorCode::StorageLocked)); }
        Ok(())
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
            self.persist(&state, request).await?;
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
        self.shared.network.install_key(key, true, self.shared.network.block()).await?;
        self.ensure_ready(request)?;
        Ok(candidate)
    }
    fn ready(&self) -> Result<Request, CoreError> {
        let request = *self.shared.requests.borrow();
        if self.cleanup_pending
            || request.suspend
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
        if matches!(self.state, Some(AccountState::Active(_) | AccountState::CandidatePinned(_) | AccountState::PendingImport(_))) {
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
        self.persist(&state, request).await?;
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
            .install_key(iroh::SecretKey::from_bytes(device.transport().as_bytes()), true, self.shared.network.block())
            .await?;
        self.ensure_ready(request)?;
        self.auto_refresh().await;
        Ok(())
    }
    async fn persist(&mut self, state: &AccountState, request: Request) -> Result<(), CoreError> {
        self.ensure_ready(request)?;
        // Observed/proposed authority changes invalidate off-actor proofs before
        // the SQL await, including record publication, revocation and pair state.
        self.restrict_message_acquisitions(match state {
            AccountState::Active(local) => Some(local),
            _ => None,
        });
        if let Err(error) = self.store.commit_account(state.encode()).await {
            // An error can follow atomic replacement. Do not trust the old
            // in-memory state or permit another setup to overwrite the new one.
            // A new suspend/resume cycle must reload and validate the actual disk.
            self.close_auth();
            if self.current(request) {
                let ticket = self.shared.network.block();
                self.store.invalidate(ticket);
                self.lock_error = Some(error.code);
                self.shared.status.fetch_max((request.ticket << 2) | 2, Ordering::AcqRel);
            }
            return Err(error);
        }
        self.ensure_ready(request)
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
    async fn commit_records(&mut self, records: RecordState, request: Request) -> Result<(), CoreError> {
        self.ensure_ready(request)?;
        let old_accepted = self.local()?.records.accepted.as_ref().map(|r| r.hash);
        if self.local()?.records.encode() == records.encode() {
            return Ok(());
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| unavailable(ErrorCode::StorageFull))?;
        let mut state = self.copy_state().unwrap();
        let AccountState::Active(local) = &mut state else {
            unreachable!()
        };
        local.records = records;
        for t in &mut local.pairs.existing {
            if t.stage == super::pair::transaction::Stage::PublishPending
                && local.records.accepted.as_ref().is_some_and(|a|Some(a.hash)==t.publication)
            {t.stage=super::pair::transaction::Stage::Observed;}
        }
        local.revision = revision;
        let result = self.persist(&state, request).await;
        if result.is_ok() {
            self.state = Some(state);
            self.revision = revision;
        }
        self.restrict_grants();
        if result.is_ok()
            && self.local()?.records.conflict.is_none()
            && self.local()?.records.state != PublicationState::Conflict
            && let Some(record) = &self.local()?.records.accepted
            && Some(record.hash) != old_accepted
        {
            self.auth
                .registry
                .sync_record(record, self.auth.cancel.child_token());
        }
        result
    }
    async fn ensure_packet_time(
        &mut self,
        signed: &super::publication::SignedRecord,
        request: Request,
    ) -> Result<(), CoreError> {
        self.ensure_ready(request)?;
        let local = self.local()?;
        if !local.records.can_publish(&signed.record, local) {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        let now = self.services.clock.unix_seconds();
        if !signed.current(now) || signed.record.is_expired(now) {
            let mut records = self.local()?.records.clone();
            records.state = PublicationState::LocalBlocked;
            self.commit_records(records, request).await?;
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        Ok(())
    }
    fn accepted_republish_packet(
        &self,
        accepted_hash: [u8; 32],
    ) -> Result<&pkarr::SignedPacket, CoreError> {
        self.ready()?;
        let local = self.local()?;
        let records = &local.records;
        let accepted = records.accepted.as_ref()
            .ok_or_else(|| unavailable(ErrorCode::AccountRestricted))?;
        if accepted.hash != accepted_hash {
            return Err(unavailable(ErrorCode::RecordConflict));
        }
        let now = self.services.clock.unix_seconds();
        if records.state != PublicationState::Conflict
            || !records.pending.is_empty()
            || accepted.seq < records.observed_seq
            || accepted.seq < records.allocated_seq
            || !accepted.current(now)
            || accepted.record.is_expired(now)
            || !records.can_publish(&accepted.record, local)
            || records.conflict.as_ref().is_none_or(|conflict| {
                conflict.seq >= accepted.seq
                    || conflict.record.generation != accepted.record.generation
                    || conflict.record.devices != accepted.record.devices
            })
            // Keep the stored expected-account signature/shape rules at this boundary.
            || !super::publication::SignedRecord::new(accepted.packet.clone(), local.keys.peer_id())
                .is_ok_and(|verified| {
                    verified.hash == accepted.hash
                        && verified.seq == accepted.seq
                        && verified.record == accepted.record
                })
        {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        Ok(&accepted.packet)
    }
    async fn republish_accepted_record(&mut self, accepted_hash: [u8; 32]) -> Result<(), CoreError> {
        let request = self.ready()?;
        let packet = self.accepted_republish_packet(accepted_hash)?.clone();
        let backend = self.services.backend()?;
        self.ensure_ready(request)?;
        self.accepted_republish_packet(accepted_hash)?;
        let result = self.service(request, backend.publish(&packet)).await;
        self.ensure_ready(request)?;
        let eligible = self.accepted_republish_packet(accepted_hash).map(|_| ());
        // Backend failures retain their codes; even a successful ACK grants no authority.
        result?;
        eligible
    }
    async fn publication(&mut self, action: Action) -> Result<(), CoreError> {
        let request = self.ready()?;
        let local = self.local()?;
        if local.records.removed(local) {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
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
        let local = self.local()?;
        let now = self.services.clock.unix_seconds();
        if local.records.pending.is_empty()
            && Self::local_authorized_at(local, now)
            && let Ok(super::Lookup::Found(packet)) = &lookup
            && let Ok(candidate) = super::publication::SignedRecord::new(packet.clone(), peer)
            && Self::stale_lookup(&candidate, local.records.accepted.as_ref().unwrap(), now)
        {
            // This lookup supplies no fresh publication base. Keep valid local
            // authority without observing the old packet or persisting changes.
            return Err(unavailable(ErrorCode::RecordUnavailable));
        }
        let mut records = self.local()?.records.clone();
        let result = match lookup {
            Ok(lookup) => records.reconcile(
                lookup,
                self.local()?,
                self.services.clock.unix_seconds(),
                action,
            ),
            Err(error) => {
                if records.conflict.is_none() && records.state != PublicationState::Conflict {
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
        self.commit_records(records, request).await?;
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
                self.ensure_packet_time(&intent.signed, request).await?;
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
                self.commit_records(records, request).await?;
                self.auth.registry.1.lock().unwrap().remove(&revoke.nonce);
            }
            // Only the exact committed packet can be retried at its sequence.
            self.ensure_packet_time(&intent.signed, request).await?;
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
                    self.commit_records(records, request).await?;
                }
                return Err(error);
            }
        }
        // A publish ACK is deliberately not an observation or completion.
        Ok(())
    }
    fn retry_open(&mut self) -> Result<u64, CoreError> {
        let previous = *self.shared.requests.borrow();
        if self.cleanup_pending || previous.suspend || self.shared.network.desired_suspend() {
            return Err(unavailable(ErrorCode::StorageLocked));
        }
        if self.ready().is_ok() { return Ok(previous.ticket); }
        let ticket = self.shared.network.block();
        let accepted = self.shared.requests.send_if_modified(|request| {
            if request.ticket != previous.ticket || request.suspend || self.shared.network.desired_suspend() { return false; }
            self.store.invalidate(ticket);
            *request = Request { ticket, suspend: false };
            self.shared.status.fetch_max(ticket << 2, Ordering::AcqRel);
            true
        });
        if accepted { Ok(ticket) } else { Err(unavailable(ErrorCode::StorageLocked)) }
    }
    fn snapshot_view(&mut self) -> AccountView {
        self.expire_pair();
        self.publish_view();
        // Native readiness, clocks, and connection grants may change after publication.
        // A revision always returns the exact projection cached for its event.
        self.last_public_view.as_ref().expect("published account projection").clone()
    }
    fn publish_view(&mut self) {
        let mut view = self.view();
        if self.last_public_view.as_ref() == Some(&view) { return; }
        // A process owns this ordering independently of durable transaction counters.
        self.public_revision = self.public_revision.checked_add(1).expect("public revision exhausted");
        view.revision = self.public_revision;
        self.last_public_view = Some(view.clone());
        self.bus.publish(crate::CoreEvent::AccountChanged { revision: view.revision, view: Box::new(view) });
    }
    fn view(&self) -> AccountView {
        let mut view = AccountView {
            disclosure_possible: self.state.as_ref().is_some_and(|s|matches!(s,AccountState::Active(l) if !l.pairs.existing.is_empty())),
            pending_pair_transactions: self.pending_pairs(),
            pair_resumes: self.pair_resumes(),
            pending_import_requires_remote_revoke: matches!(self.state,Some(AccountState::CandidatePinned(_) | AccountState::PendingImport(_))),
            revision: self.public_revision,
            lock_error: None,
            phase: AccountPhase::Unconfigured,
            public_peer_id: None,
            installation: None,
            devices: vec![],
            contacts: vec![],
            remote_records: vec![],
            pair: self.pair_view(),
            publish_intents: vec![],
            publication: PublicationView::default(),
        };
        if self.ready().is_err() {
            view.phase = AccountPhase::Locked;
            view.lock_error = Some(self.lock_error.unwrap_or(ErrorCode::StorageLocked));
            view.pair = None;
            return view;
        }
        match &self.state {
            Some(AccountState::CandidatePinned(p))=>{
                view.phase=AccountPhase::PendingImport;view.installation=Some(p.device.installation_id());view.public_peer_id=Some(p.pin.peer);
            },
            Some(AccountState::PendingImport(p))=>{
                view.phase=AccountPhase::PendingImport;view.installation=Some(p.device.installation_id());view.public_peer_id=Some(p.pin.peer);view.publication=p.records.view();
            },
            Some(AccountState::Candidate(device)) => {
                view.phase = AccountPhase::Candidate;
                view.installation = Some(device.installation_id());
            }
            Some(AccountState::Active(local)) => {
                view.phase = match local.intent {
                    PublishIntent::LocalCreate => AccountPhase::LocalReady,
                    PublishIntent::Replacement => AccountPhase::PublishPending,
                };
                view.public_peer_id = Some(local.keys.peer_id());
                view.installation = Some(local.device.installation_id());
                view.devices = local.records.accepted.as_ref().map(|r| r.record.devices.clone()).unwrap_or_else(|| vec![super::RecordDevice {
                    installation_id: local.cert.installation_id,
                    transport_public_key: local.cert.transport_public_key,
                    kind: local.cert.kind,
                }]);
                view.contacts = local.contacts.iter().map(|c| self.contact_view(c)).collect();
                view.remote_records = local.remote.iter().map(|r| super::RemoteRecordView {
                    peer: r.peer,
                    accepted_generation: r.accepted.record.generation,
                    conflict_hash: r.conflict.as_ref().map(|c| c.hash),
                    conflict: r.conflict.as_ref().map(|c| c.record.clone()),
                    cached: self.auth.registry.0.lock().unwrap().iter().any(|s| s.grant.peer == r.peer && s.cached && s.ready && s.conn.close_reason().is_none() && self.grant_authorized(&s.grant)),
                }).collect();
                view.publish_intents.push(local.intent);
                view.publication = local.records.view();
                view.publication.can_republish_accepted = local.records.accepted.as_ref()
                    .is_some_and(|accepted| self.accepted_republish_packet(accepted.hash).is_ok());
                match local.records.state {
                    PublicationState::Observed if self.local_authorized() => view.phase = AccountPhase::Active,
                    PublicationState::Observed => view.publication.state = PublicationState::LocalBlocked,
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

#[cfg(test)]
mod stale_lookup_tests;

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
    async fn explicit_retry_reloads_failed_foreground_without_overriding_suspend() {
        use std::sync::atomic::AtomicU8;
        struct Flaky(AtomicU8);
        impl KeyProtector for Flaky {
            fn protect(&self, id: StoreId, key: &Secret32) -> Result<Vec<u8>, CoreError> { Protector.protect(id, key) }
            fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
                match self.0.load(Ordering::SeqCst) {
                    1 => Err(unavailable(ErrorCode::StorageMissing)),
                    2 => Err(unavailable(ErrorCode::StorageCorrupt)),
                    _ => Protector.unprotect(id, blob),
                }
            }
            fn delete(&self, _: StoreId) -> Result<(), CoreError> { Ok(()) }
        }
        let dir = tempfile::tempdir().unwrap(); let slot = CoreSlot::new();
        slot.start(CoreConfig { data_dir: dir.path().into() }, &EventBus::new()).unwrap();
        slot.network().unwrap().configure_local(None).await.unwrap();
        let protector = Arc::new(Flaky(AtomicU8::new(0)));
        let account = slot.open_account_with_services(protector.clone(), offline_services()).unwrap();
        account.prepare_candidate().await.unwrap();
        account.request_suspend(); account.snapshot().await.unwrap();
        protector.0.store(1, Ordering::SeqCst);
        account.request_resume();
        let locked = account.snapshot().await.unwrap();
        assert_eq!(locked.lock_error, Some(ErrorCode::StorageMissing));
        protector.0.store(2, Ordering::SeqCst);
        let stale_ticket = account.shared.requests.borrow().ticket;
        account.retry_open().await.unwrap();
        let (reply, rejected) = oneshot::channel();
        account.commands.try_send(Queued { ticket: stale_ticket, command: Command::Begin(reply) }).ok().unwrap();
        assert!(matches!(rejected.await.unwrap(), Err(CoreError { code: ErrorCode::StorageLocked, .. })));
        assert_eq!(account.snapshot().await.unwrap().lock_error, Some(ErrorCode::StorageCorrupt));
        protector.0.store(0, Ordering::SeqCst);
        let retry = account.retry_open();
        tokio::pin!(retry);
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(
            std::future::Future::poll(retry.as_mut(), cx).is_pending()
        )).await);
        // The actor barrier proves Retry's accepted-ticket reply was already sent.
        // Queue directly: a public Snapshot queued before Retry accepts its new
        // ticket is correctly rejected by its own response-consumption fence.
        let (barrier, completed) = oneshot::channel();
        account.commands.try_send(Queued {
            ticket: account.shared.requests.borrow().ticket,
            command: Command::Snapshot(barrier),
        }).ok().unwrap();
        assert_eq!(completed.await.unwrap().unwrap().phase, AccountPhase::Candidate);
        account.request_suspend();
        account.request_resume();
        assert_eq!(retry.await.unwrap_err().code, ErrorCode::StorageLocked);
        assert_eq!(account.snapshot().await.unwrap().phase, AccountPhase::Candidate);
        account.retry_open().await.unwrap(); // Already-ready acknowledgement keeps the same ticket.
        // Native intent can change before its lifecycle mailbox notification arrives.
        account.shared.network.lifecycle_gate(true);
        assert_eq!(account.retry_open().await.unwrap_err().code, ErrorCode::StorageLocked);
        account.request_suspend(); account.snapshot().await.unwrap();
        assert_eq!(account.retry_open().await.unwrap_err().code, ErrorCode::StorageLocked);
        assert_eq!(account.snapshot().await.unwrap().phase, AccountPhase::Locked);
    }

    #[tokio::test]
    async fn snapshot_reuses_published_projection_when_native_readiness_changes() {
        use std::sync::atomic::AtomicBool;
        struct BoundarySink {
            network: NetworkHandle,
            armed: AtomicBool,
            views: std::sync::Mutex<Vec<AccountView>>,
        }
        impl crate::EventSink for BoundarySink {
            fn send(&self, event: crate::CoreEvent) -> bool {
                if let crate::CoreEvent::AccountChanged { revision, view } = event {
                    assert_eq!(revision, view.revision);
                    self.views.lock().unwrap().push(*view);
                    if self.armed.swap(false, Ordering::SeqCst) {
                        // One nonblocking native change at publication/reply boundary.
                        // No actor timer, lifecycle mailbox, or event-bus/core-slot reentry.
                        self.network.lifecycle_gate(true);
                    }
                }
                true
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new();
        let slot = CoreSlot::new();
        slot.start(CoreConfig { data_dir: dir.path().into() }, &bus).unwrap();
        let network = slot.network().unwrap();
        network.configure_local(None).await.unwrap();
        let ticket = network.begin_account();
        let (requests, _) = watch::channel(Request { ticket, suspend: false });
        let root = dir.path().to_owned();
        let (store, _) = tokio::task::spawn_blocking(move || StorageHandle::open_blocking(root, Arc::new(Protector))).await.unwrap().unwrap();
        store.invalidate(ticket);
        let shared = Arc::new(Shared {
            store: store.clone(),
            network: network.clone(),
            requests,
            status: AtomicU64::new((ticket << 2) | 1),
        });
        let sink = Arc::new(BoundarySink {
            network,
            armed: AtomicBool::new(true),
            views: std::sync::Mutex::new(vec![]),
        });
        bus.subscribe(sink.clone());
        let mut manager = Manager {
            messages: Default::default(),
            pair: super::super::pair::session::Runtime::default(),
            auth: super::super::connection::Runtime::default(),
            bus,
            store: store.clone(),
            state: None,
            creation: None,
            cleanup_pending: false,
            revision: 0,
            public_revision: 0,
            last_public_view: None,
            lock_error: None,
            shared,
            services: offline_services(),
        };
        // Exercise exactly the private path used by Command::Snapshot.
        let snapshots = [manager.snapshot_view(), manager.snapshot_view(), manager.snapshot_view()];
        assert!(!sink.armed.load(Ordering::SeqCst));
        assert!(manager.shared.network.desired_suspend());
        let views = sink.views.lock().unwrap();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].phase, AccountPhase::Unconfigured);
        assert_eq!(views[1].phase, AccountPhase::Locked);
        for snapshot in snapshots {
            let equal_revision: Vec<_> = views.iter().filter(|v| v.revision == snapshot.revision).collect();
            assert!(!equal_revision.is_empty());
            for published in equal_revision {
                assert_eq!(&snapshot, published, "one revision must identify exactly one public projection");
            }
        }
    }

    #[tokio::test]
    async fn public_projection_emits_ordered_changes_without_secret_replies() {
        struct Sink(std::sync::Mutex<Vec<String>>);
        impl crate::EventSink for Sink {
            fn send(&self, event: crate::CoreEvent) -> bool {
                self.0.lock().unwrap().push(serde_json::to_string(&event).unwrap()); true
            }
        }
        let dir = tempfile::tempdir().unwrap(); let bus = EventBus::new();
        let sink = Arc::new(Sink(std::sync::Mutex::new(vec![]))); bus.subscribe(sink.clone());
        let slot = CoreSlot::new(); slot.start(CoreConfig { data_dir: dir.path().into() }, &bus).unwrap();
        slot.network().unwrap().configure_local(None).await.unwrap();
        let account = slot.open_account_with_services(Arc::new(Protector), offline_services()).unwrap();
        let before = account.snapshot().await.unwrap();
        let display = account.begin_create().await.unwrap();
        let during = account.snapshot().await.unwrap();
        account.cancel_create(display.flow).await.unwrap();
        let after = account.snapshot().await.unwrap();
        account.prepare_candidate().await.unwrap();
        let qr = account.pair_begin(super::super::PairDirection::NewDisplays, None).await.unwrap();
        let paired = account.snapshot().await.unwrap();
        assert_eq!(paired.pair.as_ref().unwrap().flow, qr.flow);
        assert!(paired.revision > after.revision);
        assert!(during.revision > before.revision && after.revision > during.revision);
        let late = Arc::new(Sink(std::sync::Mutex::new(vec![])));
        bus.subscribe(late.clone());
        assert!(late.0.lock().unwrap().iter().any(|e| e.contains("account_changed")), "late registration must replay latest public projection");
        let events = sink.0.lock().unwrap();
        assert!(events.iter().any(|e| e.contains("account_changed")), "actor must publish public changes");
        let text = events.join("");
        assert!(!text.contains(&display.words.join(" ")));
        assert!(!text.contains(&qr.qr));
        let revisions: Vec<u64> = events.iter().filter_map(|e| {
            let value: serde_json::Value = serde_json::from_str(e).unwrap();
            if value["kind"] != "account_changed" { return None; }
            assert_eq!(value["revision"], value["view"]["revision"]);
            value["revision"].as_u64()
        }).collect();
        assert!(revisions.windows(2).all(|r| r[0] < r[1]));
        assert!(!text.contains("\"words\"") && !text.contains("\"qr\"") && !text.contains("psk"));
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
        let root = dir.path().to_owned();
        let (store, _) = tokio::task::spawn_blocking(move || StorageHandle::open_blocking(root, Arc::new(Protector))).await.unwrap().unwrap();
        store.invalidate(ticket);
        let shared = Arc::new(Shared {
            store: store.clone(),
            network: network.clone(),
            requests,
            status: AtomicU64::new(ticket << 2),
        });
        let backend = Arc::new(Backend {
            absent: AtomicBool::new(false),
            sent: std::sync::Mutex::new(vec![]),
        });
        let mut manager = Manager {
            messages: Default::default(),
            pair: super::super::pair::session::Runtime::default(),
            auth: super::super::connection::Runtime::default(),
            bus: EventBus::new(),
            store: store.clone(),
            state: None,
            creation: None,
                cleanup_pending: false,
            revision: 0,
            public_revision: 0,
            last_public_view: None,
            lock_error: None,
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
        crate::storage_fixture::Fixture::from_handle(manager.store.clone()).fault(crate::storage_fixture::CommitFault::AfterCommit).await;
        let result = manager.publication(Action::Refresh).await;
        assert_eq!(result.unwrap_err().code, ErrorCode::StorageUncertain);
        assert_eq!(manager.view().phase, AccountPhase::Locked);
        assert!(backend.sent.lock().unwrap().is_empty());
        manager.store.resume().await.unwrap();
        let AccountState::Active(saved) =
            AccountState::decode(&manager.store.load_account().await.unwrap().unwrap()).unwrap()
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
            manager.store.invalidate(ticket);
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
    async fn pair_disclosure_and_receipt_after_replace_error_are_durable_but_fail_closed() {
        use crate::account::pair::{
            transaction::{ExistingPin, PairTransaction, PendingImport, Stage},
            transfer::{Payload, Receipt, hash},
        };
        for existing in [true, false] {
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
        let root = dir.path().to_owned();
        let (store, _) = tokio::task::spawn_blocking(move || StorageHandle::open_blocking(root, Arc::new(Protector))).await.unwrap().unwrap();
        store.invalidate(ticket);
            let mut manager = Manager {
            messages: Default::default(),
                pair: Default::default(),
                auth: Default::default(),
                bus: EventBus::new(),
                store: store.clone(),
                state: None,
                creation: None,
                cleanup_pending: false,
                revision: 0,
            public_revision: 0,
            last_public_view: None,
            lock_error: None,
                shared: Arc::new(Shared {
            store: store.clone(),
                    network: network.clone(),
                    requests,
                    status: AtomicU64::new(ticket << 2),
                }),
                services: offline_services(),
            };
            let result = manager.lifecycle(request).await;
            manager.complete(request, result);
            manager.prepare().await.unwrap();
            let mut local = recovery::local_account(
                super::super::derive_account(&Secret32::from([7; 32])).unwrap(),
                PublishIntent::LocalCreate,
                2,
            )
            .unwrap();
            let device = derive_device(Secret32::from([8; 32])).unwrap();
            let cert = super::super::sign_cert(
                &local.keys,
                &device,
                super::super::DeviceKind::Desktop,
                1000,
            )
            .unwrap();
            let tx = super::super::TxId([4; 16]);
            let flow = FlowId([5; 16]);
            let payload = Payload::encode_parts(
                tx,
                &local.keys,
                &cert,
                local.records.generation,
                [3; 32],
                &[],
            );
            let pin = ExistingPin {
                peer: local.keys.peer_id(),
                installation: local.device.installation_id(),
                transport: local.device.transport_public_key(),
            };
            let receipt = Receipt::sign(tx, hash(&payload), &device, flow, [6; 32]);
            let state = if existing {
                local.pairs.existing.push(PairTransaction {
                    tx,
                    pin,
                    payload,
                    approved_flow: flow,
                    approved_hash: [6; 32],
                    stage: Stage::DisclosurePossible,
                    receipt: None,
                    publication: None,
                });
                AccountState::Active(Box::new(local))
            } else {
                AccountState::PendingImport(Box::new(PendingImport {
                    device,
                    revision: 2,
                    pin,
                    payload,
                    receipt,
                    records: local.records.clone(),
                }))
            };
            crate::storage_fixture::Fixture::from_handle(manager.store.clone()).fault(crate::storage_fixture::CommitFault::AfterCommit).await;
            let error = manager.commit_pair_state(state, request).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::StorageUncertain);
            assert_eq!(manager.view().phase, AccountPhase::Locked);
            assert_eq!(manager.pair.bytes, (0, 0));
            assert_eq!(
                manager.start_transfer(flow).await.unwrap_err().code,
                ErrorCode::StorageLocked
            );
            assert_eq!(
                manager.pair_plain(flow, &[]).await.unwrap_err().code,
                ErrorCode::StorageLocked
            );
            assert!(manager.begin().is_err());
            manager.store.resume().await.unwrap();
            let stored = manager.store.load_account().await.unwrap().unwrap();
            let saved = AccountState::decode(&stored).unwrap();
            match saved {
                AccountState::Active(l) => {
                    assert!(existing);
                    assert_eq!(l.pairs.existing[0].envelope().unwrap().tx, tx);
                    assert!(l.pairs.existing[0].receipt.is_none());
                }
                AccountState::PendingImport(p) => {
                    assert!(!existing);
                    assert_eq!(p.receipt.tx, tx);
                    assert_eq!(p.receipt.payload_hash, hash(&p.payload));
                }
                _ => panic!("complete new snapshot was not preserved"),
            }
            for suspend in [true, false] {
                let ticket = network.block();
                let request = Request { ticket, suspend };
                manager.store.invalidate(ticket);
            manager.shared.requests.send_replace(request);
                manager.shared.status.store(ticket << 2, Ordering::Release);
                let result = manager.lifecycle(request).await;
                manager.complete(request, result);
            }
            assert!(manager.ready().is_ok());
            assert_eq!(manager.pair.bytes, (0, 0));
            assert!(manager.pair.current.is_none());
            network.execute(NetworkOp::Stop).await.unwrap();
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
        let root = dir.path().to_owned();
        let (store, _) = tokio::task::spawn_blocking(move || StorageHandle::open_blocking(root, Arc::new(Protector))).await.unwrap().unwrap();
        store.invalidate(ticket);
        let shared = Arc::new(Shared {
            store: store.clone(),
            network: network.clone(),
            requests,
            status: AtomicU64::new(ticket << 2),
        });
        let mut manager = Manager {
            messages: Default::default(),
            pair: super::super::pair::session::Runtime::default(),
            auth: super::super::connection::Runtime::default(),
            bus: EventBus::new(),
            store: store.clone(),
            state: None,
            creation: None,
                cleanup_pending: false,
            revision: 0,
            public_revision: 0,
            last_public_view: None,
            lock_error: None,
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
        crate::storage_fixture::Fixture::from_handle(manager.store.clone()).fault(crate::storage_fixture::CommitFault::AfterCommit).await;
        let result = manager.persist(&state, request).await;
        assert_eq!(result.unwrap_err().code, ErrorCode::StorageUncertain);
        assert_eq!(manager.view().phase, AccountPhase::Locked);
        assert_eq!(
            manager.begin().err().unwrap().code,
            ErrorCode::StorageLocked
        );
        assert_eq!(
            manager.recover("invalid", true).await.unwrap_err().code,
            ErrorCode::StorageLocked
        );
        manager.store.resume().await.unwrap();
        let bytes = manager.store.load_account().await.unwrap().unwrap().payload;
        // The call returned Err, but atomic replacement really committed Active.
        manager.store.resume().await.unwrap();
        let AccountState::Active(saved) =
            AccountState::decode(&manager.store.load_account().await.unwrap().unwrap()).unwrap()
        else {
            panic!()
        };
        assert_eq!(saved.keys.peer_id(), peer);
        for suspend in [true, false] {
            let ticket = network.block();
            let request = Request { ticket, suspend };
            manager.store.invalidate(ticket);
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
        assert!(manager.store.load_account().await.unwrap().unwrap().payload == bytes);
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
        let root = dir.path().to_owned();
        let (store, _) = tokio::task::spawn_blocking(move || StorageHandle::open_blocking(root, Arc::new(Protector))).await.unwrap().unwrap();
        store.invalidate(ticket);
        let shared = Arc::new(Shared {
            store: store.clone(),
            network: network.clone(),
            requests,
            status: AtomicU64::new(ticket << 2),
        });
        let mut manager = Manager {
            messages: Default::default(),
            pair: super::super::pair::session::Runtime::default(),
            auth: super::super::connection::Runtime::default(),
            bus: EventBus::new(),
            store: store.clone(),
            state: None,
            creation: None,
                cleanup_pending: false,
            revision: 0,
            public_revision: 0,
            last_public_view: None,
            lock_error: None,
            shared,
            services: offline_services(),
        };
        let restored = manager.lifecycle(request).await;
        manager.complete(request, restored);
        let first = manager.prepare().await.unwrap();
        let persisted = manager.store.load_account().await.unwrap().unwrap();
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
        manager.store.commit_account(replacement.encode()).await.unwrap();
        manager.state = Some(replacement);
        let second = manager.prepare().await.unwrap();
        assert_ne!(second.endpoint_id, first.endpoint_id);
        assert_ne!(second.installation_id, first.installation_id);
        assert_eq!(slot.info().unwrap().instance_id, instance);
        let persisted = manager.store.load_account().await.unwrap().unwrap();
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
    fn stale_lookup(
        candidate: &super::publication::SignedRecord,
        accepted: &super::publication::SignedRecord,
        now: u64,
    ) -> bool {
        candidate.seq < accepted.seq
            && candidate.record.generation == accepted.record.generation
            && candidate.record.devices == accepted.record.devices
            && candidate.current(now)
            && !candidate.record.is_expired(now)
            && accepted.current(now)
            && !accepted.record.is_expired(now)
    }
    fn local_authorized(&self) -> bool {
        self.ready().is_ok() && self.local().is_ok_and(|local| Self::local_authorized_at(local,self.services.clock.unix_seconds()))
    }
    fn local_authorized_at(local: &LocalAccount, now:u64) -> bool {
        let Some(record)=&local.records.accepted else {return false;};
        local.records.conflict.is_none()
            && local.records.state != PublicationState::Conflict
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
            || local.records.state == PublicationState::Conflict
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
        self.restrict_message_acquisitions(None);
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
        self.restrict_message_acquisitions(self.local().ok());
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
    async fn commit_remote(
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
        let mut state = self.copy_state().unwrap();
        let AccountState::Active(local) = &mut state else {
            unreachable!()
        };
        local.remote = remote;
        local.revision = revision;
        let result = self.persist(&state, request).await;
        if result.is_ok() {
            self.state = Some(state);
            self.revision = revision;
        }
        self.restrict_grants();
        result
    }
    async fn observe_remote(
        &mut self,
        peer: PeerId,
        signed: super::publication::SignedRecord,
        request: Request,
    ) -> Result<(), CoreError> {
        let mut remote = self.local()?.remote.clone();
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
        self.commit_remote(remote, request).await?;
        result
    }
    async fn confirm_remote(&mut self, peer: PeerId, hash: [u8; 32]) -> Result<(), CoreError> {
        let request = self.ready()?;
        if peer == self.local()?.keys.peer_id() {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        let reviewed = self.local()?.remote.iter().find(|r| r.peer == peer)
            .and_then(|r| r.conflict.as_ref()).is_some_and(|r| r.hash == hash);
        let backend = self.services.backend()?;
        let lookup = self.service(request, backend.lookup(peer)).await?;
        self.ensure_ready(request)?;
        let super::Lookup::Found(packet) = lookup else {
            return Err(unavailable(ErrorCode::RecordUnavailable));
        };
        let signed = super::publication::SignedRecord::new(packet, peer)?;
        // Retain newly observed evidence even when consent is stale.
        let observed = self.observe_remote(peer, signed.clone(), request).await;
        if let Err(error) = observed && error.code != ErrorCode::RecordConflict {
            return Err(error);
        }
        let mut remote = self.local()?.remote.clone();
        let entry = remote.iter_mut().find(|r| r.peer == peer).unwrap();
        let now = self.services.clock.unix_seconds();
        if !reviewed || signed.hash != hash || signed.seq < entry.floor
            || !signed.current(now) || signed.record.is_expired(now) {
            return Err(unavailable(ErrorCode::RecordConflict));
        }
        super::parse_record(&signed.packet, peer, self.services.clock.unix_seconds())?;
        entry.accepted = signed;
        entry.conflict = None;
        self.restrict_peer_grants(peer, true);
        self.commit_remote(remote, request).await
    }
    fn restrict_peer_grants(&self, peer: PeerId, close: bool) {
        if close {self.cancel_message_proofs(peer);}
        for grant in self.auth.registry.apply(|grant| (grant.peer != peer, close && grant.peer == peer)) {
            self.bus.publish(crate::CoreEvent::AccountAuthChanged {
                peer_id: grant.peer.to_string(), installation_id: grant.installation.to_string(), ready: false,
            });
        }
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
        self.finish_remote_record(peer, lookup, request).await
    }
    async fn finish_remote_record(
        &mut self, peer: PeerId, lookup: Result<super::Lookup, CoreError>, request: Request,
    ) -> Result<super::publication::SignedRecord, CoreError> {
        self.ensure_ready(request)?;
        self.auth.cached_lookups.remove(&peer);
        let cached;
        match lookup {
            Ok(super::Lookup::Found(packet)) => {
                let signed = super::publication::SignedRecord::new(packet, peer)?;
                let now = self.services.clock.unix_seconds();
                cached = self.local()?.remote.iter().find(|r| r.peer == peer).is_some_and(|r| {
                    r.conflict.is_none()
                        && r.accepted.seq >= r.floor
                        && Self::stale_lookup(&signed, &r.accepted, now)
                });
                if !cached {
                    let observed = self.observe_remote(peer, signed, request).await;
                    if let Err(error) = &observed && error.code == ErrorCode::RecordConflict {
                        use std::io::Write;
                        let _ = writeln!(std::io::stderr().lock(), "record_conflict remote_observation source=lookup");
                    }
                    observed?;
                }
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
            self.auth.cached_lookups.insert(peer);
            self.bus.publish(crate::CoreEvent::AccountCacheUsed {
                peer_id: peer.to_string(),
            });
        } else {
            for session in self.auth.registry.0.lock().unwrap().iter_mut().filter(|s| s.grant.peer == peer) {
                session.cached = false;
            }
        }
        Ok(record)
    }
    async fn connect(&mut self, peer: PeerId) -> Result<(), CoreError> {
        self.restrict_peer_grants(peer, false);
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
            let mut evidence = None;
            let result = self
                .service(
                    request,
                    Box::pin(super::auth::authenticate(
                        conn.clone(),
                        peer,
                        self.local()?,
                        &record,
                        &*self.services.clock,
                        &mut evidence,
                        None,
                    )),
                )
                .await;
            if let Some(signed) = evidence
                && let Err(error) = self.observe_auth_record(peer, signed, request).await {
                conn.close(0u32.into(), b"account record changed");
                return Err(error);
            }
            match result {
                Ok(grant) => return self.install_grant(conn, grant, None).await,
                Err(error) => {
                    conn.close(0u32.into(), b"account authentication failed");
                    last = error;
                }
            }
        }
        Err(last)
    }
    async fn install_grant(
        &mut self,
        conn: iroh::endpoint::Connection,
        grant: super::auth::AuthGrant,
        proof_owner: Option<(crate::message::types::MessageKey, u64, InstallationId)>,
    ) -> Result<(), CoreError> {
        if let Err(error) = self.check_message_proof(proof_owner) {
            conn.close(0u32.into(), b"stale message proof");
            return Err(error);
        }
        if !self.grant_authorized(&grant) {
            conn.close(0u32.into(), b"account restricted");
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        if let Err(error) = self.observe_contact_device(grant.peer, grant.installation).await {
            conn.close(0u32.into(), b"contact persistence failed");
            return Err(error);
        }
        if let Err(error) = self.check_message_proof(proof_owner) {
            conn.close(0u32.into(), b"stale message proof");
            return Err(error);
        }
        if !self.grant_authorized(&grant) {
            conn.close(0u32.into(), b"account restricted");
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        // Release only current authorized Blocked work before announcing a ready session.
        if let Err(error) = self.release_message_peer(grant.peer).await {
            conn.close(0u32.into(), b"message release failed");
            return Err(error);
        }
        if let Err(error) = self.check_message_proof(proof_owner) {
            conn.close(0u32.into(), b"stale message proof");
            return Err(error);
        }
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
            cached: self.auth.cached_lookups.contains(&grant.peer),
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
            Input::MessageAttempt(result)=>self.message_attempt_result(result).await,
            Input::MessageLookup(key,ticket,result)=>self.message_lookup(key,ticket,result).await,
            Input::MessageProof(result)=>self.message_proof(result).await,
            Input::Incoming(conn) => {
                let result = async {
                    let request = self.ready()?;
                    if !self.local_authorized() {
                        return Err(unavailable(ErrorCode::AccountRestricted));
                    }
                    let preface = self
                        .service(request, Box::pin(super::auth::receive_preface(&conn)))
                        .await?;
                    let peer = preface.hello.peer;
                    let record = self.remote_record(peer).await?;
                    let mut evidence = None;
                    let result = self
                        .service(
                            request,
                            Box::pin(super::auth::authenticate_incoming(
                                &conn,
                                self.local()?,
                                &record,
                                &*self.services.clock,
                                preface,
                                &mut evidence,
                            )),
                        )
                        .await;
                    if let Some(signed) = evidence {
                        self.observe_auth_record(peer, signed, request).await?;
                    }
                    self.install_grant(conn.clone(), result?, None).await
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
                let result = self.receive_control(&connection, &body).await;
                let result=result.and_then(|body| {
                    let gate=if super::auth::payload(&body,16).is_ok() {Some(self.message_response_gate(&connection, #[cfg(all(feature="test-hooks",debug_assertions))] &body)?)} else {None};
                    Ok(super::connection::Response {body,gate})
                });
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
    async fn observe_auth_record(
        &mut self,
        peer: PeerId,
        signed: super::publication::SignedRecord,
        request: Request,
    ) -> Result<(), CoreError> {
        if peer == self.local()?.keys.peer_id() {
            self.apply_record_change(&signed, request).await
        } else {
            self.observe_remote(peer, signed, request).await
        }
    }
    async fn apply_record_change(
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
        self.commit_records(records, request).await?;
        result
    }
    async fn receive_control(
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
            if self.must_close(&grant) {
                return Err(unavailable(ErrorCode::AccountRestricted));
            }
            let signed = super::publication::SignedRecord::decode(&bytes, grant.peer)?;
            let current = super::parse_record(
                &signed.packet,
                grant.peer,
                self.services.clock.unix_seconds(),
            );
            if grant.peer == self.local()?.keys.peer_id() {
                current?;
                self.apply_record_change(&signed, request).await?;
            } else {
                // Verified future/rollback evidence must retain the same durable
                // floor/conflict policy as lookup, even when unusable right now.
                self.observe_remote(grant.peer, signed, request).await?;
                current?;
            }
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
        if let Ok(inner)=super::auth::payload(body,16) {
            if body.len()>61440 { return Ok(crate::message::wire::Frame::Reject(crate::message::wire::RejectReason::InvalidMessage).envelope().to_vec()); }
            return self.receive_message(conn,&inner,request).await;
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
            // Preserve exact signed packets and receipt obligations, but never
            // retry a queued membership that the new tombstone invalidates.
            records.restrict_pending(local);
            self.commit_records(records, request).await?;
        }
        self.restrict_grants();
        if !self.grant_authorized(&grant) || conn.close_reason().is_some() {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        Ok(body.to_vec())
    }
}

impl Manager {
    fn create_invite(&self, name: Option<String>) -> Result<String, CoreError> {
        self.ready()?;
        let local = self.local()?;
        super::invite::create(
            &local.keys,
            &local.cert,
            name,
            self.services.clock.unix_seconds(),
        )
    }
    fn contact_view(&self, c: &super::contacts::Contact) -> super::ContactView {
        let verified = self.auth.registry.0.lock().unwrap().iter().any(|s| {
            s.ready
                && s.conn.close_reason().is_none()
                && s.grant.peer == c.peer
                && self.grant_authorized(&s.grant)
        });
        c.view(verified)
    }
    fn contact_views(&self) -> Result<Vec<super::ContactView>, CoreError> {
        self.ready()?;
        Ok(self
            .local()?
            .contacts
            .iter()
            .map(|c| self.contact_view(c))
            .collect())
    }
    async fn commit_contacts(
        &mut self,
        contacts: Vec<super::contacts::Contact>,
        request: Request,
    ) -> Result<(), CoreError> {
        self.ensure_ready(request)?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| unavailable(ErrorCode::StorageFull))?;
        let mut state = self.copy_state().unwrap();
        let AccountState::Active(local) = &mut state else {
            unreachable!()
        };
        local.contacts = contacts;
        local.revision = revision;
        let result = self.persist(&state, request).await;
        if result.is_ok() {
            self.state = Some(state);
            self.revision = revision;
        }
        result
    }
    async fn accept_invite(
        &mut self,
        input: &str,
        alias: String,
    ) -> Result<super::ContactView, CoreError> {
        let request = self.ready()?;
        let preview = super::parse_invite(input, self.services.clock.unix_seconds())?;
        super::invite::validate_name(&alias)?;
        let local = self.local()?;
        if local.keys.peer_id() == preview.peer_id {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        if let Some(c) = local.contacts.iter().find(|c| c.peer == preview.peer_id) {
            return Ok(self.contact_view(c));
        }
        if local.contacts.len() >= 1000 {
            return Err(unavailable(ErrorCode::StorageFull));
        }
        let mut contacts = local.contacts.clone();
        let contact = super::contacts::Contact {
            peer: preview.peer_id,
            alias,
            verified_digest: None,
            known_installation: Some(preview.issuing_cert.installation_id),
            new_device: false,
        };
        contacts.push(contact.clone());
        contacts.sort_by_key(|c| c.peer);
        self.commit_contacts(contacts, request).await?;
        Ok(self.contact_view(&contact))
    }
    async fn confirm_safety(&mut self, peer: PeerId, digest: [u8; 32]) -> Result<(), CoreError> {
        let request = self.ready()?;
        let local = self.local()?;
        if super::safety_digest(local.keys.peer_id(), peer) != digest {
            return Err(super::wire::invalid());
        }
        let mut contacts = local.contacts.clone();
        let contact = contacts
            .iter_mut()
            .find(|c| c.peer == peer)
            .ok_or_else(|| unavailable(ErrorCode::AccountRestricted))?;
        if contact.verified_digest == Some(digest) {
            return Ok(());
        }
        contact.verified_digest = Some(digest);
        self.commit_contacts(contacts, request).await
    }
    async fn observe_contact_device(
        &mut self,
        peer: PeerId,
        installation: InstallationId,
    ) -> Result<(), CoreError> {
        let request = self.ready()?;
        let mut contacts = self.local()?.contacts.clone();
        let Some(c) = contacts.iter_mut().find(|c| c.peer == peer) else {
            return Ok(());
        };
        if c.known_installation == Some(installation) {
            return Ok(());
        }
        // Invite baseline is only a comparison hint. This path requires a fresh
        // authorized B3 proof, and the notice remains sticky across later proofs.
        c.new_device |= c.known_installation.is_some();
        c.known_installation = Some(installation);
        self.commit_contacts(contacts, request).await
    }
}

impl AccountHandle {
    pub async fn pair_begin(
        &self,
        direction: super::PairDirection,
        resume: Option<super::TxId>,
    ) -> Result<super::PairDisplay, CoreError> {
        self.send(|reply| Command::PairBegin(direction, resume, reply))
            .await
    }
    pub async fn pair_scan(&self, qr: String) -> Result<FlowId, CoreError> {
        self.send(|reply| Command::PairScan(Zeroizing::new(qr), reply))
            .await
    }
    pub async fn pair_confirm_code(&self, flow: FlowId, matches: bool) -> Result<(), CoreError> {
        self.send(|reply| Command::PairConfirm(flow, matches, reply))
            .await
    }
    pub async fn pair_approve(&self, flow: FlowId) -> Result<(), CoreError> {
        self.send(|reply| Command::PairApprove(flow, reply)).await
    }
    pub async fn pair_cancel(&self, flow: FlowId) -> Result<(), CoreError> {
        self.send(|reply| Command::PairCancel(flow, reply)).await
    }
    pub async fn pair_view(&self) -> Result<Option<super::PairView>, CoreError> {
        self.send(Command::PairView).await
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub async fn pair_fixture_send(
        &self,
        flow: FlowId,
        bytes: Vec<u8>,
    ) -> Result<usize, CoreError> {
        if bytes.len() > super::pair::channel::MAX_PLAIN {
            return Err(super::wire::invalid());
        }
        self.send(|reply| Command::PairFixtureSend(flow, Zeroizing::new(bytes), reply))
            .await
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub async fn pair_fixture_received(&self, flow: FlowId) -> Result<Vec<u8>, CoreError> {
        self.send(|reply| Command::PairFixtureReceived(flow, reply))
            .await
    }
}
impl Manager {
    fn expire_pair(&mut self) {
        if self
            .pair
            .current
            .as_ref()
            .is_some_and(|s| !s.live(&*self.services.clock))
        {
            self.pair.current = None;
            self.revision += 1;
        }
    }
    fn pair_session(
        &mut self,
        flow: FlowId,
    ) -> Result<&mut super::pair::session::Session, CoreError> {
        self.ready()?;
        self.expire_pair();
        self.pair
            .current
            .as_mut()
            .filter(|s| s.qr.flow == flow)
            .ok_or_else(|| unavailable(ErrorCode::PairExpired))
    }
    async fn pair_prepare(&mut self, role: super::pair::wire::Role) -> Result<(), CoreError> {
        use super::pair::wire::Role;
        let request = self.ready()?;
        if role == Role::New {
            if matches!(self.state,Some(AccountState::CandidatePinned(_) | AccountState::PendingImport(_) | AccountState::Active(_))) {
                let device=self.state.as_ref().unwrap().device();
                let key=iroh::SecretKey::from_bytes(device.transport().as_bytes());
                let ticket=self.shared.network.block();
                self.service(request,Box::pin(self.shared.network.install_key(key,true,ticket))).await?;
                self.ensure_ready(request)?;
            }else{self.prepare().await?;}
        } else {
            self.publication(Action::Refresh).await?;
            if !self.local_authorized() {
                return Err(unavailable(ErrorCode::AccountRestricted));
            }
            let request = self.ready()?;
            self.shared.network.execute(crate::NetworkOp::Start).await?;
            self.service(
                request,
                Box::pin(async {
                    loop {
                        let crate::NetworkReply::Info(info) =
                            self.shared.network.execute(crate::NetworkOp::Info).await?
                        else {
                            return Err(unavailable(ErrorCode::TransportFailed));
                        };
                        match info.state {
                            crate::NetworkState::Online => return Ok(()),
                            crate::NetworkState::Starting => {
                                tokio::time::sleep(Duration::from_millis(10)).await
                            }
                            _ => return Err(unavailable(ErrorCode::TransportFailed)),
                        }
                    }
                }),
            )
            .await?;
        }
        Ok(())
    }
    async fn pair_begin(
        &mut self,
        direction: super::PairDirection,
        resume: Option<super::TxId>,
    ) -> Result<super::PairDisplay, CoreError> {
        use super::pair::{
            session::Session,
            wire::{Qr, Role},
        };
        self.expire_pair();
        if self.pair.current.is_some() {
            return Err(unavailable(ErrorCode::Busy));
        }
        let role = if direction == super::PairDirection::NewDisplays {
            Role::New
        } else {
            Role::Existing
        };
        let request=self.ready()?;
        self.validate_pair_role(role,resume)?;
        self.pair_prepare(role).await?;
        self.ensure_ready(request)?;
        let device = self
            .state
            .as_ref()
            .ok_or_else(super::wire::invalid)?
            .device();
        let mut flow = FlowId([0; 16]);
        getrandom::fill(&mut flow.0).map_err(|_| unavailable(ErrorCode::NotAvailable))?;
        let qr = Qr {
            direction,
            flow,
            transport: device.transport_public_key(),
            installation: device.installation_id(),
            existing: if role == Role::Existing {
                Some(self.local()?.keys.peer_id())
            } else {
                None
            },
            resume,
            relay: None,
            expires: self.services.clock.unix_seconds().saturating_add(300),
            psk: super::random_secret()?,
        };
        let display = super::PairDisplay {
            flow,
            qr: qr.display(),
            expires_at: qr.expires,
        };
        self.pair.current = Some(Session::new(qr, role, false, &*self.services.clock));
        self.revision += 1;
        Ok(display)
    }
    async fn pair_scan(&mut self, text: &str) -> Result<FlowId, CoreError> {
        use super::pair::{
            session::Session,
            wire::{Qr, Role},
        };
        self.ready()?;
        self.expire_pair();
        if self.pair.current.is_some() {
            return Err(unavailable(ErrorCode::Busy));
        }
        let qr = Qr::parse(text, self.services.clock.unix_seconds())?;
        let role = if qr.owner() == Role::Existing {
            Role::New
        } else {
            Role::Existing
        };
        // An unpinned durable candidate has received no transaction offer yet.
        // E's resume QR can be its first offer, with all fresh B5 gates intact.
        let first_offer = role == Role::New
            && qr.direction == super::PairDirection::ExistingDisplays
            && qr.resume.is_some()
            && matches!(self.state, Some(AccountState::Candidate(_)));
        let request=self.ready()?;
        if !first_offer { self.validate_pair_role(role,qr.resume)?; }
        self.pair_prepare(role).await?;
        self.ensure_ready(request)?;
        let mut target = iroh::EndpointAddr::from(
            iroh::EndpointId::from_bytes(&qr.transport).map_err(|_| super::wire::invalid())?,
        );
        if let Some(relay) = &qr.relay {
            target = target.with_relay_url(relay.parse().map_err(|_| super::wire::invalid())?);
        }
        let flow = qr.flow;
        let request = self.ready()?;
        self.pair.current = Some(Session::new(qr, role, true, &*self.services.clock));
        let connection = self
            .service(request, Box::pin(self.shared.network.dial_pair(target)))
            .await;
        match connection {
            Ok(conn) => {
                let result = self.pair_handshake(conn).await;
                if result.is_err() && self.pair.current.as_ref().is_some_and(|s| s.qr.flow == flow) {
                    // A failed initiator has no display/reply owner to cancel it.
                    // Session drop cancels its tasks; the owner keeps its retry budget.
                    self.pair.current = None;
                }
                result.map(|_| flow)
            },
            Err(e) => {
                self.pair.current = None;
                Err(e)
            }
        }
    }
    async fn pair_handshake(&mut self, conn: iroh::endpoint::Connection) -> Result<(), CoreError> {
        use super::pair::{
            channel,
            wire::{Preface, Role},
        };
        self.expire_pair();
        let result = async {
            let request = self.ready()?;
            let session = self
                .pair
                .current
                .as_mut()
                .ok_or_else(super::wire::invalid)?;
            session.bind(*conn.remote_id().as_bytes())?;
            let local_role = session.role;
            if local_role == Role::Existing {
                self.publication(Action::Refresh).await?;
                if !self.local_authorized() {
                    return Err(unavailable(ErrorCode::AccountRestricted));
                }
            }
            let backend = self.services.backend()?;
            let session = self
                .pair
                .current
                .as_ref()
                .ok_or_else(super::wire::invalid)?;
            let device = self
                .state
                .as_ref()
                .ok_or_else(super::wire::invalid)?
                .device();
            let cert = if local_role == Role::Existing {
                Some(&self.local()?.cert)
            } else {
                None
            };
            let local = Preface {
                flow: session.qr.flow,
                role: local_role,
                transport: device.transport_public_key(),
                installation: device.installation_id(),
                existing: cert.map(|c| c.peer_id),
                resume: session.qr.resume,
            };
            let prepared = self
                .service(
                    request,
                    Box::pin(channel::prepare(
                        &conn,
                        &session.qr,
                        local,
                        session.initiator,
                    )),
                )
                .await?;
            let session = self
                .pair
                .current
                .as_mut()
                .ok_or_else(super::wire::invalid)?;
            if session
                .candidate_installation
                .is_some_and(|id| id != prepared.remote.installation)
            {
                return Err(super::wire::invalid());
            }
            session.candidate_installation = Some(prepared.remote.installation);
            let session = self
                .pair
                .current
                .as_ref()
                .ok_or_else(super::wire::invalid)?;
            let device = self
                .state
                .as_ref()
                .ok_or_else(super::wire::invalid)?
                .device();
            let cert = if local_role == Role::Existing {
                Some(&self.local()?.cert)
            } else {
                None
            };
            let (channel, recv) = self
                .service(
                    request,
                    Box::pin(channel::establish(channel::Establish {
                        conn: conn.clone(),
                        qr: &session.qr,
                        prepared,
                        device,
                        cert,
                        backend,
                        clock: &*self.services.clock,
                    })),
                )
                .await?;
            self.ensure_ready(request)?;
            if let Some(tx)=session.qr.resume {
                super::pair::resume::validate(self.state.as_ref().ok_or_else(super::wire::invalid)?,tx,&channel.remote,channel.candidate_kind)?;
            }
            if local_role == Role::Existing && !self.local_authorized() {
                return Err(unavailable(ErrorCode::AccountRestricted));
            }
            let session = self
                .pair
                .current
                .as_mut()
                .ok_or_else(super::wire::invalid)?;
            if !session.live(&*self.services.clock) {
                return Err(super::wire::invalid());
            }
            channel::reader(
                recv,
                conn.clone(),
                session.qr.flow,
                self.pair.send.clone(),
                session.cancel.clone(),
            );
            session.channel = Some(channel);
            Ok(())
        }
        .await;
        if result.is_err() {
            conn.close(0u32.into(), b"pair handshake failed");
            if let Some(s) = &mut self.pair.current {
                s.failures += 1;
            }
            self.expire_pair();
        }
        result
    }
    async fn pair_input(&mut self, input: super::pair::session::Input) {
        use super::pair::session::Input;
        self.expire_pair();
        match input {
            Input::Incoming(conn) => {
                if self
                    .pair
                    .current
                    .as_ref()
                    .is_none_or(|s| s.initiator || s.channel.is_some())
                {
                    conn.close(0u32.into(), b"pair not accepting");
                    return;
                }
                let _ = self.pair_handshake(conn).await;
            }
            Input::Frame {
                flow,
                connection,
                body,
            } => {
                if self.pair.current.as_ref().is_some_and(|s| {
                    s.qr.flow == flow
                        && s.channel
                            .as_ref()
                            .is_some_and(|c| c.conn.stable_id() == connection)
                }) {
                    let result = match body.and_then(|b| self.pair_session(flow)?.receive(connection, &b)) {
                        Ok(Some(plain))=>self.pair_plain(flow,&plain).await,
                        Ok(None)=>Ok(()),
                        Err(error)=>Err(error),
                    };
                    if result.is_err() {
                        self.pair.current = None;
                    }
                }
            }
        }
    }
    async fn pair_confirm(&mut self, flow: FlowId, matches: bool) -> Result<(), CoreError> {
        self.pair_session(flow)?;
        if !matches {
            self.pair.current = None;
            return Ok(());
        }
        let request = self.ready()?;
        let mut session = self.pair.current.take().ok_or_else(super::wire::invalid)?;
        let result = self.service(request, Box::pin(session.confirm())).await;
        if result.is_ok() && session.live(&*self.services.clock) {
            self.pair.current = Some(session);
        }
        result
    }
    async fn pair_approve(&mut self, flow: FlowId) -> Result<(), CoreError> {
        // Only a local command enters this method. Remote confirmation never calls it.
        self.pair_session(flow)?;
        self.publication(Action::Refresh).await?;
        if !self.local_authorized() {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        let clock = self.services.clock.clone();
        self.pair_session(flow)?.approve(&*clock)?;
        self.start_transfer(flow).await
    }
    fn pair_cancel(&mut self, flow: FlowId) -> Result<(), CoreError> {
        self.ready()?;
        self.expire_pair();
        if self.pair.current.as_ref().is_some_and(|s| s.qr.flow == flow) {
            self.pair.current = None;
            self.revision += 1;
        }
        Ok(())
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    async fn pair_fixture_send(
        &mut self,
        flow: FlowId,
        bytes: Zeroizing<Vec<u8>>,
    ) -> Result<usize, CoreError> {
        if !self.local_authorized() {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        let clock = self.services.clock.clone();
        let request = self.ready()?;
        let session = self.pair_session(flow)?;
        let permit = session.fixture_permit()?;
        // Reject denied sends without discarding a still-valid comparison session.
        if !session.approved || !session.local_confirmed || !session.remote_confirmed {
            return Err(super::wire::invalid());
        }
        let mut session = self.pair.current.take().ok_or_else(super::wire::invalid)?;
        let result = self
            .service(
                request,
                Box::pin(session.send_gated(&*clock, &permit, || {let mut tagged=Zeroizing::new(Vec::with_capacity(bytes.len()+11));tagged.extend_from_slice(b"B5 fixture:");tagged.extend_from_slice(&bytes);tagged})),
            )
            .await;
        if result.is_ok() && session.live(&*clock) {
            self.pair.current = Some(session);
        }
        result
    }
}

pub(super) mod messages;
#[cfg(all(feature="test-hooks",debug_assertions))]
pub use messages::{MessageMutation,MessagePause,MessagePoint};
