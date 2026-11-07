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
//! a text received directly, or my read sent directly, is reported on its own at once.
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
//! with `Announce`, when it changes and on every session with one that becomes ready, and never
//! a contact. What they announce is recorded (`mailbox_device`): it is where handoffs go, which
//! of my devices `mailbox_sync_now` dials first, and what `ConnectionSnapshot::mailbox_ok`
//! reports. A mailbox takes handoffs only while it is one (what it took it tries until settled),
//! and records its uptime every `UPTIME_EVERY_MS` (M6 records it, nothing judges it).
use super::mls::{Origin, Via};
use super::*;
use crate::account::{
    auth,
    connection::{Input, Session},
};
use crate::mailbox::wire::{
    EntryBody, HandoffEntry, LogEntry, MailboxFrame, MailboxReply, PlainRecord, handoff_batches,
};
use crate::message::{
    mailbox_store::{HandoffOutcome, MailboxDbOp, MailboxDbReply},
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
    /// Connections (`stable_id`) this device stopped syncing over: `Refused`, or a page that
    /// broke the order rules.
    stopped: BTreeSet<usize>,
    /// The log's head (largest seq handed out) after the last storage operation.
    pub(super) head: u64,
    /// Per own device: the head it was last told about, and when (scheduler time).
    notified: BTreeMap<InstallationId, (u64, u64)>,
    /// The other own devices last given to the store (`Devices`).
    devices: Option<Vec<InstallationId>>,
    /// Scheduler time of the next prune.
    prune_at: u64,
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
    /// Own devices whose session became ready: told at the next poll whether this one is a
    /// mailbox.
    announce_due: BTreeSet<InstallationId>,
    /// Scheduler time this device, while a mailbox, next records that it is up.
    uptime_at: u64,
    /// Each `Announce` sent: to which device, saying what.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    announces: Vec<(InstallationId, bool)>,
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
}

impl Runtime {
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
    /// Texts from the other account this pull stored, for its notification.
    fresh: Vec<MessageKey>,
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
    /// Pauses (or resumes) sync: no upkeep, pulls or `Notify`; requests answered `Busy`.
    Hold(bool),
    /// The next this many page requests of pulls fail as timed out, without being sent.
    FailPulls(u32),
    Counters,
    HandoffSends,
    Announces,
    SyncDials,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(crate) enum MailboxTestReply {
    Reply(MailboxReply),
    Done,
    Counters(MailboxCounters),
    Sends(Vec<(u64, [u8; 32])>),
    Announces(Vec<(InstallationId, bool)>),
    Dials(Vec<InstallationId>),
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
    /// Pauses own-device sync on this core (no storage work for it, requests answered
    /// `Busy`), or resumes it.
    pub async fn mailbox_test_hold(&self, hold: bool) -> Result<(), CoreError> {
        self.mailbox_test(MailboxTest::Hold(hold)).await.map(|_| ())
    }
    /// The next `requests` page requests of this core's pulls fail as a timeout would, without
    /// being sent.
    pub async fn mailbox_test_fail_pulls(&self, requests: u32) -> Result<(), CoreError> {
        self.mailbox_test(MailboxTest::FailPulls(requests))
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
    /// Each `Announce` this device sent, in order: to which device, and whether it said it is a
    /// mailbox.
    pub async fn mailbox_test_announces(&self) -> Result<Vec<(InstallationId, bool)>, CoreError> {
        match self.mailbox_test(MailboxTest::Announces).await? {
            MailboxTestReply::Announces(announces) => Ok(announces),
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
            MailboxTest::Hold(hold) => {
                self.messages.mailbox.held = hold;
                MailboxTestReply::Done
            }
            MailboxTest::FailPulls(requests) => {
                self.messages.mailbox.fail_pulls = requests;
                MailboxTestReply::Done
            }
            MailboxTest::Counters => MailboxTestReply::Counters(MailboxCounters {
                missing: self.messages.mailbox.missing,
                notifications: self.messages.mailbox.alerts,
                notified: self.messages.mailbox.alerted_messages,
            }),
            MailboxTest::HandoffSends => {
                MailboxTestReply::Sends(self.messages.mailbox.handoff_sends.clone())
            }
            MailboxTest::Announces => {
                MailboxTestReply::Announces(self.messages.mailbox.announces.clone())
            }
            MailboxTest::SyncDials => {
                MailboxTestReply::Dials(self.messages.mailbox.sync_dials.clone())
            }
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
        mailbox.announce_due.insert(grant.installation);
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

    /// Sets this device's mailbox role and tells my other devices with a ready session; one
    /// without a session is told once its session is ready.
    pub(super) async fn set_mailbox(&mut self, enabled: bool) -> Result<(), CoreError> {
        let request = self.ready()?;
        self.mailbox_db(MailboxDbOp::SetEnabled(enabled), request)
            .await?;
        let mailbox = &mut self.messages.mailbox;
        mailbox.enabled = Some(enabled);
        // Up from now on: recorded at the next poll.
        mailbox.uptime_at = 0;
        let stopped = &self.messages.mailbox.stopped;
        let sessions: Vec<Session> = self
            .own_sessions()
            .into_iter()
            .filter(|s| !stopped.contains(&s.conn.stable_id()))
            .collect();
        for session in sessions {
            self.announce(session, enabled);
        }
        Ok(())
    }

    pub(super) async fn mark_first_link_shown(&mut self) -> Result<(), CoreError> {
        let request = self.ready()?;
        self.mailbox_db(MailboxDbOp::FirstLinkShown, request)
            .await
            .map(|_| ())
    }

    /// Spec 8.4: tells another own device whether this one is a mailbox, in a spawned task (its
    /// answer changes nothing). Never a contact: the role is for my devices to know.
    fn announce(&mut self, session: Session, mailbox: bool) {
        if self.local().map(|local| local.keys.peer_id()).ok() != Some(session.grant.peer) {
            return;
        }
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.messages
            .mailbox
            .announces
            .push((session.grant.installation, mailbox));
        let body = MailboxFrame::Announce { mailbox }.encode();
        tokio::spawn(async move {
            let _ = exchange(&session, &body).await;
        });
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
                mailboxes.contains(&s.grant.installation) && !stopped.contains(&s.conn.stable_id())
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
        let MailboxDbReply::Handoff(entries) =
            self.mailbox_db(MailboxDbOp::HandoffRows, request).await?
        else {
            return Err(unavailable(ErrorCode::InvalidMessage));
        };
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
            // and `mailbox_ok`.
            Ok(MailboxFrame::Announce { mailbox }) => {
                let op = MailboxDbOp::Announced {
                    installation,
                    mailbox,
                    at: self.services.clock.wall_ms(),
                };
                match self.mailbox_db(op, request).await {
                    Ok(MailboxDbReply::Count(changed)) => {
                        let runtime = &mut self.messages.mailbox;
                        runtime.mailboxes = None;
                        // A device that just became a mailbox gets what is not delivered.
                        if mailbox && changed > 0 {
                            runtime.handoff_due = true;
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
            // Revoked devices' announcements went with them.
            mailbox.mailboxes = None;
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
        mailbox
            .stopped
            .retain(|id| sessions.iter().any(|s| s.conn.stable_id() == *id));
        let usable: Vec<Session> = sessions
            .into_iter()
            .filter(|s| !mailbox.stopped.contains(&s.conn.stable_id()))
            .collect();
        let ok = mailbox
            .mailboxes
            .as_ref()
            .is_some_and(|m| usable.iter().any(|s| m.contains(&s.grant.installation)));
        let enabled = mailbox.enabled.unwrap_or(false);
        let announce: Vec<Session> = std::mem::take(&mut mailbox.announce_due)
            .into_iter()
            .filter_map(|i| usable.iter().find(|s| s.grant.installation == i).cloned())
            .collect();
        self.bus.set_mailbox_ok(ok);
        for session in announce {
            self.announce(session, enabled);
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
    /// that it is up.
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
        self.messages.mailbox.pulls.insert(
            installation,
            Pull {
                conn: holder.conn.stable_id(),
                fresh: Vec::new(),
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
                    Ok(MailboxReply::Busy) | Err(_) => {
                        self.end_pull(holder, Ended::Failed, request).await;
                        return;
                    }
                    // `Refused`, or a page out of order: no more syncing over this session.
                    Ok(_) => {
                        self.messages.mailbox.stopped.insert(conn);
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
                    self.end_pull(holder, Ended::Failed, request).await;
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
            Step::Handoff { .. } => unreachable!("taken above"),
        };
        let pulled = MailboxDbOp::Pulled { holder, next_seq };
        if self.mailbox_db(pulled, request).await.is_err() {
            self.end_pull(holder, Ended::Failed, request).await;
            return;
        }
        let session = self
            .own_sessions()
            .into_iter()
            .find(|s| s.conn.stable_id() == conn);
        match session {
            Some(session) if more => self.request_page(&session, next_seq, request),
            // The rest of the log over another session, if one comes.
            None if more => self.end_pull(holder, Ended::Failed, request).await,
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
                // not take from the log; an old or far-ahead state), it goes to the plaintext
                // hand-over.
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
            self.end_pull(holder, Ended::Failed, request).await;
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
                        self.end_pull(holder, Ended::Failed, request).await;
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
                Ok(MailboxReply::Busy) | Err(_) => {
                    self.end_pull(holder, Ended::Failed, request).await;
                    return false;
                }
                Ok(_) => {
                    self.messages.mailbox.stopped.insert(conn);
                    self.end_pull(holder, Ended::Stopped, request).await;
                    return false;
                }
            }
        }
        if !complete {
            self.end_pull(holder, Ended::Failed, request).await;
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
            record,
            peer,
            ct_hash: entry.ct_hash,
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
    /// directly, with the rest of its pull when it came from another own device's log.
    pub(super) async fn text_stored(&mut self, key: MessageKey, via: &Via, request: Request) {
        match via {
            Via::Direct(_) => self.notify(vec![key], Vec::new(), request).await,
            Via::Mailbox { holder, .. } => {
                let mailbox = &mut self.messages.mailbox;
                if let Some(pull) = mailbox.pulls.get_mut(holder) {
                    pull.fresh.push(key);
                }
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                {
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
            Via::Direct(_) => self.notify(Vec::new(), vec![conversation], request).await,
            Via::Mailbox { holder, .. } => {
                if let Some(pull) = self.messages.mailbox.pulls.get_mut(holder) {
                    pull.read.insert(conversation);
                }
            }
        }
    }

    /// The pull from `holder` is over: what it stored and is still unread is notified, per
    /// conversation, in one event, with the conversations it found read elsewhere. A pull that
    /// failed is tried again after a back-off (final review I1: nothing else may come to pull
    /// again); one that got to the end of the log ends the back-off.
    async fn end_pull(&mut self, holder: InstallationId, ended: Ended, request: Request) {
        let Some(pull) = self.messages.mailbox.pulls.remove(&holder) else {
            return;
        };
        let now = self.schedule_now();
        let mailbox = &mut self.messages.mailbox;
        match ended {
            Ended::Done => {
                mailbox.retry.remove(&holder);
            }
            Ended::Failed => mailbox.back_off(holder, now),
            Ended::Stopped => {}
        }
        if pull.again {
            mailbox.due.insert(holder);
        }
        if !pull.fresh.is_empty() || !pull.read.is_empty() {
            self.notify(pull.fresh, pull.read.into_iter().collect(), request)
                .await;
        }
    }

    async fn notify(&mut self, keys: Vec<MessageKey>, read: Vec<ConversationId>, request: Request) {
        if let Ok(MessageDbReply::Notify(items)) = self
            .message_db(MessageDbOp::NotifyItems(keys, read), request)
            .await
            && !items.is_empty()
        {
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
}
