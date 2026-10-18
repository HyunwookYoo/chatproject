use super::{
    NetworkInfo, NetworkState, PeerInfo, PeerPath, PeerPhase,
    endpoint::{NetworkProfile, bind_endpoint, endpoint_ticket, parse_target},
    frame::{ALPN, validate_text},
    session::{self, Prepared, Reply, Session, SessionTag, Write},
};
use crate::{ConnectionSnapshot, CoreError, CoreEvent, ErrorCode, EventBus};
use futures_util::StreamExt;
use iroh::{Endpoint, EndpointAddr, SecretKey, Watcher, endpoint::Incoming};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum NetworkOp {
    Start,
    Info,
    Connect { target: String },
    Send { peer_id: String, text: String },
    Disconnect { peer_id: String },
    Stop,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    ConfigureLocal {
        relay: Option<iroh::RelayUrl>,
    },
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    SetOffline {
        offline: bool,
    },
}
#[derive(Debug)]
pub enum NetworkReply {
    Info(NetworkInfo),
    Peer(PeerInfo),
    Done,
}
struct Envelope {
    op: NetworkOp,
    reply: Reply,
}
enum Control {
    AccountRegister(mpsc::Sender<crate::account::connection::Input>),
    AccountDial { target: EndpointAddr, reply: oneshot::Sender<Result<iroh::endpoint::Connection, CoreError>> },
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    AccountTarget { target: EndpointAddr, reply: oneshot::Sender<Result<(),CoreError>> },
    Activate { key: Box<SecretKey>, start: bool, ticket: u64, reply: oneshot::Sender<Result<(), CoreError>> },
    Quiesce { reply: oneshot::Sender<Result<(), CoreError>> },
}
#[derive(Default)]
pub(crate) struct Gate {
    desired_suspend: AtomicBool,
    account_opened: AtomicBool,
    revision: AtomicU64,
    cancel: Mutex<CancellationToken>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    fail_bind: std::sync::atomic::AtomicBool,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    hold_bind: Mutex<Option<Arc<tokio::sync::Notify>>>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    token_window: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}
impl Gate {
    pub(crate) fn desired_suspend(&self) -> bool { self.desired_suspend.load(Ordering::Acquire) }
    pub(crate) fn lifecycle(&self, suspend: bool) {
        if self.desired_suspend.swap(suspend, Ordering::AcqRel) != suspend && self.account_opened.load(Ordering::Acquire) { self.block(); }
    }
    pub(crate) fn block(&self) -> u64 {
        let mut current = self.revision.load(Ordering::Acquire);
        loop {
            let next = (current + 2) | 1;
            match self.revision.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => { self.cancel(); return next; }
                Err(value) => current = value,
            }
        }
    }
    fn blocked(&self) -> bool { self.desired_suspend() || self.revision.load(Ordering::Acquire) & 1 != 0 }
    fn cancel(&self) {
        // Lifecycle callers never wait for the actor, file IO, or a held mutex.
        if let Ok(cancel) = self.cancel.try_lock() { cancel.cancel(); }
    }
}
#[derive(Clone)]
pub struct NetworkHandle {
    commands: mpsc::Sender<Envelope>,
    controls: mpsc::Sender<Control>,
    gate: Arc<Gate>,
}
impl NetworkHandle {
    pub fn spawn(handle: &tokio::runtime::Handle, profile: NetworkProfile, bus: EventBus) -> Self {
        Self::spawn_with_gate(handle, profile, bus, Arc::default())
    }
    pub(crate) fn spawn_with_gate(handle: &tokio::runtime::Handle, profile: NetworkProfile, bus: EventBus, gate: Arc<Gate>) -> Self {
        let (commands, receiver) = mpsc::channel(64);
        let (controls, control_rx) = mpsc::channel(8);
        let mut manager = Manager::new(profile, bus);
        manager.gate = gate.clone();
        manager.controls = Some(control_rx);
        handle.spawn(manager.run(receiver));
        Self { commands, controls, gate }
    }
    /// Serial installation-key transition. The account actor authorizes state
    /// changes (including active-account refusal) before persisting and calling this.
    pub async fn activate_key(&self, key: SecretKey) -> Result<(), CoreError> {
        let current = self.gate.revision.load(Ordering::Acquire);
        if self.gate.desired_suspend() || current & 1 != 0 || self.gate.revision.compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return Err(err(ErrorCode::Busy));
        }
        self.gate.cancel();
        self.install_key(key, true, current + 1).await
    }
    pub(crate) fn block(&self) -> u64 { self.gate.block() }
    pub(crate) fn begin_account(&self) -> u64 { self.gate.account_opened.store(true, Ordering::Release); self.block() }
    pub(crate) fn desired_suspend(&self) -> bool { self.gate.desired_suspend() }
    pub(crate) fn lifecycle_gate(&self, suspend: bool) { self.gate.lifecycle(suspend); }
    pub(crate) fn release(&self, ticket: u64) {
        let _ = self.gate.revision.compare_exchange(ticket, ticket + 1, Ordering::AcqRel, Ordering::Acquire);
    }
    pub(crate) async fn install_key(&self, key: SecretKey, start: bool, ticket: u64) -> Result<(), CoreError> {
        let (reply, received) = oneshot::channel();
        if self.controls.try_send(Control::Activate { key: Box::new(key), start, ticket, reply }).is_err() {
            // No transition was queued: remain blocked, with the persisted key untouched.
            return Err(err(ErrorCode::Busy));
        }
        received.await.map_err(|_| err(ErrorCode::TransportFailed))?
    }
    pub(crate) fn register_account(&self, input: mpsc::Sender<crate::account::connection::Input>) -> Result<(),CoreError> {
        self.controls.try_send(Control::AccountRegister(input)).map_err(|_|err(ErrorCode::Busy))
    }
    pub(crate) async fn dial_account(&self, target: EndpointAddr)->Result<iroh::endpoint::Connection,CoreError> {
        if self.gate.blocked(){return Err(err(ErrorCode::Busy));}
        let (reply,result)=oneshot::channel(); self.controls.try_send(Control::AccountDial{target,reply}).map_err(|_|err(ErrorCode::Busy))?;
        result.await.map_err(|_|err(ErrorCode::TransportFailed))?
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub async fn account_target_for_test(&self, ticket:String)->Result<(),CoreError> {
        let target=parse_target(&ticket)?; let(reply,result)=oneshot::channel();
        self.controls.try_send(Control::AccountTarget{target,reply}).map_err(|_|err(ErrorCode::Busy))?;
        result.await.map_err(|_|err(ErrorCode::TransportFailed))?
    }
    pub(crate) async fn quiesce(&self) -> Result<(), CoreError> {
        let (reply, received) = oneshot::channel();
        self.controls.send(Control::Quiesce { reply }).await.map_err(|_| err(ErrorCode::TransportFailed))?;
        received.await.map_err(|_| err(ErrorCode::TransportFailed))?
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub fn hold_next_token_replacement_for_test(&self) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (entered, received) = std::sync::mpsc::channel();
        let (release, resumed) = std::sync::mpsc::channel();
        *self.gate.token_window.lock().unwrap() = Some((entered, resumed));
        (received, release)
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub fn hold_next_bind_for_test(&self) -> Arc<tokio::sync::Notify> {
        let release = Arc::new(tokio::sync::Notify::new());
        *self.gate.hold_bind.lock().unwrap() = Some(release.clone());
        release
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub fn fail_next_bind_for_test(&self) {
        self.gate.fail_bind.store(true, Ordering::Release);
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub async fn configure_local(&self, relay: Option<iroh::RelayUrl>) -> Result<(), CoreError> {
        match self.execute(NetworkOp::ConfigureLocal { relay }).await? {
            NetworkReply::Done => Ok(()),
            _ => Err(err(ErrorCode::TransportFailed)),
        }
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub async fn set_offline(&self, offline: bool) -> Result<NetworkInfo, CoreError> {
        match self.execute(NetworkOp::SetOffline { offline }).await? {
            NetworkReply::Info(info) => Ok(info),
            _ => Err(err(ErrorCode::TransportFailed)),
        }
    }
    pub async fn execute(&self, op: NetworkOp) -> Result<NetworkReply, CoreError> {
        if self.gate.blocked() && !matches!(op, NetworkOp::Info) { return Err(err(ErrorCode::Busy)); }
        let (reply, received) = oneshot::channel();
        self.commands
            .try_send(Envelope { op, reply })
            .map_err(|error| {
                err(match error {
                    mpsc::error::TrySendError::Full(_) => ErrorCode::Busy,
                    mpsc::error::TrySendError::Closed(_) => ErrorCode::TransportFailed,
                })
            })?;
        received
            .await
            .map_err(|_| err(ErrorCode::TransportFailed))?
    }
}
fn err(code: ErrorCode) -> CoreError {
    CoreError::new(code, "network operation unavailable")
}

pub(crate) enum Note {
    AccountConnection { tag: SessionTag, connection: iroh::endpoint::Connection },
    Bound {
        epoch: u64,
        result: Result<Endpoint, CoreError>,
    },
    OwnAddress {
        epoch: u64,
    },
    Incoming {
        epoch: u64,
        incoming: Incoming,
        sequence: u64,
    },
    Identified {
        tag: SessionTag,
    },
    Ready {
        tag: SessionTag,
        outgoing: bool,
        started: Option<Instant>,
        result: Result<Prepared, CoreError>,
    },
    Text {
        tag: SessionTag,
        text: String,
    },
    Path {
        tag: SessionTag,
        path: PeerPath,
        observed: Instant,
    },
    Ended {
        tag: SessionTag,
        error: Option<CoreError>,
    },
}
impl Note {
    pub(crate) async fn discard(self) {
        match self {
            Note::Bound {
                result: Ok(endpoint),
                ..
            } => endpoint.close().await,
            Note::Ready {
                result: Ok(prepared),
                ..
            } => prepared.connection.close(0u32.into(), b"obsolete session"),
            Note::Incoming { incoming, .. } => incoming.refuse(),
            Note::AccountConnection { connection, .. } => connection.close(0u32.into(), b"obsolete account"),
            _ => {}
        }
    }
}
struct Pending {
    tag: SessionTag,
    cancel: CancellationToken,
    reply: Option<Reply>,
}
struct IncomingPreparation {
    sequence: u64,
    peer_id: Option<String>,
    cancel: CancellationToken,
}
struct Peer {
    info: PeerInfo,
    target: EndpointAddr,
    session: Option<Session>,
    pending: Option<Pending>,
    disconnected_before: Option<u64>,
}
struct Manager {
    account_input: Option<mpsc::Sender<crate::account::connection::Input>>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    account_targets: BTreeMap<iroh::EndpointId, EndpointAddr>,
    controls: Option<mpsc::Receiver<Control>>,
    gate: Arc<Gate>,
    activation: Option<(u64, oneshot::Sender<Result<(), CoreError>>)>,
    profile: NetworkProfile,
    bus: EventBus,
    key: SecretKey,
    endpoint: Option<Endpoint>,
    info: NetworkInfo,
    epoch: u64,
    generation: u64,
    peers: BTreeMap<String, Peer>,
    cancel: CancellationToken,
    tasks: JoinSet<()>,
    notes: mpsc::Sender<Note>,
    received: mpsc::Receiver<Note>,
    incoming: BTreeMap<u64, IncomingPreparation>,
    accepted: Arc<AtomicU64>,
    received_incoming: u64,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    offline: bool,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    offline_targets: BTreeMap<String, EndpointAddr>,
}
impl Manager {
    fn new(profile: NetworkProfile, bus: EventBus) -> Self {
        let (notes, received) = mpsc::channel(64);
        Self {
            controls: None,
            account_input: None,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            account_targets: BTreeMap::new(),
            gate: Arc::default(),
            activation: None,
            profile,
            bus,
            key: SecretKey::generate(),
            endpoint: None,
            info: NetworkInfo {
                revision: 0,
                state: NetworkState::Stopped,
                endpoint_id: None,
                ticket: None,
                peers: vec![],
            },
            epoch: 0,
            generation: 0,
            peers: BTreeMap::new(),
            cancel: CancellationToken::new(),
            tasks: JoinSet::new(),
            notes,
            received,
            incoming: BTreeMap::new(),
            accepted: Arc::new(AtomicU64::new(0)),
            received_incoming: 0,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            offline: false,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            offline_targets: BTreeMap::new(),
        }
    }
    async fn run(mut self, mut commands: mpsc::Receiver<Envelope>) {
        let mut controls = self.controls.take().unwrap();
        loop {
            tokio::select! {
                _ = self.cancel.cancelled(), if self.activation.is_some() => self.stop().await,
                Some(control) = controls.recv() => self.control(control).await,
                command = commands.recv() => match command { Some(command) => self.command(command).await, None => { if self.info.state != NetworkState::Stopped { self.stop().await; } break; } },
                Some(note) = self.received.recv() => self.note(note).await,
                Some(_) = self.tasks.join_next(), if !self.tasks.is_empty() => {},
            }
        }
    }
    async fn control(&mut self, control: Control) {
        match control {
            Control::AccountRegister(input) => { self.account_input=Some(input); }
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            Control::AccountTarget{target,reply} => {
                let result=if matches!(self.profile,NetworkProfile::Local{..}) {self.account_targets.insert(target.id,target);Ok(())}else{Err(err(ErrorCode::NotAvailable))};
                let _=reply.send(result);
            }
            Control::AccountDial{target,reply} => {
                if self.gate.blocked(){let _=reply.send(Err(err(ErrorCode::Busy)));return;}
                let Some(endpoint)=self.endpoint.clone() else {let _=reply.send(Err(err(ErrorCode::NetworkNotStarted)));return;};
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                let target=self.account_targets.get(&target.id).cloned().unwrap_or(target);
                let cancel=self.cancel.clone();
                self.tasks.spawn(async move {
                    let result=tokio::select!{biased;_=cancel.cancelled()=>Err(err(ErrorCode::Busy)),result=tokio::time::timeout(Duration::from_secs(8),endpoint.connect(target,crate::account::auth::ALPN))=>result.map_err(|_|err(ErrorCode::NetworkTimeout)).and_then(|r|r.map_err(|_|err(ErrorCode::TransportFailed)))};
                    if let Err(Ok(conn))=reply.send(result){conn.close(0u32.into(),b"account dial cancelled");}
                });
            }
            Control::Quiesce { reply } => {
                self.stop().await;
                let _ = reply.send(Ok(()));
            }
            Control::Activate { key, start, ticket, reply } => {
                if self.gate.revision.load(Ordering::Acquire) != ticket {
                    let _ = reply.send(Err(err(ErrorCode::Busy)));
                    return;
                }
                self.stop().await;
                self.key = *key;
                self.peers.clear();
                self.info.peers.clear();
                self.info.endpoint_id = Some(self.key.public().to_string());
                if start && self.gate.revision.load(Ordering::Acquire) == ticket {
                    self.activation = Some((ticket, reply));
                    self.start();
                } else {
                    // Restoring a key does not implicitly start the network.
                    let _ = reply.send(Ok(()));
                }
            }
        }
    }
    fn finish_activation(&mut self, result: Result<(), CoreError>) {
        if let Some((ticket, reply)) = self.activation.take() {
            let _ = self.gate.revision.compare_exchange(ticket, ticket + 1, Ordering::AcqRel, Ordering::Acquire);
            let _ = reply.send(result);
        }
    }
    fn publish(&mut self, changed: Option<&str>) {
        self.publish_peers(&changed.into_iter().collect::<Vec<_>>());
    }
    fn publish_peers(&mut self, changed: &[&str]) {
        self.info.revision += 1;
        self.info.peers = self.peers.values().map(|p| p.info.clone()).collect();
        self.bus
            .publish(CoreEvent::NetworkChanged(self.info.clone()));
        for peer in changed.iter().filter_map(|id| self.peers.get(*id)) {
            self.bus.publish(CoreEvent::PeerChanged {
                revision: self.info.revision,
                peer: peer.info.clone(),
            });
        }
        self.bus
            .publish(CoreEvent::ConnectionState(ConnectionSnapshot {
                direct_peers: self
                    .peers
                    .values()
                    .filter(|p| {
                        p.info.phase == PeerPhase::Connected && p.info.path == PeerPath::Direct
                    })
                    .count() as u32,
                ..ConnectionSnapshot::OFFLINE
            }));
    }
    fn tag(&mut self, peer_id: String) -> SessionTag {
        self.generation += 1;
        SessionTag {
            epoch: self.epoch,
            generation: self.generation,
            peer_id,
        }
    }
    async fn command(&mut self, envelope: Envelope) {
        let Envelope { op, reply } = envelope;
        if self.gate.blocked() && !matches!(op, NetworkOp::Info) {
            let _ = reply.send(Err(err(ErrorCode::Busy)));
            return;
        }
        let result = match op {
            NetworkOp::Info => Ok(NetworkReply::Info(self.info.clone())),
            NetworkOp::Start => {
                self.start();
                Ok(NetworkReply::Info(self.info.clone()))
            }
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            NetworkOp::ConfigureLocal { relay } => {
                if let Some(url) = &relay
                    && let Err(error) = crate::puppet::protocol::validate_relay(url.as_str()) {
                    let _ = reply.send(Err(error));
                    return;
                }
                let same = matches!(&self.profile, NetworkProfile::Local { relay: current } if current == &relay);
                if same {
                    Ok(NetworkReply::Done)
                } else if self.info.state == NetworkState::Stopped {
                    self.profile = NetworkProfile::Local { relay };
                    Ok(NetworkReply::Done)
                } else {
                    Err(err(ErrorCode::InvalidConfig))
                }
            }
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            NetworkOp::SetOffline { offline } => {
                if offline && !self.offline {
                    self.offline_targets.extend(self.peers.iter()
                        .filter(|(_, p)| p.session.is_some() || p.pending.is_some())
                        .map(|(id, p)| (id.clone(), p.target.clone())));
                    self.offline = true;
                    self.stop().await;
                } else if !offline && self.offline {
                    self.offline = false;
                    self.start();
                }
                Ok(NetworkReply::Info(self.info.clone()))
            }
            NetworkOp::Stop => {
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                {
                    self.offline = false;
                    self.offline_targets.clear();
                }
                self.stop().await;
                Ok(NetworkReply::Done)
            }
            NetworkOp::Connect { target } => {
                self.connect(target, reply);
                return;
            }
            NetworkOp::Send { peer_id, text } => {
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                if self.offline {
                    let _ = reply.send(Err(err(ErrorCode::PeerNotConnected)));
                    return;
                }
                if self.endpoint.is_none() {
                    Err(err(ErrorCode::NetworkNotStarted))
                } else if let Err(error) = validate_text(&text) {
                    Err(error)
                } else if let Some(session) =
                    self.peers.get(&peer_id).and_then(|p| p.session.as_ref())
                {
                    match session.writes.try_send(Write { text, reply }) {
                        Ok(()) => {}
                        Err(error) => {
                            let code = if matches!(error, mpsc::error::TrySendError::Full(_)) {
                                ErrorCode::Busy
                            } else {
                                ErrorCode::PeerNotConnected
                            };
                            let _ = error.into_inner().reply.send(Err(err(code)));
                        }
                    }
                    return;
                } else {
                    Err(err(ErrorCode::PeerNotConnected))
                }
            }
            NetworkOp::Disconnect { peer_id } => {
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                self.offline_targets.remove(&peer_id);
                let cutoff = self.accepted.load(Ordering::Acquire);
                // Unknown identities keep the cutoff alive, including accept notes still queued.
                if self.has_unknown_before(cutoff)
                    && !self.peers.contains_key(&peer_id)
                    && let Ok(id) = peer_id.parse::<iroh::EndpointId>()
                {
                    if !self.make_room(&peer_id) {
                        let _ = reply.send(Err(err(ErrorCode::Busy)));
                        return;
                    }
                    self.peers.insert(
                        peer_id.clone(),
                        Peer {
                            info: PeerInfo {
                                peer_id: peer_id.clone(),
                                phase: PeerPhase::Disconnected,
                                path: PeerPath::Unknown,
                                connect_ms: None,
                                direct_ms: None,
                            },
                            target: id.into(),
                            session: None,
                            pending: None,
                            disconnected_before: None,
                        },
                    );
                }
                for preparation in self.incoming.values() {
                    if preparation.sequence <= cutoff
                        && preparation.peer_id.as_deref() == Some(&peer_id)
                    {
                        preparation.cancel.cancel();
                    }
                }
                if let Some(peer) = self.peers.get_mut(&peer_id) {
                    peer.disconnected_before = Some(cutoff);
                    if let Some(pending) = peer.pending.take() {
                        pending.cancel.cancel();
                        if let Some(reply) = pending.reply {
                            let _ = reply.send(Err(err(ErrorCode::PeerNotConnected)));
                        }
                    }
                    if let Some(session) = peer.session.take() {
                        session.close();
                    }
                    peer.info.phase = PeerPhase::Disconnected;
                    peer.info.path = PeerPath::Unknown;
                    self.publish(Some(&peer_id));
                }
                self.release_cutoffs();
                Ok(NetworkReply::Done)
            }
        };
        let _ = reply.send(result);
    }
    fn start(&mut self) {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if self.offline {
            return;
        }
        if matches!(
            self.info.state,
            NetworkState::Stopped | NetworkState::Failed
        ) {
            self.cancel = CancellationToken::new();
            let published_revision = {
                let mut shared = self.gate.cancel.lock().unwrap();
                *shared = self.cancel.clone();
                let expected = self.activation.as_ref().map(|(ticket, _)| *ticket);
                let revision = self.gate.revision.load(Ordering::Acquire);
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                if let Some((entered, resumed)) = self.gate.token_window.lock().unwrap().take() {
                    // Only the test-owned actor pauses. Dropped test channels or
                    // this finite deadline release it even after an assertion fails.
                    if entered.send(()).is_ok() { let _ = resumed.recv_timeout(Duration::from_secs(5)); }
                }
                if revision & 1 != 0 && expected != Some(revision) { self.cancel.cancel(); }
                revision
            };
            // A lifecycle caller advances the revision before its nonblocking
            // try_lock. If it missed this replacement mutex, observe its durable
            // revision now; after unlock, later callers can cancel the new token.
            if self.gate.revision.load(Ordering::Acquire) != published_revision {
                self.cancel.cancel();
            }
            self.epoch += 1;
            self.info.state = NetworkState::Starting;
            self.publish(None);
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            if self.gate.fail_bind.swap(false, Ordering::AcqRel) {
                self.info.state = NetworkState::Failed;
                self.publish(None);
                self.finish_activation(Err(err(ErrorCode::TransportFailed)));
                return;
            }
            let profile = self.profile.clone();
            let key = self.key.clone();
            let epoch = self.epoch;
            let cancel = self.cancel.clone();
            let notes = self.notes.clone();
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            let hold = self.gate.hold_bind.lock().unwrap().take();
            self.tasks.spawn(async move {
                        #[cfg(all(feature = "test-hooks", debug_assertions))]
                        if let Some(hold) = hold {
                            tokio::select! { biased; _ = cancel.cancelled() => return, _ = hold.notified() => {} }
                        }
                        let result = tokio::select! { biased; _ = cancel.cancelled() => return, result = bind_endpoint(&profile, key) => result };
                        #[cfg(any(test, all(feature = "test-hooks", debug_assertions)))]
                        if let Ok(endpoint) = &result
                            && matches!(profile, NetworkProfile::Local { relay: Some(_) }) {
                                let ready = tokio::select! { biased; _ = cancel.cancelled() => false, result = tokio::time::timeout(Duration::from_secs(30), endpoint.online()) => result.is_ok() };
                                if !ready { endpoint.close().await; session::notify(&notes, &cancel, Note::Bound { epoch, result: Err(err(ErrorCode::NetworkTimeout)) }).await; return; }
                        }
                        session::notify(&notes, &cancel, Note::Bound { epoch, result }).await;
                    });
        }
    }
    fn has_unknown_before(&self, cutoff: u64) -> bool {
        self.received_incoming < cutoff
            || self
                .incoming
                .values()
                .any(|p| p.sequence <= cutoff && p.peer_id.is_none())
    }
    fn release_cutoffs(&mut self) {
        let releasable: Vec<String> = self
            .peers
            .iter()
            .filter_map(|(id, peer)| {
                peer.disconnected_before
                    .filter(|cutoff| !self.has_unknown_before(*cutoff))
                    .map(|_| id.clone())
            })
            .collect();
        for id in releasable {
            self.peers.get_mut(&id).unwrap().disconnected_before = None;
        }
    }
    fn make_room(&mut self, id: &str) -> bool {
        self.release_cutoffs();
        if !self.peers.contains_key(id) && self.peers.len() >= 8 {
            let old = self
                .peers
                .iter()
                .find(|(_, p)| {
                    p.session.is_none() && p.pending.is_none() && p.disconnected_before.is_none()
                })
                .map(|(id, _)| id.clone());
            if let Some(old) = old {
                self.peers.remove(&old);
            } else {
                return false;
            }
        }
        true
    }
    fn connect(&mut self, target: String, reply: Reply) {
        let started = Instant::now();
        let result = self.validate_connect(&target);
        let target = match result {
            Ok(target) => target,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        let id = target.id.to_string();
        if let Some(peer) = self.peers.get(&id) {
            if peer.pending.is_some() {
                let _ = reply.send(Err(err(ErrorCode::Busy)));
                return;
            }
            if peer.session.is_some() {
                let _ = reply.send(Ok(NetworkReply::Peer(peer.info.clone())));
                return;
            }
        }
        if !self.make_room(&id) {
            let _ = reply.send(Err(err(ErrorCode::Busy)));
            return;
        }
        let disconnected_before = self.peers.get(&id).and_then(|p| p.disconnected_before);
        let tag = self.tag(id.clone());
        let cancel = self.cancel.child_token();
        self.peers.insert(
            id.clone(),
            Peer {
                info: PeerInfo {
                    peer_id: id.clone(),
                    phase: PeerPhase::Connecting,
                    path: PeerPath::Unknown,
                    connect_ms: None,
                    direct_ms: None,
                },
                target: target.clone(),
                disconnected_before,
                session: None,
                pending: Some(Pending {
                    tag: tag.clone(),
                    cancel: cancel.clone(),
                    reply: Some(reply),
                }),
            },
        );
        self.publish(Some(&id));
        self.spawn_dial(target, tag, cancel, false, started);
    }
    fn validate_connect(&self, target: &str) -> Result<EndpointAddr, CoreError> {
        let endpoint = self
            .endpoint
            .as_ref()
            .ok_or_else(|| err(ErrorCode::NetworkNotStarted))?;
        let target = parse_target(target)?;
        if target.id == endpoint.id() {
            return Err(err(ErrorCode::SelfConnection));
        }
        if !self
            .peers
            .get(&target.id.to_string())
            .is_some_and(|p| p.session.is_some() || p.pending.is_some())
            && self
                .peers
                .values()
                .filter(|p| p.session.is_some() || p.pending.is_some())
                .count()
                >= 8
        {
            return Err(err(ErrorCode::Busy));
        }
        Ok(target)
    }
    fn spawn_dial(
        &mut self,
        target: EndpointAddr,
        tag: SessionTag,
        cancel: CancellationToken,
        retry: bool,
        accepted: Instant,
    ) {
        let endpoint = self.endpoint.as_ref().unwrap().clone();
        let notes = self.notes.clone();
        self.tasks.spawn(async move {
            let deadline =
                tokio::time::Instant::now() + Duration::from_secs(if retry { 30 } else { 10 });
            let mut attempt = 0;
            loop {
                if retry
                    && within_retry_budget(
                        deadline,
                        &cancel,
                        tokio::time::sleep(retry_delay(attempt)),
                    )
                    .await
                    .is_none()
                {
                    break;
                }
                let started = if retry { Instant::now() } else { accepted };
                let work = async {
                    let connection = endpoint
                        .connect(target.clone(), ALPN)
                        .await
                        .map_err(|_| err(ErrorCode::TransportFailed))?;
                    match session::prepare_session(connection.clone()).await {
                        Ok((send, recv)) => Ok(Prepared {
                            connection,
                            send,
                            recv,
                            ready_at: Instant::now(),
                        }),
                        Err(error) => {
                            connection.close(1u32.into(), b"session preparation failed");
                            Err(error)
                        }
                    }
                };
                let result = within_retry_budget(
                    deadline,
                    &cancel,
                    tokio::time::timeout(Duration::from_secs(10), work),
                )
                .await;
                match result {
                    Some(Ok(Ok(prepared))) => {
                        session::notify(
                            &notes,
                            &cancel,
                            Note::Ready {
                                tag,
                                outgoing: true,
                                started: Some(started),
                                result: Ok(prepared),
                            },
                        )
                        .await;
                        return;
                    }
                    Some(result) if !retry => {
                        session::notify(
                            &notes,
                            &cancel,
                            Note::Ready {
                                tag,
                                outgoing: true,
                                started: Some(started),
                                result: result
                                    .unwrap_or_else(|_| Err(err(ErrorCode::NetworkTimeout))),
                            },
                        )
                        .await;
                        return;
                    }
                    None => break,
                    _ => attempt += 1,
                }
            }
            session::notify(
                &notes,
                &cancel,
                Note::Ready {
                    tag,
                    outgoing: true,
                    started: None,
                    result: Err(err(ErrorCode::NetworkTimeout)),
                },
            )
            .await;
        });
    }
    async fn note(&mut self, note: Note) {
        if self.cancel.is_cancelled() { note.discard().await; return; }
        match note {
            Note::Bound { epoch, result } if epoch == self.epoch => {
                let completion = result.as_ref().map(|_| ()).map_err(Clone::clone);
                match result {
                    Ok(endpoint) => {
                        self.info.endpoint_id = Some(endpoint.id().to_string());
                        self.info.ticket = Some(endpoint_ticket(&endpoint, &self.profile));
                        self.info.state = NetworkState::Online;
                        let mut addresses = endpoint.watch_addr().stream();
                        let cancel = self.cancel.child_token();
                        let notes = self.notes.clone();
                        self.tasks.spawn(async move {
                            // Closing an Endpoint does not end watch_addr; Stop cancels it.
                            loop {
                                tokio::select! { biased; _ = cancel.cancelled() => break, address = addresses.next() => if address.is_none() { break; } }
                                session::notify(&notes, &cancel, Note::OwnAddress { epoch }).await;
                            }
                        });
                        let accept_endpoint = endpoint.clone();
                        let accepted = self.accepted.clone();
                        let cancel = self.cancel.clone();
                        let notes = self.notes.clone();
                        self.tasks.spawn(async move { loop {
                            let incoming = tokio::select! { biased; _ = cancel.cancelled() => break, incoming = accept_endpoint.accept() => match incoming { Some(incoming) => incoming, None => break } };
                            session::notify(&notes, &cancel, Note::Incoming { epoch, incoming, sequence: accepted.fetch_add(1, Ordering::AcqRel) + 1 }).await;
                        }});
                        self.endpoint = Some(endpoint);
                        #[cfg(all(feature = "test-hooks", debug_assertions))]
                        for (id, target) in std::mem::take(&mut self.offline_targets) {
                            let tag = self.tag(id.clone());
                            let cancel = self.cancel.child_token();
                            if let Some(peer) = self.peers.get_mut(&id) {
                                peer.info.phase = PeerPhase::Reconnecting;
                                peer.pending = Some(Pending {
                                    tag: tag.clone(),
                                    cancel: cancel.clone(),
                                    reply: None,
                                });
                                self.spawn_dial(target, tag, cancel, true, Instant::now());
                            }
                        }
                    }
                    Err(error) => {
                        self.info.state = NetworkState::Failed;
                        self.bus.publish(CoreEvent::Error {
                            code: error.code,
                            detail: error.detail,
                        });
                    }
                }
                self.publish(None);
                self.finish_activation(completion);
            }
            Note::OwnAddress { epoch }
                if epoch == self.epoch && self.info.state == NetworkState::Online =>
            {
                if let Some(endpoint) = &self.endpoint {
                    // Resample so queued notifications cannot restore an older address.
                    let ticket = endpoint_ticket(endpoint, &self.profile);
                    if self.info.ticket.as_ref() != Some(&ticket) {
                        self.info.ticket = Some(ticket);
                        self.publish(None);
                    }
                }
            }
            Note::Incoming {
                epoch,
                incoming,
                sequence,
            } if epoch == self.epoch => {
                self.received_incoming = sequence;
                if self.incoming.len() >= 8 {
                    incoming.refuse();
                    self.release_cutoffs();
                    return;
                }
                let mut tag = self.tag(String::new());
                let cancel = self.cancel.child_token();
                self.incoming.insert(
                    tag.generation,
                    IncomingPreparation {
                        sequence,
                        peer_id: None,
                        cancel: cancel.clone(),
                    },
                );
                let root = self.cancel.clone();
                let notes = self.notes.clone();
                self.tasks.spawn(async move {
                    let mut routed=false;
                    let work = async {
                        let connection = incoming.await.map_err(|_| err(ErrorCode::TransportFailed))?;
                        if connection.alpn()==crate::account::auth::ALPN {
                            session::notify(&notes,&cancel,Note::AccountConnection{tag:tag.clone(),connection}).await;
                            routed=true;
                            return Err(err(ErrorCode::AccountRestricted));
                        }
                        tag.peer_id = connection.remote_id().to_string();
                        session::notify(&notes, &cancel, Note::Identified { tag: tag.clone() }).await;
                        match session::prepare_session(connection.clone()).await {
                            Ok((send, recv)) => Ok(Prepared { connection, send, recv, ready_at: Instant::now() }),
                            Err(error) => { connection.close(1u32.into(), b"session preparation failed"); Err(error) },
                        }
                    };
                    let result = tokio::select! { biased; _ = cancel.cancelled() => Err(err(ErrorCode::PeerNotConnected)), result = tokio::time::timeout(Duration::from_secs(10), work) => result.unwrap_or_else(|_| Err(err(ErrorCode::NetworkTimeout))) };
                    if routed { return; }
                    // A peer-canceled preparation must still release its bounded registry slot.
                    session::notify(&notes, &root, Note::Ready { tag, outgoing: false, started: None, result }).await;
                });
            }
            Note::AccountConnection{tag,connection} if tag.epoch==self.epoch => {
                let preparation=self.incoming.remove(&tag.generation);self.release_cutoffs();
                if self.gate.blocked() || preparation.is_none_or(|p|p.cancel.is_cancelled()) {connection.close(0u32.into(),b"account blocked");return;}
                match &self.account_input {
                    Some(input)=>if let Err(error)=input.try_send(crate::account::connection::Input::Incoming(connection))
                        && let crate::account::connection::Input::Incoming(conn)=error.into_inner(){conn.close(0u32.into(),b"account busy");},
                    None=>connection.close(0u32.into(),b"account unavailable"),
                }
            }
            Note::Identified { tag } if tag.epoch == self.epoch => {
                if let Some(preparation) = self.incoming.get_mut(&tag.generation) {
                    preparation.peer_id = Some(tag.peer_id.clone());
                    if self
                        .peers
                        .get(&tag.peer_id)
                        .and_then(|p| p.disconnected_before)
                        .is_some_and(|cutoff| preparation.sequence <= cutoff)
                    {
                        preparation.cancel.cancel();
                    }
                }
                self.release_cutoffs();
            }
            Note::Ready {
                tag,
                outgoing,
                started,
                result,
            } if tag.epoch == self.epoch => {
                if !outgoing {
                    let preparation = self.incoming.remove(&tag.generation);
                    self.release_cutoffs();
                    if preparation.is_none_or(|p| p.cancel.is_cancelled()) {
                        Note::Ready {
                            tag,
                            outgoing,
                            started,
                            result,
                        }
                        .discard()
                        .await;
                        return;
                    }
                }
                self.ready(tag, outgoing, started, result);
            }
            Note::Text { tag, text } if self.current(&tag) => {
                self.bus.publish(CoreEvent::ProbeTextReceived {
                    peer_id: tag.peer_id,
                    text,
                })
            }
            Note::Path {
                tag,
                path,
                observed,
            } if self.current(&tag) => {
                let peer = self.peers.get_mut(&tag.peer_id).unwrap();
                if peer.info.path != path {
                    if path == PeerPath::Direct && peer.info.direct_ms.is_none() {
                        peer.info.direct_ms =
                            peer.session.as_ref().and_then(|s| s.started).map(|start| {
                                duration_ms(observed.saturating_duration_since(start))
                            });
                    }
                    peer.info.path = path;
                    self.publish(Some(&tag.peer_id));
                }
            }
            Note::Ended { tag, error } if self.current(&tag) => {
                if let Some(error) = error {
                    self.bus.publish(CoreEvent::Error {
                        code: error.code,
                        detail: error.detail,
                    });
                }
                let next_tag = self.tag(tag.peer_id.clone());
                let cancel = self.cancel.child_token();
                let peer = self.peers.get_mut(&tag.peer_id).unwrap();
                peer.session.take().unwrap().close();
                if peer.pending.is_some() {
                    peer.info.phase = PeerPhase::Connecting;
                    peer.info.path = PeerPath::Unknown;
                    self.publish(Some(&tag.peer_id));
                    return;
                }
                peer.info.phase = PeerPhase::Reconnecting;
                peer.info.path = PeerPath::Unknown;
                peer.info.connect_ms = None;
                peer.info.direct_ms = None;
                peer.pending = Some(Pending {
                    tag: next_tag.clone(),
                    cancel: cancel.clone(),
                    reply: None,
                });
                let target = peer.target.clone();
                self.publish(Some(&tag.peer_id));
                self.spawn_dial(target, next_tag, cancel, true, Instant::now());
            }
            note => note.discard().await,
        }
    }
    fn current(&self, tag: &SessionTag) -> bool {
        self.peers
            .get(&tag.peer_id)
            .and_then(|p| p.session.as_ref())
            .is_some_and(|s| tag.is_current(&s.tag))
    }
    fn ready(
        &mut self,
        tag: SessionTag,
        outgoing: bool,
        started: Option<Instant>,
        result: Result<Prepared, CoreError>,
    ) {
        let id = tag.peer_id.clone();
        if outgoing
            && !self
                .peers
                .get(&id)
                .and_then(|p| p.pending.as_ref())
                .is_some_and(|p| p.tag.is_current(&tag))
        {
            if let Ok(prepared) = result {
                prepared.connection.close(0u32.into(), b"obsolete dial");
            }
            return;
        }
        let pending = if outgoing {
            self.peers.get_mut(&id).and_then(|p| p.pending.take())
        } else {
            None
        };
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                if let Some(pending) = pending {
                    let peer = self.peers.get_mut(&id).unwrap();
                    let reply = if peer.session.is_some() {
                        Ok(NetworkReply::Peer(peer.info.clone()))
                    } else {
                        peer.info.phase = PeerPhase::Disconnected;
                        Err(error.clone())
                    };
                    if let Some(sender) = pending.reply {
                        let _ = sender.send(reply);
                    }
                    self.publish(Some(&id));
                }
                self.bus.publish(CoreEvent::Error {
                    code: error.code,
                    detail: error.detail,
                });
                return;
            }
        };
        let preferred_outgoing = self.key.public() < prepared.connection.remote_id();
        if let Some(existing) = self.peers.get(&id).and_then(|p| p.session.as_ref()) {
            if existing.outgoing == preferred_outgoing || outgoing != preferred_outgoing {
                prepared.connection.close(0u32.into(), b"duplicate session");
                if let Some(sender) = pending.and_then(|p| p.reply) {
                    let _ = sender.send(Ok(NetworkReply::Peer(self.peers[&id].info.clone())));
                }
                return;
            }
        } else if !self
            .peers
            .get(&id)
            .is_some_and(|p| p.pending.is_some() || pending.is_some())
            && self
                .peers
                .values()
                .filter(|p| p.session.is_some() || p.pending.is_some())
                .count()
                >= 8
        {
            prepared.connection.close(0u32.into(), b"peer limit");
            return;
        }
        let target = EndpointAddr::from_parts(
            prepared.connection.remote_id(),
            prepared
                .connection
                .paths()
                .iter()
                .map(|p| p.remote_addr().clone())
                .collect::<Vec<_>>(),
        );
        if !self.make_room(&id) {
            prepared.connection.close(0u32.into(), b"peer limit");
            return;
        }
        let session_tag = self.tag(id.clone());
        let peer = self.peers.entry(id.clone()).or_insert_with(|| Peer {
            info: PeerInfo {
                peer_id: id.clone(),
                phase: PeerPhase::Disconnected,
                path: PeerPath::Unknown,
                connect_ms: None,
                direct_ms: None,
            },
            target,
            session: None,
            pending: None,
            disconnected_before: None,
        });
        if let Some(old) = peer.session.take() {
            old.close();
        }
        peer.info.phase = PeerPhase::Connected;
        peer.info.path = PeerPath::Unknown;
        peer.info.connect_ms =
            started.map(|start| duration_ms(prepared.ready_at.saturating_duration_since(start)));
        peer.info.direct_ms = None;
        peer.session = Some(session::spawn_session(
            &mut self.tasks,
            prepared,
            session_tag,
            outgoing,
            started,
            &self.cancel,
            &self.notes,
        ));
        let info = peer.info.clone();
        self.publish(Some(&id));
        if let Some(sender) = pending.and_then(|p| p.reply) {
            let _ = sender.send(Ok(NetworkReply::Peer(info)));
        }
    }
    async fn stop(&mut self) {
        self.finish_activation(Err(err(ErrorCode::Busy)));
        self.cancel.cancel();
        self.epoch += 1;
        for peer in self.peers.values_mut() {
            if let Some(pending) = peer.pending.take() {
                pending.cancel.cancel();
                if let Some(reply) = pending.reply {
                    let _ = reply.send(Err(err(ErrorCode::NetworkNotStarted)));
                }
            }
            if let Some(session) = peer.session.take() {
                session.close();
            }
            peer.info.phase = PeerPhase::Disconnected;
            peer.info.path = PeerPath::Unknown;
        }
        if let Some(endpoint) = self.endpoint.take() {
            endpoint.close().await;
        }
        while self.tasks.join_next().await.is_some() {}
        while let Ok(note) = self.received.try_recv() {
            note.discard().await;
        }
        self.incoming.clear();
        self.received_incoming = self.accepted.load(Ordering::Acquire);
        self.release_cutoffs();
        if self.info.state != NetworkState::Stopped {
            self.info.state = NetworkState::Stopped;
            self.info.ticket = None;
            let ids: Vec<String> = self.peers.keys().cloned().collect();
            self.publish_peers(&ids.iter().map(String::as_str).collect::<Vec<_>>());
        } else {
            self.bus
                .publish(CoreEvent::ConnectionState(ConnectionSnapshot::OFFLINE));
        }
    }
}
fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}
pub(crate) fn retry_delay(attempt: u32) -> Duration {
    Duration::from_secs([1, 2, 4, 8][attempt.min(3) as usize])
}
pub(crate) async fn within_retry_budget<F: std::future::Future>(
    deadline: tokio::time::Instant,
    cancel: &CancellationToken,
    work: F,
) -> Option<F::Output> {
    tokio::select! { biased; _ = cancel.cancelled() => None, _ = tokio::time::sleep_until(deadline) => None, result = work => Some(result) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn key_transition_rejects_old_epoch_notes_and_full_control_queue_stays_gated() {
        let mut manager = Manager::new(NetworkProfile::Local { relay: None }, EventBus::new());
        manager.endpoint = Some(bind_endpoint(&manager.profile, manager.key.clone()).await.unwrap());
        let old_epoch = manager.epoch;
        let ticket = manager.gate.block();
        let (reply, received) = oneshot::channel();
        let key = SecretKey::generate();
        manager.control(Control::Activate { key: Box::new(key.clone()), start: false, ticket, reply }).await;
        received.await.unwrap().unwrap();
        let before = manager.info.clone();
        manager.cancel = CancellationToken::new();
        let obsolete = bind_endpoint(&manager.profile, SecretKey::generate()).await.unwrap();
        manager.note(Note::Bound { epoch: old_epoch, result: Ok(obsolete) }).await;
        manager.note(Note::Text { tag: SessionTag { epoch: old_epoch, generation: 0, peer_id: "old".into() }, text: "obsolete".into() }).await;
        assert_eq!(manager.info, before);
        assert_eq!(manager.key.public(), key.public());
        let (commands, _) = mpsc::channel(1);
        let (controls, _receiver) = mpsc::channel(1);
        let (reply, _) = oneshot::channel();
        controls.try_send(Control::Quiesce { reply }).ok().unwrap();
        let net = NetworkHandle { commands, controls, gate: Arc::default() };
        assert_eq!(net.activate_key(key).await.unwrap_err().code, ErrorCode::Busy);
        assert!(net.gate.blocked());
        assert_eq!(net.execute(NetworkOp::Start).await.unwrap_err().code, ErrorCode::Busy);
    }
    struct Sink(mpsc::Sender<CoreEvent>);
    impl crate::EventSink for Sink {
        fn send(&self, event: CoreEvent) -> bool {
            self.0.try_send(event).is_ok()
        }
    }
    async fn advertise(endpoint: &Endpoint, added: std::net::SocketAddr) {
        endpoint.add_external_addr(added).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut addresses = endpoint.watch_addr();
            while !addresses.get().ip_addrs().any(|addr| *addr == added) {
                addresses.updated().await.unwrap();
            }
        }).await.unwrap();
    }
    #[tokio::test]
    async fn own_ticket_follows_live_endpoint_address() {
        // Public is only the serializer policy: never Start/bind a Public endpoint.
        let bus = EventBus::new();
        let (events, mut received_events) = mpsc::channel(64);
        bus.subscribe(Arc::new(Sink(events)));
        let mut manager = Manager::new(NetworkProfile::Public, bus);
        let endpoint = bind_endpoint(
            &NetworkProfile::Local { relay: None },
            manager.key.clone(),
        )
        .await
        .unwrap();
        manager.note(Note::Bound { epoch: 0, result: Ok(endpoint.clone()) }).await;
        let initial = manager.info.clone();
        let added = "127.0.0.1:42001".parse().unwrap();
        assert!(!parse_target(manager.info.ticket.as_ref().unwrap()).unwrap().ip_addrs().any(|addr| *addr == added));
        let (commands, receiver) = mpsc::channel(64);
        let (controls, received_controls) = mpsc::channel(8);
        let handle = NetworkHandle { commands, controls, gate: manager.gate.clone() };
        manager.controls = Some(received_controls);
        let actor = tokio::spawn(manager.run(receiver));
        advertise(&endpoint, added).await;
        let fresh = tokio::time::timeout(Duration::from_millis(300), async {
            loop {
                let NetworkReply::Info(info) = handle.execute(NetworkOp::Info).await.unwrap() else { unreachable!() };
                if parse_target(info.ticket.as_ref().unwrap()).unwrap() == endpoint.addr() {
                    break info;
                }
                tokio::task::yield_now().await;
            }
        }).await;
        handle.execute(NetworkOp::Stop).await.unwrap();
        drop(handle);
        actor.await.unwrap();
        assert!(fresh.is_ok(), "manager Info kept the initial ticket after the SDK advertised the added address");
        let fresh = fresh.unwrap();
        assert_eq!(fresh.state, NetworkState::Online);
        assert_eq!(fresh.endpoint_id, initial.endpoint_id);
        assert_eq!(fresh.peers, initial.peers);
        assert!(fresh.revision > initial.revision);
        let mut published = false;
        while let Ok(event) = received_events.try_recv() {
            if let CoreEvent::NetworkChanged(info) = event {
                published |= info == fresh;
            }
        }
        assert!(published, "the current ticket must reach consumers in a full snapshot");
    }
    #[tokio::test]
    async fn own_address_revision_changes_once_and_ignores_obsolete_epochs() {
        // Only the serialization policy is Public; both injected endpoints are Local.
        let bus = EventBus::new();
        let (events, mut received_events) = mpsc::channel(64);
        bus.subscribe(Arc::new(Sink(events)));
        let mut manager = Manager::new(NetworkProfile::Public, bus);
        let local = NetworkProfile::Local { relay: None };
        let endpoint = bind_endpoint(&local, manager.key.clone()).await.unwrap();
        manager.note(Note::Bound { epoch: 0, result: Ok(endpoint.clone()) }).await;
        let added = "127.0.0.1:42002".parse().unwrap();
        advertise(&endpoint, added).await;
        while received_events.try_recv().is_ok() {}
        let before = manager.info.revision;
        manager.note(Note::OwnAddress { epoch: 0 }).await;
        assert_eq!(manager.info.revision, before + 1);
        assert!(parse_target(manager.info.ticket.as_ref().unwrap()).unwrap().ip_addrs().any(|addr| *addr == added));
        assert!(matches!(received_events.try_recv().unwrap(), CoreEvent::NetworkChanged(info) if info == manager.info));
        assert!(matches!(received_events.try_recv().unwrap(), CoreEvent::ConnectionState(state) if state.direct_peers == 0));
        assert!(received_events.try_recv().is_err());
        let current = manager.info.clone();
        manager.note(Note::OwnAddress { epoch: 0 }).await;
        assert_eq!(manager.info, current);
        assert!(received_events.try_recv().is_err());

        manager.stop().await;
        let stopped = manager.info.clone();
        manager.note(Note::OwnAddress { epoch: 0 }).await;
        manager.note(Note::OwnAddress { epoch: manager.epoch }).await;
        assert_eq!(manager.info, stopped);
        assert!(manager.info.ticket.is_none());

        // Reproduce a new Start's epoch and Bound, without ever binding Public.
        manager.epoch += 1;
        manager.cancel = CancellationToken::new();
        manager.info.state = NetworkState::Starting;
        let replacement = bind_endpoint(&local, manager.key.clone()).await.unwrap();
        let epoch = manager.epoch;
        manager.note(Note::Bound { epoch, result: Ok(replacement.clone()) }).await;
        assert_eq!(manager.info.endpoint_id, current.endpoint_id);
        let rebound = manager.info.clone();
        advertise(&replacement, added).await;
        manager.note(Note::OwnAddress { epoch: 0 }).await;
        assert_eq!(manager.info, rebound);
        manager.note(Note::OwnAddress { epoch }).await;
        assert_eq!(manager.info.revision, rebound.revision + 1);
        assert!(parse_target(manager.info.ticket.as_ref().unwrap()).unwrap() == replacement.addr());
        tokio::time::timeout(Duration::from_secs(2), manager.stop()).await.unwrap();
        assert!(manager.tasks.is_empty());
    }
    #[tokio::test]
    async fn own_address_watch_stop_joins_while_pending_or_note_queue_full() {
        for full in [false, true] {
            let mut manager = Manager::new(NetworkProfile::Local { relay: None }, EventBus::new());
            let endpoint = bind_endpoint(&manager.profile, manager.key.clone()).await.unwrap();
            manager.note(Note::Bound { epoch: 0, result: Ok(endpoint.clone()) }).await;
            // The initial watcher item proves the tracked child has started.
            let initial = tokio::time::timeout(Duration::from_secs(2), manager.received.recv()).await.unwrap().unwrap();
            assert!(matches!(initial, Note::OwnAddress { epoch: 0 }));
            manager.note(initial).await;
            if full {
                while manager.notes.try_send(Note::OwnAddress { epoch: 0 }).is_ok() {}
                assert_eq!(manager.notes.capacity(), 0);
                advertise(&endpoint, "127.0.0.1:42003".parse().unwrap()).await;
                tokio::task::yield_now().await;
                assert_eq!(manager.notes.capacity(), 0);
            }
            tokio::time::timeout(Duration::from_secs(2), manager.stop()).await.unwrap();
            assert!(manager.tasks.is_empty());
            assert!(manager.received.try_recv().is_err());
            assert_eq!(manager.info.state, NetworkState::Stopped);
        }
    }
    #[tokio::test]
    async fn local_direct_ticket_keeps_bound_loopback_sockets_after_address_update() {
        let mut manager = Manager::new(NetworkProfile::Local { relay: None }, EventBus::new());
        let endpoint = bind_endpoint(&manager.profile, manager.key.clone()).await.unwrap();
        manager.note(Note::Bound { epoch: 0, result: Ok(endpoint.clone()) }).await;
        let initial = manager.info.clone();
        let address = parse_target(initial.ticket.as_ref().unwrap()).unwrap();
        assert!(address.ip_addrs().all(|addr| addr.ip().is_loopback()));
        advertise(&endpoint, "127.0.0.1:42004".parse().unwrap()).await;
        manager.note(Note::OwnAddress { epoch: 0 }).await;
        assert_eq!(manager.info, initial);
        tokio::time::timeout(Duration::from_secs(2), manager.stop()).await.unwrap();
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    #[tokio::test]
    async fn explicit_stop_and_disconnect_clear_intent_during_rebind() {
        for stop in [false, true] {
            let mut manager = Manager::new(NetworkProfile::Local { relay: None }, EventBus::new());
            let id = SecretKey::generate().public();
            manager.peers.insert(id.to_string(), inactive_peer(id));
            manager.offline_targets.insert(id.to_string(), id.into());
            manager.info.state = NetworkState::Starting;
            let (reply, received) = oneshot::channel();
            let op = if stop { NetworkOp::Stop } else { NetworkOp::Disconnect { peer_id: id.to_string() } };
            manager.command(Envelope { op, reply }).await;
            received.await.unwrap().unwrap();
            assert!(manager.offline_targets.is_empty());
            assert!(manager.peers.values().all(|p| p.pending.is_none() && p.session.is_none()));
        }
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    #[tokio::test]
    async fn offline_during_rebind_keeps_unrestored_reconnect_intent() {
        let mut manager = Manager::new(NetworkProfile::Local { relay: None }, EventBus::new());
        let id = SecretKey::generate().public();
        manager.peers.insert(id.to_string(), inactive_peer(id));
        manager.offline_targets.insert(id.to_string(), id.into());
        manager.info.state = NetworkState::Starting;
        let (reply, received) = oneshot::channel();
        manager.command(Envelope { op: NetworkOp::SetOffline { offline: true }, reply }).await;
        received.await.unwrap().unwrap();
        assert!(manager.offline_targets.contains_key(&id.to_string()));
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    #[tokio::test]
    async fn local_config_cannot_replace_public_starting_or_online_profile() {
        let mut manager = Manager::new(NetworkProfile::Public, EventBus::new());
        for state in [
            NetworkState::Starting,
            NetworkState::Online,
            NetworkState::Failed,
        ] {
            manager.info.state = state;
            let (reply, received) = oneshot::channel();
            manager
                .command(Envelope {
                    op: NetworkOp::ConfigureLocal { relay: None },
                    reply,
                })
                .await;
            assert_eq!(
                received.await.unwrap().unwrap_err().code,
                ErrorCode::InvalidConfig
            );
            assert!(matches!(manager.profile, NetworkProfile::Public));
            assert!(manager.endpoint.is_none());
        }
    }
    fn inactive_peer(id: iroh::EndpointId) -> Peer {
        Peer {
            info: PeerInfo {
                peer_id: id.to_string(),
                phase: PeerPhase::Disconnected,
                path: PeerPath::Unknown,
                connect_ms: None,
                direct_ms: None,
            },
            target: id.into(),
            session: None,
            pending: None,
            disconnected_before: None,
        }
    }
    #[tokio::test]
    async fn unknown_identity_protects_eight_slots_until_identified_and_canceled_cleanup_arrives() {
        let mut manager = Manager::new(NetworkProfile::Local { relay: None }, EventBus::new());
        manager.endpoint = Some(
            bind_endpoint(&manager.profile, manager.key.clone())
                .await
                .unwrap(),
        );
        let ids: Vec<_> = (0..8).map(|_| SecretKey::generate().public()).collect();
        manager.accepted.store(1, Ordering::Release);
        // The accepted incoming note has not reached the actor yet.
        for id in &ids {
            manager.peers.insert(id.to_string(), inactive_peer(*id));
            let (reply, _) = oneshot::channel();
            manager
                .command(Envelope {
                    op: NetworkOp::Disconnect {
                        peer_id: id.to_string(),
                    },
                    reply,
                })
                .await;
        }
        assert!(
            manager
                .peers
                .values()
                .all(|p| p.disconnected_before == Some(1))
        );
        let (reply, rx) = oneshot::channel();
        manager.connect(SecretKey::generate().public().to_string(), reply);
        assert_eq!(rx.await.unwrap().unwrap_err().code, ErrorCode::Busy);
        assert_eq!(manager.peers.len(), 8);
        manager.received_incoming = 1;
        let cancel = manager.cancel.child_token();
        manager.incoming.insert(
            99,
            IncomingPreparation {
                sequence: 1,
                peer_id: None,
                cancel: cancel.clone(),
            },
        );
        let tag = SessionTag {
            epoch: manager.epoch,
            generation: 99,
            peer_id: ids[0].to_string(),
        };
        manager.note(Note::Identified { tag: tag.clone() }).await;
        assert!(cancel.is_cancelled());
        assert!(
            manager
                .peers
                .values()
                .all(|p| p.disconnected_before.is_none())
        );
        // Completion is processed even when the per-preparation token is canceled.
        manager
            .note(Note::Ready {
                tag,
                outgoing: false,
                started: None,
                result: Err(err(ErrorCode::PeerNotConnected)),
            })
            .await;
        assert!(manager.incoming.is_empty());
        assert!(manager.make_room(&SecretKey::generate().public().to_string()));
        assert_eq!(manager.peers.len(), 7);
        manager.stop().await;
    }
    #[tokio::test(start_paused = true)]
    async fn late_identity_only_cancels_matching_pre_disconnect_preparation() {
        let mut manager = Manager::new(NetworkProfile::Local { relay: None }, EventBus::new());
        let id = SecretKey::generate().public();
        manager.peers.insert(id.to_string(), inactive_peer(id));
        manager.accepted.store(2, Ordering::Release);
        manager.received_incoming = 2;
        let unrelated = manager.cancel.child_token();
        let matching = manager.cancel.child_token();
        manager.incoming.insert(
            1,
            IncomingPreparation {
                sequence: 1,
                peer_id: None,
                cancel: unrelated.clone(),
            },
        );
        manager.incoming.insert(
            2,
            IncomingPreparation {
                sequence: 2,
                peer_id: None,
                cancel: matching.clone(),
            },
        );
        let (reply, _) = oneshot::channel();
        manager
            .command(Envelope {
                op: NetworkOp::Disconnect {
                    peer_id: id.to_string(),
                },
                reply,
            })
            .await;
        manager
            .note(Note::Identified {
                tag: SessionTag {
                    epoch: 0,
                    generation: 1,
                    peer_id: SecretKey::generate().public().to_string(),
                },
            })
            .await;
        assert!(!unrelated.is_cancelled());
        assert_eq!(manager.peers[&id.to_string()].disconnected_before, Some(2));
        manager
            .note(Note::Identified {
                tag: SessionTag {
                    epoch: 0,
                    generation: 2,
                    peer_id: id.to_string(),
                },
            })
            .await;
        assert!(matching.is_cancelled());
        assert!(!unrelated.is_cancelled());
        manager.stop().await;
        assert!(manager.incoming.is_empty());
    }
    async fn prepared_pair(
        local: &Endpoint,
        remote: &Endpoint,
        outgoing: bool,
    ) -> (Prepared, Prepared) {
        let profile = NetworkProfile::Local { relay: None };
        let (dialer, receiver) = if outgoing {
            (local, remote)
        } else {
            (remote, local)
        };
        let target = parse_target(&endpoint_ticket(receiver, &profile)).unwrap();
        let (dialed, accepted) = tokio::join!(dialer.connect(target, ALPN), async {
            receiver.accept().await.unwrap().await.unwrap()
        });
        let (local, remote) = if outgoing {
            (dialed.unwrap(), accepted)
        } else {
            (accepted, dialed.unwrap())
        };
        let (left, right) = tokio::join!(
            session::prepare_session(local.clone()),
            session::prepare_session(remote.clone())
        );
        let (send, recv) = left.unwrap();
        let left = Prepared {
            connection: local,
            send,
            recv,
            ready_at: Instant::now(),
        };
        let (send, recv) = right.unwrap();
        (
            left,
            Prepared {
                connection: remote,
                send,
                recv,
                ready_at: Instant::now(),
            },
        )
    }
    #[tokio::test]
    async fn old_session_notes_cannot_change_an_installed_preferred_replacement() {
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::channel(256);
        bus.subscribe(std::sync::Arc::new(Sink(tx)));
        let mut manager = Manager::new(NetworkProfile::Local { relay: None }, bus);
        let mut remote_key = SecretKey::generate();
        if manager.key.public() > remote_key.public() {
            std::mem::swap(&mut manager.key, &mut remote_key);
        }
        let local = bind_endpoint(&manager.profile, manager.key.clone())
            .await
            .unwrap();
        let remote = bind_endpoint(&manager.profile, remote_key).await.unwrap();
        manager.endpoint = Some(local.clone());
        let id = remote.id().to_string();
        let (first, _first_remote) = prepared_pair(&local, &remote, false).await;
        let attempt = manager.tag(id.clone());
        manager.ready(attempt, false, None, Ok(first));
        let old = manager.peers[&id].session.as_ref().unwrap().tag.clone();
        let (replacement, _replacement_remote) = prepared_pair(&local, &remote, true).await;
        let attempt = manager.tag(id.clone());
        manager.peers.get_mut(&id).unwrap().pending = Some(Pending {
            tag: attempt.clone(),
            cancel: manager.cancel.child_token(),
            reply: None,
        });
        manager.ready(attempt, true, Some(Instant::now()), Ok(replacement));
        let current = manager.peers[&id].session.as_ref().unwrap().tag.clone();
        assert!(current.generation > old.generation);
        manager
            .note(Note::Path {
                tag: current.clone(),
                path: PeerPath::Direct,
                observed: Instant::now(),
            })
            .await;
        let revision = manager.info.revision;
        while rx.try_recv().is_ok() {}
        manager
            .note(Note::Ended {
                tag: old.clone(),
                error: None,
            })
            .await;
        manager
            .note(Note::Path {
                tag: old.clone(),
                path: PeerPath::Relay,
                observed: Instant::now(),
            })
            .await;
        manager
            .note(Note::Text {
                tag: old,
                text: "obsolete".into(),
            })
            .await;
        assert!(manager.current(&current));
        assert_eq!(manager.peers[&id].info.phase, PeerPhase::Connected);
        assert_eq!(manager.peers[&id].info.path, PeerPath::Direct);
        assert_eq!(manager.info.revision, revision);
        assert!(rx.try_recv().is_err());
        manager
            .note(Note::Text {
                tag: current,
                text: "current".into(),
            })
            .await;
        assert!(
            matches!(rx.try_recv().unwrap(), CoreEvent::ProbeTextReceived { text, .. } if text == "current")
        );
        assert!(rx.try_recv().is_err());
        manager.stop().await;
        remote.close().await;
    }
    #[tokio::test(start_paused = true)]
    async fn relay_and_connecting_peers_are_not_counted_as_direct() {
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::channel(256);
        bus.subscribe(std::sync::Arc::new(Sink(tx)));
        let mut manager = Manager::new(NetworkProfile::Local { relay: None }, bus);
        for (phase, path) in [
            (PeerPhase::Connected, PeerPath::Relay),
            (PeerPhase::Connecting, PeerPath::Direct),
            (PeerPhase::Connected, PeerPath::Direct),
        ] {
            let id = SecretKey::generate().public();
            manager.peers.insert(
                id.to_string(),
                Peer {
                    info: PeerInfo {
                        peer_id: id.to_string(),
                        phase,
                        path,
                        connect_ms: None,
                        direct_ms: None,
                    },
                    target: id.into(),
                    session: None,
                    pending: None,
                    disconnected_before: None,
                },
            );
        }
        manager.publish(None);
        assert!(matches!(
            rx.recv().await,
            Some(CoreEvent::NetworkChanged(_))
        ));
        assert!(matches!(
            rx.recv().await,
            Some(CoreEvent::ConnectionState(ConnectionSnapshot {
                direct_peers: 1,
                mailbox_ok: false,
                queue_ok: None
            }))
        ));
    }
    #[tokio::test(start_paused = true)]
    async fn never_started_actor_is_silent_even_when_last_handle_is_dropped() {
        let bus = EventBus::new();
        let (tx, mut rx) = mpsc::channel(256);
        bus.subscribe(std::sync::Arc::new(Sink(tx)));
        let net = NetworkHandle::spawn(
            &tokio::runtime::Handle::current(),
            NetworkProfile::Local { relay: None },
            bus,
        );
        net.execute(NetworkOp::Info).await.unwrap();
        assert!(rx.try_recv().is_err());
        drop(net);
        tokio::task::yield_now().await;
        assert!(
            rx.try_recv().is_err(),
            "implicit drop emitted an extra Offline event"
        );
    }
    #[test]
    fn earlier_session_cannot_remove_replacement() {
        let old = SessionTag {
            epoch: 1,
            generation: 4,
            peer_id: "p".into(),
        };
        let current = SessionTag {
            epoch: 1,
            generation: 5,
            peer_id: "p".into(),
        };
        assert!(!old.is_current(&current));
        assert!(current.is_current(&current));
        assert!(
            !SessionTag {
                epoch: 2,
                ..current.clone()
            }
            .is_current(&current)
        );
    }
    #[tokio::test(start_paused = true)]
    async fn retry_intervals_and_original_deadline_cancel_inflight_work() {
        assert_eq!(
            (0..6).map(retry_delay).collect::<Vec<_>>(),
            [1, 2, 4, 8, 8, 8].map(Duration::from_secs)
        );
        let start = tokio::time::Instant::now();
        let cancel = CancellationToken::new();
        let deadline = start + Duration::from_secs(30);
        for attempt in 0..4 {
            assert!(
                within_retry_budget(deadline, &cancel, tokio::time::sleep(retry_delay(attempt)))
                    .await
                    .is_some()
            );
        }
        assert_eq!(tokio::time::Instant::now() - start, Duration::from_secs(15));
        assert!(
            within_retry_budget(deadline, &cancel, std::future::pending::<()>())
                .await
                .is_none()
        );
        assert_eq!(tokio::time::Instant::now() - start, Duration::from_secs(30));
        cancel.cancel();
        assert!(
            within_retry_budget(
                deadline + Duration::from_secs(30),
                &cancel,
                std::future::ready(())
            )
            .await
            .is_none()
        );
    }
    #[tokio::test(start_paused = true)]
    async fn command_queue_is_bounded_and_closed_is_transport_failed() {
        let net = NetworkHandle::spawn(
            &tokio::runtime::Handle::current(),
            NetworkProfile::Local { relay: None },
            EventBus::new(),
        );
        for _ in 0..64 {
            assert!(futures_util::poll!(Box::pin(net.execute(NetworkOp::Info))).is_pending());
        }
        assert_eq!(
            net.execute(NetworkOp::Info).await.unwrap_err().code,
            ErrorCode::Busy
        );
        let (commands, receiver) = mpsc::channel(64);
        let (controls, _) = mpsc::channel(8);
        let net = NetworkHandle { commands, controls, gate: Arc::default() };
        drop(receiver);
        assert_eq!(
            net.execute(NetworkOp::Info).await.unwrap_err().code,
            ErrorCode::TransportFailed
        );
    }
    #[tokio::test(start_paused = true)]
    async fn full_note_queue_does_not_block_cancellation() {
        let (notes, mut received) = mpsc::channel(64);
        for _ in 0..64 {
            notes
                .try_send(Note::Ended {
                    tag: SessionTag {
                        epoch: 0,
                        generation: 0,
                        peer_id: String::new(),
                    },
                    error: None,
                })
                .ok()
                .unwrap();
        }
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let task = tokio::spawn(async move {
            session::notify(
                &notes,
                &token,
                Note::Ended {
                    tag: SessionTag {
                        epoch: 0,
                        generation: 0,
                        peer_id: String::new(),
                    },
                    error: None,
                },
            )
            .await;
        });
        tokio::task::yield_now().await;
        cancel.cancel();
        task.await.unwrap();
        assert_eq!(received.len(), 64);
        received.close();
    }
    #[tokio::test]
    async fn disconnected_history_is_evicted_at_eight_peers() {
        let mut manager = Manager::new(NetworkProfile::Local { relay: None }, EventBus::new());
        manager.endpoint = Some(
            bind_endpoint(&manager.profile, manager.key.clone())
                .await
                .unwrap(),
        );
        for _ in 0..12 {
            let id = SecretKey::generate().public().to_string();
            let (reply, _) = oneshot::channel();
            manager.connect(id.clone(), reply);
            let (reply, _) = oneshot::channel();
            manager
                .command(Envelope {
                    op: NetworkOp::Disconnect { peer_id: id },
                    reply,
                })
                .await;
        }
        assert!(
            manager.peers.len() <= 8,
            "disconnected history exceeded peer cap"
        );
        manager.stop().await;
    }
    #[tokio::test(start_paused = true)]
    async fn stale_ready_cannot_complete_new_pending_dial() {
        let mut manager = Manager::new(NetworkProfile::Local { relay: None }, EventBus::new());
        manager.epoch = 2;
        let id = SecretKey::generate().public();
        let tag = SessionTag {
            epoch: 2,
            generation: 5,
            peer_id: id.to_string(),
        };
        let (reply, mut received) = oneshot::channel();
        manager.peers.insert(
            id.to_string(),
            Peer {
                info: PeerInfo {
                    peer_id: id.to_string(),
                    phase: PeerPhase::Connecting,
                    path: PeerPath::Unknown,
                    connect_ms: None,
                    direct_ms: None,
                },
                target: id.into(),
                disconnected_before: None,
                session: None,
                pending: Some(Pending {
                    tag: tag.clone(),
                    cancel: CancellationToken::new(),
                    reply: Some(reply),
                }),
            },
        );
        for obsolete in [
            SessionTag {
                generation: 4,
                ..tag.clone()
            },
            SessionTag {
                epoch: 1,
                ..tag.clone()
            },
        ] {
            manager
                .note(Note::Ready {
                    tag: obsolete.clone(),
                    outgoing: true,
                    started: None,
                    result: Err(err(ErrorCode::TransportFailed)),
                })
                .await;
            manager
                .note(Note::Ended {
                    tag: obsolete.clone(),
                    error: None,
                })
                .await;
            manager
                .note(Note::Path {
                    tag: obsolete,
                    path: PeerPath::Direct,
                    observed: Instant::now(),
                })
                .await;
        }
        assert!(matches!(
            received.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(
            manager.peers[&id.to_string()].info.phase,
            PeerPhase::Connecting
        );
        assert_eq!(manager.info.revision, 0);
        manager.stop().await;
        assert_eq!(
            received.await.unwrap().unwrap_err().code,
            ErrorCode::NetworkNotStarted
        );
    }
}
