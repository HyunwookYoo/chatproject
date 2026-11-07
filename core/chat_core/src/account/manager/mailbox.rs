//! Own-device sync of the account actor (M6, spec 6.1–6.2, frame kind 18).
//!
//! Every device logs the kind 17 frames it accepted or sent (`message/mailbox_store.rs`). When a
//! session with another device of this account becomes ready (whichever side dialed), when that
//! device sends `Notify`, and on `mailbox_sync_now`, this device pulls the other's log from where
//! it stopped: one `SyncReq` per page, its entries processed in `seq` order through the M5
//! receive paths (`receive_app`, `mls_request`) under the device that originally sent them. The
//! holder's cursor (`Pulled`) moves only once a whole page is processed and stored; a page cut
//! short is pulled again, and what it stored already is filtered by `processed_ct`.
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
use super::mls::{Origin, Via};
use super::*;
use crate::account::{
    auth,
    connection::{Input, Session},
};
use crate::mailbox::wire::{EntryBody, LogEntry, MailboxFrame, MailboxReply};
use crate::message::{
    mailbox_store::{MailboxDbOp, MailboxDbReply},
    store::{MessageDbOp, MessageDbReply},
    types::{ConversationId, MessageKey},
};
use crate::mls::wire::{MlsFrame, MlsReply};
use std::collections::{BTreeMap, BTreeSet};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Another own device is told about new log entries at most this often.
const NOTIFY_GAP_MS: u64 = 200;
/// The log is pruned at least this often (Ruling 11), on every device.
const PRUNE_EVERY_MS: u64 = 3_600_000;

#[derive(Default)]
pub(crate) struct Runtime {
    /// Own devices to pull from at the next poll.
    due: BTreeSet<InstallationId>,
    /// Pulls running, by the device whose log is pulled.
    pulls: BTreeMap<InstallationId, Pull>,
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
    /// After this many texts stored by pulls, processing stops as if the device died there.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    halt_after: Option<u64>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    stored: u64,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    halted: bool,
    /// Sync is paused: no upkeep, pulls or `Notify`, and requests are answered `Busy`.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    held: bool,
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

/// The answer to one `SyncReq` of a running pull.
pub(crate) struct MailboxEvent {
    ticket: u64,
    holder: InstallationId,
    conn: usize,
    since: u64,
    answer: Result<MailboxReply, ErrorCode>,
}

/// What one processed page left.
pub(super) struct PullSummary {
    /// Seqs of the `App` entries this device could not open (spec 6.3, Task 7).
    pub unopened: Vec<u64>,
}

/// What became of one log entry.
enum Entry {
    /// Processed: stored, applied, already known, or nothing this device can use.
    Taken,
    /// An `App` frame this device could not open.
    Unopened(u64),
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
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(crate) enum MailboxTestReply {
    Reply(MailboxReply),
    Done,
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
            MailboxTestReply::Done => unreachable!(),
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
    /// log, and that device (doing the same) needs no `Notify` for what is logged so far.
    pub(super) fn mailbox_session_ready(&mut self, grant: &auth::AuthGrant) {
        let Ok(local) = self.local() else { return };
        if grant.peer != local.keys.peer_id()
            || grant.installation == local.device.installation_id()
        {
            return;
        }
        let now = self.schedule_now();
        let mailbox = &mut self.messages.mailbox;
        mailbox
            .notified
            .insert(grant.installation, (mailbox.head, now));
        mailbox.schedule(grant.installation);
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
                    Err(error)
                        if matches!(
                            error.code,
                            ErrorCode::AccountRestricted | ErrorCode::StorageLocked
                        ) =>
                    {
                        return Err(error);
                    }
                    Err(_) => MailboxReply::Busy,
                }
            }
            Ok(MailboxFrame::Notify { .. }) => {
                self.messages.mailbox.schedule(installation);
                MailboxReply::Ok
            }
            // Plaintext hand-over, send hand-over and mailbox announcements come with Tasks 7,
            // 9 and 10; until then (and for anything undecodable) the asker tries later.
            Ok(_) | Err(_) => MailboxReply::Busy,
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
    /// retention of the log, due pulls, and `Notify` to devices behind this log's head.
    pub(super) async fn poll_mailbox(&mut self) {
        let Ok(request) = self.ready() else { return };
        let Ok(local) = self.local() else { return };
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
            self.messages.mailbox.devices = Some(others);
        }
        let now = self.schedule_now();
        if now >= self.messages.mailbox.prune_at {
            let op = MailboxDbOp::Prune(self.services.clock.wall_ms());
            if self.mailbox_db(op, request).await.is_err() {
                return;
            }
            self.messages.mailbox.prune_at = now.saturating_add(PRUNE_EVERY_MS);
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
        for installation in std::mem::take(&mut mailbox.due) {
            if let Some(session) = usable.iter().find(|s| s.grant.installation == installation) {
                // A device without a session pulls when one is ready again.
                let session = session.clone();
                if self.pull_from(session, request).await.is_err() {
                    return;
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
        tokio::spawn(async move {
            let answer = exchange(&session, &body).await;
            let _ = mailbox
                .send(Input::Mailbox(MailboxEvent {
                    ticket,
                    holder: session.grant.installation,
                    conn: session.conn.stable_id(),
                    since,
                    answer,
                }))
                .await;
        });
    }

    /// A page (or the failure) of a running pull came back.
    pub(super) async fn mailbox_event(&mut self, event: MailboxEvent) {
        let MailboxEvent {
            ticket,
            holder,
            conn,
            since,
            answer,
        } = event;
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
        let (entries, next_seq, more) = match answer {
            Ok(MailboxReply::SyncResp {
                entries,
                next_seq,
                more,
            }) if page_in_order(&entries, since, next_seq, more) => (entries, next_seq, more),
            // Not now: the next sync asks again.
            Ok(MailboxReply::Busy) | Err(_) => {
                self.end_pull(holder, request).await;
                return;
            }
            // `Refused`, or a page out of order: no more syncing over this session.
            Ok(_) => {
                self.messages.mailbox.stopped.insert(conn);
                self.end_pull(holder, request).await;
                return;
            }
        };
        let summary = self.pull_page(holder, entries, request).await;
        if self.messages.mailbox.halted() {
            self.messages.mailbox.pulls.remove(&holder);
            return;
        }
        let Ok(summary) = summary else {
            self.end_pull(holder, request).await;
            return;
        };
        if !summary.unopened.is_empty() && !self.resolve_unopened(holder, &summary.unopened) {
            self.end_pull(holder, request).await;
            return;
        }
        let pulled = MailboxDbOp::Pulled { holder, next_seq };
        if self.mailbox_db(pulled, request).await.is_err() {
            self.end_pull(holder, request).await;
            return;
        }
        let session = self
            .own_sessions()
            .into_iter()
            .find(|s| s.conn.stable_id() == conn);
        match session {
            Some(session) if more => self.request_page(&session, next_seq, request),
            _ => self.end_pull(holder, request).await,
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
            if let Entry::Unopened(seq) = self.pull_entry(holder, entry, own, request).await? {
                unopened.push(seq);
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
        // message is how it learns what another device did with it. HandoffStop comes with
        // Task 9.
        let inner = match body {
            EntryBody::Frame(inner) => inner,
            body @ EntryBody::Delivered { .. } => {
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
            EntryBody::HandoffStop { .. } => return Ok(Entry::Taken),
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
            }) if id == conversation => self
                .receive_app(from, id, ciphertext, &via, request)
                .await
                .map(|reply| match reply {
                    Some(MlsReply::DecryptFailed { .. }) => Entry::Unopened(seq),
                    _ => Entry::Taken,
                }),
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

    /// The `App` entries of a page this device could not open (spec 6.3): Task 7 asks the
    /// holder for their plaintext records. Until then they stay unresolved, so the page's cursor
    /// does not move (Ruling 5) and the page is pulled again at the next sync.
    fn resolve_unopened(&mut self, _holder: InstallationId, _unopened: &[u64]) -> bool {
        false
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
    /// conversation, in one event, with the conversations it found read elsewhere.
    async fn end_pull(&mut self, holder: InstallationId, request: Request) {
        let Some(pull) = self.messages.mailbox.pulls.remove(&holder) else {
            return;
        };
        if pull.again {
            self.messages.mailbox.due.insert(holder);
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
            self.bus.publish(crate::CoreEvent::Notify { items });
        }
    }
}
