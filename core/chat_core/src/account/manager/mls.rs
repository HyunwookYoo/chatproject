//! The MLS group lifecycle protocol of the account actor (spec 4.4-4.6, frame kind 17).
//!
//! * `ensure_group` first asks the connected devices of the peer and of this account whether
//!   the conversation has a group. If one says yes this device sends it a `JoinRequest` with a
//!   new KeyPackage and waits for the Welcome; otherwise it collects KeyPackages from those
//!   devices and creates the group.
//! * `sync_members` keeps the group in step with both accounts' device lists: one own Commit at
//!   a time, queued in `mls_delivery` for every other member installation (and a Welcome for
//!   every added one) until each of them acknowledges it.
//! * A received Commit or Welcome is applied, settled as a fork or a second group, or held while
//!   a device is missing from a cached record and that record is looked up again.
//! * A fork deeper than the one stored epoch drops the group and rejoins it, rate limited.
//!
//! Every MLS change is one storage-writer operation (`message/mls_group.rs`), a single SQL
//! transaction. Requests to other devices run in spawned tasks and report back through the
//! actor mailbox (`Input::Mls`), so neither a transaction nor an actor turn waits on a peer.
//!
//! A Welcome does not carry the first-Commit hash that `mls_conversation` keeps and that picks
//! the winner between two groups. A device takes it from a `GroupInfo` reply of the installation
//! that sends the Welcome: after its own `GroupQuery` (the join path) it has it already; for a
//! Welcome it did not ask for (the creator added it) it answers `Busy`, queries the sender, and
//! joins on the sender's next delivery attempt.
use super::*;
use crate::account::{
    Lookup, auth,
    connection::{Input, Session},
};
use crate::message::mls_group::{Committed, Delivery, DueDelivery, GroupOp, GroupReply, Outcome};
use crate::message::store::{MessageDbOp, MessageDbReply, conversation_id};
use crate::message::types::ConversationId;
use crate::message::wire::{Frame, RejectReason};
use crate::mls::{
    group::{CLASS_FUTURE_EPOCH, CLASS_OTHER, inspect_key_package},
    identity::MlsCredential,
    wire::{MlsFrame, MlsReply},
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// How long a sent `JoinRequest` waits for its Welcome before the device asks again.
const JOIN_WAIT_MS: u64 = 10_000;
/// Pause before asking again when no device could help.
const RETRY_MS: u64 = 2_000;
/// Pause between two lookups of one account for the holds waiting on it.
const LOOKUP_RETRY_MS: u64 = 30_000;
/// Pause before planning again while the own Commit is unacknowledged.
const SYNC_RETRY_MS: u64 = 1_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);
/// How often an empty `mls_hold` is looked at again (rows can be written outside this actor).
const HOLD_CHECK_MS: u64 = 30_000;
/// A conversation that keeps answering class 2 (a Commit it will never get) this long rejoins.
const MISSING_REJOIN_MS: u64 = 30_000;
/// An account that refused kind 17 (an M4 app) is not asked again for this long, doubling up to
/// `OUTDATED_MAX_MS`, unless its record changes.
const OUTDATED_RETRY_MS: u64 = 300_000;
const OUTDATED_MAX_MS: u64 = 3_600_000;
/// The conversation argument of group operations that are not about one conversation.
const NO_CONVERSATION: ConversationId = ConversationId([0; 32]);

/// Where `ensure_group` stands for a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupState {
    /// The group exists on this device.
    Ready,
    /// A `JoinRequest` was accepted; the Welcome has not arrived yet.
    Joining,
    /// Asking, collecting KeyPackages, or no device to ask is connected.
    WaitingForPeer,
}

enum Phase {
    Querying {
        peer: PeerId,
        waiting: BTreeSet<InstallationId>,
        /// The answering installation with the smallest first-Commit hash.
        found: Option<(InstallationId, [u8; 32])>,
        asked: Vec<InstallationId>,
    },
    Collecting {
        peer: PeerId,
        waiting: BTreeSet<InstallationId>,
        key_packages: Vec<Vec<u8>>,
    },
    Joining {
        since: u64,
    },
    Idle {
        until: u64,
    },
}

/// KeyPackages being collected for one `sync_members` Commit.
struct Collect {
    peer: PeerId,
    waiting: BTreeSet<InstallationId>,
    key_packages: Vec<Vec<u8>>,
    remove: Vec<InstallationId>,
}

#[derive(Default)]
pub(crate) struct Runtime {
    /// Conversations that need a group; every poll drives `ensure_group` until it is ready.
    wanted: BTreeMap<ConversationId, PeerId>,
    phases: BTreeMap<ConversationId, Phase>,
    /// First-Commit hashes from `GroupInfo` replies, by conversation and answering installation;
    /// each is used for one Welcome from that installation.
    known: BTreeMap<(ConversationId, InstallationId), [u8; 32]>,
    learning: BTreeSet<(ConversationId, InstallationId)>,
    /// Hash of the Welcome each conversation's group was joined with, to tell a resent Welcome
    /// from a competing one for the same epoch.
    joined_with: BTreeMap<ConversationId, [u8; 32]>,
    /// Conversations to compare with the device lists, not before the given time.
    sync: BTreeMap<ConversationId, (PeerId, u64)>,
    collecting: BTreeMap<ConversationId, Collect>,
    /// One delivery per recipient of a conversation at a time, in epoch order.
    pub(super) inflight: BTreeSet<(ConversationId, InstallationId)>,
    /// `(rejoin_count, rejoin_after_ms)` of a conversation that left its group to rejoin, for
    /// the group it joins or creates next.
    rejoins: BTreeMap<ConversationId, (u32, u64)>,
    /// Earliest next lookup per account with holds.
    lookups: BTreeMap<PeerId, u64>,
    /// Accounts whose lookup is running in a spawned task.
    looking: BTreeSet<PeerId>,
    /// `mls_hold` was found empty at this (scheduler) time; checked again after
    /// `HOLD_CHECK_MS` or as soon as something is held.
    no_holds_since: Option<u64>,
    /// Records or sessions changed since holds were last tried without a lookup.
    recheck: bool,
    /// Conversations answering class 2 (a Commit missing here): since when, and at which epoch.
    missing: BTreeMap<ConversationId, (u64, u64)>,
    /// Hash of the device lists and ready sessions seen by the last poll.
    fingerprint: Option<[u8; 32]>,
    /// Accounts that refused kind 17 like an M4 app: they answered the M4 `Reject
    /// (UnsupportedVersion)`, or, with no sign of MLS ever, closed the stream or connection
    /// without an answer. Messages for them wait for the update (`waiting_peer_update`), and
    /// they are not asked again before the back-off ends or their record changes.
    pub(super) outdated: BTreeMap<PeerId, Outdated>,
    /// Accounts that answered a kind 17 request properly (this run); with `mls_peer` the
    /// evidence that an account speaks MLS, so a closed stream is not taken for a refusal.
    pub(super) spoke: BTreeSet<PeerId>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) hold_delivery: bool,
    /// The M4-peer fixture: this core behaves like a released M4 app (kind 16 only).
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) m4_peer: bool,
    /// The M4-peer fixture closes the connection on a kind 17 frame (what a released M4 app
    /// really does) instead of answering `Reject(UnsupportedVersion)`.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    m4_close: bool,
    /// Every kind 16/17 request frame this actor sent (group requests and message attempts).
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) frames: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    sent: Vec<String>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    held: u64,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    rejoined: u64,
}

/// Back-off of an account that refused kind 17.
#[derive(Clone, Copy)]
pub(crate) struct Outdated {
    until: u64,
    backoff: u64,
    /// Its accepted record when it refused; a newer one may come with an updated app.
    record: Option<[u8; 32]>,
}

#[derive(Clone, Copy)]
enum Task {
    Query {
        conversation: ConversationId,
        installation: InstallationId,
    },
    KeyPackage {
        conversation: ConversationId,
        installation: InstallationId,
        sync: bool,
    },
    Join {
        conversation: ConversationId,
    },
    Learn {
        conversation: ConversationId,
        installation: InstallationId,
    },
    Deliver {
        conversation: ConversationId,
        recipient: InstallationId,
        kind: u8,
        epoch: u64,
    },
    /// A record lookup for the holds waiting on `peer`.
    Lookup {
        peer: PeerId,
    },
}

enum Answer {
    Reply(Result<MlsReply, ErrorCode>),
    Lookup(Result<Lookup, CoreError>),
}

/// The answer to one kind 17 request, or one record lookup, this actor started.
pub(crate) struct MlsEvent {
    ticket: u64,
    /// The account of the device that answered (the looked-up account for a lookup).
    peer: PeerId,
    task: Task,
    answer: Answer,
}

/// One request/response exchange on an authenticated session. A released M4 app answers any
/// kind 17 request with its kind 16 `Reject(UnsupportedVersion)`: `UnsupportedMessageVersion`.
pub(super) async fn exchange(session: &Session, body: &[u8]) -> Result<MlsReply, ErrorCode> {
    let work = async {
        if session.conn.close_reason().is_some() {
            return Err(ErrorCode::TransportFailed);
        }
        let (mut send, mut recv) = session
            .conn
            .open_bi()
            .await
            .map_err(|_| ErrorCode::TransportFailed)?;
        auth::write_frame(&mut send, body)
            .await
            .map_err(|_| ErrorCode::TransportFailed)?;
        send.finish().map_err(|_| ErrorCode::TransportFailed)?;
        // Sent, and the stream or connection ended without an answer: `PeerNotConnected`.
        let reply = auth::read_frame(&mut recv, 61_440)
            .await
            .map_err(|_| ErrorCode::PeerNotConnected)?;
        if let Ok(inner) = auth::payload(&reply, 16) {
            return Err(match crate::message::wire::decode(&inner) {
                Ok(Frame::Reject(RejectReason::UnsupportedVersion)) => {
                    ErrorCode::UnsupportedMessageVersion
                }
                _ => ErrorCode::InvalidMessage,
            });
        }
        let inner = auth::payload(&reply, 17).map_err(|_| ErrorCode::InvalidMessage)?;
        MlsReply::decode(&inner).map_err(|_| ErrorCode::InvalidMessage)
    };
    tokio::time::timeout(REQUEST_TIMEOUT, work)
        .await
        .unwrap_or(Err(ErrorCode::NetworkTimeout))
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// The conversation a request was about (none for a record lookup or a KeyPackage request
/// made for no particular conversation).
fn task_conversation(task: &Task) -> Option<ConversationId> {
    match *task {
        Task::Query { conversation, .. }
        | Task::KeyPackage { conversation, .. }
        | Task::Join { conversation }
        | Task::Learn { conversation, .. }
        | Task::Deliver { conversation, .. } => Some(conversation),
        Task::Lookup { .. } => None,
    }
}

/// What a Commit/Welcome delivery attempt's answer means for its queued row.
pub(super) fn delivery_outcome(result: &Result<MlsReply, ErrorCode>) -> Delivery {
    match result {
        Ok(MlsReply::Ack { .. }) => Delivery::Acked,
        // An earlier Commit is still on its way to the recipient: send this again.
        Ok(MlsReply::DecryptFailed {
            class: CLASS_FUTURE_EPOCH,
            ..
        }) => Delivery::Retry,
        // Refused, or settled as a fork on the other side: resending cannot help.
        Ok(MlsReply::DecryptFailed { .. }) => Delivery::Settled,
        _ => Delivery::Retry,
    }
}

/// What a sent kind 17 frame is, for the test hook: the op, and for Commits and application
/// messages the MLS content type inside.
#[cfg(all(feature = "test-hooks", debug_assertions))]
fn label(body: &[u8]) -> String {
    use crate::mls::membership::content_kind;
    let Some(frame) = auth::payload(body, 17)
        .ok()
        .and_then(|inner| MlsFrame::decode(&inner).ok())
    else {
        return "unknown".into();
    };
    match frame {
        MlsFrame::App { ciphertext, .. } => format!("app:{}", content_kind(&ciphertext)),
        MlsFrame::GroupQuery { .. } => "group_query".into(),
        MlsFrame::KeyPackageRequest => "key_package_request".into(),
        MlsFrame::JoinRequest { .. } => "join_request".into(),
        MlsFrame::Commit { commit, .. } => format!("commit:{}", content_kind(&commit)),
        MlsFrame::Welcome { .. } => "welcome".into(),
    }
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(crate) enum MlsTest {
    Ensure(PeerId),
    Rejoin(PeerId),
    HoldDelivery(bool),
    Sent,
    Held,
    Rejoined,
    Query(PeerId),
    M4Peer(bool),
    M4Close(bool),
    Frames,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(crate) enum MlsTestReply {
    State(GroupState),
    Allowed(bool),
    Done,
    Sent(Vec<String>),
    Count(u64),
    Answers(Vec<(InstallationId, bool)>),
    Frames(Vec<Vec<u8>>),
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
impl AccountHandle {
    async fn mls_test(&self, test: MlsTest) -> Result<MlsTestReply, CoreError> {
        self.send(|reply| Command::Mls(test, reply)).await
    }
    /// What `ensure_group` reports for the conversation with `peer` (and it keeps driving it).
    pub async fn mls_test_ensure_group(&self, peer: PeerId) -> Result<GroupState, CoreError> {
        match self.mls_test(MlsTest::Ensure(peer)).await? {
            MlsTestReply::State(state) => Ok(state),
            _ => unreachable!(),
        }
    }
    /// Leaves the conversation's group to rejoin it; false when the rate limit refuses.
    pub async fn mls_test_rejoin(&self, peer: PeerId) -> Result<bool, CoreError> {
        match self.mls_test(MlsTest::Rejoin(peer)).await? {
            MlsTestReply::Allowed(allowed) => Ok(allowed),
            _ => unreachable!(),
        }
    }
    /// Stops (or restarts) sending queued Commits and Welcomes.
    pub async fn mls_test_hold_delivery(&self, hold: bool) -> Result<(), CoreError> {
        self.mls_test(MlsTest::HoldDelivery(hold)).await.map(|_| ())
    }
    /// Every kind 17 frame this actor sent, as `op` or `op:content type`.
    pub async fn mls_test_sent_frames(&self) -> Result<Vec<String>, CoreError> {
        match self.mls_test(MlsTest::Sent).await? {
            MlsTestReply::Sent(sent) => Ok(sent),
            _ => unreachable!(),
        }
    }
    /// How many received Commits/Welcomes were held for a missing device.
    pub async fn mls_test_held_total(&self) -> Result<u64, CoreError> {
        match self.mls_test(MlsTest::Held).await? {
            MlsTestReply::Count(count) => Ok(count),
            _ => unreachable!(),
        }
    }
    /// How many rejoins this actor started (any cause).
    pub async fn mls_test_rejoin_total(&self) -> Result<u64, CoreError> {
        match self.mls_test(MlsTest::Rejoined).await? {
            MlsTestReply::Count(count) => Ok(count),
            _ => unreachable!(),
        }
    }
    /// Sends a `GroupQuery` for the conversation with `peer` to every connected device of the
    /// peer and of this account and waits for each (5 s limit each): installation, has a group.
    pub async fn mls_test_query(
        &self,
        peer: PeerId,
    ) -> Result<Vec<(InstallationId, bool)>, CoreError> {
        match self.mls_test(MlsTest::Query(peer)).await? {
            MlsTestReply::Answers(answers) => Ok(answers),
            _ => unreachable!(),
        }
    }
    /// The M4-peer fixture: from now on this core behaves like a released M4 app. It sends its
    /// queued texts as kind 16 plaintext frames, answers every kind 17 request with the M4
    /// `Reject(UnsupportedVersion)`, and keeps no MLS group. `false` is the update to M5.
    pub async fn message_test_m4_peer(&self, m4: bool) -> Result<(), CoreError> {
        self.mls_test(MlsTest::M4Peer(m4)).await.map(|_| ())
    }
    /// The M4-peer fixture as a released M4 app really answers kind 17: it closes the
    /// connection (the frame is no account frame it knows).
    pub async fn message_test_m4_peer_closing(&self, m4: bool) -> Result<(), CoreError> {
        self.mls_test(MlsTest::M4Close(m4)).await.map(|_| ())
    }
    /// Every kind 16/17 request frame this core sent so far (complete envelopes).
    pub async fn message_test_sent_frames(&self) -> Result<Vec<Vec<u8>>, CoreError> {
        match self.mls_test(MlsTest::Frames).await? {
            MlsTestReply::Frames(frames) => Ok(frames),
            _ => unreachable!(),
        }
    }
}

impl Manager {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) async fn mls_test(&mut self, test: MlsTest) -> Result<MlsTestReply, CoreError> {
        let own = self.local()?.keys.peer_id();
        Ok(match test {
            MlsTest::Ensure(peer) => {
                MlsTestReply::State(self.ensure_group(conversation_id(own, peer)?, peer).await?)
            }
            MlsTest::Rejoin(peer) => {
                let request = self.ready()?;
                let conversation = conversation_id(own, peer)?;
                MlsTestReply::Allowed(self.rejoin(conversation, peer, request).await?)
            }
            MlsTest::HoldDelivery(hold) => {
                self.messages.mls.hold_delivery = hold;
                MlsTestReply::Done
            }
            MlsTest::Sent => MlsTestReply::Sent(self.messages.mls.sent.clone()),
            MlsTest::M4Peer(m4) => {
                self.messages.mls.m4_peer = m4;
                self.messages.mls.m4_close = false;
                MlsTestReply::Done
            }
            MlsTest::M4Close(m4) => {
                self.messages.mls.m4_peer = m4;
                self.messages.mls.m4_close = m4;
                MlsTestReply::Done
            }
            MlsTest::Frames => {
                MlsTestReply::Frames(self.messages.mls.frames.lock().unwrap().clone())
            }
            MlsTest::Held => MlsTestReply::Count(self.messages.mls.held),
            MlsTest::Rejoined => MlsTestReply::Count(self.messages.mls.rejoined),
            MlsTest::Query(peer) => {
                // Test only: this actor waits on the answers (the devices asked do not).
                let conversation = conversation_id(own, peer)?;
                let body = MlsFrame::GroupQuery { conversation }.encode().to_vec();
                let mut answers = Vec::new();
                for session in self.mls_sessions() {
                    if session.grant.peer != peer && session.grant.peer != own {
                        continue;
                    }
                    if let Ok(MlsReply::GroupInfo { exists, .. }) = exchange(&session, &body).await
                    {
                        answers.push((session.grant.installation, exists));
                    }
                }
                MlsTestReply::Answers(answers)
            }
        })
    }

    async fn group_db(
        &mut self,
        conversation: ConversationId,
        op: GroupOp,
        request: Request,
    ) -> Result<GroupReply, CoreError> {
        match self
            .message_db(MessageDbOp::MlsGroupOp(conversation, op), request)
            .await?
        {
            MessageDbReply::Group(reply) => Ok(reply),
            _ => Err(unavailable(ErrorCode::InvalidMessage)),
        }
    }

    /// Ready, currently authorized sessions to other installations, one per installation.
    fn mls_sessions(&self) -> Vec<Session> {
        let Ok(local) = self.local() else {
            return Vec::new();
        };
        let own = local.device.installation_id();
        let candidates: Vec<Session> = self
            .auth
            .registry
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.ready && s.grant.installation != own && s.conn.close_reason().is_none())
            .cloned()
            .collect();
        let mut seen = BTreeSet::new();
        candidates
            .into_iter()
            .filter(|s| self.grant_authorized(&s.grant) && seen.insert(s.grant.installation))
            .collect()
    }
    fn mls_session_for(&self, installation: InstallationId) -> Option<Session> {
        self.mls_sessions()
            .into_iter()
            .find(|s| s.grant.installation == installation)
    }

    fn launch(&mut self, session: &Session, body: Vec<u8>, task: Task, request: Request) {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            self.messages.mls.sent.push(label(&body));
            self.messages.mls.frames.lock().unwrap().push(body.clone());
        }
        let mailbox = self.auth.send.clone();
        let session = session.clone();
        let ticket = request.ticket;
        tokio::spawn(async move {
            let answer = Answer::Reply(exchange(&session, &body).await);
            let _ = mailbox
                .send(Input::Mls(MlsEvent {
                    ticket,
                    peer: session.grant.peer,
                    task,
                    answer,
                }))
                .await;
        });
    }

    /// Records how the Commits/Welcomes sent ahead of an application message ended (the
    /// message attempt carried them over its session, `manager/messages.rs`).
    pub(super) async fn settle_ahead(
        &mut self,
        deliveries: Vec<(DueDelivery, Delivery)>,
        request: Request,
    ) -> Result<(), CoreError> {
        for (delivery, outcome) in deliveries {
            let now = self.schedule_now();
            let op = GroupOp::Delivered {
                recipient: delivery.recipient,
                kind: delivery.kind,
                epoch: delivery.epoch,
                outcome,
                now,
            };
            self.group_db(delivery.conversation, op, request).await?;
        }
        Ok(())
    }

    /// The epoch of the conversation's group on this device, if it has one.
    pub(super) async fn group_epoch(
        &mut self,
        conversation: ConversationId,
        request: Request,
    ) -> Result<Option<u64>, CoreError> {
        match self.group_db(conversation, GroupOp::Info, request).await? {
            GroupReply::Info(info) => Ok(info.map(|info| info.epoch)),
            _ => Err(unavailable(ErrorCode::InvalidMessage)),
        }
    }

    /// Whether the account answered kind 17 properly this run, or ever spoke MLS here.
    pub(super) async fn peer_speaks_mls(
        &mut self,
        conversation: ConversationId,
        peer: PeerId,
        request: Request,
    ) -> Result<bool, CoreError> {
        if self.messages.mls.spoke.contains(&peer) {
            return Ok(true);
        }
        match self
            .group_db(conversation, GroupOp::PeerSpokeMls { mark: false }, request)
            .await?
        {
            GroupReply::Flag(spoke) => Ok(spoke),
            _ => Err(unavailable(ErrorCode::InvalidMessage)),
        }
    }

    /// The account refused kind 17: not asked again before the back-off (doubling from
    /// `OUTDATED_RETRY_MS`) ends. A second refusal within one back-off does not extend it.
    pub(super) fn mark_outdated(&mut self, peer: PeerId) {
        let now = self.schedule_now();
        let record = self.local().ok().and_then(|local| {
            local
                .remote
                .iter()
                .find(|r| r.peer == peer)
                .map(|r| r.accepted.hash)
        });
        let backoff = match self.messages.mls.outdated.get(&peer) {
            Some(o) if now < o.until => return,
            Some(o) => o.backoff.saturating_mul(2).min(OUTDATED_MAX_MS),
            None => OUTDATED_RETRY_MS,
        };
        self.messages.mls.outdated.insert(
            peer,
            Outdated {
                until: now.saturating_add(backoff),
                backoff,
                record,
            },
        );
    }

    /// Whether kind 17 requests to `peer` wait now: it refused within its back-off and its
    /// record did not change since.
    fn outdated_now(&self, peer: PeerId) -> bool {
        let Some(outdated) = self.messages.mls.outdated.get(&peer) else {
            return false;
        };
        let record = self.local().ok().and_then(|local| {
            local
                .remote
                .iter()
                .find(|r| r.peer == peer)
                .map(|r| r.accepted.hash)
        });
        self.schedule_now() < outdated.until && record == outdated.record
    }

    /// Queued Commits/Welcomes of the conversation for `recipient`, to send ahead of an
    /// application message (none while delivery is held in tests).
    pub(super) async fn deliveries_ahead(
        &mut self,
        conversation: ConversationId,
        recipient: InstallationId,
        request: Request,
    ) -> Result<Vec<DueDelivery>, CoreError> {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if self.messages.mls.hold_delivery {
            return Ok(Vec::new());
        }
        match self
            .group_db(conversation, GroupOp::Pending { recipient }, request)
            .await?
        {
            GroupReply::Due(rows) => Ok(rows),
            _ => Err(unavailable(ErrorCode::InvalidMessage)),
        }
    }

    /// The peer of a conversation a request may concern: the sending account itself when it is
    /// a contact, or, for this account's other devices, the contact the conversation is with.
    fn conversation_peer(&self, conversation: ConversationId, sender: PeerId) -> Option<PeerId> {
        let local = self.local().ok()?;
        let own = local.keys.peer_id();
        if sender == own {
            local
                .contacts
                .iter()
                .map(|c| c.peer)
                .find(|peer| conversation_id(own, *peer).ok() == Some(conversation))
        } else {
            (local.contacts.iter().any(|c| c.peer == sender)
                && conversation_id(own, sender).ok() == Some(conversation))
            .then_some(sender)
        }
    }

    /// Whether a Welcome for `conversation` from `sender` may be joined, and the peer it is
    /// with. Until Task 9 this is the contact rule of every other request: a Welcome for a
    /// conversation with someone who is not a local contact is answered `ContactRequired`.
    /// Task 9 extends this one function for Welcomes from this account's own devices (spec
    /// 4.4a).
    fn welcome_peer(&self, conversation: ConversationId, sender: PeerId) -> Option<PeerId> {
        self.conversation_peer(conversation, sender)
    }

    fn mls_trusted(&self, sender: PeerId) -> bool {
        self.local().is_ok_and(|local| {
            sender == local.keys.peer_id() || local.contacts.iter().any(|c| c.peer == sender)
        })
    }

    /// Makes sure the conversation has a group, or starts getting one (spec 4.4): ask first,
    /// join what exists, create only when nobody has one.
    pub(super) async fn ensure_group(
        &mut self,
        conversation: ConversationId,
        peer: PeerId,
    ) -> Result<GroupState, CoreError> {
        let request = self.ready()?;
        if let GroupReply::Info(Some(_)) =
            self.group_db(conversation, GroupOp::Info, request).await?
        {
            self.messages.mls.wanted.remove(&conversation);
            self.messages.mls.phases.remove(&conversation);
            return Ok(GroupState::Ready);
        }
        self.messages.mls.wanted.insert(conversation, peer);
        let now = self.schedule_now();
        match self.messages.mls.phases.get(&conversation) {
            Some(Phase::Joining { since }) if now < since.saturating_add(JOIN_WAIT_MS) => {
                return Ok(GroupState::Joining);
            }
            Some(Phase::Idle { until }) if now < *until => return Ok(GroupState::WaitingForPeer),
            Some(Phase::Querying { .. } | Phase::Collecting { .. }) => {
                return Ok(GroupState::WaitingForPeer);
            }
            _ => {}
        }
        let own = self.local()?.keys.peer_id();
        // An account that refused kind 17 is not asked again before its back-off ends.
        let sessions: Vec<Session> = self
            .mls_sessions()
            .into_iter()
            .filter(|s| {
                (s.grant.peer == peer || s.grant.peer == own) && !self.outdated_now(s.grant.peer)
            })
            .collect();
        if sessions.is_empty() {
            self.messages.mls.phases.insert(
                conversation,
                Phase::Idle {
                    until: now.saturating_add(RETRY_MS),
                },
            );
            return Ok(GroupState::WaitingForPeer);
        }
        let asked: Vec<InstallationId> = sessions.iter().map(|s| s.grant.installation).collect();
        self.messages.mls.phases.insert(
            conversation,
            Phase::Querying {
                peer,
                waiting: asked.iter().copied().collect(),
                found: None,
                asked,
            },
        );
        let body = MlsFrame::GroupQuery { conversation }.encode().to_vec();
        for session in &sessions {
            let task = Task::Query {
                conversation,
                installation: session.grant.installation,
            };
            self.launch(session, body.clone(), task, request);
        }
        Ok(GroupState::WaitingForPeer)
    }

    fn idle(&mut self, conversation: ConversationId) {
        let until = self.schedule_now().saturating_add(RETRY_MS);
        self.messages
            .mls
            .phases
            .insert(conversation, Phase::Idle { until });
    }

    async fn queried(
        &mut self,
        conversation: ConversationId,
        installation: InstallationId,
        result: Result<MlsReply, ErrorCode>,
        request: Request,
    ) -> Result<(), CoreError> {
        let mls = &mut self.messages.mls;
        let Some(Phase::Querying { waiting, found, .. }) = mls.phases.get_mut(&conversation) else {
            return Ok(());
        };
        waiting.remove(&installation);
        if let Ok(MlsReply::GroupInfo {
            exists: true,
            first_commit_hash,
            ..
        }) = result
        {
            mls.known
                .insert((conversation, installation), first_commit_hash);
            if found.is_none_or(|(_, best)| first_commit_hash < best) {
                *found = Some((installation, first_commit_hash));
            }
        }
        if !waiting.is_empty() {
            return Ok(());
        }
        let Some(Phase::Querying {
            peer, found, asked, ..
        }) = mls.phases.remove(&conversation)
        else {
            return Ok(());
        };
        if let Some((via, _)) = found {
            // Join the group that exists (the smaller first-Commit hash if two answered).
            let Some(session) = self.mls_session_for(via) else {
                self.idle(conversation);
                return Ok(());
            };
            let GroupReply::KeyPackage(key_package) = self
                .group_db(NO_CONVERSATION, GroupOp::NewKeyPackage, request)
                .await?
            else {
                self.idle(conversation);
                return Ok(());
            };
            let since = self.schedule_now();
            self.messages
                .mls
                .phases
                .insert(conversation, Phase::Joining { since });
            let body = MlsFrame::JoinRequest {
                conversation,
                key_package,
            }
            .encode()
            .to_vec();
            self.launch(&session, body, Task::Join { conversation }, request);
            return Ok(());
        }
        // Nobody has a group: collect KeyPackages from every device that answered and create it.
        let sessions: Vec<Session> = asked
            .iter()
            .filter_map(|installation| self.mls_session_for(*installation))
            .filter(|s| !self.outdated_now(s.grant.peer))
            .collect();
        if sessions.is_empty() {
            self.idle(conversation);
            return Ok(());
        }
        self.messages.mls.phases.insert(
            conversation,
            Phase::Collecting {
                peer,
                waiting: sessions.iter().map(|s| s.grant.installation).collect(),
                key_packages: Vec::new(),
            },
        );
        let body = MlsFrame::KeyPackageRequest.encode().to_vec();
        for session in &sessions {
            let task = Task::KeyPackage {
                conversation,
                installation: session.grant.installation,
                sync: false,
            };
            self.launch(session, body.clone(), task, request);
        }
        Ok(())
    }

    async fn collected(
        &mut self,
        conversation: ConversationId,
        installation: InstallationId,
        result: Result<MlsReply, ErrorCode>,
        request: Request,
    ) -> Result<(), CoreError> {
        let Some(Phase::Collecting {
            waiting,
            key_packages,
            ..
        }) = self.messages.mls.phases.get_mut(&conversation)
        else {
            return Ok(());
        };
        waiting.remove(&installation);
        if let Ok(MlsReply::KeyPackage(bytes)) = result {
            key_packages.push(bytes);
        }
        if !waiting.is_empty() {
            return Ok(());
        }
        let Some(Phase::Collecting {
            peer, key_packages, ..
        }) = self.messages.mls.phases.remove(&conversation)
        else {
            return Ok(());
        };
        if key_packages.is_empty() {
            self.idle(conversation);
            return Ok(());
        }
        let rejoin = self.messages.mls.rejoins.get(&conversation).copied();
        let now = self.schedule_now();
        let op = GroupOp::Create {
            peer,
            members: key_packages,
            rejoin,
            now,
        };
        match self.group_db(conversation, op, request).await {
            Ok(_) => self.after_group(conversation, peer, request).await,
            // A Welcome made the group in the meantime.
            Err(error) if error.code == ErrorCode::Busy => {}
            Err(_) => self.idle(conversation),
        }
        Ok(())
    }

    /// The conversation got a group (created or joined): stop asking, compare it with the
    /// device lists, and seal what waited for it (including what was sent unacknowledged in a
    /// group this device left).
    async fn after_group(&mut self, conversation: ConversationId, peer: PeerId, request: Request) {
        let mls = &mut self.messages.mls;
        mls.phases.remove(&conversation);
        mls.wanted.remove(&conversation);
        mls.rejoins.remove(&conversation);
        mls.sync.insert(conversation, (peer, 0));
        // Rows that backed off while waiting for the group become due at this time.
        let now = self.schedule_now();
        let _ = self
            .message_db(MessageDbOp::SealPending(conversation, now), request)
            .await;
    }

    /// Compares the group with both accounts' device lists and commits the difference (spec
    /// 4.5): devices it can reach now are added with fresh KeyPackages, devices no longer listed
    /// are removed. No Commit is made while the own previous one is unacknowledged.
    pub(super) async fn sync_members(
        &mut self,
        conversation: ConversationId,
        peer: PeerId,
        request: Request,
    ) {
        let plan = match self
            .group_db(conversation, GroupOp::Plan { peer }, request)
            .await
        {
            Ok(GroupReply::Plan(Some(plan))) => plan,
            _ => {
                self.messages.mls.sync.remove(&conversation);
                return;
            }
        };
        if plan.unacked {
            let at = self.schedule_now().saturating_add(SYNC_RETRY_MS);
            self.messages.mls.sync.insert(conversation, (peer, at));
            return;
        }
        self.messages.mls.sync.remove(&conversation);
        let sessions: Vec<Session> = plan
            .add
            .iter()
            .filter_map(|installation| self.mls_session_for(*installation))
            .collect();
        if sessions.is_empty() {
            if !plan.remove.is_empty() {
                self.commit_members(conversation, peer, Vec::new(), plan.remove, request)
                    .await;
            }
            return;
        }
        self.messages.mls.collecting.insert(
            conversation,
            Collect {
                peer,
                waiting: sessions.iter().map(|s| s.grant.installation).collect(),
                key_packages: Vec::new(),
                remove: plan.remove,
            },
        );
        let body = MlsFrame::KeyPackageRequest.encode().to_vec();
        for session in &sessions {
            let task = Task::KeyPackage {
                conversation,
                installation: session.grant.installation,
                sync: true,
            };
            self.launch(session, body.clone(), task, request);
        }
    }

    async fn sync_collected(
        &mut self,
        conversation: ConversationId,
        installation: InstallationId,
        result: Result<MlsReply, ErrorCode>,
        request: Request,
    ) {
        let Some(collect) = self.messages.mls.collecting.get_mut(&conversation) else {
            return;
        };
        collect.waiting.remove(&installation);
        if let Ok(MlsReply::KeyPackage(bytes)) = result {
            collect.key_packages.push(bytes);
        }
        if !collect.waiting.is_empty() {
            return;
        }
        let Some(Collect {
            peer,
            key_packages,
            remove,
            ..
        }) = self.messages.mls.collecting.remove(&conversation)
        else {
            return;
        };
        if !key_packages.is_empty() || !remove.is_empty() {
            self.commit_members(conversation, peer, key_packages, remove, request)
                .await;
        }
    }

    async fn commit_members(
        &mut self,
        conversation: ConversationId,
        peer: PeerId,
        add: Vec<Vec<u8>>,
        remove: Vec<InstallationId>,
        request: Request,
    ) {
        let now = self.schedule_now();
        let op = GroupOp::Commit {
            peer,
            add,
            remove,
            now,
        };
        match self.group_db(conversation, op, request).await {
            Ok(GroupReply::Committed(Committed::Busy)) => {
                let at = now.saturating_add(SYNC_RETRY_MS);
                self.messages.mls.sync.insert(conversation, (peer, at));
            }
            Ok(GroupReply::Committed(Committed::Held(lookup))) => self.note_hold(lookup),
            _ => {}
        }
    }

    /// Leaves the conversation's group to rejoin it (spec 4.6), when the rate limit allows
    /// (checked in the store with the up-to-date count, Ruling 15). The unacknowledged sent
    /// messages wait for the next group; `ensure_group` starts asking right away.
    pub(super) async fn rejoin(
        &mut self,
        conversation: ConversationId,
        peer: PeerId,
        request: Request,
    ) -> Result<bool, CoreError> {
        let now = self.services.clock.wall_ms();
        let GroupReply::Rejoin(Some(counters)) = self
            .group_db(conversation, GroupOp::Rejoin { now }, request)
            .await?
        else {
            return Ok(false);
        };
        let mls = &mut self.messages.mls;
        mls.rejoins.insert(conversation, counters);
        mls.phases.remove(&conversation);
        mls.collecting.remove(&conversation);
        mls.sync.remove(&conversation);
        mls.joined_with.remove(&conversation);
        mls.missing.remove(&conversation);
        mls.known.retain(|(id, _), _| *id != conversation);
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            mls.rejoined += 1;
        }
        self.ensure_group(conversation, peer).await?;
        Ok(true)
    }

    /// Something was held for `_lookup`'s record; the next poll looks it up unless a lookup of
    /// that account ran within `LOOKUP_RETRY_MS`.
    fn note_hold(&mut self, _lookup: PeerId) {
        let mls = &mut self.messages.mls;
        mls.no_holds_since = None;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            mls.held += 1;
        }
    }

    /// A received Commit was answered class 2 at `epoch`: a Commit is missing here. If that has
    /// gone on for `MISSING_REJOIN_MS` without this device moving on, it will not arrive (it was
    /// dropped as unknown, or settled elsewhere) and the device rejoins (spec 4.6), subject to
    /// the rejoin limit.
    async fn note_missing(
        &mut self,
        conversation: ConversationId,
        peer: PeerId,
        epoch: u64,
        request: Request,
    ) {
        let now = self.schedule_now();
        let since = match self.messages.mls.missing.get(&conversation) {
            Some((since, at)) if *at == epoch => *since,
            _ => {
                self.messages.mls.missing.insert(conversation, (now, epoch));
                now
            }
        };
        if now.saturating_sub(since) >= MISSING_REJOIN_MS {
            self.messages.mls.missing.remove(&conversation);
            let _ = self.rejoin(conversation, peer, request).await;
        }
    }

    async fn after_outcome(
        &mut self,
        conversation: ConversationId,
        peer: PeerId,
        outcome: Outcome,
        request: Request,
    ) {
        match outcome {
            Outcome::Joined => self.after_group(conversation, peer, request).await,
            // A lost fork: the changes the losing Commit carried may still be needed.
            Outcome::Adopted => {
                self.messages.mls.sync.insert(conversation, (peer, 0));
            }
            Outcome::Rejoin { .. } => {
                let _ = self.rejoin(conversation, peer, request).await;
            }
            Outcome::Held(_) => self.messages.mls.no_holds_since = None,
            Outcome::Ack | Outcome::Failed { .. } | Outcome::Removed | Outcome::Later => {}
        }
    }

    /// Starts a lookup of `peer`'s record in a spawned task (the actor never waits on the
    /// network); the answer comes back as `Task::Lookup`.
    fn start_lookup(&mut self, peer: PeerId, request: Request) {
        let Ok(backend) = self.services.backend() else {
            return;
        };
        if !self.messages.mls.looking.insert(peer) {
            return;
        }
        let mailbox = self.auth.send.clone();
        let ticket = request.ticket;
        tokio::spawn(async move {
            let lookup = tokio::time::timeout(LOOKUP_TIMEOUT, backend.lookup(peer))
                .await
                .unwrap_or_else(|_| Err(unavailable(ErrorCode::RecordUnavailable)));
            let _ = mailbox
                .send(Input::Mls(MlsEvent {
                    ticket,
                    peer,
                    task: Task::Lookup { peer },
                    answer: Answer::Lookup(lookup),
                }))
                .await;
        });
    }

    /// A lookup for holds came back. A fresh list (found and accepted unchanged or newer; for
    /// this account, found and equal to the accepted record) settles the holds: what is still
    /// unknown is dropped. A failed lookup, an absent record, a conflict, or a newer own record
    /// not accepted yet keeps them for the next try or record change.
    async fn looked_up(
        &mut self,
        peer: PeerId,
        lookup: Result<Lookup, CoreError>,
        request: Request,
    ) {
        let Ok(local) = self.local() else { return };
        let fresh = if peer == local.keys.peer_id() {
            match (&lookup, local.records.accepted.as_ref()) {
                (Ok(Lookup::Found(packet)), Some(accepted)) => {
                    crate::account::publication::SignedRecord::new(packet.clone(), peer)
                        .is_ok_and(|found| found.hash == accepted.hash)
                }
                _ => false,
            }
        } else {
            let found = matches!(lookup, Ok(Lookup::Found(_)));
            self.finish_remote_record(peer, lookup, request)
                .await
                .is_ok()
                && found
        };
        self.reprocess(peer, fresh, request).await;
    }

    /// Holds waiting on an account's record (spec 4.2): it is looked up again (at most every
    /// `LOOKUP_RETRY_MS`, in a spawned task) and the holds processed with the new list; what
    /// is still unknown after a successful lookup is dropped. While the record cannot be
    /// fetched or is in conflict the holds stay, and they are tried again whenever the records
    /// change.
    async fn poll_holds(&mut self, request: Request) {
        let now = self.schedule_now();
        if self
            .messages
            .mls
            .no_holds_since
            .is_some_and(|since| now < since.saturating_add(HOLD_CHECK_MS))
        {
            return;
        }
        let peers = match self
            .group_db(NO_CONVERSATION, GroupOp::HoldPeers, request)
            .await
        {
            Ok(GroupReply::Peers(peers)) => peers,
            _ => return,
        };
        let recheck = std::mem::take(&mut self.messages.mls.recheck);
        if peers.is_empty() {
            self.messages.mls.no_holds_since = Some(now);
            return;
        }
        for peer in peers {
            if self
                .messages
                .mls
                .lookups
                .get(&peer)
                .is_none_or(|at| now >= *at)
                && !self.messages.mls.looking.contains(&peer)
            {
                self.messages
                    .mls
                    .lookups
                    .insert(peer, now.saturating_add(LOOKUP_RETRY_MS));
                self.start_lookup(peer, request);
            }
            if recheck {
                self.reprocess(peer, false, request).await;
            }
        }
    }

    async fn reprocess(&mut self, lookup: PeerId, drop_unknown: bool, request: Request) {
        let now = self.schedule_now();
        let op = GroupOp::Reprocess {
            lookup,
            drop_unknown,
            now,
            wall: self.services.clock.wall_ms(),
        };
        let Ok(GroupReply::Reprocessed(settled)) =
            self.group_db(NO_CONVERSATION, op, request).await
        else {
            return;
        };
        for (conversation, peer, outcome) in settled {
            self.after_outcome(conversation, peer, outcome, request)
                .await;
        }
    }

    /// Sends due Commits/Welcomes over ready sessions, one per recipient of a conversation at a
    /// time and in epoch order. Only rows of recipients with a ready session are read, so a
    /// recipient without one simply waits (no attempt counted) and never holds others back.
    async fn poll_deliveries(&mut self, request: Request) {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if self.messages.mls.hold_delivery {
            return;
        }
        let sessions = self.mls_sessions();
        if sessions.is_empty() {
            return;
        }
        let now = self.schedule_now();
        let recipients = sessions.iter().map(|s| s.grant.installation).collect();
        let Ok(GroupReply::Due(rows)) = self
            .group_db(
                NO_CONVERSATION,
                GroupOp::DueDeliveries { now, recipients },
                request,
            )
            .await
        else {
            return;
        };
        for row in rows {
            if self
                .messages
                .mls
                .inflight
                .contains(&(row.conversation, row.recipient))
            {
                continue;
            }
            let Some(session) = sessions
                .iter()
                .find(|s| s.grant.installation == row.recipient)
            else {
                continue;
            };
            self.messages
                .mls
                .inflight
                .insert((row.conversation, row.recipient));
            let task = Task::Deliver {
                conversation: row.conversation,
                recipient: row.recipient,
                kind: row.kind,
                epoch: row.epoch,
            };
            self.launch(session, row.body, task, request);
        }
    }

    fn mls_fingerprint(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        if let Ok(local) = self.local() {
            if let Some(accepted) = &local.records.accepted {
                hash.update(accepted.hash);
            }
            for tombstone in &local.records.tombstones {
                hash.update(tombstone.target_installation.0);
            }
            for remote in &local.remote {
                hash.update(remote.peer.0);
                hash.update(remote.accepted.hash);
            }
        }
        let mut sessions: Vec<([u8; 32], [u8; 32])> = self
            .mls_sessions()
            .iter()
            .map(|s| (s.grant.peer.0, s.grant.installation.0))
            .collect();
        sessions.sort();
        for (peer, installation) in sessions {
            hash.update(peer);
            hash.update(installation);
        }
        hash.finalize().into()
    }

    /// One tick of group upkeep, run with the message scheduler's poll.
    pub(super) async fn poll_mls(&mut self) {
        let Ok(request) = self.ready() else { return };
        if self.local().is_err() {
            return;
        }
        // The M4-peer fixture keeps no group.
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if self.messages.mls.m4_peer {
            return;
        }
        // A changed device list or a new session may change who belongs in each group.
        let fingerprint = self.mls_fingerprint();
        if self.messages.mls.fingerprint != Some(fingerprint) {
            self.messages.mls.fingerprint = Some(fingerprint);
            self.messages.mls.recheck = true;
            if let Ok(GroupReply::Groups(groups)) = self
                .group_db(NO_CONVERSATION, GroupOp::Groups, request)
                .await
            {
                for (conversation, peer) in groups {
                    self.messages.mls.sync.insert(conversation, (peer, 0));
                }
            }
        }
        let wanted: Vec<(ConversationId, PeerId)> = self
            .messages
            .mls
            .wanted
            .iter()
            .map(|(conversation, peer)| (*conversation, *peer))
            .collect();
        for (conversation, peer) in wanted {
            if self.ensure_group(conversation, peer).await.is_err() {
                return;
            }
        }
        let now = self.schedule_now();
        let due: Vec<(ConversationId, PeerId)> = self
            .messages
            .mls
            .sync
            .iter()
            .filter(|(conversation, (_, at))| {
                *at <= now && !self.messages.mls.collecting.contains_key(conversation)
            })
            .map(|(conversation, (peer, _))| (*conversation, *peer))
            .collect();
        for (conversation, peer) in due {
            self.sync_members(conversation, peer, request).await;
        }
        self.poll_holds(request).await;
        self.poll_deliveries(request).await;
    }

    pub(super) async fn mls_event(&mut self, event: MlsEvent) {
        let MlsEvent {
            ticket,
            peer: answered,
            task,
            answer,
        } = event;
        // A device answering a kind 17 request with the M4 refusal has not updated yet; so has
        // one that just closed the stream or connection, if its account never showed MLS. Any
        // proper answer from the account means it has updated.
        let own = self.local().ok().map(|local| local.keys.peer_id());
        match (&answer, task_conversation(&task)) {
            (Answer::Reply(Err(ErrorCode::UnsupportedMessageVersion)), _)
                if Some(answered) != own =>
            {
                self.mark_outdated(answered);
            }
            (Answer::Reply(Err(ErrorCode::PeerNotConnected)), Some(conversation))
                if Some(answered) != own =>
            {
                if let Some(request) = self.ready().ok().filter(|r| r.ticket == ticket)
                    && let Ok(false) = self.peer_speaks_mls(conversation, answered, request).await
                {
                    self.mark_outdated(answered);
                }
            }
            (Answer::Reply(Ok(_)), _) => {
                self.messages.mls.spoke.insert(answered);
                self.messages.mls.outdated.remove(&answered);
            }
            _ => {}
        }
        match task {
            Task::Lookup { peer } => {
                self.messages.mls.looking.remove(&peer);
            }
            Task::Deliver {
                conversation,
                recipient,
                ..
            } => {
                self.messages
                    .mls
                    .inflight
                    .remove(&(conversation, recipient));
            }
            Task::Learn {
                conversation,
                installation,
            } => {
                self.messages
                    .mls
                    .learning
                    .remove(&(conversation, installation));
            }
            _ => {}
        }
        let Some(request) = self.ready().ok().filter(|r| r.ticket == ticket) else {
            return;
        };
        match (task, answer) {
            (Task::Lookup { peer }, Answer::Lookup(lookup)) => {
                self.looked_up(peer, lookup, request).await
            }
            (task, Answer::Reply(result)) => {
                let _ = self.mls_answer(task, result, request).await;
            }
            (_, Answer::Lookup(_)) => {}
        }
    }

    async fn mls_answer(
        &mut self,
        task: Task,
        result: Result<MlsReply, ErrorCode>,
        request: Request,
    ) -> Result<(), CoreError> {
        match task {
            Task::Query {
                conversation,
                installation,
            } => {
                self.queried(conversation, installation, result, request)
                    .await
            }
            Task::KeyPackage {
                conversation,
                installation,
                sync: false,
            } => {
                self.collected(conversation, installation, result, request)
                    .await
            }
            Task::KeyPackage {
                conversation,
                installation,
                sync: true,
            } => {
                self.sync_collected(conversation, installation, result, request)
                    .await;
                Ok(())
            }
            Task::Join { conversation } => {
                if !matches!(result, Ok(MlsReply::Accepted))
                    && matches!(
                        self.messages.mls.phases.get(&conversation),
                        Some(Phase::Joining { .. })
                    )
                {
                    self.idle(conversation);
                }
                Ok(())
            }
            Task::Learn {
                conversation,
                installation,
            } => {
                if let Ok(MlsReply::GroupInfo {
                    exists: true,
                    first_commit_hash,
                    ..
                }) = result
                {
                    self.messages
                        .mls
                        .known
                        .insert((conversation, installation), first_commit_hash);
                }
                Ok(())
            }
            Task::Deliver {
                conversation,
                recipient,
                kind,
                epoch,
            } => {
                let outcome = delivery_outcome(&result);
                let now = self.schedule_now();
                let op = GroupOp::Delivered {
                    recipient,
                    kind,
                    epoch,
                    outcome,
                    now,
                };
                self.group_db(conversation, op, request).await.map(|_| ())
            }
            Task::Lookup { .. } => Ok(()),
        }
    }

    /// A kind 17 request from an authenticated session. Only authority failures close the
    /// stream; anything else is answered (`Busy` when this device cannot act on it now).
    pub(super) async fn receive_mls(
        &mut self,
        conn: &iroh::endpoint::Connection,
        inner: &[u8],
        request: Request,
    ) -> Result<Vec<u8>, CoreError> {
        let session = self.message_session(conn, request)?;
        // The M4-peer fixture answers like a released M4 app.
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if self.messages.mls.m4_peer {
            // A released M4 app takes kind 17 for no account frame it knows: the error ends
            // the stream reader, which closes the connection.
            if self.messages.mls.m4_close {
                return Err(unavailable(ErrorCode::InvalidMessage));
            }
            return Ok(Frame::Reject(RejectReason::UnsupportedVersion)
                .envelope()
                .to_vec());
        }
        let reply = match MlsFrame::decode(inner) {
            Ok(MlsFrame::App {
                conversation,
                ciphertext,
            }) => match self
                .receive_app(&session, conversation, ciphertext, request)
                .await
            {
                Ok(Some(reply)) => reply,
                // A text M4 would refuse: the M4 refusal, which the sender takes as final.
                Ok(None) => {
                    self.message_session(conn, request)?;
                    return Ok(Frame::Reject(RejectReason::InvalidMessage)
                        .envelope()
                        .to_vec());
                }
                Err(error)
                    if matches!(
                        error.code,
                        ErrorCode::AccountRestricted | ErrorCode::StorageLocked
                    ) =>
                {
                    return Err(error);
                }
                Err(_) => MlsReply::Busy,
            },
            Ok(frame) => match self.mls_request(&session, frame, request).await {
                Ok(reply) => reply,
                Err(error)
                    if matches!(
                        error.code,
                        ErrorCode::AccountRestricted | ErrorCode::StorageLocked
                    ) =>
                {
                    return Err(error);
                }
                Err(_) => MlsReply::Busy,
            },
            Err(_) => MlsReply::DecryptFailed {
                class: CLASS_OTHER,
                epoch: 0,
            },
        };
        self.message_session(conn, request)?;
        Ok(reply.encode().to_vec())
    }

    async fn mls_request(
        &mut self,
        session: &Session,
        frame: MlsFrame,
        request: Request,
    ) -> Result<MlsReply, CoreError> {
        let sender = session.grant.peer;
        let now = self.schedule_now();
        Ok(match frame {
            MlsFrame::GroupQuery { conversation } => {
                if self.conversation_peer(conversation, sender).is_none() {
                    return Ok(MlsReply::ContactRequired);
                }
                match self.group_db(conversation, GroupOp::Info, request).await? {
                    GroupReply::Info(Some(info)) => MlsReply::GroupInfo {
                        exists: true,
                        epoch: info.epoch,
                        first_commit_hash: info.first_commit_hash,
                    },
                    _ => MlsReply::GroupInfo {
                        exists: false,
                        epoch: 0,
                        first_commit_hash: [0; 32],
                    },
                }
            }
            MlsFrame::KeyPackageRequest => {
                if !self.mls_trusted(sender) {
                    return Ok(MlsReply::ContactRequired);
                }
                match self
                    .group_db(NO_CONVERSATION, GroupOp::NewKeyPackage, request)
                    .await?
                {
                    GroupReply::KeyPackage(bytes) => MlsReply::KeyPackage(bytes),
                    _ => MlsReply::Busy,
                }
            }
            MlsFrame::JoinRequest {
                conversation,
                key_package,
            } => {
                let Some(peer) = self.conversation_peer(conversation, sender) else {
                    return Ok(MlsReply::ContactRequired);
                };
                // Only the asking device itself is added, and only with its own KeyPackage;
                // its leaf is then checked against its account's current record.
                let asker = inspect_key_package(&key_package)
                    .ok()
                    .and_then(|info| MlsCredential::decode(&info.credential).ok());
                if !asker.is_some_and(|credential| {
                    credential.peer == sender
                        && credential.installation == session.grant.installation
                }) {
                    return Ok(MlsReply::Busy);
                }
                // A device that rejoins still has its old leaf: it is replaced in one Commit. A
                // device missing from its account's record is held and looked up (spec 4.2).
                let op = GroupOp::JoinRequest {
                    peer,
                    sender_peer: sender,
                    key_package,
                    now,
                };
                match self.group_db(conversation, op, request).await? {
                    GroupReply::Committed(Committed::Done { .. }) => MlsReply::Accepted,
                    GroupReply::Committed(Committed::Held(lookup)) => {
                        self.note_hold(lookup);
                        MlsReply::Busy
                    }
                    _ => MlsReply::Busy,
                }
            }
            MlsFrame::Commit {
                conversation,
                epoch,
                commit,
            } => {
                let Some(peer) = self.conversation_peer(conversation, sender) else {
                    return Ok(MlsReply::DecryptFailed {
                        class: CLASS_OTHER,
                        epoch: 0,
                    });
                };
                let ct_hash = sha256(&commit);
                let op = GroupOp::ApplyCommit {
                    peer,
                    sender_peer: sender,
                    epoch,
                    commit,
                    now,
                };
                let GroupReply::Outcome(outcome) = self.group_db(conversation, op, request).await?
                else {
                    return Ok(MlsReply::Busy);
                };
                match outcome {
                    Outcome::Held(lookup) => self.note_hold(lookup),
                    Outcome::Failed {
                        class: CLASS_FUTURE_EPOCH,
                        epoch,
                    } => self.note_missing(conversation, peer, epoch, request).await,
                    Outcome::Failed { .. } => {}
                    // This device moved on: nothing is missing any more.
                    _ => {
                        self.messages.mls.missing.remove(&conversation);
                    }
                }
                if sender == peer && matches!(outcome, Outcome::Ack | Outcome::Adopted) {
                    self.peer_spoke(conversation, sender, request).await?;
                }
                let reply = match outcome {
                    Outcome::Failed { class, epoch } => MlsReply::DecryptFailed { class, epoch },
                    Outcome::Rejoin { epoch } => MlsReply::DecryptFailed {
                        class: CLASS_OTHER,
                        epoch,
                    },
                    Outcome::Later => MlsReply::Busy,
                    // Applied, already applied, settled as a fork in its favour, or held (kept
                    // durably in `mls_hold`).
                    _ => MlsReply::Ack { ct_hash },
                };
                self.after_outcome(conversation, peer, outcome, request)
                    .await;
                reply
            }
            MlsFrame::Welcome {
                conversation,
                welcome,
                sender_install,
            } => {
                let Some(peer) = self.welcome_peer(conversation, sender) else {
                    return Ok(MlsReply::ContactRequired);
                };
                if sender_install != session.grant.installation {
                    return Ok(MlsReply::Busy);
                }
                let ct_hash = sha256(&welcome);
                let Some(first_commit_hash) = self
                    .messages
                    .mls
                    .known
                    .remove(&(conversation, sender_install))
                else {
                    // Learn the group's first-Commit hash from the sender; its next attempt
                    // is joined.
                    if self
                        .messages
                        .mls
                        .learning
                        .insert((conversation, sender_install))
                    {
                        let body = MlsFrame::GroupQuery { conversation }.encode().to_vec();
                        let task = Task::Learn {
                            conversation,
                            installation: sender_install,
                        };
                        self.launch(session, body, task, request);
                    }
                    return Ok(MlsReply::Busy);
                };
                let op = GroupOp::Join {
                    peer,
                    sender_peer: sender,
                    sender_install,
                    welcome,
                    first_commit_hash,
                    rejoin: self.messages.mls.rejoins.get(&conversation).copied(),
                    joined_with: self.messages.mls.joined_with.get(&conversation).copied(),
                    now,
                    wall: self.services.clock.wall_ms(),
                };
                let GroupReply::Outcome(outcome) = self.group_db(conversation, op, request).await?
                else {
                    return Ok(MlsReply::Busy);
                };
                match outcome {
                    Outcome::Held(lookup) => self.note_hold(lookup),
                    Outcome::Joined => {
                        self.messages.mls.joined_with.insert(conversation, ct_hash);
                        self.messages.mls.missing.remove(&conversation);
                    }
                    _ => {}
                }
                if sender == peer && matches!(outcome, Outcome::Ack | Outcome::Joined) {
                    self.peer_spoke(conversation, sender, request).await?;
                }
                self.after_outcome(conversation, peer, outcome, request)
                    .await;
                match outcome {
                    // Switching groups is not allowed yet (rejoin limit): send it again later.
                    Outcome::Later => MlsReply::Busy,
                    // Settled here, joined or not: the sender stops resending it.
                    _ => MlsReply::Ack { ct_hash },
                }
            }
            // Answered by `receive_app`.
            MlsFrame::App { .. } => MlsReply::Busy,
        })
    }

    /// The conversation's peer account just spoke MLS (an accepted Commit or Welcome): kept for
    /// the downgrade guard (`mls_peer`, never cleared) and as evidence it is no M4 app.
    async fn peer_spoke(
        &mut self,
        conversation: ConversationId,
        peer: PeerId,
        request: Request,
    ) -> Result<(), CoreError> {
        self.messages.mls.spoke.insert(peer);
        self.messages.mls.outdated.remove(&peer);
        self.group_db(conversation, GroupOp::PeerSpokeMls { mark: true }, request)
            .await
            .map(|_| ())
    }

    /// A received application message (spec 5.2): opened and stored in one transaction. `None`
    /// when it carried a text M4 would refuse (nothing stored; the ratchet still moved).
    async fn receive_app(
        &mut self,
        session: &Session,
        conversation: ConversationId,
        ciphertext: Vec<u8>,
        request: Request,
    ) -> Result<Option<MlsReply>, CoreError> {
        let sender = session.grant.peer;
        // Application messages come from the conversation's peer account (this account's
        // other devices send none in 1:1, spec 4.4a).
        if sender == self.local()?.keys.peer_id()
            || self.conversation_peer(conversation, sender) != Some(sender)
        {
            return Ok(Some(MlsReply::ContactRequired));
        }
        let op = MessageDbOp::OpenApp(
            sender,
            session.grant.installation,
            conversation,
            ciphertext,
            self.services.clock.wall_ms(),
        );
        let result = self.message_db(op, request).await;
        if result.is_ok() {
            self.messages.mls.spoke.insert(sender);
            self.messages.mls.outdated.remove(&sender);
        }
        match result {
            Ok(MessageDbReply::Opened(reply)) => Ok(Some(reply)),
            Ok(MessageDbReply::OpenedText(reply, _key)) => {
                // The keyed response checkpoint of tests: the Ack that stores this text.
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                {
                    self.messages.ack_key = Some(_key);
                }
                Ok(Some(reply))
            }
            Ok(MessageDbReply::Rejected) => Ok(None),
            Ok(_) => Err(unavailable(ErrorCode::InvalidMessage)),
            // The MLS sender is not the authenticated device, or the message conflicts.
            Err(error)
                if matches!(
                    error.code,
                    ErrorCode::InvalidMessage | ErrorCode::MessageConflict
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}
