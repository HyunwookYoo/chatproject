//! Fetching from this device's queues (spec 6.4), never two runs at once:
//!
//! * A run starts at a network start, when a push's fetch or a full sync asks, at a new address,
//!   and every `FETCH_EVERY_MS` (the M4 back-off after a failed request), but not while the
//!   server refuses requests (`Runtime::quiet_until`); its first step looks whether the network
//!   is on and asks nothing while it is off.
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

use super::*;

/// How often this device fetches from its queues while its network is on (spec 6.4).
const FETCH_EVERY_MS: u64 = 300_000;

/// Fetching from this device's queues (spec 6.4).
#[derive(Default)]
pub(super) struct Fetch {
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

impl Manager {
    /// A push's fetch or a full sync asks for a fetch: the next poll starts one.
    pub(in crate::account::manager) fn queue_fetch_soon(&mut self) {
        self.messages.queue.fetch.asked = true;
    }

    /// Whether a fetch run is on its way: the texts it stores are notified when it ends.
    pub(in crate::account::manager) fn queue_fetching(&self) -> bool {
        self.messages.queue.fetch.run.is_some()
    }

    /// Takes the next entries of the run on its way; or starts a fetch when one was asked for,
    /// the network started again, or it is time.
    pub(super) async fn queue_fetch_poll(&mut self, request: Request) {
        let starts = self.shared.network.starts();
        let now = self.schedule_now();
        let quiet = self.queue_quiet();
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
        if quiet || !(fetch.asked || now >= fetch.next.at) {
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
                            refused: false,
                            offline: true,
                        }))
                        .await;
                    return;
                }
            }
            let client = server.client.clone();
            let (answers, failure) = run_batches(batches(items), |items| client.recv(items)).await;
            let _ = mailbox
                .send(Input::Queue(QueueEvent::Received {
                    ticket,
                    answers,
                    failed: failure.is_some(),
                    refused: failure.is_some_and(|f| f.refused()),
                    offline: false,
                }))
                .await;
        });
    }

    /// The server of the run on its way refused a request.
    pub(super) fn queue_fetch_refused(&mut self) {
        let url = self.messages.queue.fetch.run.as_ref().map(|run| run.server.url.clone());
        if let Some(url) = url {
            self.queue_refused(&url);
        }
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
    pub(super) async fn queue_received(
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
            let (answers, failure) = run_batches(batches(items), |items| client.ack(items)).await;
            let _ = mailbox
                .send(Input::Queue(QueueEvent::Acked {
                    ticket,
                    answers,
                    failed: failure.is_some(),
                    refused: failure.is_some_and(|f| f.refused()),
                }))
                .await;
        });
    }

    /// A round's `ACK` answers: the cursors of what the server took move.
    pub(super) async fn queue_acked(
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
}
