//! Own-device sync of the account actor (M6, spec 6.1–6.3, frame kind 18).
//!
//! Every device logs the kind 17 frames it accepted or sent (`message/mailbox_store.rs`). When a
//! session with another device of this account becomes ready (whichever side dialed), when that
//! device sends `Notify`, and on `mailbox_sync_now`, this device pulls the other's log from where
//! it stopped: one `SyncReq` per page, its entries processed in `seq` order through the M5
//! receive paths (`receive_app`, `mls_request`) under the device that originally sent them. The
//! holder's cursor (`Pulled`) moves only once a whole page is processed and stored; a page cut
//! short is pulled again, and what it stored already is filtered by `processed_ct`.
//!
//! An `App` entry this device cannot open (spec 6.3) is asked for as plaintext (`PlainReq`) once
//! its page is processed; the holder answers only a current device of this account. The record
//! is stored under the message's logical key, with no receipt; `NoPlain` (the holder no longer
//! has it) skips the entry. The page is done only when every such entry is resolved.
//!
//! The requests run in spawned tasks and come back as `Input::Mailbox`, like kind 17 requests:
//! both devices pull from each other at once, so an actor turn waiting on the other would wait
//! for a turn waiting on it. One pull per device at a time. A device that answered `Refused`, or
//! sent a page out of order, is not pulled from again over that session.
//!
//! Notifications (`CoreEvent::Notify`): a pull, once it ends, reports per conversation the texts
//! from the other account it stored that are still unread, and the conversations whose messages
//! it found read on another own device (spec 6.4: the app clears or updates their notification);
//! a text received directly, or my read sent directly, is reported on its own at once. Each
//! text a pull stores is marked for that in the transaction that stores it (Ruling 35), so what
//! a run stored and died before reporting is reported after it starts again: with the next pull
//! from that device that ends, or `RESTART_NOTIFY_MS` after the start.
//!
//! A pull that fails short of the end of the log (no answer, `Busy`, storage) is tried again
//! after a back-off, `PULL_RETRY_MIN_MS` doubling to `PULL_RETRY_MAX_MS` (final review I1).
//!
//! Outbox handoff (spec 7): when the app goes to the background (`mailbox_background`), and
//! once a session with another own device is ready and its pull is done, this device hands the
//! sealed ciphertexts of its undelivered messages to its mailbox devices (Ruling 12: only the
//! devices recorded as mailboxes). A mailbox retries each as it is (kind 17 `App` to the peer,
//! back-off capped at `MAILBOX_MAX_DELAY_MS`) and logs how it ended as a note for the origin:
//! `Delivered`, or `HandoffStop` when the peer could not open it, which the origin takes as its
//! own decrypt failure (only the origin re-encrypts). The origin keeps retrying too; the peer
//! drops the duplicate.
//!
//! The mailbox role (spec 8.4): a device set as a mailbox (`set_mailbox`) tells my other devices
//! with `Announce`, when it changes and on every session with one that becomes ready, again
//! until that device took it (one at a time per device), and never a contact. What they announce is recorded (`mailbox_device`): it is where handoffs go, which
//! of my devices `mailbox_sync_now` dials first, and what `ConnectionSnapshot::mailbox_ok`
//! reports. A mailbox takes handoffs only while it is one (what it took it tries until settled),
//! and records its uptime every `UPTIME_EVERY_MS`.
//!
//! The verdict (M8 spec 5): every `VERDICT_EVERY_MS`, a mailbox works out from its uptime
//! whether it qualifies, and every device works out from what it knows of my devices whether
//! the account has a qualified mailbox (`mailbox/verdict.rs`). The verdict is worked out again
//! sooner only when what it is made of changes: this device's role, or what a received
//! `Announce` or a session says of a device in a way that counts (`changes_verdict`). The
//! hourly run first sees again my devices whose session is still open, so a long session keeps
//! its device counted. A changed qualification is announced like a changed role.
use super::mls::{Origin, Via};
use super::*;
use crate::account::{
    auth,
    connection::{Input, Session},
};
use crate::mailbox::verdict::{
    self, MailboxFact, QueueMode, VERDICT_EVERY_MS, Verdict, changes_verdict, qualifies,
};
use crate::mailbox::wire::{
    EntryBody, HandoffEntry, LogEntry, MailboxFrame, MailboxReply, PlainRecord, handoff_batches,
};
use crate::message::{
    mailbox_store::{HandoffOutcome, MailboxDbOp, MailboxDbReply, QueueFacts},
    store::{MessageDbOp, MessageDbReply},
    types::{ConversationId, MessageId, MessageKey},
};
use crate::mls::wire::{MlsFrame, MlsReply};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Another own device is told about new log entries at most this often.
const NOTIFY_GAP_MS: u64 = 200;
/// The log is pruned at least this often (Ruling 11), on every device.
const PRUNE_EVERY_MS: u64 = 3_600_000;
/// A peer without a session is dialed for the ciphertexts handed over for it at most this often.
const HANDOFF_DIAL_GAP_MS: u64 = 30_000;
/// A mailbox records that it is up this often.
const UPTIME_EVERY_MS: u64 = 60_000;
/// A pull that failed (no answer, `Busy`, storage) is tried again after this long, doubling
/// with each failure in a row up to `PULL_RETRY_MAX_MS`.
const PULL_RETRY_MIN_MS: u64 = 1_000;
const PULL_RETRY_MAX_MS: u64 = 30_000;
/// Texts a run stored from another own device's log and died before notifying (Ruling 35) are
/// notified with the next pull from that device that ends, or this long after the start when no
/// pull from it runs by then.
const RESTART_NOTIFY_MS: u64 = 60_000;
/// An `Announce` my other device did not take (`Busy`, no answer) is sent again after a
/// back-off, `ANNOUNCE_RETRY_MIN_MS` doubling with each failure in a row up to
/// `ANNOUNCE_RETRY_MAX_MS`. An M6 device answers `Busy` to the four-field form it cannot read,
/// for as long as the session lasts: the cap keeps that to one `Announce` per 5 minutes.
const ANNOUNCE_RETRY_MIN_MS: u64 = 5_000;
const ANNOUNCE_RETRY_MAX_MS: u64 = 300_000;
/// A session whose pull was answered `Refused` or broke the order rules is not synced over for
/// `STOPPED_RETRY_MIN_MS`, doubling with each such stop in a row from the same device up to
/// `STOPPED_RETRY_MAX_MS`; then that device is pulled from again (user decision 2026-11-16: a
/// device that refused right after a join serves the new device a moment later).
const STOPPED_RETRY_MIN_MS: u64 = 5_000;
const STOPPED_RETRY_MAX_MS: u64 = 300_000;

/// What an `Announce` says: whether this device is a mailbox, its own-device push handle,
/// whether it qualifies.
type Told = (bool, Option<Vec<u8>>, bool);

/// A handed-off ciphertext's row: the device that handed it over, conversation, message.
type HandoffKey = (InstallationId, ConversationId, MessageId);

#[derive(Default)]
pub(crate) struct Runtime {
    /// Own devices to pull from at the next poll.
    due: BTreeSet<InstallationId>,
    /// Pulls running, by the device whose log is pulled.
    pulls: BTreeMap<InstallationId, Pull>,
    /// Own devices whose last pull failed: the scheduler time it is tried again (`u64::MAX`:
    /// it was), and the back-off that wait was. Gone once a pull from it gets to the end.
    retry: BTreeMap<InstallationId, (u64, u64)>,
    /// Connections (`stable_id`) this device stopped syncing over for now (`Refused`, or a page
    /// that broke the order rules): the device, and the scheduler time it is pulled from again.
    stopped: BTreeMap<usize, (InstallationId, u64)>,
    /// Per own device: the last wait of `stopped` (`STOPPED_RETRY_MIN_MS` doubling). Gone once a
    /// pull from it gets to the end.
    stop_delay: BTreeMap<InstallationId, u64>,
    /// The log's head (largest seq handed out) after the last storage operation.
    pub(super) head: u64,
    /// Per own device: the head it was last told about, and when (scheduler time).
    notified: BTreeMap<InstallationId, (u64, u64)>,
    /// The other own devices last given to the store (`Devices`).
    devices: Option<Vec<InstallationId>>,
    /// Scheduler time of the next prune.
    prune_at: u64,
    /// Scheduler time the texts marked for notification by an earlier run are notified, those
    /// of holders no pull runs from then (`None`: not started; `u64::MAX`: done).
    restart_notify_at: Option<u64>,
    /// A session with another own device became ready: what is not delivered goes to the
    /// mailboxes among them once no pull runs (what a pull brings may settle some of it first).
    handoff_due: bool,
    /// Handed-off ciphertexts being tried now.
    sending: BTreeSet<HandoffKey>,
    /// Scheduler time handed-off ciphertexts are next due (`None`: read the table to know;
    /// `u64::MAX`: none is).
    handoff_at: Option<u64>,
    /// Peers whose session became ready: what was handed over for them is due now.
    woken: BTreeSet<PeerId>,
    /// Per peer, when its devices were last dialed for what was handed over for it.
    dialed: BTreeMap<PeerId, u64>,
    /// This device is set as a mailbox (`None`: read at the next poll).
    enabled: Option<bool>,
    /// My devices recorded as mailboxes (`None`: read at the next poll).
    mailboxes: Option<BTreeSet<InstallationId>>,
    /// What each of my other devices took from this one's last `Announce` (whether this one is
    /// a mailbox, its own-device push handle), over its current session. A device whose entry
    /// differs from what holds now is told at the next poll, one `Announce` at a time, until it
    /// takes it: a lost or `Busy` answer is not the end (e2e m7 run 2).
    told: BTreeMap<InstallationId, Told>,
    /// Own devices an `Announce` is on its way to.
    telling: BTreeSet<InstallationId>,
    /// Own devices whose last `Announce` was not taken: the scheduler time they are told again,
    /// the back-off that wait was, and what that `Announce` said (a changed value is told at
    /// once, with the back-off from the start).
    tell_at: BTreeMap<InstallationId, (u64, u64, Told)>,
    /// Scheduler time this device, while a mailbox, next records that it is up.
    uptime_at: u64,
    /// This device's qualification as last worked out (`None`: not yet since the start).
    qualified: Option<bool>,
    /// The verdict as last worked out (`None`: work it out at the next poll).
    verdict: Option<Verdict>,
    /// Scheduler time the qualification and the verdict are next worked out.
    judge_at: u64,
    /// My devices with a session that became ready since the last poll (seen, spec 5.1).
    seen: BTreeSet<InstallationId>,
    /// Verdicts worked out since this core started.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    verdicts: u64,
    /// Each `Announce` sent: to which device, whether it said mailbox and qualified.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    announces: Vec<(InstallationId, bool, bool)>,
    /// The devices the last `mailbox_sync_now` dialed, in order.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    sync_dials: Vec<InstallationId>,
    /// Each handed-off ciphertext sent: scheduler time and SHA-256 of the ciphertext.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    handoff_sends: Vec<(u64, [u8; 32])>,
    /// After this many texts stored by pulls, processing stops as if the device died there.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    halt_after: Option<u64>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    stored: u64,
    /// After this many pages stored by pulls, processing stops as if the device died there.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    halt_after_pages: Option<u64>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pages: u64,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    halted: bool,
    /// Entries skipped because their holder had no plaintext record for them (`NoPlain`).
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    missing: u64,
    /// Notifications published with a new message, and those new messages summed.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    alerts: u64,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    alerted_messages: u64,
    /// Sync is paused: no upkeep, pulls or `Notify`, and requests are answered `Busy`.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    held: bool,
    /// Page requests still to fail as timed out, without being sent.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    fail_pulls: u32,
    /// Page requests from my other devices still to answer `Refused`.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    refuse_pulls: u32,
    /// How sessions with my other devices came about and how pulls ended (e2e m7).
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    trace: SyncTrace,
}

impl Runtime {
    /// e2e m6: a session with one of my other devices closed (`close_name`).
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) fn note_own_close(&mut self, conn: &iroh::endpoint::Connection) {
        self.trace.own_closes += 1;
        self.trace.own_close = Some(close_name(conn));
    }
    /// Pulls from `installation` at the next poll, or again once its running pull ends.
    fn schedule(&mut self, installation: InstallationId) {
        match self.pulls.get_mut(&installation) {
            Some(pull) => pull.again = true,
            None => {
                self.due.insert(installation);
            }
        }
    }
    /// A pull from `installation` failed: it is tried again after a back-off twice the last one
    /// (`PULL_RETRY_MIN_MS` the first time), at most `PULL_RETRY_MAX_MS`.
    fn back_off(&mut self, installation: InstallationId, now: u64) {
        let delay = self
            .retry
            .get(&installation)
            .map_or(PULL_RETRY_MIN_MS, |(_, last)| {
                last.saturating_mul(2).min(PULL_RETRY_MAX_MS)
            });
        self.retry
            .insert(installation, (now.saturating_add(delay), delay));
    }
    /// Sync over connection `conn` with `installation` stops for now (`Refused`, or a page out
    /// of order): that device is pulled from again after a back-off twice the last one
    /// (`STOPPED_RETRY_MIN_MS` the first time), at most `STOPPED_RETRY_MAX_MS`.
    fn stop(&mut self, conn: usize, installation: InstallationId, now: u64) {
        let delay = self
            .stop_delay
            .get(&installation)
            .map_or(STOPPED_RETRY_MIN_MS, |last| {
                last.saturating_mul(2).min(STOPPED_RETRY_MAX_MS)
            });
        self.stop_delay.insert(installation, delay);
        self.stopped
            .insert(conn, (installation, now.saturating_add(delay)));
    }
    /// The `Announce` saying `told` was not taken by `installation`: it is told again after a
    /// back-off twice the last one for the same value (`ANNOUNCE_RETRY_MIN_MS` the first time),
    /// at most `ANNOUNCE_RETRY_MAX_MS`.
    fn tell_later(&mut self, installation: InstallationId, told: Told, now: u64) {
        let delay = match self.tell_at.get(&installation) {
            Some((_, last, tried)) if *tried == told => {
                last.saturating_mul(2).min(ANNOUNCE_RETRY_MAX_MS)
            }
            _ => ANNOUNCE_RETRY_MIN_MS,
        };
        self.tell_at
            .insert(installation, (now.saturating_add(delay), delay, told));
    }
    fn halted(&self) -> bool {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            self.halted
        }
        #[cfg(not(all(feature = "test-hooks", debug_assertions)))]
        {
            false
        }
    }
    fn held(&self) -> bool {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            self.held
        }
        #[cfg(not(all(feature = "test-hooks", debug_assertions)))]
        {
            false
        }
    }
}

struct Pull {
    /// The session's connection.
    conn: usize,
    /// Conversations whose messages this pull found read on another own device.
    read: BTreeSet<ConversationId>,
    /// Another pull was asked for while this one ran.
    again: bool,
}

/// The answer to one exchange of a running pull.
pub(crate) struct MailboxEvent {
    ticket: u64,
    holder: InstallationId,
    conn: usize,
    step: Step,
}

enum Step {
    /// The answer to `SyncReq { since_seq: since }`.
    Page {
        since: u64,
        answer: Result<MailboxReply, ErrorCode>,
    },
    /// The answers to the `PlainReq`s for a processed page's unopened entries, in order, up to
    /// the first one that did not resolve its entry.
    Plain {
        page: Resolving,
        answers: Vec<Result<MailboxReply, ErrorCode>>,
    },
    /// The answer of my other device (the event's `holder`) to an `Announce` saying `told`.
    Announced {
        told: Told,
        answer: Result<MailboxReply, ErrorCode>,
    },
    /// The peer's answer to one attempt of a handed-off ciphertext (`grant`: the session it
    /// went over; the event's `holder` is that session's device).
    Handoff {
        key: HandoffKey,
        ct_hash: [u8; 32],
        grant: auth::AuthGrant,
        answer: Result<MlsReply, ErrorCode>,
    },
}

/// A processed page waiting for the plaintext records of the entries it could not open.
struct Resolving {
    next_seq: u64,
    more: bool,
    unopened: Vec<Unopened>,
}

/// What one processed page left.
pub(super) struct PullSummary {
    /// The `App` entries this device could not open (spec 6.3).
    pub unopened: Vec<Unopened>,
}

/// An `App` entry this device could not open: what its plaintext record must match.
pub(super) struct Unopened {
    seq: u64,
    conversation: ConversationId,
    sender_peer: PeerId,
    sender_install: InstallationId,
    /// SHA-256 of its ciphertext, the payload hash of the text it carries.
    ct_hash: [u8; 32],
}

impl Unopened {
    /// The holder's record is of the message this entry carries, as far as the entry tells.
    fn matches(&self, record: &PlainRecord) -> bool {
        record.conversation == self.conversation
            && record.sender_peer == self.sender_peer
            && record.sender_install == self.sender_install
    }
}

/// How a pull ended.
#[derive(Clone, Copy)]
enum Ended {
    /// At the end of the log.
    Done,
    /// Short of it, for now (no answer, `Busy`, storage, the session gone).
    Failed,
    /// No more over that session (`Refused`, a page out of order).
    Stopped,
}

/// What became of one log entry.
enum Entry {
    /// Processed: stored, applied, already known, or nothing this device can use.
    Taken,
    /// An `App` frame this device could not open.
    Unopened(Unopened),
}

/// One kind 18 request/response exchange on an authenticated session.
async fn exchange(session: &Session, body: &[u8]) -> Result<MailboxReply, ErrorCode> {
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
        let reply = auth::read_frame(&mut recv, 61_440)
            .await
            .map_err(|_| ErrorCode::PeerNotConnected)?;
        let inner =
            auth::payload(&reply, crate::mailbox::KIND).map_err(|_| ErrorCode::InvalidMessage)?;
        MailboxReply::decode(&inner).map_err(|_| ErrorCode::InvalidMessage)
    };
    tokio::time::timeout(REQUEST_TIMEOUT, work)
        .await
        .unwrap_or(Err(ErrorCode::NetworkTimeout))
}

/// A `SyncResp` keeps the page rules: entries in strictly ascending `seq`, none below the
/// request, `next_seq` past the last one; an empty page ends the log.
fn page_in_order(entries: &[LogEntry], since: u64, next_seq: u64, more: bool) -> bool {
    let mut floor = since;
    for entry in entries {
        if entry.seq < floor {
            return false;
        }
        let Some(next) = entry.seq.checked_add(1) else {
            return false;
        };
        floor = next;
    }
    if entries.is_empty() {
        !more && next_seq >= since
    } else {
        next_seq >= floor
    }
}

/// A storage failure that ends the request rather than being answered `Busy`.
fn fatal(error: &CoreError) -> bool {
    matches!(
        error.code,
        ErrorCode::AccountRestricted | ErrorCode::StorageLocked
    )
}

/// This account's devices other than this one that are not revoked, sorted.
fn other_devices(local: &LocalAccount) -> Option<Vec<InstallationId>> {
    let record = local.records.accepted.as_ref()?;
    let me = local.device.installation_id();
    let mut others: Vec<InstallationId> = record
        .record
        .devices
        .iter()
        .map(|d| d.installation_id)
        .filter(|i| {
            *i != me
                && !local
                    .records
                    .tombstones
                    .iter()
                    .any(|t| t.target_installation == *i)
        })
        .collect();
    others.sort();
    Some(others)
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(crate) enum MailboxTest {
    /// Sends one kind 18 request to `installation` and waits for the answer.
    Exchange(InstallationId, MailboxFrame),
    /// Stops all sync processing after this many texts stored by pulls.
    HaltAfter(u64),
    /// Stops all sync processing once pulls stored this many pages (their cursors moved).
    HaltAfterPages(u64),
    /// Pauses (or resumes) sync: no upkeep, pulls or `Notify`; requests answered `Busy`.
    Hold(bool),
    /// The next this many page requests of pulls fail as timed out, without being sent.
    FailPulls(u32),
    /// The next this many page requests from my other devices are answered `Refused`.
    RefusePulls(u32),
    Counters,
    /// My other devices with a ready session now.
    OwnSessions,
    /// Each read clear decided (M7 spec 7): my other devices with a ready session then, and the
    /// `read` items pushed.
    ReadClears,
    HandoffSends,
    Announces,
    SyncDials,
    SyncTrace,
    /// e2e m8: this device's uptime records become `permille` with `recorded_ms` recorded, and
    /// its qualification is worked out at the next poll.
    SeedUptime { permille: u32, recorded_ms: u64 },
    Verdicts,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(crate) enum MailboxTestReply {
    Reply(MailboxReply),
    Done,
    Counters(MailboxCounters),
    Sends(Vec<(u64, [u8; 32])>),
    Announces(Vec<(InstallationId, bool, bool)>),
    Verdicts(u64),
    Sessions(Vec<InstallationId>),
    ReadClears(Vec<(u32, u32)>),
    Dials(Vec<InstallationId>),
    Trace(SyncTrace),
}

/// Since this core started (e2e m7 `read_cleared`): dials of my other devices that came back
/// installed or failed, connections from them accepted or refused (each with the last failure's
/// code and, for an authentication refusal, the check that refused it: `auth::AuthCheck::name`),
/// and pulls started and how they ended.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncTrace {
    pub dials_ok: u64,
    pub dials_failed: u64,
    pub dial_error: Option<ErrorCode>,
    pub dial_check: Option<&'static str>,
    pub accepts_ok: u64,
    pub accepts_failed: u64,
    pub accept_error: Option<ErrorCode>,
    pub accept_check: Option<&'static str>,
    pub pulls: u64,
    pub pulls_done: u64,
    pub pulls_failed: u64,
    pub pulls_stopped: u64,
    /// Why the last failed pull failed (`pull_failed`).
    pub pull_failure: Option<&'static str>,
    /// Sessions with my other devices that closed, and how the last one did (`close_name`).
    pub own_closes: u64,
    pub own_close: Option<&'static str>,
}

/// How a connection ended, as a fixed name (e2e facts): by this side (`local`), by the peer
/// with one of this app's close reasons (`peer_*`) or another one (`peer_other`), an idle
/// timeout, a reset, or something else.
#[cfg(all(feature = "test-hooks", debug_assertions))]
fn close_name(conn: &iroh::endpoint::Connection) -> &'static str {
    use iroh::endpoint::ConnectionError as E;
    const PEER: [(&[u8], &str); 6] = [
        (b"simultaneous dial", "peer_simultaneous_dial"),
        (b"new account proof", "peer_new_proof"),
        (b"account restricted", "peer_restricted"),
        (b"account authentication failed", "peer_auth_failed"),
        (b"account record changed", "peer_record_changed"),
        (b"account stopped", "peer_stopped"),
    ];
    match conn.close_reason() {
        None => "open",
        Some(E::LocallyClosed) => "local",
        Some(E::TimedOut) => "timeout",
        Some(E::Reset) => "reset",
        Some(E::ApplicationClosed(close)) => PEER
            .iter()
            .find(|(reason, _)| close.reason.as_ref() == *reason)
            .map_or("peer_other", |(_, name)| name),
        Some(_) => "other",
    }
}

/// What own-device sync counted since this core started (for the M6 results).
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxCounters {
    /// Entries this device could not open whose holder had no plaintext record (spec 6.3: the
    /// message is missing here). Unopened receipt entries are counted too: a receipt has no
    /// plaintext record.
    pub missing: u64,
    /// `CoreEvent::Notify` events published with at least one new message (an alerting
    /// notification; an update with no new message is not one).
    pub notifications: u64,
    /// The new messages of those events, summed (`NotifyItem::new_messages`).
    pub notified: u64,
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
impl AccountHandle {
    async fn mailbox_test(&self, test: MailboxTest) -> Result<MailboxTestReply, CoreError> {
        self.send(|reply| Command::MailboxTest(test, reply)).await
    }
    /// Sends `frame` to `installation` (any account) over the ready session with it and returns
    /// the answer (5 s limit).
    pub async fn mailbox_test_exchange(
        &self,
        installation: InstallationId,
        frame: MailboxFrame,
    ) -> Result<MailboxReply, CoreError> {
        match self
            .mailbox_test(MailboxTest::Exchange(installation, frame))
            .await?
        {
            MailboxTestReply::Reply(reply) => Ok(reply),
            _ => unreachable!(),
        }
    }
    pub async fn mailbox_test_counters(&self) -> Result<MailboxCounters, CoreError> {
        match self.mailbox_test(MailboxTest::Counters).await? {
            MailboxTestReply::Counters(counters) => Ok(counters),
            _ => unreachable!(),
        }
    }
    /// A crash in the middle of a sync: once pulls stored `texts` texts, this core stops
    /// processing them (no cursor moves, no notification) until it is restarted.
    pub async fn mailbox_test_halt_after(&self, texts: u64) -> Result<(), CoreError> {
        self.mailbox_test(MailboxTest::HaltAfter(texts))
            .await
            .map(|_| ())
    }
    /// A crash between pages: once pulls stored `pages` pages (and moved their cursors), this
    /// core stops processing (no further page, no notification) until it is restarted.
    pub async fn mailbox_test_halt_after_pages(&self, pages: u64) -> Result<(), CoreError> {
        self.mailbox_test(MailboxTest::HaltAfterPages(pages))
            .await
            .map(|_| ())
    }
    /// Pauses own-device sync on this core (no storage work for it, requests answered
    /// `Busy`), or resumes it.
    pub async fn mailbox_test_hold(&self, hold: bool) -> Result<(), CoreError> {
        self.mailbox_test(MailboxTest::Hold(hold)).await.map(|_| ())
    }
    /// My other devices this core has a ready session with now.
    pub async fn mailbox_test_own_sessions(&self) -> Result<Vec<InstallationId>, CoreError> {
        match self.mailbox_test(MailboxTest::OwnSessions).await? {
            MailboxTestReply::Sessions(sessions) => Ok(sessions),
            _ => unreachable!(),
        }
    }
    /// Each read clear this core decided, in order: my other devices with a ready session at
    /// that moment, and the `read` items it pushed (M7 spec 7).
    pub async fn push_test_read_clears(&self) -> Result<Vec<(u32, u32)>, CoreError> {
        match self.mailbox_test(MailboxTest::ReadClears).await? {
            MailboxTestReply::ReadClears(clears) => Ok(clears),
            _ => unreachable!(),
        }
    }
    /// How sessions with my other devices came about and how pulls ended, since this core
    /// started.
    pub async fn mailbox_test_sync_trace(&self) -> Result<SyncTrace, CoreError> {
        match self.mailbox_test(MailboxTest::SyncTrace).await? {
            MailboxTestReply::Trace(trace) => Ok(trace),
            _ => unreachable!(),
        }
    }
    /// The next `requests` page requests of this core's pulls fail as a timeout would, without
    /// being sent.
    pub async fn mailbox_test_fail_pulls(&self, requests: u32) -> Result<(), CoreError> {
        self.mailbox_test(MailboxTest::FailPulls(requests))
            .await
            .map(|_| ())
    }
    /// The next `requests` page requests from my other devices are answered `Refused`.
    pub async fn mailbox_test_refuse_pulls(&self, requests: u32) -> Result<(), CoreError> {
        self.mailbox_test(MailboxTest::RefusePulls(requests))
            .await
            .map(|_| ())
    }
    /// Each attempt this device (as a mailbox) made of a handed-off ciphertext, in order: the
    /// scheduler time it was sent at and the SHA-256 of the ciphertext.
    pub async fn mailbox_test_handoff_sends(&self) -> Result<Vec<(u64, [u8; 32])>, CoreError> {
        match self.mailbox_test(MailboxTest::HandoffSends).await? {
            MailboxTestReply::Sends(sends) => Ok(sends),
            _ => unreachable!(),
        }
    }
    /// Each `Announce` this device sent, in order: to which device, whether it said it is a
    /// mailbox, and whether it said it qualifies.
    pub async fn mailbox_test_announces(
        &self,
    ) -> Result<Vec<(InstallationId, bool, bool)>, CoreError> {
        match self.mailbox_test(MailboxTest::Announces).await? {
            MailboxTestReply::Announces(announces) => Ok(announces),
            _ => unreachable!(),
        }
    }
    /// e2e m8: replaces this device's uptime records so that `permille` of the 7-day window is
    /// up, the earliest record starting `recorded_ms` ago; the qualification is worked out
    /// again at the next poll. Refused when more is up than recorded.
    pub async fn mailbox_test_seed_uptime(
        &self,
        permille: u32,
        recorded_ms: u64,
    ) -> Result<(), CoreError> {
        self.mailbox_test(MailboxTest::SeedUptime {
            permille,
            recorded_ms,
        })
        .await
        .map(|_| ())
    }
    /// How many verdicts this core worked out since it started.
    pub async fn mailbox_test_verdicts(&self) -> Result<u64, CoreError> {
        match self.mailbox_test(MailboxTest::Verdicts).await? {
            MailboxTestReply::Verdicts(n) => Ok(n),
            _ => unreachable!(),
        }
    }
    /// The devices the last `mailbox_sync_now` dialed, in the order it dialed them.
    pub async fn mailbox_test_sync_dials(&self) -> Result<Vec<InstallationId>, CoreError> {
        match self.mailbox_test(MailboxTest::SyncDials).await? {
            MailboxTestReply::Dials(dials) => Ok(dials),
            _ => unreachable!(),
        }
    }
}

impl Manager {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) async fn mailbox_test(
        &mut self,
        test: MailboxTest,
    ) -> Result<MailboxTestReply, CoreError> {
        Ok(match test {
            MailboxTest::Exchange(installation, frame) => {
                // Test only: this actor waits on the answer.
                let session = self
                    .auth
                    .registry
                    .0
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|s| {
                        s.ready
                            && s.grant.installation == installation
                            && s.conn.close_reason().is_none()
                    })
                    .cloned()
                    .ok_or_else(|| unavailable(ErrorCode::PeerNotConnected))?;
                let reply = exchange(&session, &frame.encode())
                    .await
                    .map_err(unavailable)?;
                MailboxTestReply::Reply(reply)
            }
            MailboxTest::HaltAfter(texts) => {
                let mailbox = &mut self.messages.mailbox;
                mailbox.halt_after = Some(texts);
                mailbox.halted = mailbox.stored >= texts;
                MailboxTestReply::Done
            }
            MailboxTest::HaltAfterPages(pages) => {
                self.messages.mailbox.halt_after_pages = Some(pages);
                MailboxTestReply::Done
            }
            MailboxTest::Hold(hold) => {
                self.messages.mailbox.held = hold;
                MailboxTestReply::Done
            }
            MailboxTest::FailPulls(requests) => {
                self.messages.mailbox.fail_pulls = requests;
                MailboxTestReply::Done
            }
            MailboxTest::RefusePulls(requests) => {
                self.messages.mailbox.refuse_pulls = requests;
                MailboxTestReply::Done
            }
            MailboxTest::SyncTrace => MailboxTestReply::Trace(self.messages.mailbox.trace),
            MailboxTest::Counters => MailboxTestReply::Counters(MailboxCounters {
                missing: self.messages.mailbox.missing,
                notifications: self.messages.mailbox.alerts,
                notified: self.messages.mailbox.alerted_messages,
            }),
            MailboxTest::HandoffSends => {
                MailboxTestReply::Sends(self.messages.mailbox.handoff_sends.clone())
            }
            MailboxTest::OwnSessions => MailboxTestReply::Sessions(
                self.own_sessions()
                    .iter()
                    .map(|s| s.grant.installation)
                    .collect(),
            ),
            MailboxTest::ReadClears => {
                MailboxTestReply::ReadClears(self.messages.push.read_clear_log.clone())
            }
            MailboxTest::Announces => {
                MailboxTestReply::Announces(self.messages.mailbox.announces.clone())
            }
            MailboxTest::SyncDials => {
                MailboxTestReply::Dials(self.messages.mailbox.sync_dials.clone())
            }
            MailboxTest::SeedUptime {
                permille,
                recorded_ms,
            } => {
                let request = self.ready()?;
                let op = MailboxDbOp::SeedUptime {
                    permille,
                    recorded_ms,
                    now: self.services.clock.wall_ms(),
                };
                self.mailbox_db(op, request).await?;
                self.messages.mailbox.judge_at = 0;
                MailboxTestReply::Done
            }
            MailboxTest::Verdicts => MailboxTestReply::Verdicts(self.messages.mailbox.verdicts),
        })
    }

    async fn mailbox_db(
        &mut self,
        op: MailboxDbOp,
        request: Request,
    ) -> Result<MailboxDbReply, CoreError> {
        match self.message_db(MessageDbOp::Mailbox(op), request).await? {
            MessageDbReply::Mailbox(reply) => Ok(reply),
            _ => Err(unavailable(ErrorCode::InvalidMessage)),
        }
    }

    /// Ready, authorized sessions with this account's other (not revoked) devices, one each.
    fn own_sessions(&self) -> Vec<Session> {
        let Ok(own) = self.local().map(|local| local.keys.peer_id()) else {
            return Vec::new();
        };
        self.mls_sessions()
            .into_iter()
            .filter(|s| s.grant.peer == own)
            .collect()
    }

    /// e2e m7: a dial of `peer`'s device came back, installed or not; counted for my devices.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) fn trace_own_dial(&mut self, peer: PeerId, result: &Result<(), CoreError>) {
        if self.local().map(|local| local.keys.peer_id()).ok() != Some(peer) {
            return;
        }
        let trace = &mut self.messages.mailbox.trace;
        match result {
            Ok(()) => trace.dials_ok += 1,
            Err(error) => {
                trace.dials_failed += 1;
                trace.dial_error = Some(error.code);
                trace.dial_check = auth::AuthCheck::of(error).map(auth::AuthCheck::name);
            }
        }
    }

    /// e2e m7: a connection that came in was installed or refused; counted when its endpoint is
    /// one of my other devices.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(super) fn trace_own_accept(
        &mut self,
        conn: &iroh::endpoint::Connection,
        result: &Result<(), CoreError>,
    ) {
        let remote = *conn.remote_id().as_bytes();
        let own = self
            .local()
            .ok()
            .and_then(|local| local.records.accepted.as_ref())
            .is_some_and(|record| {
                record
                    .record
                    .devices
                    .iter()
                    .any(|d| d.transport_public_key == remote)
            });
        if !own {
            return;
        }
        let trace = &mut self.messages.mailbox.trace;
        match result {
            Ok(()) => trace.accepts_ok += 1,
            Err(error) => {
                trace.accepts_failed += 1;
                trace.accept_error = Some(error.code);
                trace.accept_check = auth::AuthCheck::of(error).map(auth::AuthCheck::name);
            }
        }
    }

    /// A session became ready (`install_grant`): with another own device, this device pulls its
    /// log, and that device (doing the same) needs no `Notify` for what is logged so far; it is
    /// told whether this device is a mailbox (spec 8.4); then what this device has not delivered
    /// is handed over if it is a mailbox (spec 7). With another account, what was handed over to
    /// this device for it is due at once.
    pub(super) fn mailbox_session_ready(&mut self, grant: &auth::AuthGrant) {
        let Ok(local) = self.local() else { return };
        if grant.installation == local.device.installation_id() {
            return;
        }
        if grant.peer != local.keys.peer_id() {
            self.messages.mailbox.woken.insert(grant.peer);
            return;
        }
        let now = self.schedule_now();
        let mailbox = &mut self.messages.mailbox;
        mailbox
            .notified
            .insert(grant.installation, (mailbox.head, now));
        mailbox.schedule(grant.installation);
        // A new session: that device is told again whether this one is a mailbox.
        mailbox.told.remove(&grant.installation);
        mailbox.seen.insert(grant.installation);
        mailbox.tell_at.remove(&grant.installation);
        mailbox.handoff_due = true;
    }

    /// This device's mailbox setting and uptime (spec 8.4).
    pub(super) async fn mailbox_state(&mut self) -> Result<MailboxStateView, CoreError> {
        let request = self.ready()?;
        let op = MailboxDbOp::Local {
            now: self.services.clock.wall_ms(),
        };
        match self.mailbox_db(op, request).await? {
            MailboxDbReply::Local {
                enabled,
                first_link_shown,
                uptime_7d_permille,
            } => Ok(MailboxStateView {
                enabled,
                first_link_shown,
                uptime_7d_permille,
            }),
            _ => Err(unavailable(ErrorCode::InvalidMessage)),
        }
    }

    /// Sets this device's mailbox role; my other devices are told at the next poll (one
    /// without a session once its session is ready).
    pub(super) async fn set_mailbox(&mut self, enabled: bool) -> Result<(), CoreError> {
        let request = self.ready()?;
        self.mailbox_db(MailboxDbOp::SetEnabled(enabled), request)
            .await?;
        let mailbox = &mut self.messages.mailbox;
        mailbox.enabled = Some(enabled);
        // Up from now on: recorded at the next poll, and qualified (or not) again with it.
        mailbox.uptime_at = 0;
        mailbox.judge_at = 0;
        Ok(())
    }

    /// The server queue setting, the verdict and this device's uptime (M8 spec 5). The verdict
    /// is the one last worked out (worked out now if none is yet).
    pub(super) async fn queue_state(&mut self) -> Result<QueueStateView, CoreError> {
        let request = self.ready()?;
        let (verdict, facts) = match self.messages.mailbox.verdict {
            Some(verdict) => (verdict, self.queue_facts(None, request).await?),
            None => self.judge(request).await?,
        };
        Ok(QueueStateView {
            mode: facts.mode,
            verdict,
            using_queue: self.queue_using(request).await?,
            uptime_7d_permille: facts.uptime_7d_permille,
        })
    }

    /// The verdict as last worked out (`None`: not yet since it last had to be worked out
    /// again), for the queues (`manager/queue.rs`) to follow.
    pub(super) fn mailbox_verdict(&self) -> Option<Verdict> {
        self.messages.mailbox.verdict
    }

    /// Sets the server queue setting (M8 spec 5.3). The verdict does not depend on it; the
    /// queues follow it at the next poll.
    pub(super) async fn set_queue_mode(&mut self, mode: QueueMode) -> Result<(), CoreError> {
        let request = self.ready()?;
        self.mailbox_db(MailboxDbOp::SetQueueMode(mode), request)
            .await?;
        self.queue_changed();
        Ok(())
    }

    async fn queue_facts(
        &mut self,
        hourly: Option<Vec<InstallationId>>,
        request: Request,
    ) -> Result<QueueFacts, CoreError> {
        let op = MailboxDbOp::QueueFacts {
            now: self.services.clock.wall_ms(),
            hourly,
        };
        match self.mailbox_db(op, request).await? {
            MailboxDbReply::Queue(facts) => Ok(facts),
            _ => Err(unavailable(ErrorCode::InvalidMessage)),
        }
    }

    /// Works the verdict out (spec 5.1) from the stored facts, and first this device's
    /// qualification when it is due (`judge_at`: hourly, and after the role changed), when my
    /// devices with a ready session are also seen now (spec 5.1 rule 3). The result is stored;
    /// a changed qualification is announced at the next poll (`told`).
    async fn judge(&mut self, request: Request) -> Result<(Verdict, QueueFacts), CoreError> {
        let me = self.local()?.device.installation_id();
        let now = self.schedule_now();
        let wall = self.services.clock.wall_ms();
        let requalify = now >= self.messages.mailbox.judge_at;
        let hourly = requalify.then(|| {
            self.own_sessions()
                .iter()
                .map(|s| s.grant.installation)
                .collect()
        });
        let facts = self.queue_facts(hourly, request).await?;
        let qualified = if requalify {
            facts.enabled && qualifies(facts.uptime_7d_permille, facts.recorded_ms)
        } else {
            facts.qualified
        };
        let mine = MailboxFact {
            installation: me,
            mailbox: facts.enabled,
            qualified,
            last_seen_ms: None,
        };
        let verdict = verdict::verdict(&mine, &facts.devices, wall);
        let op = MailboxDbOp::Judged {
            qualified: requalify.then_some(qualified),
            at: wall,
        };
        self.mailbox_db(op, request).await?;
        let mailbox = &mut self.messages.mailbox;
        mailbox.qualified = Some(qualified);
        mailbox.verdict = Some(verdict);
        if requalify {
            mailbox.judge_at = now.saturating_add(VERDICT_EVERY_MS);
        }
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            mailbox.verdicts += 1;
        }
        Ok((verdict, facts))
    }

    pub(super) async fn mark_first_link_shown(&mut self) -> Result<(), CoreError> {
        let request = self.ready()?;
        self.mailbox_db(MailboxDbOp::FirstLinkShown, request)
            .await
            .map(|_| ())
    }

    /// Spec 8.4: tells another own device whether this one is a mailbox, with the own-device
    /// push handle (M7 spec 5.2), in a spawned task; its answer comes back as
    /// `Step::Announced`. Never a contact: the role is for my devices to know.
    fn announce(&mut self, session: Session, told: Told, ticket: u64) {
        if self.local().map(|local| local.keys.peer_id()).ok() != Some(session.grant.peer) {
            return;
        }
        let installation = session.grant.installation;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.messages
            .mailbox
            .announces
            .push((installation, told.0, told.2));
        self.messages.mailbox.telling.insert(installation);
        let body = MailboxFrame::Announce {
            mailbox: told.0,
            handle: told.1.clone(),
            qualified: told.2,
        }
        .encode();
        let mailbox = self.auth.send.clone();
        tokio::spawn(async move {
            let answer = exchange(&session, &body).await;
            let _ = mailbox
                .send(Input::Mailbox(MailboxEvent {
                    ticket,
                    holder: installation,
                    conn: session.conn.stable_id(),
                    step: Step::Announced { told, answer },
                }))
                .await;
        });
    }

    /// My other device's answer to an `Announce`: taken (`Ok`, or `Refused`: not mine any more;
    /// not sent again over this session until it changes), otherwise told again after a growing
    /// back-off (`tell_later`). An M6 device answers `Busy` to the four-field form it cannot
    /// decode, so it is asked at most every `ANNOUNCE_RETRY_MAX_MS` while the session lasts.
    fn announce_answered(
        &mut self,
        ticket: u64,
        installation: InstallationId,
        told: Told,
        answer: Result<MailboxReply, ErrorCode>,
    ) {
        self.messages.mailbox.telling.remove(&installation);
        if !self.ready().is_ok_and(|r| r.ticket == ticket) {
            return;
        }
        let now = self.schedule_now();
        let mailbox = &mut self.messages.mailbox;
        match answer {
            Ok(MailboxReply::Ok | MailboxReply::Refused) => {
                mailbox.tell_at.remove(&installation);
                mailbox.told.insert(installation, told);
            }
            _ => mailbox.tell_later(installation, told, now),
        }
    }

    /// The app goes to the background or exits (spec 7): hands what this device has not
    /// delivered to its mailbox devices now. Returns the rows handed over.
    pub(super) async fn mailbox_background(&mut self) -> Result<u32, CoreError> {
        self.restrict_grants();
        let request = self.ready()?;
        if !self.local_authorized() {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        self.hand_off(request).await
    }

    /// Spec 7: the sealed ciphertexts of this device's undelivered messages go to each mailbox
    /// device of this account with a ready session (Ruling 12: no other device), rows awaiting
    /// encryption sealed first. The sends run in a spawned task (Ruling 17) and their answers
    /// change nothing here: this device goes on retrying too. Returns the rows handed over (0
    /// without a mailbox session).
    async fn hand_off(&mut self, request: Request) -> Result<u32, CoreError> {
        let MailboxDbReply::Installations(mailboxes) =
            self.mailbox_db(MailboxDbOp::Mailboxes, request).await?
        else {
            return Err(unavailable(ErrorCode::InvalidMessage));
        };
        let stopped = &self.messages.mailbox.stopped;
        let targets: Vec<Session> = self
            .own_sessions()
            .into_iter()
            .filter(|s| {
                mailboxes.contains(&s.grant.installation)
                    && !stopped.contains_key(&s.conn.stable_id())
            })
            .collect();
        if targets.is_empty() {
            return Ok(0);
        }
        let MailboxDbReply::Conversations(unsealed) =
            self.mailbox_db(MailboxDbOp::Unsealed, request).await?
        else {
            return Err(unavailable(ErrorCode::InvalidMessage));
        };
        let now = self.schedule_now();
        for conversation in unsealed {
            // Without a group (or room in the queue) a row stays unsealed and is not handed over.
            let _ = self
                .message_db(MessageDbOp::SealPending(conversation, now), request)
                .await;
        }
        let MailboxDbReply::Handoff(mut entries) =
            self.mailbox_db(MailboxDbOp::HandoffRows, request).await?
        else {
            return Err(unavailable(ErrorCode::InvalidMessage));
        };
        // M7: a text handed over before its first attempt had a result is pushed now; the
        // mailbox never pushes (spec 6.1).
        self.push_handed_off(&entries, request).await;
        // M7: the queued `PushHandle` frames go along (one per conversation and frame).
        if let MessageDbReply::Push(crate::message::push_store::PushDbReply::Handoff(push)) = self
            .message_db(
                MessageDbOp::Push(crate::message::push_store::PushDbOp::HandOff),
                request,
            )
            .await?
        {
            entries.extend(push);
        }
        let batches = handoff_batches(entries);
        let handed: usize = batches.iter().map(Vec::len).sum();
        let frames: Vec<_> = batches
            .into_iter()
            .map(|entries| MailboxFrame::Handoff { entries }.encode())
            .collect();
        for session in targets {
            let frames = frames.clone();
            tokio::spawn(async move {
                for body in frames {
                    if !matches!(exchange(&session, &body).await, Ok(MailboxReply::Ok)) {
                        break;
                    }
                }
            });
        }
        Ok(handed as u32)
    }

    /// The app came to the foreground: pulls from every connected own device, and dials each
    /// one without a session (not revoked); a dial that succeeds pulls when its session is ready.
    /// The dials run in spawned tasks (`start_dial`, Ruling 17): an own device that is off must
    /// not hold this actor for a dial timeout.
    pub(super) async fn mailbox_sync_now(&mut self) -> Result<(), CoreError> {
        self.restrict_grants();
        let request = self.ready()?;
        if !self.local_authorized() {
            return Err(unavailable(ErrorCode::AccountRestricted));
        }
        let local = self.local()?;
        let own = local.keys.peer_id();
        let record = local
            .records
            .accepted
            .clone()
            .ok_or_else(|| unavailable(ErrorCode::AccountRestricted))?;
        let unconnected: Vec<InstallationId> = record
            .record
            .devices
            .iter()
            .map(|d| d.installation_id)
            .filter(|i| {
                self.dial_target(own, &record, *i).is_some() && !self.ready_session(own, *i)
            })
            .collect();
        let MailboxDbReply::Installations(mailboxes) =
            self.mailbox_db(MailboxDbOp::Mailboxes, request).await?
        else {
            return Err(unavailable(ErrorCode::InvalidMessage));
        };
        // Spec 8.4: my mailboxes first (they are the ones likely up), the rest in record order.
        let mut unconnected = unconnected;
        unconnected.sort_by_key(|i| !mailboxes.contains(i));
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            self.messages.mailbox.sync_dials = unconnected.clone();
        }
        for installation in unconnected {
            self.start_dial(own, installation, request);
        }
        for session in self.own_sessions() {
            self.messages.mailbox.schedule(session.grant.installation);
        }
        Ok(())
    }

    /// A kind 18 request. Only a ready session with a current, other device of this account is
    /// served; any other gets `Refused`, and nothing is read or written for it.
    pub(super) async fn receive_mailbox(
        &mut self,
        conn: &iroh::endpoint::Connection,
        inner: &[u8],
        request: Request,
    ) -> Result<Vec<u8>, CoreError> {
        let Some(installation) = self.mailbox_peer(conn) else {
            return Ok(MailboxReply::Refused.encode().to_vec());
        };
        if self.messages.mailbox.held() {
            return Ok(MailboxReply::Busy.encode().to_vec());
        }
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if self.messages.mailbox.refuse_pulls > 0
            && matches!(MailboxFrame::decode(inner), Ok(MailboxFrame::SyncReq { .. }))
        {
            self.messages.mailbox.refuse_pulls -= 1;
            return Ok(MailboxReply::Refused.encode().to_vec());
        }
        let reply = match MailboxFrame::decode(inner) {
            Ok(MailboxFrame::SyncReq { since_seq }) => {
                let op = MailboxDbOp::Page {
                    puller: installation,
                    since_seq,
                    now: self.services.clock.wall_ms(),
                };
                match self.mailbox_db(op, request).await {
                    Ok(MailboxDbReply::Page(page)) => MailboxReply::SyncResp {
                        entries: page.entries,
                        next_seq: page.next_seq,
                        more: page.more,
                    },
                    Ok(_) => MailboxReply::Busy,
                    Err(error) if fatal(&error) => return Err(error),
                    Err(_) => MailboxReply::Busy,
                }
            }
            Ok(MailboxFrame::Notify { .. }) => {
                self.messages.mailbox.schedule(installation);
                MailboxReply::Ok
            }
            // Spec 6.3: a plaintext record leaves this device only for a current device of
            // this account, over its authenticated session; checked again right before the
            // read, so a revoke applied since the request came in is never passed.
            Ok(MailboxFrame::PlainReq { seq }) => {
                if self.mailbox_peer(conn) != Some(installation) {
                    return Ok(MailboxReply::Refused.encode().to_vec());
                }
                match self.mailbox_db(MailboxDbOp::Plain { seq }, request).await {
                    Ok(MailboxDbReply::Plain(Some(record))) => MailboxReply::Plain(record),
                    Ok(MailboxDbReply::Plain(None)) => MailboxReply::NoPlain,
                    Ok(_) => MailboxReply::Busy,
                    Err(error) if fatal(&error) => return Err(error),
                    Err(_) => MailboxReply::Busy,
                }
            }
            // Spec 7: ciphertexts this device retries, as they are, for the device that sent
            // them.
            Ok(MailboxFrame::Handoff { entries }) => {
                let op = MailboxDbOp::TakeHandoff {
                    origin: installation,
                    entries,
                    now: self.schedule_now(),
                };
                match self.mailbox_db(op, request).await {
                    // Not a mailbox (any more): `Busy`, not `Refused`, which would stop all sync
                    // with a device that is still mine. Its `Announce { mailbox: false }` (sent on
                    // the change and on every session) is what stops handoffs to it.
                    Ok(MailboxDbReply::Disabled) => MailboxReply::Busy,
                    Ok(_) => {
                        self.messages.mailbox.handoff_at = None;
                        MailboxReply::Ok
                    }
                    Err(error) if fatal(&error) => return Err(error),
                    Err(_) => MailboxReply::Busy,
                }
            }
            // Spec 8.4: whether that device is a mailbox, recorded for handoffs, dialing order
            // and `mailbox_ok`; M8 spec 5.1: whether it qualifies, and that it was seen now.
            Ok(MailboxFrame::Announce {
                mailbox,
                handle,
                qualified,
            }) => {
                let at = self.services.clock.wall_ms();
                let op = MailboxDbOp::Announced {
                    installation,
                    mailbox,
                    handle,
                    qualified,
                    at,
                };
                match self.mailbox_db(op, request).await {
                    Ok(MailboxDbReply::Fact(before)) => {
                        let after = MailboxFact {
                            installation,
                            mailbox,
                            qualified,
                            last_seen_ms: Some(at),
                        };
                        let runtime = &mut self.messages.mailbox;
                        runtime.mailboxes = None;
                        // A device that just became a mailbox gets what is not delivered.
                        if mailbox && !before.is_some_and(|b| b.mailbox) {
                            runtime.handoff_due = true;
                        }
                        // The same `Announce` again changes nothing the verdict is made of.
                        if changes_verdict(before.as_ref(), &after, at) {
                            runtime.verdict = None;
                        }
                        MailboxReply::Ok
                    }
                    Ok(_) => MailboxReply::Busy,
                    Err(error) if fatal(&error) => return Err(error),
                    Err(_) => MailboxReply::Busy,
                }
            }
            // Anything undecodable: the asker tries later.
            Err(_) => MailboxReply::Busy,
        };
        // The device may have been revoked while the page was read: it gets nothing.
        if self.mailbox_peer(conn) != Some(installation) {
            return Ok(MailboxReply::Refused.encode().to_vec());
        }
        Ok(reply.encode().to_vec())
    }

    /// The installation of `conn` if it is a ready session with a current device of this account
    /// other than this one (global constraint: the only devices a log goes to).
    fn mailbox_peer(&self, conn: &iroh::endpoint::Connection) -> Option<InstallationId> {
        let local = self.local().ok()?;
        let session = self
            .auth
            .registry
            .0
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.conn.stable_id() == conn.stable_id())
            .cloned()?;
        (session.ready
            && conn.close_reason().is_none()
            && session.grant.peer == local.keys.peer_id()
            && session.grant.installation != local.device.installation_id()
            && self.grant_authorized(&session.grant))
        .then_some(session.grant.installation)
    }

    /// One tick of own-device sync, with the message scheduler's poll: the device list and
    /// retention of the log, the mailbox role (uptime, `Announce` to newly ready own devices,
    /// `mailbox_ok`), due pulls, and `Notify` to devices behind this log's head.
    pub(super) async fn poll_mailbox(&mut self) {
        let (Ok(request), Ok(local)) = (self.ready(), self.local()) else {
            self.bus.set_mailbox_ok(false);
            return;
        };
        if self.messages.mailbox.halted() || self.messages.mailbox.held() {
            return;
        }
        // New devices get a cursor at the head, revoked ones lose theirs (also right after
        // the account opens: `devices` starts empty).
        if let Some(others) = other_devices(local)
            && self.messages.mailbox.devices.as_ref() != Some(&others)
        {
            let op = MailboxDbOp::Devices {
                others: others.clone(),
                now: self.services.clock.wall_ms(),
            };
            if self.mailbox_db(op, request).await.is_err() {
                return;
            }
            let mailbox = &mut self.messages.mailbox;
            mailbox.devices = Some(others);
            // Revoked devices' announcements went with them, and with them what they counted
            // for in the verdict.
            mailbox.mailboxes = None;
            mailbox.verdict = None;
        }
        let now = self.schedule_now();
        if now >= self.messages.mailbox.prune_at {
            let op = MailboxDbOp::Prune(self.services.clock.wall_ms());
            if self.mailbox_db(op, request).await.is_err() {
                return;
            }
            self.messages.mailbox.prune_at = now.saturating_add(PRUNE_EVERY_MS);
        }
        if self.mailbox_role(request).await.is_err() {
            return;
        }
        let sessions = self.own_sessions();
        let mailbox = &mut self.messages.mailbox;
        let Runtime { stopped, due, .. } = mailbox;
        // A stopped session whose wait is over is synced over again: its device is due.
        stopped.retain(|id, (installation, until)| {
            if !sessions.iter().any(|s| s.conn.stable_id() == *id) {
                return false;
            }
            if now >= *until {
                due.insert(*installation);
                return false;
            }
            true
        });
        let usable: Vec<Session> = sessions
            .into_iter()
            .filter(|s| !mailbox.stopped.contains_key(&s.conn.stable_id()))
            .collect();
        let ok = mailbox
            .mailboxes
            .as_ref()
            .is_some_and(|m| usable.iter().any(|s| m.contains(&s.grant.installation)));
        let enabled = mailbox.enabled.unwrap_or(false);
        let qualified = mailbox.qualified;
        let want = (
            enabled,
            self.push_own_handle(),
            qualified.unwrap_or(false),
        );
        // Before `push_local` is read the own-device handle is not known: an `Announce` then
        // would say "none" and my other device would drop the handle it keeps. Nor is it sent
        // before the qualification is worked out.
        let loaded = self.push_loaded() && qualified.is_some();
        let now_tell = self.schedule_now();
        let mailbox = &self.messages.mailbox;
        let announce: Vec<Session> = usable
            .iter()
            .filter(|s| {
                let i = &s.grant.installation;
                loaded
                    && !mailbox.telling.contains(i)
                    && mailbox.told.get(i) != Some(&want)
                    && mailbox
                        .tell_at
                        .get(i)
                        .is_none_or(|(at, _, tried)| now_tell >= *at || *tried != want)
            })
            .cloned()
            .collect();
        self.bus.set_mailbox_ok(ok);
        for session in announce {
            self.announce(session, want.clone(), request.ticket);
        }
        let mailbox = &mut self.messages.mailbox;
        // Failed pulls whose back-off is over are due again.
        for (installation, (at, _)) in mailbox.retry.iter_mut() {
            if now >= *at {
                *at = u64::MAX;
                mailbox.due.insert(*installation);
            }
        }
        for installation in std::mem::take(&mut mailbox.due) {
            if let Some(session) = usable.iter().find(|s| s.grant.installation == installation) {
                // A device without a session pulls when one is ready again.
                let session = session.clone();
                if self.pull_from(session, request).await.is_err() {
                    // The other due devices are still pulled from.
                    self.messages.mailbox.back_off(installation, now);
                }
            }
        }
        // Ruling 35: what an earlier run stored from a log and did not notify before it died,
        // for holders no pull runs from now (a running one notifies it when it ends). A queue
        // fetch on its way runs under this device's own installation.
        let fetching = self
            .queue_fetching()
            .then(|| self.local().map(|l| l.device.installation_id()).ok())
            .flatten();
        let mailbox = &mut self.messages.mailbox;
        let at = *mailbox
            .restart_notify_at
            .get_or_insert(now.saturating_add(RESTART_NOTIFY_MS));
        if now >= at {
            let op = MailboxDbOp::TakeNotify {
                holder: None,
                running: mailbox.pulls.keys().copied().chain(fetching).collect(),
                read: Vec::new(),
            };
            if let Ok(MailboxDbReply::Notify(items)) = self.mailbox_db(op, request).await {
                self.messages.mailbox.restart_notify_at = Some(u64::MAX);
                self.publish_notify(items);
            }
        }
        let mailbox = &mut self.messages.mailbox;
        let head = mailbox.head;
        for session in &usable {
            let installation = session.grant.installation;
            let (told, at) = mailbox
                .notified
                .get(&installation)
                .copied()
                .unwrap_or_default();
            if head <= told || now < at.saturating_add(NOTIFY_GAP_MS) {
                continue;
            }
            mailbox.notified.insert(installation, (head, now));
            let body = MailboxFrame::Notify { head_seq: head }.encode();
            let session = session.clone();
            // Nothing to do with the answer: the device pulls, or it pulls when it can.
            tokio::spawn(async move {
                let _ = exchange(&session, &body).await;
            });
        }
        let mailbox = &mut self.messages.mailbox;
        if mailbox.handoff_due && mailbox.pulls.is_empty() && mailbox.due.is_empty() {
            mailbox.handoff_due = false;
            let _ = self.hand_off(request).await;
        }
        self.poll_handoffs(request).await;
    }

    /// The mailbox role's upkeep (spec 8.4): this device's setting and my mailboxes are read
    /// when not known, and while this device is a mailbox it records every `UPTIME_EVERY_MS`
    /// that it is up. Then the verdict (M8 spec 5.1): my devices seen over a new session are
    /// recorded as seen now, and the verdict is worked out when it is due.
    async fn mailbox_role(&mut self, request: Request) -> Result<(), CoreError> {
        let wall = self.services.clock.wall_ms();
        if self.messages.mailbox.enabled.is_none() {
            let MailboxDbReply::Local { enabled, .. } = self
                .mailbox_db(MailboxDbOp::Local { now: wall }, request)
                .await?
            else {
                return Err(unavailable(ErrorCode::InvalidMessage));
            };
            self.messages.mailbox.enabled = Some(enabled);
        }
        if self.messages.mailbox.mailboxes.is_none() {
            let MailboxDbReply::Installations(mailboxes) =
                self.mailbox_db(MailboxDbOp::Mailboxes, request).await?
            else {
                return Err(unavailable(ErrorCode::InvalidMessage));
            };
            self.messages.mailbox.mailboxes = Some(mailboxes.into_iter().collect());
        }
        let now = self.schedule_now();
        let mailbox = &self.messages.mailbox;
        if mailbox.enabled == Some(true) && now >= mailbox.uptime_at {
            self.mailbox_db(MailboxDbOp::Uptime { now: wall }, request)
                .await?;
            self.messages.mailbox.uptime_at = now.saturating_add(UPTIME_EVERY_MS);
        }
        while let Some(installation) = self.messages.mailbox.seen.first().copied() {
            let op = MailboxDbOp::Seen {
                installation,
                at: wall,
            };
            let MailboxDbReply::Fact(before) = self.mailbox_db(op, request).await? else {
                return Err(unavailable(ErrorCode::InvalidMessage));
            };
            let mailbox = &mut self.messages.mailbox;
            mailbox.seen.remove(&installation);
            if let Some(before) = before {
                let after = MailboxFact {
                    last_seen_ms: Some(wall),
                    ..before
                };
                if changes_verdict(Some(&before), &after, wall) {
                    mailbox.verdict = None;
                }
            }
        }
        let mailbox = &self.messages.mailbox;
        if mailbox.verdict.is_none() || now >= mailbox.judge_at {
            self.judge(request).await?;
        }
        Ok(())
    }

    /// Spec 7, as a mailbox: each due handed-off ciphertext is sent as it is (kind 17 `App`) to
    /// its peer over a ready session, one attempt per ciphertext at a time, in a spawned task
    /// (Ruling 17). With no session the attempt counts as failed and the peer's devices are
    /// dialed (`start_dial`). The table is read only when something may be due.
    async fn poll_handoffs(&mut self, request: Request) {
        let now = self.schedule_now();
        let woken = std::mem::take(&mut self.messages.mailbox.woken);
        if self.messages.mailbox.handoff_at != Some(u64::MAX) {
            for peer in woken {
                let op = MailboxDbOp::HandoffWake { peer, now };
                if self.mailbox_db(op, request).await.is_err() {
                    return;
                }
                self.messages.mailbox.handoff_at = None;
            }
        }
        if self.messages.mailbox.handoff_at.is_some_and(|at| now < at) {
            return;
        }
        let Ok(MailboxDbReply::HandoffDue { jobs, next }) =
            self.mailbox_db(MailboxDbOp::HandoffDue(now), request).await
        else {
            return;
        };
        self.messages.mailbox.handoff_at = Some(next.unwrap_or(u64::MAX));
        for (origin, entry) in jobs {
            let key = (origin, entry.conversation, entry.msg_id);
            if self.messages.mailbox.sending.contains(&key) {
                continue;
            }
            let ct_hash: [u8; 32] = Sha256::digest(&entry.ciphertext).into();
            let session = self
                .mls_sessions()
                .into_iter()
                .filter(|s| s.grant.peer == entry.peer)
                .min_by_key(|s| self.outdated_now(s.grant.installation));
            let Some(session) = session else {
                self.dial_peer(entry.peer, now, request);
                // M8 spec 8.2: no device of the peer reached, so into their queues.
                self.queue_handed_off(key, entry.peer, &[], request).await;
                let op = MailboxDbOp::HandoffResult {
                    origin,
                    conversation: entry.conversation,
                    msg_id: entry.msg_id,
                    ct_hash,
                    outcome: HandoffOutcome::Retry,
                    now,
                    wall: self.services.clock.wall_ms(),
                };
                // Backed off: when the next one is due is read again.
                self.messages.mailbox.handoff_at = None;
                if self.mailbox_db(op, request).await.is_err() {
                    return;
                }
                continue;
            };
            self.send_handoff(key, entry, ct_hash, session, request);
        }
    }

    fn send_handoff(
        &mut self,
        key: HandoffKey,
        entry: HandoffEntry,
        ct_hash: [u8; 32],
        session: Session,
        request: Request,
    ) {
        self.messages.mailbox.sending.insert(key);
        let body = MlsFrame::App {
            conversation: entry.conversation,
            ciphertext: entry.ciphertext,
        }
        .encode();
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            let now = self.schedule_now();
            self.messages.mailbox.handoff_sends.push((now, ct_hash));
            self.messages.mls.frames.lock().unwrap().push(body.to_vec());
        }
        let mailbox = self.auth.send.clone();
        let ticket = request.ticket;
        tokio::spawn(async move {
            let answer = super::mls::exchange(&session, &body).await;
            let _ = mailbox
                .send(Input::Mailbox(MailboxEvent {
                    ticket,
                    holder: session.grant.installation,
                    conn: session.conn.stable_id(),
                    step: Step::Handoff {
                        key,
                        ct_hash,
                        grant: session.grant.clone(),
                        answer,
                    },
                }))
                .await;
        });
    }

    /// A text handed off to this mailbox goes into the queues of the devices of `peer` its
    /// attempt did not reach, as its origin's own attempts put it (M8 spec 8.2).
    async fn queue_handed_off(
        &mut self,
        (origin, conversation, msg_id): HandoffKey,
        peer: PeerId,
        reached: &[InstallationId],
        request: Request,
    ) {
        let Ok(local) = self.local() else {
            return;
        };
        let key = MessageKey {
            conversation,
            sender: local.keys.peer_id(),
            installation: origin,
            id: msg_id,
        };
        self.queue_put(key, peer, reached, request).await;
    }

    /// Dials the devices of `peer` (as its record here lists them) that have no session, unless
    /// that was done within `HANDOFF_DIAL_GAP_MS`; a session that comes up makes what was
    /// handed over for the peer due.
    fn dial_peer(&mut self, peer: PeerId, now: u64, request: Request) {
        let dialed = &mut self.messages.mailbox.dialed;
        if dialed
            .get(&peer)
            .is_some_and(|at| now < at.saturating_add(HANDOFF_DIAL_GAP_MS))
        {
            return;
        }
        dialed.insert(peer, now);
        let Some(record) = self.local().ok().and_then(|local| {
            local
                .remote
                .iter()
                .find(|r| r.peer == peer)
                .map(|r| r.accepted.clone())
        }) else {
            return;
        };
        let targets: Vec<InstallationId> = record
            .record
            .devices
            .iter()
            .map(|d| d.installation_id)
            .filter(|i| {
                self.dial_target(peer, &record, *i).is_some() && !self.ready_session(peer, *i)
            })
            .collect();
        for installation in targets {
            self.start_dial(peer, installation, request);
        }
    }

    /// How an attempt of a handed-off ciphertext ended (spec 7). Only an answer over a session
    /// still authorized speaks for the peer.
    async fn handoff_answered(
        &mut self,
        ticket: u64,
        (origin, conversation, msg_id): HandoffKey,
        ct_hash: [u8; 32],
        grant: auth::AuthGrant,
        answer: Result<MlsReply, ErrorCode>,
    ) {
        let mailbox = &mut self.messages.mailbox;
        mailbox.sending.remove(&(origin, conversation, msg_id));
        mailbox.handoff_at = None;
        let Some(request) = self.ready().ok().filter(|r| r.ticket == ticket) else {
            return;
        };
        let outcome = match answer {
            Ok(MlsReply::Ack { ct_hash: acked })
                if acked == ct_hash && self.grant_authorized(&grant) =>
            {
                HandoffOutcome::Acked {
                    receiver: grant.installation,
                }
            }
            Ok(MlsReply::DecryptFailed { class, epoch }) if self.grant_authorized(&grant) => {
                HandoffOutcome::Stop { class, epoch }
            }
            _ => HandoffOutcome::Retry,
        };
        // M8 spec 8.2: the peer's other devices get it in their queues, read before the result
        // is stored (an acknowledged text's row goes with it).
        let reached = match outcome {
            HandoffOutcome::Acked { receiver } => Some(vec![receiver]),
            HandoffOutcome::Retry => Some(Vec::new()),
            HandoffOutcome::Stop { .. } => None,
        };
        if let Some(reached) = reached {
            let key = (origin, conversation, msg_id);
            self.queue_handed_off(key, grant.peer, &reached, request)
                .await;
        }
        let op = MailboxDbOp::HandoffResult {
            origin,
            conversation,
            msg_id,
            ct_hash,
            outcome,
            now: self.schedule_now(),
            wall: self.services.clock.wall_ms(),
        };
        let _ = self.mailbox_db(op, request).await;
    }

    /// Ruling 1: a mailbox's `HandoffStop` about a message this device sent. Its peer could not
    /// open the ciphertext handed over, which is taken as this device's own `DecryptFailed`
    /// (M5 rules: re-encrypted here, counted against the cap of 3); the new ciphertext goes to
    /// the mailbox with the next handoff.
    async fn handoff_stopped(
        &mut self,
        conversation: ConversationId,
        msg_id: MessageId,
        (class, epoch): (u8, u64),
        request: Request,
    ) -> Result<(), CoreError> {
        let local = self.local()?;
        let own = local.keys.peer_id();
        let key = MessageKey {
            conversation,
            sender: own,
            installation: local.device.installation_id(),
            id: msg_id,
        };
        let Some(peer) = self.conversation_peer(conversation, own) else {
            return Ok(());
        };
        match self
            .after_decrypt_failed(key, peer, class, epoch, request)
            .await
        {
            Err(error) if fatal(&error) => Err(error),
            _ => Ok(()),
        }
    }

    /// Starts a pull of `holder`'s log from where this device stopped.
    pub(super) async fn pull_from(
        &mut self,
        holder: Session,
        request: Request,
    ) -> Result<(), CoreError> {
        let installation = holder.grant.installation;
        if self.messages.mailbox.pulls.contains_key(&installation) {
            self.messages.mailbox.schedule(installation);
            return Ok(());
        }
        let MailboxDbReply::Since(since) = self
            .mailbox_db(MailboxDbOp::PullFrom(installation), request)
            .await?
        else {
            return Err(unavailable(ErrorCode::InvalidMessage));
        };
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            self.messages.mailbox.trace.pulls += 1;
        }
        self.messages.mailbox.pulls.insert(
            installation,
            Pull {
                conn: holder.conn.stable_id(),
                read: BTreeSet::new(),
                again: false,
            },
        );
        self.request_page(&holder, since, request);
        Ok(())
    }

    fn request_page(&mut self, holder: &Session, since: u64, request: Request) {
        let body = MailboxFrame::SyncReq { since_seq: since }.encode();
        let mailbox = self.auth.send.clone();
        let session = holder.clone();
        let ticket = request.ticket;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        let fail = {
            let runtime = &mut self.messages.mailbox;
            let fail = runtime.fail_pulls > 0;
            runtime.fail_pulls = runtime.fail_pulls.saturating_sub(1);
            fail
        };
        #[cfg(not(all(feature = "test-hooks", debug_assertions)))]
        let fail = false;
        tokio::spawn(async move {
            let answer = if fail {
                Err(ErrorCode::NetworkTimeout)
            } else {
                exchange(&session, &body).await
            };
            let _ = mailbox
                .send(Input::Mailbox(MailboxEvent {
                    ticket,
                    holder: session.grant.installation,
                    conn: session.conn.stable_id(),
                    step: Step::Page { since, answer },
                }))
                .await;
        });
    }

    /// A page (or the failure) of a running pull came back, or the plaintext records for what
    /// it could not open.
    pub(super) async fn mailbox_event(&mut self, event: MailboxEvent) {
        let MailboxEvent {
            ticket,
            holder,
            conn,
            step,
        } = event;
        let step = match step {
            Step::Announced { told, answer } => {
                self.announce_answered(ticket, holder, told, answer);
                return;
            }
            Step::Handoff {
                key,
                ct_hash,
                grant,
                answer,
            } => {
                self.handoff_answered(ticket, key, ct_hash, grant, answer)
                    .await;
                return;
            }
            step => step,
        };
        // Only the answer of the pull running over that session counts.
        if self
            .messages
            .mailbox
            .pulls
            .get(&holder)
            .is_none_or(|pull| pull.conn != conn)
        {
            return;
        }
        let Some(request) = self.ready().ok().filter(|r| r.ticket == ticket) else {
            self.messages.mailbox.pulls.remove(&holder);
            return;
        };
        let (next_seq, more) = match step {
            Step::Page { since, answer } => {
                let (entries, next_seq, more) = match answer {
                    Ok(MailboxReply::SyncResp {
                        entries,
                        next_seq,
                        more,
                    }) if page_in_order(&entries, since, next_seq, more) => {
                        (entries, next_seq, more)
                    }
                    // Not now: asked again after a back-off.
                    Ok(MailboxReply::Busy) => {
                        self.pull_failed(holder, "busy", request).await;
                        return;
                    }
                    Err(_) => {
                        self.pull_failed(holder, "request", request).await;
                        return;
                    }
                    // `Refused`, or a page out of order: no syncing over this session for now.
                    Ok(_) => {
                        let now = self.schedule_now();
                        self.messages.mailbox.stop(conn, holder, now);
                        self.end_pull(holder, Ended::Stopped, request).await;
                        return;
                    }
                };
                let summary = self.pull_page(holder, entries, request).await;
                if self.messages.mailbox.halted() {
                    self.messages.mailbox.pulls.remove(&holder);
                    return;
                }
                let Ok(summary) = summary else {
                    self.pull_failed(holder, "entries", request).await;
                    return;
                };
                if !summary.unopened.is_empty() {
                    let page = Resolving {
                        next_seq,
                        more,
                        unopened: summary.unopened,
                    };
                    self.resolve_unopened(holder, conn, page, request).await;
                    return;
                }
                (next_seq, more)
            }
            Step::Plain { page, answers } => {
                if !self
                    .take_plain(holder, conn, &page.unopened, answers, request)
                    .await
                {
                    return;
                }
                (page.next_seq, page.more)
            }
            Step::Handoff { .. } | Step::Announced { .. } => unreachable!("taken above"),
        };
        let pulled = MailboxDbOp::Pulled { holder, next_seq };
        if self.mailbox_db(pulled, request).await.is_err() {
            self.pull_failed(holder, "storage", request).await;
            return;
        }
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            let mailbox = &mut self.messages.mailbox;
            mailbox.pages += 1;
            if mailbox.halt_after_pages.is_some_and(|n| mailbox.pages >= n) {
                mailbox.halted = true;
                mailbox.pulls.remove(&holder);
                return;
            }
        }
        let session = self
            .own_sessions()
            .into_iter()
            .find(|s| s.conn.stable_id() == conn);
        match session {
            Some(session) if more => self.request_page(&session, next_seq, request),
            // The rest of the log over another session, if one comes.
            None if more => self.pull_failed(holder, "session_gone", request).await,
            _ => self.end_pull(holder, Ended::Done, request).await,
        }
    }

    /// Processes one page's entries in `seq` order. `Err` when an entry could not be processed
    /// for now (storage, authority): the page is pulled again later.
    pub(super) async fn pull_page(
        &mut self,
        holder: InstallationId,
        entries: Vec<LogEntry>,
        request: Request,
    ) -> Result<PullSummary, CoreError> {
        let own = self.local()?.device.installation_id();
        let mut unopened = Vec::new();
        for entry in entries {
            if self.messages.mailbox.halted() {
                break;
            }
            if let Entry::Unopened(entry) = self.pull_entry(holder, entry, own, request).await? {
                unopened.push(entry);
            }
        }
        Ok(PullSummary { unopened })
    }

    async fn pull_entry(
        &mut self,
        holder: InstallationId,
        entry: LogEntry,
        own: InstallationId,
        request: Request,
    ) -> Result<Entry, CoreError> {
        let LogEntry {
            seq,
            conversation,
            sender_peer,
            sender_install,
            recorded_at_ms,
            body,
        } = entry;
        // Notes are always processed, whoever logged them: a note about this device's own
        // message is how it learns what another device did with it.
        let inner = match body {
            EntryBody::Frame(inner) => inner,
            body @ (EntryBody::Delivered { .. } | EntryBody::HandoffWithdraw { .. }) => {
                let entry = LogEntry {
                    seq,
                    conversation,
                    sender_peer,
                    sender_install,
                    recorded_at_ms,
                    body,
                };
                let op = MailboxDbOp::ApplyNote {
                    entry,
                    now: self.services.clock.wall_ms(),
                };
                return match self.mailbox_db(op, request).await {
                    Err(error) if error.code != ErrorCode::InvalidMessage => Err(error),
                    _ => Ok(Entry::Taken),
                };
            }
            // Only the device that sent the message acts on it (Ruling 1).
            EntryBody::HandoffStop {
                msg_id,
                class,
                epoch,
            } => {
                if sender_install == own && sender_peer == self.local()?.keys.peer_id() {
                    self.handoff_stopped(conversation, msg_id, (class, epoch), request)
                        .await?;
                }
                return Ok(Entry::Taken);
            }
        };
        // Ruling 1: a frame this device sent is in its own state already.
        if sender_install == own {
            return Ok(Entry::Taken);
        }
        let from = Origin {
            peer: sender_peer,
            installation: sender_install,
        };
        let via = Via::Mailbox { holder, seq };
        let result = match MlsFrame::decode(&inner) {
            Ok(MlsFrame::App {
                conversation: id,
                ciphertext,
            }) if id == conversation => {
                let ct_hash: [u8; 32] = Sha256::digest(&ciphertext).into();
                // Whatever kept it from opening (no group: a Welcome or Commit this device did
                // not take from the log; an old or far-ahead state) or from being taken (its
                // leaf no longer a current device here), it goes to the plaintext hand-over.
                self.receive_app(from, id, ciphertext, &via, request)
                    .await
                    .map(|reply| match reply {
                        Some(MlsReply::DecryptFailed { .. }) => Entry::Unopened(Unopened {
                            seq,
                            conversation,
                            sender_peer,
                            sender_install,
                            ct_hash,
                        }),
                        _ => Entry::Taken,
                    })
            }
            Ok(
                frame @ (MlsFrame::Commit {
                    conversation: id, ..
                }
                | MlsFrame::Welcome {
                    conversation: id, ..
                }),
            ) if id == conversation => self
                .mls_request(from, frame, &via, request)
                .await
                .map(|_| Entry::Taken),
            // Nothing a log holds: nothing to do with it.
            _ => Ok(Entry::Taken),
        };
        match result {
            // An entry that can never be taken (it contradicts this device's state) does not
            // hold the log up.
            Err(error) if error.code == ErrorCode::InvalidMessage => Ok(Entry::Taken),
            result => result,
        }
    }

    /// The `App` entries of a page this device could not open (spec 6.3): the holder is asked
    /// for their plaintext records, one `PlainReq` after another in a spawned task (Ruling 17),
    /// and the answers come back as `Step::Plain`. Until every entry is resolved the page's
    /// cursor does not move (Ruling 5).
    async fn resolve_unopened(
        &mut self,
        holder: InstallationId,
        conn: usize,
        page: Resolving,
        request: Request,
    ) {
        let Some(session) = self
            .own_sessions()
            .into_iter()
            .find(|s| s.conn.stable_id() == conn)
        else {
            // The session is gone: the page is pulled again over the next one.
            self.pull_failed(holder, "session_gone", request).await;
            return;
        };
        let mailbox = self.auth.send.clone();
        let ticket = request.ticket;
        tokio::spawn(async move {
            let mut answers = Vec::new();
            for entry in &page.unopened {
                let body = MailboxFrame::PlainReq { seq: entry.seq }.encode();
                let answer = exchange(&session, &body).await;
                let resolved = matches!(answer, Ok(MailboxReply::Plain(_) | MailboxReply::NoPlain));
                answers.push(answer);
                if !resolved {
                    break;
                }
            }
            let _ = mailbox
                .send(Input::Mailbox(MailboxEvent {
                    ticket,
                    holder,
                    conn,
                    step: Step::Plain { page, answers },
                }))
                .await;
        });
    }

    /// Takes the holder's answers to the `PlainReq`s for `unopened`, in order: a record is
    /// stored, `NoPlain` skips its entry (the message is missing here). True when every entry
    /// was resolved so the page is done; otherwise the pull ended and the page is pulled again
    /// later (a failed request, storage), or no more over this session (`Refused`, a record
    /// of another message).
    async fn take_plain(
        &mut self,
        holder: InstallationId,
        conn: usize,
        unopened: &[Unopened],
        answers: Vec<Result<MailboxReply, ErrorCode>>,
        request: Request,
    ) -> bool {
        let complete = answers.len() == unopened.len();
        for (entry, answer) in unopened.iter().zip(answers) {
            match answer {
                Ok(MailboxReply::Plain(record)) if entry.matches(&record) => {
                    let stored = self.store_plain(holder, entry, record, request).await;
                    if self.messages.mailbox.halted() {
                        self.messages.mailbox.pulls.remove(&holder);
                        return false;
                    }
                    if stored.is_err() {
                        self.pull_failed(holder, "entries", request).await;
                        return false;
                    }
                }
                Ok(MailboxReply::NoPlain) => {
                    #[cfg(all(feature = "test-hooks", debug_assertions))]
                    {
                        self.messages.mailbox.missing += 1;
                    }
                }
                // Not now: the page is pulled (and its entries asked for) again after a back-off.
                Ok(MailboxReply::Busy) => {
                    self.pull_failed(holder, "busy", request).await;
                    return false;
                }
                Err(_) => {
                    self.pull_failed(holder, "request", request).await;
                    return false;
                }
                Ok(_) => {
                    let now = self.schedule_now();
                    self.messages.mailbox.stop(conn, holder, now);
                    self.end_pull(holder, Ended::Stopped, request).await;
                    return false;
                }
            }
        }
        if !complete {
            self.pull_failed(holder, "plain", request).await;
        }
        complete
    }

    /// Stores one handed-over plaintext record of `entry` in the conversation it belongs to
    /// here, under the rules its ciphertext would have been taken by (`receive_app`). A text
    /// from the other account stored just now is notified with the rest of the pull.
    async fn store_plain(
        &mut self,
        holder: InstallationId,
        entry: &Unopened,
        record: PlainRecord,
        request: Request,
    ) -> Result<(), CoreError> {
        let own = record.sender_peer == self.local()?.keys.peer_id();
        let Some(peer) = self
            .conversation_peer(record.conversation, record.sender_peer)
            .filter(|peer| own || *peer == record.sender_peer)
        else {
            // No longer a conversation this device takes texts in.
            return Ok(());
        };
        let key = MessageKey {
            conversation: record.conversation,
            sender: record.sender_peer,
            installation: record.sender_install,
            id: record.msg_id,
        };
        let op = MailboxDbOp::StorePlain {
            record: Box::new(record),
            peer,
            ct_hash: entry.ct_hash,
            holder,
            now: self.services.clock.wall_ms(),
        };
        match self.mailbox_db(op, request).await {
            Ok(MailboxDbReply::Stored(true)) if !own => {
                let via = Via::Mailbox {
                    holder,
                    seq: entry.seq,
                };
                self.text_stored(key, &via, request).await;
                Ok(())
            }
            // A record that contradicts this device's state is not taken.
            Ok(_) => Ok(()),
            Err(error) if error.code == ErrorCode::InvalidMessage => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// A text from the other account was stored just now: notified at once when it came
    /// directly, with the rest of its pull when it came from another own device's log (the
    /// storing transaction marked it for that, Ruling 35), with the rest of its fetch when it came
    /// from a server queue (marked the same way, under this device), and by the push's own
    /// processing when a push brought it (`push_received`, which knows whether it was read
    /// already).
    pub(super) async fn text_stored(&mut self, key: MessageKey, via: &Via, request: Request) {
        match via {
            Via::Direct(_) => self.notify(vec![key], Vec::new(), request).await,
            Via::Push | Via::Queue => {}
            Via::Mailbox { .. } => {
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                {
                    let mailbox = &mut self.messages.mailbox;
                    mailbox.stored += 1;
                    if mailbox.halt_after.is_some_and(|n| mailbox.stored >= n) {
                        mailbox.halted = true;
                    }
                }
            }
        }
    }

    /// My `Read` receipt from another own device made messages of `conversation` read here:
    /// notified at once when it came directly, at the end of its pull when it came from a log.
    pub(super) async fn read_synced(
        &mut self,
        conversation: ConversationId,
        via: &Via,
        request: Request,
    ) {
        match via {
            Via::Direct(_) | Via::Push | Via::Queue => {
                self.notify(Vec::new(), vec![conversation], request).await
            }
            Via::Mailbox { holder, .. } => {
                if let Some(pull) = self.messages.mailbox.pulls.get_mut(holder) {
                    pull.read.insert(conversation);
                }
            }
        }
    }

    /// The pull from `holder` is over: what pulls from it stored and is still unread is
    /// notified, per conversation, in one event, with the conversations it found read
    /// elsewhere; also what a run that died before its pull ended stored (Ruling 35). A pull that
    /// failed is tried again after a back-off (final review I1: nothing else may come to pull
    /// again); one that got to the end of the log ends the back-off.
    /// A pull from `holder` failed for now (`reason`, a fixed name the e2e m6 facts note:
    /// busy, request, entries, storage, session_gone, plain); it is tried again after a back-off.
    async fn pull_failed(&mut self, holder: InstallationId, reason: &'static str, request: Request) {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if self.messages.mailbox.pulls.contains_key(&holder) {
            self.messages.mailbox.trace.pull_failure = Some(reason);
        }
        #[cfg(not(all(feature = "test-hooks", debug_assertions)))]
        let _ = reason;
        self.end_pull(holder, Ended::Failed, request).await;
    }

    async fn end_pull(&mut self, holder: InstallationId, ended: Ended, request: Request) {
        let Some(pull) = self.messages.mailbox.pulls.remove(&holder) else {
            return;
        };
        let now = self.schedule_now();
        let mailbox = &mut self.messages.mailbox;
        let done = matches!(ended, Ended::Done);
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        match ended {
            Ended::Done => mailbox.trace.pulls_done += 1,
            Ended::Failed => mailbox.trace.pulls_failed += 1,
            Ended::Stopped => mailbox.trace.pulls_stopped += 1,
        }
        match ended {
            Ended::Done => {
                mailbox.retry.remove(&holder);
                mailbox.stop_delay.remove(&holder);
            }
            Ended::Failed => mailbox.back_off(holder, now),
            Ended::Stopped => {}
        }
        if pull.again {
            mailbox.due.insert(holder);
        }
        let op = MailboxDbOp::TakeNotify {
            holder: Some(holder),
            running: Vec::new(),
            read: pull.read.into_iter().collect(),
        };
        // A failure leaves the marks for the next pull from it, or the next start.
        if let Ok(MailboxDbReply::Notify(items)) = self.mailbox_db(op, request).await {
            self.publish_notify(items);
        }
        if done {
            // A read clear push (M7 spec 7) waits for a sync to end.
            self.push_pull_done();
        }
    }

    /// The texts a fetch from this device's server queues stored (marked under this device,
    /// like a pull's) are notified per conversation in one event (M8 spec 4: alerted once).
    pub(super) async fn notify_fetched(&mut self, request: Request) {
        let Ok(local) = self.local() else {
            return;
        };
        let op = MailboxDbOp::TakeNotify {
            holder: Some(local.device.installation_id()),
            running: Vec::new(),
            read: Vec::new(),
        };
        // A failure leaves the marks for the next fetch's end, or the next start.
        if let Ok(MailboxDbReply::Notify(items)) = self.mailbox_db(op, request).await {
            self.publish_notify(items);
        }
    }

    async fn notify(&mut self, keys: Vec<MessageKey>, read: Vec<ConversationId>, request: Request) {
        if let Ok(MessageDbReply::Notify(items)) = self
            .message_db(MessageDbOp::NotifyItems(keys, read), request)
            .await
        {
            self.publish_notify(items);
        }
    }

    pub(super) fn publish_notify(&mut self, mut items: Vec<crate::event::NotifyItem>) {
        if items.is_empty() {
            return;
        }
        // The alias, and a fetch for a wake push taking over its generic notification (M7).
        self.push_notify_items(&mut items);
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        {
            let new: u64 = items.iter().map(|i| u64::from(i.new_messages)).sum();
            let mailbox = &mut self.messages.mailbox;
            mailbox.alerts += u64::from(new > 0);
            mailbox.alerted_messages += new;
        }
        self.bus.publish(crate::CoreEvent::Notify { items });
    }
}
