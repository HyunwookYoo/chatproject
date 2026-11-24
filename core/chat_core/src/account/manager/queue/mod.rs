//! Server queues of the account actor (M8 spec 6, 7, 8): three jobs, one file each, sharing
//! the configured queue server (`Server`), the request batching and the M4 back-off.
//!
//! * `reconcile`: one job compares what the verdict wants with the queues this device has and
//!   makes them agree.
//! * `fetch`: takes what this device's queues hold (spec 6.4).
//! * `put`: puts frames into the queues of devices no direct send reaches (spec 8).
//! * No address, or the mode "never": no request to the queue server of any kind.

mod fetch;
mod put;
mod reconcile;

use fetch::Fetch;
use put::Put;

use super::mls::{Origin, Via};
use super::*;
use crate::account::{auth, connection::Input};
use crate::mailbox::PAGE_MAX_ENTRIES;
use crate::mailbox::verdict::{self, QueueMode, Verdict};
use crate::message::{
    queue_store::{
        Fetchable, NewQueue, OwnQueue, QueueDbOp, QueueDbReply, QueueState, Queued, SWEEP_MAX,
        ToSend, Unreached,
    },
    store::{MessageDbOp, MessageDbReply},
    types::{ConversationId, MessageKey},
};
use crate::mls::{
    group::{CLASS_FUTURE_EPOCH, CLASS_NO_GROUP},
    wire::{MlsFrame, MlsReply},
};
use crate::queue::{
    AckItem, CreateItem, DeleteItem, ItemResult, QueueEntry, QueueServer, RecvItem, SendItem,
};
use std::collections::{BTreeSet, VecDeque};
use crate::{NetworkOp, NetworkReply, NetworkState};
use queue_wire::{
    BASE64URL_NOPAD, ENTRY_TTL_MS, Entry, MAX_BODY, MAX_ITEMS, QueueId, RETIRE_MS, ROTATE_MS,
    Signed, public_key, sign, signing_bytes,
};
use zeroize::Zeroizing;


/// How often the conversations are looked at for one without a queue while nothing else
/// changes.
const CHECK_MS: u64 = 3_600_000;

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
    put: Put,
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
    /// A `SEND` came back, for a text's or receipt's attempt (`what`) or the standing pass
    /// (`None`): what each item put where, and the answers in the same order (none when the
    /// request failed). `ts`: the time its items were signed at.
    Sent {
        ticket: u64,
        what: Option<MessageKey>,
        items: Vec<Queued>,
        answers: Vec<ItemResult>,
        ts: u64,
    },
}

fn b64(bytes: &[u8]) -> String {
    BASE64URL_NOPAD.encode(bytes)
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
        self.queue_fetch_soon();
    }

    /// One tick of queue upkeep, with the message scheduler's poll, after the verdict is worked
    /// out (`poll_mailbox`). Cheap unless a run is due.
    pub(super) async fn poll_queue(&mut self) {
        let Ok(request) = self.ready() else { return };
        if self.local().is_err() {
            return;
        }
        self.queue_take_config();
        self.queue_sweep(request).await;
        self.queue_standing(request).await;
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
            QueueEvent::Sent {
                ticket,
                what,
                items,
                answers,
                ts,
            } => self.queue_sent(ticket, what, items, answers, ts).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
