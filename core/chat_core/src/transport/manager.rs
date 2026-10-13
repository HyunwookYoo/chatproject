use super::{
    NetworkInfo, NetworkState, PeerInfo, PeerPath, PeerPhase,
    endpoint::{NetworkProfile, bind_endpoint, endpoint_ticket, parse_target},
    frame::{ALPN, validate_text},
    session::{self, Prepared, Reply, Session, SessionTag, Write},
};
use crate::{ConnectionSnapshot, CoreError, CoreEvent, ErrorCode, EventBus};
use iroh::{Endpoint, EndpointAddr, SecretKey, endpoint::Incoming};
use std::{
    collections::BTreeMap,
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
#[derive(Clone)]
pub struct NetworkHandle {
    commands: mpsc::Sender<Envelope>,
}
impl NetworkHandle {
    pub fn spawn(handle: &tokio::runtime::Handle, profile: NetworkProfile, bus: EventBus) -> Self {
        let (commands, receiver) = mpsc::channel(64);
        handle.spawn(Manager::new(profile, bus).run(receiver));
        Self { commands }
    }
    pub async fn execute(&self, op: NetworkOp) -> Result<NetworkReply, CoreError> {
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
    Bound {
        epoch: u64,
        result: Result<Endpoint, CoreError>,
    },
    Incoming {
        epoch: u64,
        incoming: Incoming,
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
            _ => {}
        }
    }
}
struct Pending {
    tag: SessionTag,
    cancel: CancellationToken,
    reply: Option<Reply>,
}
struct Peer {
    info: PeerInfo,
    target: EndpointAddr,
    session: Option<Session>,
    pending: Option<Pending>,
}
struct Manager {
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
    incoming: usize,
}
impl Manager {
    fn new(profile: NetworkProfile, bus: EventBus) -> Self {
        let (notes, received) = mpsc::channel(64);
        Self {
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
            incoming: 0,
        }
    }
    async fn run(mut self, mut commands: mpsc::Receiver<Envelope>) {
        loop {
            tokio::select! {
                command = commands.recv() => match command { Some(command) => self.command(command).await, None => { if self.info.state != NetworkState::Stopped { self.stop().await; } break; } },
                Some(note) = self.received.recv() => self.note(note).await,
                Some(_) = self.tasks.join_next(), if !self.tasks.is_empty() => {},
            }
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
        let result = match op {
            NetworkOp::Info => Ok(NetworkReply::Info(self.info.clone())),
            NetworkOp::Start => {
                if matches!(
                    self.info.state,
                    NetworkState::Stopped | NetworkState::Failed
                ) {
                    self.cancel = CancellationToken::new();
                    self.epoch += 1;
                    self.info.state = NetworkState::Starting;
                    self.publish(None);
                    let profile = self.profile.clone();
                    let key = self.key.clone();
                    let epoch = self.epoch;
                    let cancel = self.cancel.clone();
                    let notes = self.notes.clone();
                    self.tasks.spawn(async move {
                        let result = tokio::select! { biased; _ = cancel.cancelled() => return, result = bind_endpoint(&profile, key) => result };
                        #[cfg(any(test, all(feature = "test-hooks", debug_assertions)))]
                        if let Ok(endpoint) = &result {
                            if matches!(profile, NetworkProfile::Local { relay: Some(_) }) {
                                let ready = tokio::select! { biased; _ = cancel.cancelled() => false, result = tokio::time::timeout(Duration::from_secs(30), endpoint.online()) => result.is_ok() };
                                if !ready { endpoint.close().await; session::notify(&notes, &cancel, Note::Bound { epoch, result: Err(err(ErrorCode::NetworkTimeout)) }).await; return; }
                            }
                        }
                        session::notify(&notes, &cancel, Note::Bound { epoch, result }).await;
                    });
                }
                Ok(NetworkReply::Info(self.info.clone()))
            }
            NetworkOp::Stop => {
                self.stop().await;
                Ok(NetworkReply::Done)
            }
            NetworkOp::Connect { target } => {
                self.connect(target, reply);
                return;
            }
            NetworkOp::Send { peer_id, text } => {
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
                if let Some(peer) = self.peers.get_mut(&peer_id) {
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
                Ok(NetworkReply::Done)
            }
        };
        let _ = reply.send(result);
    }
    fn make_room(&mut self, id: &str) {
        if !self.peers.contains_key(id) && self.peers.len() >= 8 {
            let old = self
                .peers
                .iter()
                .find(|(_, p)| p.session.is_none() && p.pending.is_none())
                .map(|(id, _)| id.clone());
            if let Some(old) = old {
                self.peers.remove(&old);
            }
        }
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
        self.make_room(&id);
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
        match note {
            Note::Bound { epoch, result } if epoch == self.epoch => {
                match result {
                    Ok(endpoint) => {
                        self.info.endpoint_id = Some(endpoint.id().to_string());
                        self.info.ticket = Some(endpoint_ticket(&endpoint, &self.profile));
                        self.info.state = NetworkState::Online;
                        let accept_endpoint = endpoint.clone();
                        let cancel = self.cancel.clone();
                        let notes = self.notes.clone();
                        self.tasks.spawn(async move { loop {
                            let incoming = tokio::select! { biased; _ = cancel.cancelled() => break, incoming = accept_endpoint.accept() => match incoming { Some(incoming) => incoming, None => break } };
                            session::notify(&notes, &cancel, Note::Incoming { epoch, incoming }).await;
                        }});
                        self.endpoint = Some(endpoint);
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
            }
            Note::Incoming { epoch, incoming } if epoch == self.epoch => {
                if self.incoming >= 8 {
                    incoming.refuse();
                    return;
                }
                self.incoming += 1;
                let mut tag = self.tag(String::new());
                let cancel = self.cancel.clone();
                let notes = self.notes.clone();
                self.tasks.spawn(async move {
                    let work = async {
                        let connection = incoming.await.map_err(|_| err(ErrorCode::TransportFailed))?;
                        tag.peer_id = connection.remote_id().to_string();
                        match session::prepare_session(connection.clone()).await {
                            Ok((send, recv)) => Ok(Prepared { connection, send, recv, ready_at: Instant::now() }),
                            Err(error) => { connection.close(1u32.into(), b"session preparation failed"); Err(error) },
                        }
                    };
                    let result = tokio::select! { biased; _ = cancel.cancelled() => return, result = tokio::time::timeout(Duration::from_secs(10), work) => result.unwrap_or_else(|_| Err(err(ErrorCode::NetworkTimeout))) };
                    session::notify(&notes, &cancel, Note::Ready { tag, outgoing: false, started: None, result }).await;
                });
            }
            Note::Ready {
                tag,
                outgoing,
                started,
                result,
            } if tag.epoch == self.epoch => {
                if !outgoing {
                    self.incoming = self.incoming.saturating_sub(1);
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
        self.make_room(&id);
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
        self.incoming = 0;
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
    struct Sink(mpsc::Sender<CoreEvent>);
    impl crate::EventSink for Sink {
        fn send(&self, event: CoreEvent) -> bool {
            self.0.try_send(event).is_ok()
        }
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
        let net = NetworkHandle { commands };
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
