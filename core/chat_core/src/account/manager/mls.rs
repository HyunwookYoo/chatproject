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
use crate::message::mls_group::{
    Committed, Delivery, DueDelivery, GroupOp, GroupReply, Outcome, OwnWelcome,
};
use crate::message::store::{MessageDbOp, MessageDbReply, OpenFrom, conversation_id};
use crate::message::types::{BlockReason, ConversationId};
use crate::message::wire::{Frame, RejectReason};
use crate::mls::{
    group::{CLASS_FUTURE_EPOCH, CLASS_OTHER, CLASS_OUT_OF_ORDER, inspect_key_package},
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
/// A recipient of a queued Commit/Welcome without a session is dialed at most this often.
const DIAL_RETRY_MS: u64 = 30_000;
/// Limit for one such dial and its handshake.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// The queued deliveries are looked through for recipients to dial at most this often.
const DIAL_CHECK_MS: u64 = 1_000;
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
/// A KeyPackageRequest from one installation is answered at most once per this long (`Busy`
/// otherwise), and at most `KEY_PACKAGES_PER_ASKER` KeyPackages handed to it are kept; minting
/// another deletes its oldest. The store keeps at most `KEY_PACKAGES_TOTAL` in all.
const KEY_PACKAGE_GAP_MS: u64 = 1_000;
const KEY_PACKAGES_PER_ASKER: usize = 4;
/// A device moves a conversation's group to a new epoch on its own (an empty Commit, C1) at
/// most this many times per `RECOVERY_WINDOW_MS`, so a sender cannot make it spin Commits.
const RECOVERY_LIMIT: usize = 4;
const RECOVERY_WINDOW_MS: u64 = 3_600_000;
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
        /// The peer account's installations asked; whether one of them sent a KeyPackage, and
        /// whether one answered `ContactRequired` (it has not added this account).
        peer_devices: BTreeSet<InstallationId>,
        peer_answered: bool,
        peer_refused: bool,
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
    /// When each recipient of a queued delivery without a session was last dialed (or found not
    /// to be dialed), within `DIAL_RETRY_MS`.
    dials: BTreeMap<InstallationId, u64>,
    /// Scheduler time from which `dial_unreachable` may read the queue again.
    dial_check_at: u64,
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
    pub(super) outdated: BTreeMap<InstallationId, Outdated>,
    /// Installations of other accounts that answered or sent a kind 17 request properly (this
    /// run); with `mls_peer` the evidence that an installation speaks MLS, so a stream it closed
    /// is not taken for a refusal. Kept per installation: one account can run an M4 app on one
    /// device and M5 on another.
    pub(super) spoke: BTreeSet<InstallationId>,
    /// When this device last moved each conversation's group to a new epoch on its own (C1),
    /// within `RECOVERY_WINDOW_MS`.
    recoveries: BTreeMap<ConversationId, Vec<u64>>,
    /// The KeyPackage sent with each conversation's last `JoinRequest`: deleted when the device
    /// asks again (it is not going to be used), so a join that keeps failing keeps one.
    join_key_packages: BTreeMap<ConversationId, Vec<u8>>,
    /// KeyPackages handed to each asking installation (oldest first) and when it last got one.
    issued: BTreeMap<InstallationId, (std::collections::VecDeque<Vec<u8>>, u64)>,
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
    /// The recipient and label of every kind 17 request frame this actor sent.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    sent: Vec<(InstallationId, String)>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    held: u64,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    rejoined: u64,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    recovered: u64,
    /// While true, the dials `start_dial` spawns wait before they connect.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    dial_hold: tokio::sync::watch::Sender<bool>,
    /// How many connections this actor finished with: results of the dials `start_dial`
    /// spawned, and accepted ones.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) settled: u64,
}

/// Back-off of an installation that refused kind 17.
#[derive(Clone, Copy)]
pub(crate) struct Outdated {
    /// Its account.
    peer: PeerId,
    until: u64,
    backoff: u64,
    /// Its account's accepted record when it refused; a newer one may come with an updated app.
    record: Option<[u8; 32]>,
    /// A newer record this account was already asked once about within the back-off: a record
    /// change buys one request, never repeated ones.
    asked: Option<[u8; 32]>,
}

/// Who a received Welcome is from (`welcome_peer`).
enum WelcomeFrom {
    /// The conversation's peer, a local contact (or this account's device, for that contact).
    Contact(PeerId),
    /// A device of this account, for a conversation with someone who is not a contact.
    OwnDevice,
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
    /// A dial of a delivery recipient that had no session.
    Dial,
}

enum Answer {
    Reply(Result<MlsReply, ErrorCode>),
    Lookup(Result<Lookup, CoreError>),
    /// A dial run outside the actor (`dial_unreachable`): the connection and its proof.
    Dial(Result<(iroh::endpoint::Connection, auth::AuthGrant), CoreError>),
}

/// Who made a received kind 17 request: the account and installation that sent it (its MLS
/// sender), which for an entry of another own device's log (M6) is not the session's device.
/// An `App` it carries may have been sealed by any current device of that account (M6 spec 7).
#[derive(Clone, Copy)]
pub(super) struct Origin {
    pub peer: PeerId,
    pub installation: InstallationId,
}

/// How a received kind 17 request reached this device.
pub(super) enum Via {
    /// Over an authenticated session with the device that sent it; it waits for the answer.
    Direct(Session),
    /// Entry `seq` of own device `holder`'s log (M6 sync): nobody waits for an answer, and
    /// nothing is sent to the original sender.
    Mailbox {
        holder: InstallationId,
        // Names the entry for the plaintext hand-over (Task 7).
        #[allow(dead_code)]
        seq: u64,
    },
}

/// The answer to one kind 17 request, or one record lookup, this actor started.
pub(crate) struct MlsEvent {
    ticket: u64,
    /// The account of the device that answered (the looked-up account for a lookup).
    peer: PeerId,
    /// The device that answered (none for a lookup).
    installation: Option<InstallationId>,
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

/// Whether a conversation may get another self-made epoch (C1) at `now`: fewer than
/// `RECOVERY_LIMIT` in the last `RECOVERY_WINDOW_MS`. `recent` keeps only those.
fn recovery_allowed(recent: &mut Vec<u64>, now: u64) -> bool {
    recent.retain(|at| now.saturating_sub(*at) < RECOVERY_WINDOW_MS);
    recent.len() < RECOVERY_LIMIT
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
    SentTo(InstallationId),
    CloseSession(InstallationId),
    Held,
    Rejoined,
    Recovered,
    Query(PeerId),
    AskKeyPackage(PeerId),
    M4Peer(bool),
    M4Close(bool),
    Frames,
    Exchange(InstallationId, MlsFrame),
    HoldDials(bool),
    Dial(PeerId, InstallationId),
    Sessions,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(crate) enum MlsTestReply {
    State(GroupState),
    Reply(MlsReply),
    Allowed(bool),
    Done,
    Sent(Vec<String>),
    Count(u64),
    Answers(Vec<(InstallationId, bool)>),
    Frames(Vec<Vec<u8>>),
    Sessions(u64, Vec<(InstallationId, bool)>),
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
    /// Every kind 17 frame this actor sent to `installation`, as `op` or `op:content type`.
    pub async fn mls_test_sent_frames_to(
        &self,
        installation: InstallationId,
    ) -> Result<Vec<String>, CoreError> {
        match self.mls_test(MlsTest::SentTo(installation)).await? {
            MlsTestReply::Sent(sent) => Ok(sent),
            _ => unreachable!(),
        }
    }
    /// Closes this device's sessions with `installation` (both ends see the connection close).
    pub async fn mls_test_close_session(
        &self,
        installation: InstallationId,
    ) -> Result<(), CoreError> {
        self.mls_test(MlsTest::CloseSession(installation))
            .await
            .map(|_| ())
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
    /// Sends a `KeyPackageRequest` to every connected device of `peer` and waits for each (5 s
    /// limit each): installation, got a KeyPackage.
    pub async fn mls_test_ask_key_package(
        &self,
        peer: PeerId,
    ) -> Result<Vec<(InstallationId, bool)>, CoreError> {
        match self.mls_test(MlsTest::AskKeyPackage(peer)).await? {
            MlsTestReply::Answers(answers) => Ok(answers),
            _ => unreachable!(),
        }
    }
    /// How many times this actor moved a group to a new epoch on its own (C1).
    pub async fn mls_test_recovery_total(&self) -> Result<u64, CoreError> {
        match self.mls_test(MlsTest::Recovered).await? {
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
    /// Sends `frame` as is to `installation` (any account) over the ready session with it and
    /// returns the answer; a kind 16 refusal is its error code.
    pub async fn mls_test_exchange(
        &self,
        installation: InstallationId,
        frame: MlsFrame,
    ) -> Result<MlsReply, CoreError> {
        match self
            .mls_test(MlsTest::Exchange(installation, frame))
            .await?
        {
            MlsTestReply::Reply(reply) => Ok(reply),
            _ => unreachable!(),
        }
    }
    /// Holds the dials this core spawns (`start_dial`) before they connect, or lets them go.
    pub async fn mls_test_hold_dials(&self, hold: bool) -> Result<(), CoreError> {
        self.mls_test(MlsTest::HoldDials(hold)).await.map(|_| ())
    }
    /// Spawns a dial of `peer`'s device `installation` (`start_dial`); false when there is no
    /// record to dial it under.
    pub async fn mls_test_dial(
        &self,
        peer: PeerId,
        installation: InstallationId,
    ) -> Result<bool, CoreError> {
        match self.mls_test(MlsTest::Dial(peer, installation)).await? {
            MlsTestReply::Allowed(started) => Ok(started),
            _ => unreachable!(),
        }
    }
    /// How many connections this core finished with so far (results of the dials it spawned,
    /// and connections it accepted), and its live ready sessions: with which installation, and
    /// whether this device dialed it.
    pub async fn mls_test_sessions(&self) -> Result<(u64, Vec<(InstallationId, bool)>), CoreError> {
        match self.mls_test(MlsTest::Sessions).await? {
            MlsTestReply::Sessions(settled, sessions) => Ok((settled, sessions)),
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
            MlsTest::Sent => MlsTestReply::Sent(
                self.messages
                    .mls
                    .sent
                    .iter()
                    .map(|(_, label)| label.clone())
                    .collect(),
            ),
            MlsTest::SentTo(installation) => MlsTestReply::Sent(
                self.messages
                    .mls
                    .sent
                    .iter()
                    .filter(|(to, _)| *to == installation)
                    .map(|(_, label)| label.clone())
                    .collect(),
            ),
            MlsTest::CloseSession(installation) => {
                for session in self.auth.registry.0.lock().unwrap().iter() {
                    if session.grant.installation == installation {
                        session.conn.close(0u32.into(), b"test close");
                    }
                }
                MlsTestReply::Done
            }
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
            MlsTest::Exchange(installation, frame) => {
                // Test only: this actor waits on the answer.
                let session = self
                    .mls_sessions()
                    .into_iter()
                    .find(|s| s.grant.installation == installation)
                    .ok_or_else(|| unavailable(ErrorCode::PeerNotConnected))?;
                let reply = exchange(&session, &frame.encode())
                    .await
                    .map_err(unavailable)?;
                MlsTestReply::Reply(reply)
            }
            MlsTest::Held => MlsTestReply::Count(self.messages.mls.held),
            MlsTest::Rejoined => MlsTestReply::Count(self.messages.mls.rejoined),
            MlsTest::Recovered => MlsTestReply::Count(self.messages.mls.recovered),
            MlsTest::HoldDials(hold) => {
                self.messages.mls.dial_hold.send_replace(hold);
                MlsTestReply::Done
            }
            MlsTest::Dial(peer, installation) => {
                let request = self.ready()?;
                MlsTestReply::Allowed(self.start_dial(peer, installation, request))
            }
            MlsTest::Sessions => MlsTestReply::Sessions(
                self.messages.mls.settled,
                self.auth
                    .registry
                    .0
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|s| s.ready && s.conn.close_reason().is_none())
                    .map(|s| (s.grant.installation, s.conn.side().is_client()))
                    .collect(),
            ),
            MlsTest::AskKeyPackage(peer) => {
                // Test only: this actor waits on the answers.
                let body = MlsFrame::KeyPackageRequest.encode().to_vec();
                let mut answers = Vec::new();
                for session in self.mls_sessions() {
                    if session.grant.peer == peer {
                        let got =
                            matches!(exchange(&session, &body).await, Ok(MlsReply::KeyPackage(_)));
                        answers.push((session.grant.installation, got));
                    }
                }
                MlsTestReply::Answers(answers)
            }
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
    pub(super) fn mls_sessions(&self) -> Vec<Session> {
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
            self.messages
                .mls
                .sent
                .push((session.grant.installation, label(&body)));
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
                    installation: Some(session.grant.installation),
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

    /// The installations holding a leaf in the conversation's group on this device.
    pub(super) async fn group_members(
        &mut self,
        conversation: ConversationId,
        request: Request,
    ) -> Result<Vec<InstallationId>, CoreError> {
        match self
            .group_db(conversation, GroupOp::Members, request)
            .await?
        {
            GroupReply::Members(members) => Ok(members),
            _ => Err(unavailable(ErrorCode::InvalidMessage)),
        }
    }

    /// Whether installation `installation` of account `peer` shows it speaks MLS, so a stream
    /// it closed is no M4 refusal: it answered or sent a kind 17 request this run (`spoke`), it
    /// ever spoke MLS here (`mls_peer`), or it holds a leaf in our group with the account.
    /// Looked up in the account's 1:1 conversation with this one, whatever request was refused.
    /// Only for this decision: the downgrade guard reads `mls_peer` alone.
    pub(super) async fn peer_speaks_mls(
        &mut self,
        peer: PeerId,
        installation: InstallationId,
        request: Request,
    ) -> Result<bool, CoreError> {
        if self.messages.mls.spoke.contains(&installation) {
            return Ok(true);
        }
        let conversation = conversation_id(self.local()?.keys.peer_id(), peer)?;
        match self
            .group_db(
                conversation,
                GroupOp::PeerSpokeMls {
                    mark: false,
                    installation,
                },
                request,
            )
            .await?
        {
            GroupReply::Flag(spoke) => Ok(spoke),
            _ => Err(unavailable(ErrorCode::InvalidMessage)),
        }
    }

    /// The account's accepted record hash on this device.
    fn record_of(&self, peer: PeerId) -> Option<[u8; 32]> {
        self.local().ok().and_then(|local| {
            local
                .remote
                .iter()
                .find(|r| r.peer == peer)
                .map(|r| r.accepted.hash)
        })
    }

    /// The installation refused kind 17: not asked again before the back-off (doubling from
    /// `OUTDATED_RETRY_MS`) ends. A second refusal under the same record within one back-off
    /// does not extend it; a refusal under a newer record (the one re-query a record change
    /// allows) keeps that record and starts a new, doubled back-off. The account's other
    /// installations are not affected.
    pub(super) fn mark_outdated(&mut self, peer: PeerId, installation: InstallationId) {
        let now = self.schedule_now();
        let record = self.record_of(peer);
        let backoff = match self.messages.mls.outdated.get(&installation) {
            Some(o) if now < o.until && o.record == record => return,
            Some(o) => o.backoff.saturating_mul(2).min(OUTDATED_MAX_MS),
            None => OUTDATED_RETRY_MS,
        };
        self.messages.mls.outdated.insert(
            installation,
            Outdated {
                peer,
                until: now.saturating_add(backoff),
                backoff,
                record,
                asked: None,
            },
        );
    }

    /// Whether kind 17 requests to `installation` wait now: it refused within its back-off,
    /// and its account's record is the one it refused under, or a newer one it was already
    /// asked about once.
    pub(super) fn outdated_now(&self, installation: InstallationId) -> bool {
        let Some(outdated) = self.messages.mls.outdated.get(&installation) else {
            return false;
        };
        let record = self.record_of(outdated.peer);
        self.schedule_now() < outdated.until
            && (record == outdated.record || (record.is_some() && record == outdated.asked))
    }

    /// Whether the account's messages wait for its update: an installation of it refused kind
    /// 17, and no ready session reaches one that has not.
    pub(super) fn account_outdated(&self, peer: PeerId) -> bool {
        self.messages.mls.outdated.values().any(|o| o.peer == peer)
            && !self
                .mls_sessions()
                .iter()
                .any(|s| s.grant.peer == peer && !self.outdated_now(s.grant.installation))
    }

    /// A kind 17 request goes to an installation that refused before (its back-off ended, or
    /// its record changed): that record's one request is spent.
    fn note_requery(&mut self, installation: InstallationId) {
        let Some(peer) = self
            .messages
            .mls
            .outdated
            .get(&installation)
            .map(|o| o.peer)
        else {
            return;
        };
        let record = self.record_of(peer);
        if let Some(outdated) = self.messages.mls.outdated.get_mut(&installation) {
            outdated.asked = record;
        }
    }

    /// The installation answered or sent kind 17 properly: it runs M5.
    pub(super) fn speaks_mls(&mut self, installation: InstallationId) {
        self.messages.mls.spoke.insert(installation);
        self.messages.mls.outdated.remove(&installation);
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
    pub(super) fn conversation_peer(&self, conversation: ConversationId, sender: PeerId) -> Option<PeerId> {
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

    /// Whether a Welcome for `conversation` from `sender` may be joined, and who it is from: a
    /// local contact (the contact rule of every other request), or, spec 4.4a, one of this
    /// account's own devices, whose Welcome may also bring in a conversation with someone who is
    /// not a contact yet (`own_welcome_peer` decides that). A Welcome from the other account's
    /// devices without a contact is answered `ContactRequired`.
    fn welcome_peer(&self, conversation: ConversationId, sender: PeerId) -> Option<WelcomeFrom> {
        if let Some(peer) = self.conversation_peer(conversation, sender) {
            return Some(WelcomeFrom::Contact(peer));
        }
        let own = self.local().ok()?.keys.peer_id();
        (sender == own).then_some(WelcomeFrom::OwnDevice)
    }

    /// Spec 4.4a: a Welcome from this account's own device for a conversation with an account
    /// that is not a contact. Vetted first (nothing kept; the sending device is a leaf checked
    /// against this account's record, every peer leaf against the peer's record), then the
    /// contact is added (empty alias, no safety verification, no known device). The join comes
    /// after this: a crash in between leaves a contact without a group, which the sender's
    /// resent Welcome (unacknowledged, KeyPackage unused) joins through the contact rule; the
    /// other order could leave a group whose peer's messages this device refuses. `Err(reply)`
    /// is what the sender gets instead.
    async fn own_welcome_peer(
        &mut self,
        conversation: ConversationId,
        welcome: &[u8],
        sender_install: InstallationId,
        request: Request,
    ) -> Result<Result<PeerId, MlsReply>, CoreError> {
        let op = GroupOp::OwnWelcome {
            sender_install,
            welcome: welcome.to_vec(),
        };
        let GroupReply::OwnWelcome(vetted) = self.group_db(conversation, op, request).await? else {
            return Ok(Err(MlsReply::Busy));
        };
        let peer = match vetted {
            OwnWelcome::Refused => return Ok(Err(MlsReply::ContactRequired)),
            OwnWelcome::Held(lookup) => {
                // The record is looked up in a spawned task; the sender's next attempt is vetted
                // with it.
                let now = self.schedule_now();
                let mls = &mut self.messages.mls;
                if mls.lookups.get(&lookup).is_none_or(|at| now >= *at)
                    && !mls.looking.contains(&lookup)
                {
                    mls.lookups
                        .insert(lookup, now.saturating_add(LOOKUP_RETRY_MS));
                    self.start_lookup(lookup, request);
                }
                return Ok(Err(MlsReply::Busy));
            }
            OwnWelcome::Peer(peer) => peer,
        };
        let local = self.local()?;
        if !local.contacts.iter().any(|c| c.peer == peer) {
            if local.contacts.len() >= 1000 {
                return Ok(Err(MlsReply::ContactRequired));
            }
            let mut contacts = local.contacts.clone();
            contacts.push(crate::account::contacts::Contact {
                peer,
                alias: String::new(),
                verified_digest: None,
                known_installation: None,
                new_device: false,
            });
            contacts.sort_by_key(|c| c.peer);
            self.commit_contacts(contacts, request).await?;
        }
        Ok(Ok(peer))
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
                (s.grant.peer == peer || s.grant.peer == own)
                    && !self.outdated_now(s.grant.installation)
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
        for session in &sessions {
            self.note_requery(session.grant.installation);
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

    /// The user retried a text of the conversation: a group setup that is backing off asks
    /// again at the text's next attempt instead of after the back-off.
    pub(super) fn retry_group_now(&mut self, conversation: ConversationId) {
        if matches!(
            self.messages.mls.phases.get(&conversation),
            Some(Phase::Idle { .. })
        ) {
            self.messages.mls.phases.remove(&conversation);
        }
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
            // The KeyPackage of an earlier JoinRequest that led nowhere goes.
            let replace = self
                .messages
                .mls
                .join_key_packages
                .remove(&conversation)
                .into_iter()
                .collect();
            let GroupReply::KeyPackage(key_package) = self
                .group_db(NO_CONVERSATION, GroupOp::NewKeyPackage { replace }, request)
                .await?
            else {
                self.idle(conversation);
                return Ok(());
            };
            self.messages
                .mls
                .join_key_packages
                .insert(conversation, key_package.clone());
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
            .filter(|s| !self.outdated_now(s.grant.installation))
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
                peer_devices: sessions
                    .iter()
                    .filter(|s| s.grant.peer == peer)
                    .map(|s| s.grant.installation)
                    .collect(),
                peer_answered: false,
                peer_refused: false,
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
            peer_devices,
            peer_answered,
            peer_refused,
            ..
        }) = self.messages.mls.phases.get_mut(&conversation)
        else {
            return Ok(());
        };
        waiting.remove(&installation);
        let of_peer = peer_devices.contains(&installation);
        match result {
            Ok(MlsReply::KeyPackage(bytes)) => {
                key_packages.push(bytes);
                *peer_answered |= of_peer;
            }
            Ok(MlsReply::ContactRequired) => *peer_refused |= of_peer,
            _ => {}
        }
        if !waiting.is_empty() {
            return Ok(());
        }
        let Some(Phase::Collecting {
            peer,
            key_packages,
            peer_answered,
            peer_refused,
            ..
        }) = self.messages.mls.phases.remove(&conversation)
        else {
            return Ok(());
        };
        if peer_refused && !peer_answered {
            // The peer has not added this account: M4's contact hold (spec 3, 4.4a). Nothing is
            // asked again; the texts waiting for the group stay held until the user's retry.
            match self
                .message_db(
                    MessageDbOp::BlockPending(conversation, BlockReason::ContactRequired),
                    request,
                )
                .await
            {
                Ok(_) => {
                    self.messages.mls.wanted.remove(&conversation);
                }
                Err(_) => self.idle(conversation),
            }
            return Ok(());
        }
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
                    installation: None,
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
    /// recipient without one waits (no attempt counted) and never holds others back; it is
    /// dialed instead (`dial_unreachable`).
    async fn poll_deliveries(&mut self, request: Request) {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if self.messages.mls.hold_delivery {
            return;
        }
        let sessions = self.mls_sessions();
        let now = self.schedule_now();
        self.deliver_due(&sessions, now, request).await;
        self.dial_unreachable(&sessions, now, request).await;
    }

    async fn deliver_due(&mut self, sessions: &[Session], now: u64, request: Request) {
        if sessions.is_empty() {
            return;
        }
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

    /// Dials one recipient of a due delivery that has no session (spec 10.1): a device in this
    /// account's record (not a revoked one), or else in the conversation peer's record; any
    /// other recipient is not dialed. The queue is read at most once per `DIAL_CHECK_MS` (not
    /// every tick), one dial is started per read, and one per installation within
    /// `DIAL_RETRY_MS`. No lookup: the record this device holds is used, and a stale record or
    /// one with a conflict pending is left to the hold and record-review flows (no dial). The
    /// dial and handshake run in a spawned task; `dialed` installs the session.
    async fn dial_unreachable(&mut self, sessions: &[Session], now: u64, request: Request) {
        if now < self.messages.mls.dial_check_at {
            return;
        }
        self.messages.mls.dial_check_at = now.saturating_add(DIAL_CHECK_MS);
        self.messages
            .mls
            .dials
            .retain(|_, at| now < at.saturating_add(DIAL_RETRY_MS));
        let Ok(GroupReply::Unreachable(rows)) = self
            .group_db(NO_CONVERSATION, GroupOp::UnreachableDue { now }, request)
            .await
        else {
            return;
        };
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|(_, to)| {
                !sessions.iter().any(|s| s.grant.installation == *to)
                    && !self.messages.mls.dials.contains_key(to)
            })
            .collect();
        if rows.is_empty() {
            return;
        }
        let Ok(GroupReply::Groups(groups)) = self
            .group_db(NO_CONVERSATION, GroupOp::Groups, request)
            .await
        else {
            return;
        };
        for (conversation, recipient) in rows {
            if self.messages.mls.dials.contains_key(&recipient) {
                continue;
            }
            self.messages.mls.dials.insert(recipient, now);
            let Some(peer) = groups
                .iter()
                .find(|(c, _)| *c == conversation)
                .and_then(|(_, peer)| self.recipient_account(*peer, recipient))
            else {
                continue;
            };
            if self.start_dial(peer, recipient, request) {
                return;
            }
        }
    }

    /// Spawns the dial of `peer`'s device `installation` under the record this device holds;
    /// false when there is no record to dial it under.
    pub(super) fn start_dial(
        &mut self,
        peer: PeerId,
        installation: InstallationId,
        request: Request,
    ) -> bool {
        if !self.local_authorized() {
            return false;
        }
        let Some(record) = self.held_record(peer) else {
            return false;
        };
        let Some(id) = self
            .dial_target(peer, &record, installation)
            .and_then(|key| iroh::EndpointId::from_bytes(&key).ok())
        else {
            return false;
        };
        let Some(AccountState::Active(local)) = self.copy_state() else {
            return false;
        };
        let shared = self.shared.clone();
        let clock = self.services.clock.clone();
        let cancel = self.auth.cancel.child_token();
        let mailbox = self.auth.send.clone();
        let ticket = request.ticket;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let mut hold = self.messages.mls.dial_hold.subscribe();
        tokio::spawn(async move {
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            tokio::select! {biased;
                _ = cancel.cancelled() => {},
                _ = hold.wait_for(|held| !*held) => {},
            }
            let mut conn = None;
            let work = async {
                if !super::messages::acquisition_current(&shared, ticket) {
                    return Err(unavailable(ErrorCode::StorageLocked));
                }
                let connected = shared
                    .network
                    .dial_account(iroh::EndpointAddr::from(id))
                    .await?;
                conn = Some(connected.clone());
                if !super::messages::acquisition_current(&shared, ticket) {
                    return Err(unavailable(ErrorCode::StorageLocked));
                }
                // A device presenting another record than this one fails the handshake. The
                // record it showed is not observed here: record changes belong to the lookup
                // and record-review flows (a lagging device must not raise a conflict).
                let mut evidence = None;
                let grant = auth::authenticate(
                    connected.clone(),
                    peer,
                    &local,
                    &record,
                    &*clock,
                    &mut evidence,
                    None,
                )
                .await?;
                Ok((connected, grant))
            };
            let result = tokio::select! {biased;
                _ = cancel.cancelled() => Err(unavailable(ErrorCode::StorageLocked)),
                result = tokio::time::timeout(DIAL_TIMEOUT, work) =>
                    result.unwrap_or_else(|_| Err(unavailable(ErrorCode::NetworkTimeout))),
            };
            if result.is_err()
                && let Some(conn) = &conn
            {
                conn.close(0u32.into(), b"account authentication failed");
            }
            let _ = mailbox
                .send(Input::Mls(MlsEvent {
                    ticket,
                    peer,
                    installation: None,
                    task: Task::Dial,
                    answer: Answer::Dial(result),
                }))
                .await;
        });
        true
    }

    /// `peer`'s record as this device holds it, if it is fit to authenticate with: current, not
    /// expired and (another account's) without a conflict pending.
    fn held_record(&self, peer: PeerId) -> Option<crate::account::publication::SignedRecord> {
        let local = self.local().ok()?;
        let record = if peer == local.keys.peer_id() {
            local.records.accepted.clone()?
        } else {
            let remote = local.remote.iter().find(|r| r.peer == peer)?;
            if remote.conflict.is_some() || remote.accepted.seq < remote.floor {
                return None;
            }
            remote.accepted.clone()
        };
        let now = self.services.clock.unix_seconds();
        (record.current(now) && !record.record.is_expired(now)).then_some(record)
    }

    /// A spawned dial came back: the session is installed (checked again here, as for
    /// `connect_account`).
    async fn dialed(
        &mut self,
        dialed: Result<(iroh::endpoint::Connection, auth::AuthGrant), CoreError>,
    ) {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            self.messages.mls.settled += 1;
        }
        if let Ok((conn, grant)) = dialed {
            let _ = self.install_grant(conn, grant, None).await;
        }
    }

    /// The account to dial `recipient` under: this one if its record lists the device (none if
    /// this account revoked it), else the conversation's `peer` if its record lists it.
    fn recipient_account(&self, peer: PeerId, recipient: InstallationId) -> Option<PeerId> {
        let local = self.local().ok()?;
        let lists = |record: &crate::account::publication::SignedRecord| {
            record
                .record
                .devices
                .iter()
                .any(|d| d.installation_id == recipient)
        };
        if local.records.accepted.as_ref().is_some_and(lists) {
            let revoked = local
                .records
                .tombstones
                .iter()
                .any(|t| t.target_installation == recipient);
            return (!revoked).then(|| local.keys.peer_id());
        }
        local
            .remote
            .iter()
            .find(|r| r.peer == peer)
            .filter(|r| lists(&r.accepted))
            .map(|_| peer)
    }

    /// The current devices of `peer` as this device holds its record: those its accepted
    /// record lists, and for this account without the ones it revoked (by installation or
    /// transport key, as the group's leaf vetting drops them).
    fn current_installations(&self, peer: PeerId) -> Vec<InstallationId> {
        let Ok(local) = self.local() else {
            return Vec::new();
        };
        let record = if peer == local.keys.peer_id() {
            local.records.accepted.as_ref()
        } else {
            local
                .remote
                .iter()
                .find(|r| r.peer == peer)
                .map(|r| &r.accepted)
        };
        let Some(record) = record else {
            return Vec::new();
        };
        let own = peer == local.keys.peer_id();
        record
            .record
            .devices
            .iter()
            .filter(|d| {
                !own || !local.records.tombstones.iter().any(|t| {
                    t.target_installation == d.installation_id
                        || t.target_transport == d.transport_public_key
                })
            })
            .map(|d| d.installation_id)
            .collect()
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
            installation,
            task,
            answer,
        } = event;
        // A device answering a kind 17 request with the M4 refusal has not updated yet; so has
        // one that just closed the stream or connection, if its account never showed MLS. Any
        // proper answer from the account means it has updated.
        let own = self.local().ok().map(|local| local.keys.peer_id());
        if let Some(installation) = installation {
            match (&answer, Some(answered) != own) {
                (Answer::Reply(Err(ErrorCode::UnsupportedMessageVersion)), true) => {
                    self.mark_outdated(answered, installation);
                }
                (Answer::Reply(Err(ErrorCode::PeerNotConnected)), true) => {
                    if let Some(request) = self.ready().ok().filter(|r| r.ticket == ticket)
                        && let Ok(false) =
                            self.peer_speaks_mls(answered, installation, request).await
                    {
                        self.mark_outdated(answered, installation);
                    }
                }
                (Answer::Reply(Ok(_)), true) => self.speaks_mls(installation),
                _ => {}
            }
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
            (_, Answer::Dial(dialed)) => self.dialed(dialed).await,
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
            Task::Lookup { .. } | Task::Dial => Ok(()),
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
        let from = Origin {
            peer: session.grant.peer,
            installation: session.grant.installation,
        };
        let via = Via::Direct(session);
        let reply = match MlsFrame::decode(inner) {
            Ok(MlsFrame::App {
                conversation,
                ciphertext,
            }) => match self
                .receive_app(from, conversation, ciphertext, &via, request)
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
            Ok(frame) => match self.mls_request(from, frame, &via, request).await {
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

    /// A kind 17 request other than `App`, made by `from`. Authority (contact rule, member
    /// leaves) is decided by `from`, however it came; what needs the session (asking the sender
    /// back) runs only `Via::Direct`.
    pub(super) async fn mls_request(
        &mut self,
        from: Origin,
        frame: MlsFrame,
        via: &Via,
        request: Request,
    ) -> Result<MlsReply, CoreError> {
        let sender = from.peer;
        let now = self.schedule_now();
        // An authenticated kind 17 request from another account shows it speaks MLS (for the
        // M4-refusal decision only; the downgrade guard needs an accepted App/Commit/Welcome).
        if self
            .local()
            .is_ok_and(|local| local.keys.peer_id() != sender)
        {
            self.speaks_mls(from.installation);
        }
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
                // Each KeyPackage keeps private keys here until it is used: an installation gets
                // at most one per `KEY_PACKAGE_GAP_MS`, and only its newest few are kept.
                let asker = from.installation;
                let (handed, last) = self
                    .messages
                    .mls
                    .issued
                    .get(&asker)
                    .cloned()
                    .unwrap_or_default();
                if !handed.is_empty() && now < last.saturating_add(KEY_PACKAGE_GAP_MS) {
                    return Ok(MlsReply::Busy);
                }
                let mut handed = handed;
                let mut replace = Vec::new();
                while handed.len() >= KEY_PACKAGES_PER_ASKER {
                    replace.extend(handed.pop_front());
                }
                match self
                    .group_db(NO_CONVERSATION, GroupOp::NewKeyPackage { replace }, request)
                    .await?
                {
                    GroupReply::KeyPackage(bytes) => {
                        handed.push_back(bytes.clone());
                        self.messages.mls.issued.insert(asker, (handed, now));
                        MlsReply::KeyPackage(bytes)
                    }
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
                    credential.peer == sender && credential.installation == from.installation
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
                // Ruling 25: a logged Commit that cannot be vetted because the peer's record is
                // not here is held and the record looked up (as for a missing device), so the
                // log behind it goes on.
                let logged = matches!(via, Via::Mailbox { .. }).then(|| commit.clone());
                let op = GroupOp::ApplyCommit {
                    peer,
                    sender_peer: sender,
                    epoch,
                    commit,
                    now,
                };
                let result = match (self.group_db(conversation, op, request).await, logged) {
                    (Err(error), Some(commit)) if error.code == ErrorCode::RecordUnavailable => {
                        let op = GroupOp::HoldCommit {
                            peer,
                            sender_peer: sender,
                            epoch,
                            commit,
                            now,
                        };
                        self.group_db(conversation, op, request).await
                    }
                    (result, _) => result,
                };
                let GroupReply::Outcome(outcome) = result? else {
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
                    self.peer_spoke(conversation, from.installation, request)
                        .await?;
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
                let Some(welcome_from) = self.welcome_peer(conversation, sender) else {
                    return Ok(MlsReply::ContactRequired);
                };
                if sender_install != from.installation {
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
                    // is joined. A logged Welcome has no sender to ask: the sender's own
                    // delivery of it is joined instead.
                    if let Via::Direct(session) = via
                        && self
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
                let peer = match welcome_from {
                    WelcomeFrom::Contact(peer) => peer,
                    WelcomeFrom::OwnDevice => match self
                        .own_welcome_peer(conversation, &welcome, sender_install, request)
                        .await?
                    {
                        Ok(peer) => peer,
                        Err(reply) => {
                            // Waiting for a record: the learned hash is still good next time.
                            if reply == MlsReply::Busy {
                                self.messages
                                    .mls
                                    .known
                                    .insert((conversation, sender_install), first_commit_hash);
                            }
                            return Ok(reply);
                        }
                    },
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
                        let mls = &mut self.messages.mls;
                        mls.joined_with.insert(conversation, ct_hash);
                        mls.missing.remove(&conversation);
                        // Its KeyPackage (if this join was asked for) is used now.
                        mls.join_key_packages.remove(&conversation);
                    }
                    _ => {}
                }
                if sender == peer && matches!(outcome, Outcome::Ack | Outcome::Joined) {
                    self.peer_spoke(conversation, from.installation, request)
                        .await?;
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

    /// C1: an application message of the current `epoch` came from a sender whose ratchet is more
    /// than `MAX_FORWARD` generations past this device's view of it (it sent that many to other
    /// devices of this account). No resend in this epoch can be opened here, so this device makes
    /// an empty Commit (a path update) through the own-Commit path: queued for every member like
    /// any Commit, refused while an own Commit is unacknowledged (one in flight at most), and
    /// rate limited (`RECOVERY_LIMIT` per `RECOVERY_WINDOW_MS`). The sender is told a Commit is
    /// on its way (class 2 with the new epoch): it waits for it and seals the message again in
    /// that epoch without spending its re-encryption budget. When no epoch can be made now, the
    /// answer is the plain out-of-order failure.
    async fn move_epoch(
        &mut self,
        conversation: ConversationId,
        peer: PeerId,
        epoch: u64,
        request: Request,
    ) -> Result<MlsReply, CoreError> {
        let now = self.schedule_now();
        let out_of_order = MlsReply::DecryptFailed {
            class: CLASS_OUT_OF_ORDER,
            epoch,
        };
        let recent = self
            .messages
            .mls
            .recoveries
            .entry(conversation)
            .or_default();
        if !recovery_allowed(recent, now) {
            return Ok(out_of_order);
        }
        match self
            .group_db(conversation, GroupOp::Update { peer, now }, request)
            .await?
        {
            GroupReply::Committed(Committed::Done { epoch: moved }) if moved > epoch => {
                let mls = &mut self.messages.mls;
                mls.recoveries.entry(conversation).or_default().push(now);
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                {
                    mls.recovered += 1;
                }
                Ok(MlsReply::DecryptFailed {
                    class: CLASS_FUTURE_EPOCH,
                    epoch: moved,
                })
            }
            _ => Ok(out_of_order),
        }
    }

    /// An installation of the conversation's peer account just spoke MLS (an accepted Commit or
    /// Welcome): kept for the downgrade guard (`mls_peer`, never cleared) and as evidence it
    /// is no M4 app.
    async fn peer_spoke(
        &mut self,
        conversation: ConversationId,
        installation: InstallationId,
        request: Request,
    ) -> Result<(), CoreError> {
        self.speaks_mls(installation);
        let op = GroupOp::PeerSpokeMls {
            mark: true,
            installation,
        };
        self.group_db(conversation, op, request).await.map(|_| ())
    }

    /// A received application message (spec 5.2): opened and stored in one transaction. Its MLS
    /// sender may be any current device of `from`'s account, not only the one that carried it
    /// (M6 spec 7: a mailbox hands it over as is); that leaf is its sender. `None` when it
    /// carried a text M4
    /// would refuse (nothing stored; the ratchet still moved). A text from the other account
    /// stored just now is notified: at once `Via::Direct`, with the rest of the sync
    /// `Via::Mailbox`; so is my read on another own device.
    pub(super) async fn receive_app(
        &mut self,
        from: Origin,
        conversation: ConversationId,
        ciphertext: Vec<u8>,
        via: &Via,
        request: Request,
    ) -> Result<Option<MlsReply>, CoreError> {
        let sender = from.peer;
        // Application messages come from the conversation's peer account, or (spec 5.3) are a
        // copy of what another device of this account sent in a conversation with a contact.
        let own = sender == self.local()?.keys.peer_id();
        let Some(peer) = self
            .conversation_peer(conversation, sender)
            .filter(|peer| own || *peer == sender)
        else {
            return Ok(Some(MlsReply::ContactRequired));
        };
        let open = OpenFrom {
            peer: sender,
            transport: from.installation,
            allowed: self.current_installations(sender),
            pull: match via {
                Via::Mailbox { holder, .. } => Some(*holder),
                Via::Direct(_) => None,
            },
        };
        let op = MessageDbOp::OpenApp(
            open,
            conversation,
            ciphertext,
            self.services.clock.wall_ms(),
        );
        let result = self.message_db(op, request).await;
        if result.is_ok() && !own {
            self.speaks_mls(from.installation);
        }
        match result {
            Ok(MessageDbReply::Opened(reply)) => Ok(Some(reply)),
            Ok(MessageDbReply::OpenedText(reply, key, new)) => {
                // The keyed response checkpoint of tests: the Ack that stores this text.
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                if matches!(via, Via::Direct(_)) {
                    self.messages.ack_key = Some(key);
                }
                if new {
                    self.text_stored(key, via, request).await;
                }
                Ok(Some(reply))
            }
            // My read on another own device: shown here as read, its notification cleared.
            Ok(MessageDbReply::OpenedRead(reply, conversation)) => {
                self.read_synced(conversation, via, request).await;
                Ok(Some(reply))
            }
            Ok(MessageDbReply::Rejected) => Ok(None),
            // Sealed by a device of the account that is not current here. Directly: refused,
            // whoever carries it. From another own device's log (final review I3): that device
            // took it while the leaf was current (it was a member of the group the message
            // opened in), so it is not taken from the ciphertext but goes to the plaintext
            // hand-over (spec 6.3), not skipped.
            Ok(MessageDbReply::NotCurrent) => match via {
                Via::Direct(_) => Ok(None),
                Via::Mailbox { .. } => Ok(Some(MlsReply::DecryptFailed {
                    class: CLASS_OTHER,
                    epoch: 0,
                })),
            },
            Ok(MessageDbReply::FarAhead { epoch }) => match via {
                Via::Direct(_) => Ok(Some(
                    self.move_epoch(conversation, peer, epoch, request).await?,
                )),
                // No sender waits to seal it again in a new epoch: it is not opened here.
                Via::Mailbox { .. } => Ok(Some(MlsReply::DecryptFailed {
                    class: CLASS_OUT_OF_ORDER,
                    epoch,
                })),
            },
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

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn self_made_epochs_are_limited_per_hour() {
        let mut recent = Vec::new();
        for n in 0..RECOVERY_LIMIT as u64 {
            assert!(recovery_allowed(&mut recent, 1_000 + n));
            recent.push(1_000 + n);
        }
        assert!(!recovery_allowed(&mut recent, 2_000));
        assert!(!recovery_allowed(
            &mut recent,
            1_000 + RECOVERY_WINDOW_MS - 1
        ));
        // The oldest one leaves the window: one more is allowed.
        assert!(recovery_allowed(&mut recent, 1_000 + RECOVERY_WINDOW_MS));
        assert_eq!(recent.len(), RECOVERY_LIMIT - 1);
    }
}
