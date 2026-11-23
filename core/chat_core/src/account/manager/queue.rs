//! Server queues of the account actor (M8 spec 6.2, 6.3, 7): one job compares what the verdict
//! wants with the queues this device has and makes them agree.
//!
//! * `plan` is pure: from what this device wants (`Want`), its conversations with a group and
//!   its `queue_own` rows, it lists the actions and when the next timer falls due. A
//!   conversation without an `Active` queue gets one; an unconfirmed one is created at the
//!   server again (its id is this device's, so a `CREATE` sent twice is one queue: a crash
//!   before the answer converges, Review Focus 2); one older than `ROTATE_MS` is replaced, the
//!   old one `Retiring`; with no queue wanted every `Active` one retires with a withdrawal; a
//!   `Retiring` one past its `delete_at` is deleted at the server, or with the mode "never"
//!   only dropped here.
//! * The actor carries a plan out: the local half (new rows with their keys written before any
//!   `CREATE`, retirements with their withdrawals sealed) is written one queue per transaction,
//!   and only rows that were written go to the server; the `CREATE` and `DELETE` batches (at
//!   most `MAX_ITEMS` items a request) run in a spawned task, nothing waits on them in a
//!   transaction or an actor turn, and their answers are stored the same way, one transaction
//!   confirming each created queue and sealing its `QueueRef`. The frames ride `push_delivery`
//!   (kind 2), sent by the push handles' delivery loop.
//! * It runs when what it reads changes: the queue address, the verdict, the mode, a device
//!   list; when a timer is due (rotation, deletion); hourly for new conversations; and after a
//!   failed request, with the M4 back-off (1 s doubling to 5 min). Never two at once.
//! * A queue whose `QueueRef` cannot be sealed, an item the server refused (`limited`, `bad`),
//!   or answers that could not be stored wait for the hourly check: a lasting fault costs the
//!   server one `CREATE` item an hour for each queue (its limit is 500 an hour), not one each
//!   back-off step.
//! * No address, or the mode "never": no request to the queue server of any kind.
//!
//! Fetching (spec 6.4) is a second job beside it, never two runs at once:
//!
//! * A run starts at a network start, when a push's fetch or a full sync asks, at a new address,
//!   and every `FETCH_EVERY_MS` (the M4 back-off after a failed request); its first step looks
//!   whether the network is on and asks nothing while it is off.
//! * Each round is one `RECV` (at most `MAX_ITEMS` items a request) for every `Active` queue the
//!   server took and every `Retiring` one, after what the run read of each (its `last_seq` at
//!   first); queues the last answer had no room for (`limited`) come first. The run keeps the
//!   server it began with, so its `ACK`s go where its `RECV`s went.
//! * An entry is a frame as its sender writes it to a device directly, with that sender named
//!   (`queue::QueueEntry`), and is taken as that frame is taken directly or from an own
//!   device's log: an application message opened (from the peer account or another device of
//!   mine), a `Commit` or `Welcome` applied (`Via::Queue`), one transaction each, `processed_ct`
//!   dropping what came another way. At most `PAGE_MAX_ENTRIES` are taken a turn; the rest at
//!   the next polls, with nothing downloaded again.
//! * An application message of an epoch or group this device does not have yet, or a `Commit`
//!   ahead of one it lacks, is held: tried again in the run once a `Commit` or `Welcome` of its
//!   conversation is taken, and never acknowledged by this device, so the next run reads it
//!   again. The server drops it when it expires (`ENTRY_TTL_MS`); a direct sender keeps such a
//!   frame the same way. What comes after it is still taken.
//! * An `ACK` goes for each queue's entries up to the first held one, or one that could not be
//!   stored. Only once the server took it does `last_seq` move, so what was stored and not
//!   acknowledged (a lost `ACK`, a crash) comes again and is taken once. A queue that gave
//!   entries past what was read, or `limited`, is asked again in the next round; a run ends
//!   when none did. Entries that hold no frame of the queue's conversation are taken as
//!   nothing, so they never hold a queue up.
//! * `gone`: an `Active` queue goes `Retiring` with no withdrawal and the reconcile makes a
//!   fresh one; a `Retiring` one is dropped.
//! * The texts a run stored are notified when it ends, every end, in one event (marked when
//!   stored, so a run that dies first is notified by the next one, or after a start, as a
//!   pull's are).
//! * Putting messages into peers' queues (`SEND`) is not here yet.
use super::mls::{Origin, Via};
use super::*;
use crate::account::{auth, connection::Input};
use crate::mailbox::PAGE_MAX_ENTRIES;
use crate::mailbox::verdict::{self, QueueMode, Verdict};
use crate::message::{
    queue_store::{Fetchable, NewQueue, OwnQueue, QueueDbOp, QueueDbReply, QueueState},
    store::{MessageDbOp, MessageDbReply},
    types::ConversationId,
};
use crate::mls::{
    group::{CLASS_FUTURE_EPOCH, CLASS_NO_GROUP},
    wire::{MlsFrame, MlsReply},
};
use crate::queue::{
    AckItem, CreateItem, DeleteItem, ItemResult, QueueEntry, QueueServer, RecvItem,
};
use std::collections::VecDeque;
use crate::{NetworkOp, NetworkReply, NetworkState};
use queue_wire::{
    BASE64URL_NOPAD, Entry, MAX_ITEMS, QueueId, RETIRE_MS, ROTATE_MS, Signed, public_key, sign,
    signing_bytes,
};
use zeroize::Zeroizing;

/// How often the conversations are looked at for one without a queue while nothing else
/// changes.
const CHECK_MS: u64 = 3_600_000;
/// How often this device fetches from its queues while its network is on (spec 6.4).
const FETCH_EVERY_MS: u64 = 300_000;
/// Outer bound of one queue server request, whatever the implementation bounds itself to.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// What `AccountHandle` shares with the actor for the queue.
#[derive(Default)]
pub(super) struct Shared {
    /// The latest `queue_configure` (the address it resolved to) and its version.
    config: std::sync::Mutex<(u64, Option<Arc<str>>)>,
    /// How many entries each turn of a fetch took, in order.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    takes: std::sync::Mutex<Vec<usize>>,
}

impl Shared {
    fn set(&self, url: Option<String>) {
        let mut slot = self.config.lock().unwrap_or_else(|e| e.into_inner());
        *slot = (slot.0.wrapping_add(1), url.map(Arc::from));
    }
    /// The address if it is newer than version `seen`.
    fn newer(&self, seen: u64) -> Option<(u64, Option<Arc<str>>)> {
        let slot = self.config.lock().unwrap_or_else(|e| e.into_inner());
        (slot.0 != seen).then(|| slot.clone())
    }
    /// The latest address, taken by a poll or not.
    fn url(&self) -> Option<Arc<str>> {
        let slot = self.config.lock().unwrap_or_else(|e| e.into_inner());
        slot.1.clone()
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    fn took(&self, entries: usize) {
        self.takes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(entries);
    }
}

/// The queue server of the configured address.
#[derive(Clone)]
struct Server {
    url: Arc<str>,
    client: Arc<dyn QueueServer>,
}

#[derive(Default)]
pub(crate) struct Runtime {
    config_seen: u64,
    /// `None`: no address, no request of any kind.
    server: Option<Server>,
    /// A run's requests are on their way.
    running: bool,
    /// Run at the next poll: what it reads changed (address, mode, device list), or a run asked
    /// for another.
    due: bool,
    /// The verdict the last run planned for.
    planned: Option<Verdict>,
    /// Hash of the device lists the last run saw (`push`'s fingerprint).
    devices: Option<[u8; 32]>,
    /// Wall time the next rotation or deletion is due.
    wake_at: Option<u64>,
    /// The next run while nothing changes: hourly, or after a failure the back-off.
    check: Backoff,
    fetch: Fetch,
}

/// Failed runs in a row, and the scheduler time the next run is due: the M4 back-off (1 s
/// doubling to 5 min) after a failure, the job's own period after a success.
#[derive(Default)]
struct Backoff {
    failures: u64,
    at: u64,
}

impl Backoff {
    fn fail(&mut self, now: u64) {
        self.failures = self.failures.saturating_add(1);
        self.at = now.saturating_add(crate::message::scheduler::retry_delay(self.failures));
    }
    fn ok(&mut self, now: u64, every: u64) {
        self.failures = 0;
        self.at = now.saturating_add(every);
    }
}

/// Fetching from this device's queues (spec 6.4).
#[derive(Default)]
struct Fetch {
    /// A fetch was asked for (a new address, a push's fetch, a full sync): the next poll starts
    /// one, or the next after the run on its way.
    asked: bool,
    /// The network starts seen: a new one asks for a fetch.
    starts: u64,
    /// The run on its way.
    run: Option<FetchRun>,
    /// The next fetch while nothing asks for one: `FETCH_EVERY_MS`, or the back-off.
    next: Backoff,
    /// Queues an answer had no room for (`limited`): asked first in the next `RECV`.
    first: Vec<QueueId>,
}

/// Where a fetch run is: a round is a `RECV`, its entries taken (at most `PAGE_MAX_ENTRIES`
/// each poll, so other work goes on between), then an `ACK` for what was taken.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Asking,
    Taking,
    Acking,
}

/// One fetch, from its first `RECV` until it ends.
struct FetchRun {
    /// The account's request it runs under: a lock in between ends it.
    ticket: u64,
    /// The server it asks, kept for the whole run: its `ACK`s go where its `RECV`s went.
    server: Server,
    step: Step,
    /// This round's queues, as the store had them when it began.
    queues: Vec<Fetchable>,
    /// What this run read of each queue.
    read: Vec<(QueueId, Read)>,
    /// Entries of this round not taken yet, in order.
    untaken: VecDeque<Untaken>,
    /// Conversations where a `Commit` or `Welcome` was taken since their held entries were
    /// last tried.
    moved: Vec<ConversationId>,
    /// Asked again next round: it gave entries past what was read (there may be more), or
    /// `limited`.
    more: Vec<QueueId>,
    /// Unknown at the server.
    gone: Vec<QueueId>,
    /// A request failed, or an entry or the cursors could not be stored: no next round, and
    /// the next run after the back-off.
    failed: bool,
}

/// One entry to take, from queue `queue`; `again`: a held one, tried once more.
struct Untaken {
    queue: QueueId,
    entry: Entry,
    again: bool,
}

/// What a run read of one queue.
struct Read {
    /// The seq read up to: the next round asks after it.
    since: u64,
    /// The cursor the server took an `ACK` for.
    acked: u64,
    /// Each entry read, in seq order, and whether it is held (not taken yet).
    seqs: Vec<(u64, bool)>,
    /// The held entries: tried again once a `Commit` or `Welcome` of their conversation is
    /// taken. Never acknowledged by this device: the next run reads them again, and the
    /// server drops them when they expire (`ENTRY_TTL_MS`).
    held: Vec<Entry>,
    /// An entry could not be stored: nothing after it is read in this run.
    stopped: bool,
}

impl Read {
    fn new(last_seq: u64) -> Self {
        Self {
            since: last_seq,
            acked: last_seq,
            seqs: Vec::new(),
            held: Vec::new(),
            stopped: false,
        }
    }

    /// The seq every entry up to which is taken: what an `ACK` may cover.
    fn upto(&self) -> u64 {
        self.seqs
            .iter()
            .take_while(|(_, held)| !held)
            .last()
            .map_or(self.acked, |(seq, _)| *seq)
    }

    /// Notes what entry `seq` came to: read now, or a held one tried again.
    fn record(&mut self, seq: u64, held: bool) {
        match self.seqs.binary_search_by_key(&seq, |(s, _)| *s) {
            Ok(at) => self.seqs[at].1 = held,
            Err(at) => self.seqs.insert(at, (seq, held)),
        }
        self.since = self.since.max(seq);
    }
}

/// What one entry came to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Took {
    /// Done with: stored, applied, already here, or nothing this device can ever use.
    Taken,
    /// A `Commit` or `Welcome`, done with: entries it may let open are tried again.
    Moved,
    /// Not yet: an application message of an epoch or group this device does not have, or a
    /// `Commit` ahead of one it lacks. A direct sender keeps such a frame and sends it again
    /// later; here it stays in the queue.
    Held,
    /// It could not be stored now: the run stops reading that queue, and tries again later.
    Failed,
}

/// A run's requests came back.
pub(crate) enum QueueEvent {
    Answered {
        ticket: u64,
        /// The address they went to.
        url: Arc<str>,
        /// The server took their `CREATE`.
        created: Vec<QueueId>,
        /// Their id is another queue's there (`exists`).
        refused: Vec<QueueId>,
        /// The server took their `DELETE`, or knew them no more.
        deleted: Vec<QueueId>,
        /// A request failed (network, timeout, a status): tried again with the M4 back-off. Not
        /// when the server answered some item `limited` or `bad`: then the next run waits for
        /// the hourly check, the time the server's `CREATE` limit refills in, so a lasting
        /// refusal costs it one item an hour for each queue, not one each back-off.
        failed: bool,
    },
    /// A fetch round's `RECV` answers, in its queues' order (fewer when a request `failed`);
    /// `offline`: the network was off, nothing was asked.
    Received {
        ticket: u64,
        answers: Vec<(QueueId, ItemResult)>,
        failed: bool,
        offline: bool,
    },
    /// Its `ACK` answers, in the taken queues' order (fewer when a request `failed`).
    Acked {
        ticket: u64,
        answers: Vec<(QueueId, ItemResult)>,
        failed: bool,
    },
}

/// What this device wants of its queues (spec 5.3): one per conversation, none, or none and no
/// request to the queue server either (the mode "never").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Want {
    Queues,
    None,
    Never,
}

impl Want {
    fn of(mode: QueueMode, verdict: &Verdict) -> Self {
        match mode {
            QueueMode::Never => Self::Never,
            mode if verdict::wants_queue(mode, verdict) => Self::Queues,
            _ => Self::None,
        }
    }
}

/// One step that makes the queues what is wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QueueAction {
    /// A new `Active` queue: its keys are written, then it is created at the server and its
    /// `QueueRef` sealed.
    Create { conversation: ConversationId },
    /// An `Active` queue the server may not have: created there (again), then announced.
    Announce { queue: QueueId },
    /// `old` goes `Retiring` without a withdrawal, and a new queue replaces it (spec 6.3).
    Rotate {
        conversation: ConversationId,
        old: QueueId,
    },
    /// `Active` goes `Retiring`, its withdrawal sealed.
    Retire { queue: QueueId },
    /// A `Retiring` queue past its `delete_at`: deleted at the server, then its row.
    Delete { queue: QueueId },
    /// The same, with no request: its row only (the mode "never").
    Forget { queue: QueueId },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub actions: Vec<QueueAction>,
    /// Wall time the next rotation or deletion falls due, once these actions are done.
    pub wake_at: Option<u64>,
}

/// The actions that make `own` (this device's queues) what `want` asks for at wall time `now`,
/// given the conversations with a group. The same input gives the same plan; done, it gives
/// none.
pub(super) fn plan(
    want: Want,
    conversations: &[ConversationId],
    own: &[OwnQueue],
    now: u64,
) -> Plan {
    let mut actions = Vec::new();
    let mut wake_at: Option<u64> = None;
    let mut wake = |at: u64| wake_at = Some(wake_at.map_or(at, |w| w.min(at)));
    for queue in own {
        if let QueueState::Retiring { delete_at_ms } = queue.state {
            if now < delete_at_ms {
                wake(delete_at_ms);
            } else if want == Want::Never {
                actions.push(QueueAction::Forget { queue: queue.id });
            } else {
                actions.push(QueueAction::Delete { queue: queue.id });
            }
        }
    }
    let active = |conversation: &ConversationId| {
        own.iter()
            .find(|q| q.state == QueueState::Active && q.conversation == *conversation)
    };
    if want == Want::Queues {
        for conversation in conversations {
            match active(conversation) {
                None => {
                    actions.push(QueueAction::Create {
                        conversation: *conversation,
                    });
                    wake(now.saturating_add(ROTATE_MS));
                }
                Some(queue) if !queue.confirmed => {
                    actions.push(QueueAction::Announce { queue: queue.id });
                }
                Some(queue) if now >= queue.created_at_ms.saturating_add(ROTATE_MS) => {
                    actions.push(QueueAction::Rotate {
                        conversation: *conversation,
                        old: queue.id,
                    });
                    wake(now.saturating_add(RETIRE_MS));
                    wake(now.saturating_add(ROTATE_MS));
                }
                Some(queue) => wake(queue.created_at_ms.saturating_add(ROTATE_MS)),
            }
        }
    }
    for queue in own {
        let kept = want == Want::Queues && conversations.contains(&queue.conversation);
        if queue.state == QueueState::Active && !kept {
            actions.push(QueueAction::Retire { queue: queue.id });
            wake(now.saturating_add(RETIRE_MS));
        }
    }
    Plan { actions, wake_at }
}

fn b64(bytes: &[u8]) -> String {
    BASE64URL_NOPAD.encode(bytes)
}

/// A `CREATE` of queue `id`, signed by its recipient key at `ts`.
fn create_item(id: &QueueId, recipient: &[u8; 32], sender: &[u8; 32], ts: u64) -> CreateItem {
    let sender_pub = public_key(sender);
    let signed = Signed::Create {
        sender_pub: &sender_pub,
    };
    CreateItem {
        queue_id: b64(id),
        recipient_pub: b64(&public_key(recipient)),
        sender_pub: b64(&sender_pub),
        ts,
        sig: b64(&sign(recipient, &signing_bytes(&signed, id, ts))),
    }
}

/// A `RECV` of `queue` after `since_seq`, signed by its recipient key at `ts`.
fn recv_item(queue: &Fetchable, since_seq: u64, ts: u64) -> RecvItem {
    let signed = Signed::Recv { since_seq };
    RecvItem {
        queue_id: b64(&queue.id),
        since_seq,
        ts,
        sig: b64(&sign(
            &queue.recipient_secret,
            &signing_bytes(&signed, &queue.id, ts),
        )),
    }
}

/// An `ACK` of `queue`'s entries up to `upto_seq`.
fn ack_item(queue: &Fetchable, upto_seq: u64, ts: u64) -> AckItem {
    let signed = Signed::Ack { upto_seq };
    AckItem {
        queue_id: b64(&queue.id),
        upto_seq,
        ts,
        sig: b64(&sign(
            &queue.recipient_secret,
            &signing_bytes(&signed, &queue.id, ts),
        )),
    }
}

/// The kind 17 frame a queue entry holds (`QueueEntry`: the frame as its sender writes it to a
/// device directly), and the device it names as its sender, if the frame is one of the queue's
/// `conversation`. Anything else is no frame for it.
fn entry_frame(entry: &Entry, conversation: ConversationId) -> Option<(Origin, MlsFrame)> {
    let bytes = queue_wire::decode_ct(&entry.ct).ok()?;
    let held = QueueEntry::decode(&bytes).ok()?;
    let frame = MlsFrame::decode(&auth::payload(&held.frame, 17).ok()?).ok()?;
    let id = match &frame {
        MlsFrame::App { conversation, .. }
        | MlsFrame::GroupQuery { conversation }
        | MlsFrame::JoinRequest { conversation, .. }
        | MlsFrame::Commit { conversation, .. }
        | MlsFrame::Welcome { conversation, .. } => *conversation,
        MlsFrame::KeyPackageRequest => return None,
    };
    let from = Origin {
        peer: held.sender_peer,
        installation: held.sender_install,
    };
    (id == conversation).then_some((from, frame))
}

fn delete_item(id: &QueueId, recipient: &[u8; 32], ts: u64) -> DeleteItem {
    DeleteItem {
        queue_id: b64(id),
        ts,
        sig: b64(&sign(recipient, &signing_bytes(&Signed::Delete, id, ts))),
    }
}

/// A queue id and its two keys, from the OS.
fn new_queue(conversation: ConversationId) -> Result<NewQueue, CoreError> {
    let random = || {
        let mut bytes = Zeroizing::new([0u8; 32]);
        getrandom::fill(bytes.as_mut())
            .map_err(|_| CoreError::new(ErrorCode::NotAvailable, "OS randomness unavailable"))?;
        Ok::<_, CoreError>(bytes)
    };
    Ok(NewQueue {
        id: *random()?,
        conversation,
        recipient_secret: random()?,
        sender_secret: random()?,
    })
}

/// One batch of requests of a run: `calls` (each with its items' queue ids, at most
/// `MAX_ITEMS`) one after another, stopping at the first that fails.
async fn run_batches<T: Send + 'static>(
    calls: Vec<(Vec<QueueId>, Vec<T>)>,
    send: impl Fn(Vec<T>) -> crate::queue::QueueFuture,
) -> (Vec<(QueueId, ItemResult)>, bool) {
    let mut answers = Vec::new();
    for (ids, items) in calls {
        match tokio::time::timeout(REQUEST_TIMEOUT, send(items)).await {
            Ok(Ok(results)) if results.len() == ids.len() => {
                answers.extend(ids.into_iter().zip(results));
            }
            _ => return (answers, true),
        }
    }
    (answers, false)
}

/// `items` (with the queue id of each) cut into requests of at most `MAX_ITEMS`.
fn batches<T>(items: Vec<(QueueId, T)>) -> Vec<(Vec<QueueId>, Vec<T>)> {
    let mut out: Vec<(Vec<QueueId>, Vec<T>)> = Vec::new();
    for (id, item) in items {
        match out.last_mut() {
            Some((ids, batch)) if batch.len() < MAX_ITEMS => {
                ids.push(id);
                batch.push(item);
            }
            _ => out.push((vec![id], vec![item])),
        }
    }
    out
}

impl AccountHandle {
    /// Tells the core the queue server address (at every start; spec 7.2): `None` takes this
    /// build's. Release builds take only this build's `https://` address, debug builds also a
    /// given one (`http://` too). Without one the queue is off: no request of any kind. The
    /// actor takes it at its next poll.
    pub fn queue_configure(&self, server_url: Option<String>) -> Result<(), CoreError> {
        let url = crate::queue::queue_url(server_url.as_deref())?;
        self.shared.queue.set(url);
        Ok(())
    }
    /// How many entries each actor turn of a fetch took, in order (at most
    /// `PAGE_MAX_ENTRIES` each).
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub fn queue_test_takes(&self) -> Vec<usize> {
        self.shared
            .queue
            .takes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[cfg(feature = "push-http")]
fn http_server(url: &str) -> Option<Arc<dyn QueueServer>> {
    let server = crate::queue::HttpQueueServer::new(url).ok()?;
    Some(Arc::new(server))
}
#[cfg(not(feature = "push-http"))]
fn http_server(_url: &str) -> Option<Arc<dyn QueueServer>> {
    None
}

impl Manager {
    async fn queue_db(
        &mut self,
        op: QueueDbOp,
        request: Request,
    ) -> Result<QueueDbReply, CoreError> {
        match self.message_db(MessageDbOp::Queue(op), request).await? {
            MessageDbReply::Queue(reply) => Ok(reply),
            _ => Err(unavailable(ErrorCode::InvalidMessage)),
        }
    }

    /// The queue server a received `QueueRef` must name to be taken (`None`: none is): the
    /// latest `queue_configure`, whether or not a poll took it yet. A frame can come before
    /// that poll (one pulled earlier in the same tick, or at start), and is acknowledged
    /// either way.
    pub(super) fn queue_url(&self) -> Option<Arc<str>> {
        self.shared.queue.url()
    }

    /// What the queue reads changed (the mode): the next poll runs it.
    pub(super) fn queue_changed(&mut self) {
        self.messages.queue.due = true;
    }

    /// Whether this device has an `Active` queue the server took (spec 4: the home card).
    pub(super) async fn queue_using(&mut self, request: Request) -> Result<bool, CoreError> {
        match self.queue_db(QueueDbOp::Using, request).await? {
            QueueDbReply::Using(using) => Ok(using),
            _ => Err(unavailable(ErrorCode::InvalidMessage)),
        }
    }

    /// Takes a newer `queue_configure`: the server of its address.
    fn queue_take_config(&mut self) {
        let Some((version, url)) = self.shared.queue.newer(self.messages.queue.config_seen) else {
            return;
        };
        let server = url.and_then(|url| {
            let client = self.services.queue.clone().or_else(|| http_server(&url))?;
            Some(Server { url, client })
        });
        let queue = &mut self.messages.queue;
        queue.config_seen = version;
        queue.server = server;
        queue.due = true;
        queue.fetch.asked = true;
    }

    /// One tick of queue upkeep, with the message scheduler's poll, after the verdict is worked
    /// out (`poll_mailbox`). Cheap unless a run is due.
    pub(super) async fn poll_queue(&mut self) {
        let Ok(request) = self.ready() else { return };
        if self.local().is_err() {
            return;
        }
        self.queue_take_config();
        // A run on its way goes on even if the address changed: it ends where it began.
        self.queue_fetch_poll(request).await;
        if self.messages.queue.server.is_none() || self.messages.queue.running {
            return;
        }
        let Some(verdict) = self.mailbox_verdict() else {
            return;
        };
        let devices = self.push_devices();
        let now = self.schedule_now();
        let wall = self.services.clock.wall_ms();
        let queue = &mut self.messages.queue;
        if queue.devices != devices {
            queue.devices = devices;
            queue.due = true;
        }
        let due = queue.due
            || queue.planned != Some(verdict)
            || now >= queue.check.at
            || queue.wake_at.is_some_and(|at| wall >= at);
        if due {
            self.queue_run(verdict, request).await;
        }
    }

    /// A run failed: the next one waits the M4 back-off.
    fn queue_failed(&mut self) {
        let now = self.schedule_now();
        self.messages.queue.check.fail(now);
    }

    /// The next run waits for the hourly check: a run is done, or what is left of it would
    /// only spend the server's `CREATE` limit sooner (see `QueueEvent::Answered.failed`).
    fn queue_check_later(&mut self) {
        let now = self.schedule_now();
        self.messages.queue.check.ok(now, CHECK_MS);
    }

    /// Each peer account's current devices, for the frames sealed for them.
    fn queue_devices(&self) -> Vec<(PeerId, Vec<InstallationId>)> {
        let Ok(local) = self.local() else {
            return Vec::new();
        };
        local
            .remote
            .iter()
            .map(|r| (r.peer, self.current_installations(r.peer)))
            .collect()
    }

    /// Plans for `verdict` and carries the plan out.
    async fn queue_run(&mut self, verdict: Verdict, request: Request) {
        let Some(server) = self.messages.queue.server.clone() else {
            return;
        };
        let queue = &mut self.messages.queue;
        queue.due = false;
        queue.planned = Some(verdict);
        let view = match self.queue_db(QueueDbOp::Load, request).await {
            Ok(QueueDbReply::Loaded(view)) => view,
            _ => return self.queue_failed(),
        };
        let wall = self.services.clock.wall_ms();
        let want = Want::of(view.mode, &verdict);
        let Plan { actions, wake_at } = plan(want, &view.conversations, &view.own, wall);
        self.messages.queue.wake_at = wake_at;
        let mut retire = Vec::new();
        let mut new = Vec::new();
        let mut forget = Vec::new();
        let mut creates = Vec::new();
        // The new queues' `CREATE`s, sent once their rows are written.
        let mut fresh = Vec::new();
        let mut deletes = Vec::new();
        let row = |id: QueueId| view.own.iter().find(|q| q.id == id);
        for action in actions {
            let made = match action {
                QueueAction::Create { conversation } => Some(conversation),
                QueueAction::Rotate { conversation, old } => {
                    retire.push((old, false));
                    Some(conversation)
                }
                QueueAction::Announce { queue } => {
                    if let Some(q) = row(queue) {
                        let item = create_item(&q.id, &q.recipient_secret, &q.sender_secret, wall);
                        creates.push((q.id, item));
                    }
                    None
                }
                QueueAction::Retire { queue } => {
                    retire.push((queue, true));
                    None
                }
                QueueAction::Delete { queue } => {
                    if let Some(q) = row(queue) {
                        deletes.push((q.id, delete_item(&q.id, &q.recipient_secret, wall)));
                    }
                    None
                }
                QueueAction::Forget { queue } => {
                    forget.push(queue);
                    None
                }
            };
            if let Some(conversation) = made {
                let Ok(queue) = new_queue(conversation) else {
                    return self.queue_failed();
                };
                let item = create_item(
                    &queue.id,
                    &queue.recipient_secret,
                    &queue.sender_secret,
                    wall,
                );
                fresh.push((queue.id, item));
                new.push(queue);
            }
        }
        if !(retire.is_empty() && new.is_empty() && forget.is_empty()) {
            let op = QueueDbOp::Apply {
                retire,
                new,
                forget,
                devices: self.queue_devices(),
                wall,
                now: self.schedule_now(),
            };
            match self.queue_db(op, request).await {
                Ok(QueueDbReply::Applied { withdrawn, written }) => {
                    if withdrawn > 0 {
                        self.push_deliver_soon();
                    }
                    // No request goes for a row that is not there (its keys are written first).
                    creates.extend(fresh.into_iter().filter(|(id, _)| written.contains(id)));
                }
                _ => return self.queue_failed(),
            }
        }
        if creates.is_empty() && deletes.is_empty() {
            return self.queue_check_later();
        }
        self.messages.queue.running = true;
        let mailbox = self.auth.send.clone();
        let ticket = request.ticket;
        let (creates, deletes) = (batches(creates), batches(deletes));
        tokio::spawn(async move {
            let client = server.client.clone();
            let (created, mut failed) = run_batches(creates, |items| client.create(items)).await;
            let (deleted, delete_failed) = if failed {
                (Vec::new(), true)
            } else {
                run_batches(deletes, |items| client.delete(items)).await
            };
            failed |= delete_failed;
            let mut event = (Vec::new(), Vec::new(), Vec::new());
            // Some item answered `limited` or `bad` (see `QueueEvent::Answered.failed`).
            let mut rejected = false;
            for (id, result) in created {
                match result {
                    ItemResult::Ok => event.0.push(id),
                    ItemResult::Exists => event.1.push(id),
                    _ => rejected = true,
                }
            }
            for (id, result) in deleted {
                match result {
                    ItemResult::Ok | ItemResult::Gone => event.2.push(id),
                    _ => rejected = true,
                }
            }
            let failed = failed && !rejected;
            let (created, refused, deleted) = event;
            let _ = mailbox
                .send(Input::Queue(QueueEvent::Answered {
                    ticket,
                    url: server.url,
                    created,
                    refused,
                    deleted,
                    failed,
                }))
                .await;
        });
    }

    pub(super) async fn queue_event(&mut self, event: QueueEvent) {
        match event {
            QueueEvent::Answered {
                ticket,
                url,
                created,
                refused,
                deleted,
                failed,
            } => {
                self.queue_answered(ticket, url, created, refused, deleted, failed)
                    .await
            }
            QueueEvent::Received {
                ticket,
                answers,
                failed,
                offline,
            } => self.queue_received(ticket, answers, failed, offline).await,
            QueueEvent::Acked {
                ticket,
                answers,
                failed,
            } => self.queue_acked(ticket, answers, failed).await,
        }
    }

    /// A reconcile run's `CREATE`s and `DELETE`s came back.
    async fn queue_answered(
        &mut self,
        ticket: u64,
        url: Arc<str>,
        created: Vec<QueueId>,
        refused: Vec<QueueId>,
        deleted: Vec<QueueId>,
        failed: bool,
    ) {
        self.messages.queue.running = false;
        let Some(request) = self.ready().ok().filter(|r| r.ticket == ticket) else {
            // The account was locked meanwhile: the rows say what is left.
            self.messages.queue.due = true;
            return;
        };
        if !(created.is_empty() && refused.is_empty() && deleted.is_empty()) {
            // A queue some other keys hold its id: a new one is made for its conversation.
            let again = !refused.is_empty();
            let op = QueueDbOp::Settle {
                created,
                refused,
                deleted,
                server_url: url.to_string(),
                devices: self.queue_devices(),
                wall: self.services.clock.wall_ms(),
                now: self.schedule_now(),
            };
            match self.queue_db(op, request).await {
                Ok(QueueDbReply::Settled { announced, .. }) => {
                    if announced > 0 {
                        self.push_deliver_soon();
                    }
                }
                // The server holds the created queues already: they are asked for again at
                // the hourly check, not at each back-off step.
                _ => return self.queue_check_later(),
            }
            self.messages.queue.due |= again;
        }
        if failed {
            self.queue_failed();
        } else {
            self.queue_check_later();
        }
    }

    /// A push's fetch or a full sync asks for a fetch: the next poll starts one.
    pub(super) fn queue_fetch_soon(&mut self) {
        self.messages.queue.fetch.asked = true;
    }

    /// Whether a fetch run is on its way: the texts it stores are notified when it ends.
    pub(super) fn queue_fetching(&self) -> bool {
        self.messages.queue.fetch.run.is_some()
    }

    /// Takes the next entries of the run on its way; or starts a fetch when one was asked for,
    /// the network started again, or it is time.
    async fn queue_fetch_poll(&mut self, request: Request) {
        let starts = self.shared.network.starts();
        let now = self.schedule_now();
        let queue = &mut self.messages.queue;
        let fetch = &mut queue.fetch;
        if fetch.starts != starts {
            fetch.starts = starts;
            fetch.asked = true;
        }
        match fetch.run.as_ref().map(|run| (run.ticket == request.ticket, run.step)) {
            Some((true, Step::Taking)) => return self.queue_take_some(request).await,
            Some((true, _)) => return,
            // The account was locked meanwhile: fetched again from the start.
            Some((false, _)) => {
                fetch.run = None;
                fetch.asked = true;
            }
            None => {}
        }
        let Some(server) = queue.server.clone() else {
            return;
        };
        if !(fetch.asked || now >= fetch.next.at) {
            return;
        }
        fetch.asked = false;
        fetch.run = Some(FetchRun {
            ticket: request.ticket,
            server,
            step: Step::Asking,
            queues: Vec::new(),
            read: Vec::new(),
            untaken: VecDeque::new(),
            moved: Vec::new(),
            more: Vec::new(),
            gone: Vec::new(),
            failed: false,
        });
        self.queue_fetch_round(true, request).await;
    }

    /// A round of the run: one `RECV` for every queue to ask (after the `first` round only
    /// those in `more`), `limited` ones first, each after what this run read of it, sent in a
    /// spawned task. With nothing to ask (no queue, or the mode "never") the run ends with no
    /// request. The first round looks whether the network is on first.
    async fn queue_fetch_round(&mut self, first: bool, request: Request) {
        let (mode, queues) = match self.queue_db(QueueDbOp::Fetchable, request).await {
            Ok(QueueDbReply::Fetchable { mode, queues }) => (mode, queues),
            _ => return self.queue_fetch_end(true, request).await,
        };
        let wall = self.services.clock.wall_ms();
        let fetch = &mut self.messages.queue.fetch;
        let Some(run) = fetch.run.as_mut() else {
            return;
        };
        let mut ask: Vec<&Fetchable> = queues
            .iter()
            .filter(|q| first || run.more.contains(&q.id))
            .collect();
        run.more.clear();
        if mode == QueueMode::Never || ask.is_empty() {
            return self.queue_fetch_end(false, request).await;
        }
        ask.sort_by_key(|q| !fetch.first.contains(&q.id));
        let items: Vec<(QueueId, RecvItem)> = ask
            .iter()
            .map(|q| {
                let since = run
                    .read
                    .iter()
                    .find(|(id, _)| *id == q.id)
                    .map_or(q.last_seq, |(_, read)| read.since);
                (q.id, recv_item(q, since, wall))
            })
            .collect();
        run.queues = queues;
        run.step = Step::Asking;
        let server = run.server.clone();
        let network = first.then(|| self.shared.network.clone());
        let mailbox = self.auth.send.clone();
        let ticket = request.ticket;
        tokio::spawn(async move {
            if let Some(network) = network {
                let on = matches!(
                    network.execute(NetworkOp::Info).await,
                    Ok(NetworkReply::Info(info))
                        if matches!(info.state, NetworkState::Online | NetworkState::Starting)
                );
                if !on {
                    let _ = mailbox
                        .send(Input::Queue(QueueEvent::Received {
                            ticket,
                            answers: Vec::new(),
                            failed: false,
                            offline: true,
                        }))
                        .await;
                    return;
                }
            }
            let client = server.client.clone();
            let (answers, failed) = run_batches(batches(items), |items| client.recv(items)).await;
            let _ = mailbox
                .send(Input::Queue(QueueEvent::Received {
                    ticket,
                    answers,
                    failed,
                    offline: false,
                }))
                .await;
        });
    }

    /// The request of the run on its way, when `ticket` and `step` are its own (an answer of
    /// a run that is no more is dropped) and the account is still that request's: the account
    /// was locked meanwhile otherwise, and it is fetched again once it is not.
    fn queue_fetch_resume(&mut self, ticket: u64, step: Step) -> Option<Request> {
        let fetch = &mut self.messages.queue.fetch;
        if !fetch
            .run
            .as_ref()
            .is_some_and(|run| run.ticket == ticket && run.step == step)
        {
            return None;
        }
        let request = self.ready().ok().filter(|r| r.ticket == ticket);
        if request.is_none() {
            let fetch = &mut self.messages.queue.fetch;
            fetch.run = None;
            fetch.asked = true;
        }
        request
    }

    /// A round's `RECV` answers: the entries past what was read are kept to be taken.
    async fn queue_received(
        &mut self,
        ticket: u64,
        answers: Vec<(QueueId, ItemResult)>,
        failed: bool,
        offline: bool,
    ) {
        let Some(request) = self.queue_fetch_resume(ticket, Step::Asking) else {
            return;
        };
        if offline {
            return self.queue_fetch_end(false, request).await;
        }
        let fetch = &mut self.messages.queue.fetch;
        let Some(run) = fetch.run.as_mut() else {
            return;
        };
        run.failed |= failed;
        let answered: Vec<QueueId> = answers.iter().map(|(id, _)| *id).collect();
        let mut limited = Vec::new();
        let mut read_any = false;
        for (id, result) in answers {
            match result {
                ItemResult::Entries { entries } => {
                    let Some(queue) = run.queues.iter().find(|q| q.id == id) else {
                        continue;
                    };
                    let at = match run.read.iter().position(|(q, _)| *q == id) {
                        Some(at) => at,
                        None => {
                            run.read.push((id, Read::new(queue.last_seq)));
                            run.read.len() - 1
                        }
                    };
                    let since = run.read[at].1.since;
                    // What is at or below what was read is not followed back.
                    let mut new = entries.into_iter().filter(|e| e.seq > since).peekable();
                    if new.peek().is_some() {
                        read_any = true;
                        run.more.push(id);
                    }
                    run.untaken.extend(new.map(|entry| Untaken {
                        queue: id,
                        entry,
                        again: false,
                    }));
                }
                ItemResult::Limited => {
                    limited.push(id);
                    run.more.push(id);
                }
                ItemResult::Gone => run.gone.push(id),
                // `bad`: asked again at the next fetch.
                _ => {}
            }
        }
        // A round that read nothing new asks nothing more (`limited` for every queue it asked
        // is no answer the server gives: the first item of a request always has room).
        if !read_any {
            run.more.clear();
        }
        run.step = Step::Taking;
        fetch.first.retain(|id| !answered.contains(id));
        fetch.first.extend(limited);
        self.queue_take_some(request).await;
    }

    /// Takes at most `PAGE_MAX_ENTRIES` entries of the round, one transaction each, so the
    /// actor does other work between (the next ones at the next poll). Once none is left the
    /// held entries of a conversation a `Commit` or `Welcome` was taken in are tried again;
    /// then the round's `ACK` goes.
    async fn queue_take_some(&mut self, request: Request) {
        let mut taken = 0;
        while taken < PAGE_MAX_ENTRIES {
            let Some(run) = self.messages.queue.fetch.run.as_mut() else {
                return;
            };
            let Some(Untaken {
                queue: id,
                entry,
                again,
            }) = run.untaken.pop_front()
            else {
                break;
            };
            let Some(conversation) = run
                .queues
                .iter()
                .find(|q| q.id == id)
                .map(|q| q.conversation)
            else {
                continue;
            };
            let Some((_, read)) = run.read.iter().find(|(q, _)| *q == id) else {
                continue;
            };
            if read.stopped || (!again && entry.seq <= read.since) {
                continue;
            }
            taken += 1;
            let took = self.queue_entry(conversation, &entry, request).await;
            let Some(run) = self.messages.queue.fetch.run.as_mut() else {
                return;
            };
            let Some((_, read)) = run.read.iter_mut().find(|(q, _)| *q == id) else {
                continue;
            };
            match took {
                Took::Failed => {
                    read.stopped = true;
                    run.failed = true;
                }
                Took::Held => {
                    read.record(entry.seq, true);
                    read.held.push(entry);
                }
                Took::Taken => read.record(entry.seq, false),
                Took::Moved => {
                    read.record(entry.seq, false);
                    if !run.moved.contains(&conversation) {
                        run.moved.push(conversation);
                    }
                }
            }
        }
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if taken > 0 {
            self.shared.queue.took(taken);
        }
        let Some(run) = self.messages.queue.fetch.run.as_mut() else {
            return;
        };
        if !run.untaken.is_empty() {
            return;
        }
        let moved = std::mem::take(&mut run.moved);
        for (id, read) in &mut run.read {
            let conversation = run.queues.iter().find(|q| q.id == *id).map(|q| q.conversation);
            if read.stopped || !conversation.is_some_and(|c| moved.contains(&c)) {
                continue;
            }
            run.untaken.extend(read.held.drain(..).map(|entry| Untaken {
                queue: *id,
                entry,
                again: true,
            }));
        }
        if run.untaken.is_empty() {
            self.queue_fetch_ack(request).await;
        }
    }

    /// Takes one entry as the frame it holds is taken when its sender sends it directly
    /// (`receive_mls`) or it comes from an own device's log (`pull_entry`): an application
    /// message is opened (from the conversation's peer account, or another device of mine), a
    /// `Commit` or `Welcome` applied; what needs an answer over a session is no frame for a
    /// queue. What it came to decides whether the entry may be acknowledged (`Took`).
    async fn queue_entry(
        &mut self,
        conversation: ConversationId,
        entry: &Entry,
        request: Request,
    ) -> Took {
        let Some((from, frame)) = entry_frame(entry, conversation) else {
            return Took::Taken;
        };
        let Ok(local) = self.local() else {
            return Took::Failed;
        };
        // Ruling 1: a frame this device sent is in its own state already.
        if from.installation == local.device.installation_id() {
            return Took::Taken;
        }
        let result = match frame {
            MlsFrame::App { ciphertext, .. } => self
                .open_received(
                    from.peer,
                    None,
                    conversation,
                    ciphertext,
                    &Via::Queue,
                    request,
                )
                .await
                .map(|received| match received.reply {
                    // Of an epoch or a group a Commit or Welcome still has to bring.
                    Some(MlsReply::DecryptFailed {
                        class: CLASS_FUTURE_EPOCH | CLASS_NO_GROUP,
                        ..
                    }) => Took::Held,
                    _ => Took::Taken,
                }),
            frame @ MlsFrame::Commit { .. } => self
                .mls_request(from, frame, &Via::Queue, request)
                .await
                .map(|reply| match reply {
                    // A Commit before it is missing here, or it cannot be applied now: a
                    // direct sender sends it again (`delivery_outcome`).
                    MlsReply::DecryptFailed {
                        class: CLASS_FUTURE_EPOCH,
                        ..
                    }
                    | MlsReply::Busy => Took::Held,
                    _ => Took::Moved,
                }),
            // Not joined now (`Busy`) when no first-Commit hash was learned from its sender,
            // which only a session can ask: as from a log, the sender's own delivery of it is
            // joined instead.
            frame @ MlsFrame::Welcome { .. } => self
                .mls_request(from, frame, &Via::Queue, request)
                .await
                .map(|_| Took::Moved),
            _ => Ok(Took::Taken),
        };
        match result {
            Ok(took) => took,
            // An entry that can never be taken does not hold the queue up.
            Err(error) if error.code == ErrorCode::InvalidMessage => Took::Taken,
            Err(_) => Took::Failed,
        }
    }

    /// The round's `ACK`, in a spawned task, for each queue whose taken prefix grew.
    async fn queue_fetch_ack(&mut self, request: Request) {
        let wall = self.services.clock.wall_ms();
        let Some(run) = self.messages.queue.fetch.run.as_mut() else {
            return;
        };
        let items: Vec<(QueueId, AckItem)> = run
            .read
            .iter()
            .filter_map(|(id, read)| {
                let upto = read.upto();
                let queue = run.queues.iter().find(|q| q.id == *id)?;
                (upto > read.acked).then(|| (*id, ack_item(queue, upto, wall)))
            })
            .collect();
        if items.is_empty() {
            return self.queue_fetch_settle(Vec::new(), request).await;
        }
        run.step = Step::Acking;
        let server = run.server.clone();
        let mailbox = self.auth.send.clone();
        let ticket = request.ticket;
        tokio::spawn(async move {
            let client = server.client.clone();
            let (answers, failed) = run_batches(batches(items), |items| client.ack(items)).await;
            let _ = mailbox
                .send(Input::Queue(QueueEvent::Acked {
                    ticket,
                    answers,
                    failed,
                }))
                .await;
        });
    }

    /// A round's `ACK` answers: the cursors of what the server took move.
    async fn queue_acked(
        &mut self,
        ticket: u64,
        answers: Vec<(QueueId, ItemResult)>,
        failed: bool,
    ) {
        let Some(request) = self.queue_fetch_resume(ticket, Step::Acking) else {
            return;
        };
        let Some(run) = self.messages.queue.fetch.run.as_mut() else {
            return;
        };
        run.failed |= failed;
        let mut acked = Vec::new();
        for (id, result) in answers {
            let read = run.read.iter_mut().find(|(q, _)| *q == id);
            match (result, read) {
                (ItemResult::Ok, Some((_, read))) => {
                    read.acked = read.upto();
                    acked.push((id, read.acked));
                }
                (ItemResult::Gone, _) => run.gone.push(id),
                // Not taken (`bad`): its entries come again at the next fetch, not now.
                _ => run.more.retain(|q| *q != id),
            }
        }
        self.queue_fetch_settle(acked, request).await;
    }

    /// Stores what a round's answers said (the cursors the server took, the queues it lost),
    /// then goes on with the next round, or ends the run.
    async fn queue_fetch_settle(&mut self, acked: Vec<(QueueId, u64)>, request: Request) {
        let Some(run) = self.messages.queue.fetch.run.as_mut() else {
            return;
        };
        let gone = std::mem::take(&mut run.gone);
        run.more.retain(|id| !gone.contains(id));
        let stored = if acked.is_empty() && gone.is_empty() {
            true
        } else {
            let op = QueueDbOp::Fetched {
                acked,
                gone,
                wall: self.services.clock.wall_ms(),
            };
            match self.queue_db(op, request).await {
                // Their conversations get fresh queues from the reconcile.
                Ok(QueueDbReply::Fetched { replaced }) => {
                    self.messages.queue.due |= replaced > 0;
                    true
                }
                _ => false,
            }
        };
        let Some(run) = self.messages.queue.fetch.run.as_mut() else {
            return;
        };
        run.failed |= !stored;
        if run.failed || run.more.is_empty() {
            let failed = run.failed;
            return self.queue_fetch_end(failed, request).await;
        }
        self.queue_fetch_round(false, request).await;
    }

    /// The run is over: the next one in `FETCH_EVERY_MS`, or after the back-off when it
    /// `failed`. What a run stored is notified at its end, every end: marks a run that died
    /// after its `ACK` left (a lock) go out with the next one.
    async fn queue_fetch_end(&mut self, failed: bool, request: Request) {
        let now = self.schedule_now();
        let fetch = &mut self.messages.queue.fetch;
        fetch.run = None;
        if failed {
            fetch.next.fail(now);
        } else {
            fetch.next.ok(now, FETCH_EVERY_MS);
        }
        self.notify_fetched(request).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_800_000_000_000;

    fn conv(n: u8) -> ConversationId {
        ConversationId([n; 32])
    }

    fn row(
        n: u8,
        conversation: u8,
        created_at_ms: u64,
        state: QueueState,
        confirmed: bool,
    ) -> OwnQueue {
        OwnQueue {
            id: [n; 32],
            conversation: conv(conversation),
            recipient_secret: Zeroizing::new([n; 32]),
            sender_secret: Zeroizing::new([n; 32]),
            created_at_ms,
            state,
            confirmed,
        }
    }

    #[test]
    fn plan_creates_one_queue_per_conversation_without_one() {
        let own = [row(1, 1, T, QueueState::Active, true)];
        let plan = plan(Want::Queues, &[conv(1), conv(2), conv(3)], &own, T + 5);
        assert_eq!(
            plan.actions,
            [
                QueueAction::Create {
                    conversation: conv(2)
                },
                QueueAction::Create {
                    conversation: conv(3)
                },
            ]
        );
        // The kept queue rotates before the new ones would.
        assert_eq!(plan.wake_at, Some(T + ROTATE_MS));
    }

    #[test]
    fn plan_announces_an_unconfirmed_queue_again_and_makes_no_second_one() {
        let own = [row(1, 1, T, QueueState::Active, false)];
        let plan = plan(Want::Queues, &[conv(1)], &own, T + ROTATE_MS + 1);
        assert_eq!(plan.actions, [QueueAction::Announce { queue: [1; 32] }]);
    }

    #[test]
    fn plan_rotates_after_7_days_and_deletes_after_48_hours() {
        let own = [
            row(1, 1, T, QueueState::Active, true),
            row(
                2,
                1,
                T - 10,
                QueueState::Retiring {
                    delete_at_ms: T + RETIRE_MS,
                },
                true,
            ),
        ];
        let early = plan(Want::Queues, &[conv(1)], &own, T + RETIRE_MS - 1);
        assert!(early.actions.is_empty());
        assert_eq!(early.wake_at, Some(T + RETIRE_MS));
        let now = T + ROTATE_MS;
        let late = plan(Want::Queues, &[conv(1)], &own, now);
        assert_eq!(
            late.actions,
            [
                QueueAction::Delete { queue: [2; 32] },
                QueueAction::Rotate {
                    conversation: conv(1),
                    old: [1; 32]
                },
            ]
        );
        assert_eq!(late.wake_at, Some(now + RETIRE_MS));
    }

    #[test]
    fn plan_retires_every_active_queue_when_none_is_wanted() {
        let own = [
            row(1, 1, T, QueueState::Active, true),
            row(2, 2, T, QueueState::Active, false),
            row(
                3,
                3,
                T,
                QueueState::Retiring {
                    delete_at_ms: T + 9,
                },
                true,
            ),
        ];
        for want in [Want::None, Want::Never] {
            let plan = plan(want, &[conv(1), conv(2), conv(3)], &own, T);
            assert_eq!(
                plan.actions,
                [
                    QueueAction::Retire { queue: [1; 32] },
                    QueueAction::Retire { queue: [2; 32] },
                ]
            );
            assert_eq!(plan.wake_at, Some(T + 9));
        }
        // A queue whose conversation has no group retires too.
        let plan = plan(Want::Queues, &[conv(2)], &own, T);
        assert_eq!(
            plan.actions,
            [
                QueueAction::Announce { queue: [2; 32] },
                QueueAction::Retire { queue: [1; 32] }
            ]
        );
    }

    #[test]
    fn plan_never_drops_retired_queues_without_a_request() {
        let own = [row(3, 3, T, QueueState::Retiring { delete_at_ms: T }, true)];
        assert_eq!(
            plan(Want::Never, &[], &own, T).actions,
            [QueueAction::Forget { queue: [3; 32] }]
        );
        assert_eq!(
            plan(Want::None, &[], &own, T).actions,
            [QueueAction::Delete { queue: [3; 32] }]
        );
    }

    #[test]
    fn plan_done_is_empty() {
        let own = [
            row(1, 1, T, QueueState::Active, true),
            row(2, 2, T, QueueState::Active, true),
        ];
        let plan = plan(Want::Queues, &[conv(1), conv(2)], &own, T + 1);
        assert!(plan.actions.is_empty());
        assert!(
            super::plan(Want::None, &[conv(1)], &[], T)
                .actions
                .is_empty()
        );
    }

    #[test]
    fn batches_hold_at_most_max_items() {
        let items: Vec<(QueueId, u32)> = (0..250u32).map(|n| ([n as u8; 32], n)).collect();
        let cut = batches(items);
        assert_eq!(
            cut.iter()
                .map(|(ids, b)| (ids.len(), b.len()))
                .collect::<Vec<_>>(),
            [(100, 100), (100, 100), (50, 50)]
        );
        assert_eq!(batches(Vec::<(QueueId, u32)>::new()).len(), 0);
        let one = batches(vec![([1; 32], 1u32)]);
        assert_eq!(one.len(), 1);
    }

    /// Plan "성능": 100 conversations turning on make 100 `Create` actions, and their items go
    /// in one request; one more goes in a second.
    #[tokio::test]
    async fn create_batches_100_conversations_in_one_request() {
        struct Counting(std::sync::Mutex<Vec<usize>>);
        impl QueueServer for Counting {
            fn create(&self, items: Vec<CreateItem>) -> crate::queue::QueueFuture {
                self.0.lock().unwrap().push(items.len());
                let n = items.len();
                Box::pin(async move { Ok(vec![ItemResult::Ok; n]) })
            }
            fn send(&self, _: Vec<crate::queue::SendItem>) -> crate::queue::QueueFuture {
                unreachable!()
            }
            fn recv(&self, _: Vec<crate::queue::RecvItem>) -> crate::queue::QueueFuture {
                unreachable!()
            }
            fn ack(&self, _: Vec<crate::queue::AckItem>) -> crate::queue::QueueFuture {
                unreachable!()
            }
            fn delete(&self, _: Vec<DeleteItem>) -> crate::queue::QueueFuture {
                unreachable!()
            }
        }
        for (conversations, requests) in [(100u8, vec![100]), (101, vec![100, 1])] {
            let all: Vec<ConversationId> = (0..conversations).map(conv).collect();
            let actions = plan(Want::Queues, &all, &[], T).actions;
            assert_eq!(actions.len(), usize::from(conversations));
            let items = actions
                .into_iter()
                .map(|action| {
                    let QueueAction::Create { conversation } = action else {
                        panic!("{action:?}");
                    };
                    let queue = new_queue(conversation).unwrap();
                    let item =
                        create_item(&queue.id, &queue.recipient_secret, &queue.sender_secret, T);
                    (queue.id, item)
                })
                .collect();
            let server = Counting(std::sync::Mutex::new(Vec::new()));
            let (answers, failed) = run_batches(batches(items), |items| server.create(items)).await;
            assert!(!failed);
            assert_eq!(answers.len(), usize::from(conversations));
            assert_eq!(*server.0.lock().unwrap(), requests);
        }
    }

    #[test]
    fn recv_and_ack_items_verify_under_the_recipient_key() {
        let queue = Fetchable {
            id: [4; 32],
            conversation: conv(1),
            peer: PeerId([2; 32]),
            recipient_secret: Zeroizing::new([5; 32]),
            last_seq: 77,
        };
        let sig = |s: &str| -> [u8; 64] {
            BASE64URL_NOPAD
                .decode(s.as_bytes())
                .unwrap()
                .try_into()
                .unwrap()
        };
        let public = public_key(&[5; 32]);
        let recv = recv_item(&queue, 77, T);
        assert_eq!(recv.since_seq, 77);
        let signed = Signed::Recv { since_seq: 77 };
        assert!(queue_wire::verify(
            &public,
            &signing_bytes(&signed, &queue.id, T),
            &sig(&recv.sig)
        ));
        let ack = ack_item(&queue, 90, T);
        assert_eq!(ack.upto_seq, 90);
        let signed = Signed::Ack { upto_seq: 90 };
        assert!(queue_wire::verify(
            &public,
            &signing_bytes(&signed, &queue.id, T),
            &sig(&ack.sig)
        ));
    }

    /// An entry holds a kind 17 frame of the queue's conversation, as its sender writes it to a
    /// device, and the sender it names; anything else is no frame.
    #[test]
    fn an_entry_holds_a_frame_of_its_conversation_or_nothing() {
        let entry = |body: &[u8]| Entry {
            seq: 1,
            ct: b64(body),
            ts: T,
        };
        let held = |frame: &MlsFrame| {
            QueueEntry {
                sender_peer: PeerId([7; 32]),
                sender_install: InstallationId([8; 32]),
                frame: frame.encode().to_vec(),
            }
            .encode()
        };
        let app = |conversation| MlsFrame::App {
            conversation,
            ciphertext: vec![9; 40],
        };
        let commit = MlsFrame::Commit {
            conversation: conv(1),
            epoch: 3,
            commit: vec![5; 40],
        };
        for frame in [app(conv(1)), commit] {
            let (from, got) = entry_frame(&entry(&held(&frame)), conv(1)).unwrap();
            assert!(got == frame);
            assert_eq!((from.peer, from.installation), (PeerId([7; 32]), InstallationId([8; 32])));
        }
        // Another conversation's, no conversation, the bare inner body, no frame at all.
        assert!(entry_frame(&entry(&held(&app(conv(2)))), conv(1)).is_none());
        assert!(entry_frame(&entry(&held(&MlsFrame::KeyPackageRequest)), conv(1)).is_none());
        let inner = QueueEntry {
            sender_peer: PeerId([7; 32]),
            sender_install: InstallationId([8; 32]),
            frame: app(conv(1)).inner().to_vec(),
        };
        assert!(entry_frame(&entry(&inner.encode()), conv(1)).is_none());
        assert!(entry_frame(&entry(&app(conv(1)).encode()), conv(1)).is_none());
        assert!(entry_frame(&entry(b"not a frame"), conv(1)).is_none());
        let bad = Entry {
            seq: 1,
            ct: "*".into(),
            ts: T,
        };
        assert!(entry_frame(&bad, conv(1)).is_none());
    }

    /// What an `ACK` may cover: every entry up to it taken, none held.
    #[test]
    fn ack_covers_the_taken_prefix_only() {
        let mut read = Read::new(10);
        assert_eq!(read.upto(), 10);
        read.record(11, false);
        read.record(12, true);
        read.record(13, false);
        assert_eq!((read.upto(), read.since), (11, 13));
        // The held one taken once more: the prefix runs on to the last one read.
        read.record(12, false);
        assert_eq!(read.upto(), 13);
        read.record(14, true);
        assert_eq!(read.upto(), 13);
    }

    #[test]
    fn create_and_delete_items_verify_under_the_recipient_key() {
        let (id, recipient, sender) = ([4; 32], [5; 32], [6; 32]);
        let item = create_item(&id, &recipient, &sender, T);
        let sender_pub = public_key(&sender);
        let bytes = signing_bytes(
            &Signed::Create {
                sender_pub: &sender_pub,
            },
            &id,
            T,
        );
        let sig: [u8; 64] = BASE64URL_NOPAD
            .decode(item.sig.as_bytes())
            .unwrap()
            .try_into()
            .unwrap();
        assert!(queue_wire::verify(&public_key(&recipient), &bytes, &sig));
        assert_eq!(item.sender_pub, b64(&sender_pub));
        let item = delete_item(&id, &recipient, T);
        let sig: [u8; 64] = BASE64URL_NOPAD
            .decode(item.sig.as_bytes())
            .unwrap()
            .try_into()
            .unwrap();
        assert!(queue_wire::verify(
            &public_key(&recipient),
            &signing_bytes(&Signed::Delete, &id, T),
            &sig
        ));
    }
}
