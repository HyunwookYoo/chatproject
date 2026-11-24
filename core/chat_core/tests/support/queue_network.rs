// M8 queues between real cores: the push harness (own-device sync and an in-memory push
// proxy), with one queue server every core reaches. `FakeQueueServer` runs the real `queue_server` router on a temp SQLite file (plan
// "Engineering Principles"), so it answers as the server does, and it records what it was
// asked and by which core. The server's clock is the asking core's wall clock while it handles
// that core's request (requests are handled one at a time), so a core whose clock steps days
// ahead still signs within the server's window.
include!("push_network.rs");

use axum::extract::ConnectInfo;
use chat_core::account::clock::Clock;
use chat_core::account::{QueueMode, Verdict};
use chat_core::queue::{QueueEntry, QueueError, QueueFuture, QueueServer};
use chat_core::storage_fixture::{QueueOwnRow, QueuePeerRow, QueueTables};
use queue_wire::{
    AckItem, CommandName, CreateItem, DeleteItem, ItemResult, QueueId, QueueRequest, QueueResponse,
    RecvItem, SendItem, Signed, public_key, sign, signing_bytes,
};
use std::sync::atomic::AtomicU64;

/// The queue address the cores are configured with; the fake answers in memory.
const QUEUE_URL: &str = "https://queue.test";
const HOUR_MS: u64 = 3_600_000;
const DAY_MS: u64 = 24 * HOUR_MS;

/// One request the server got: which core sent it (its clock, `None` for the test itself),
/// the command, the queue of each item, and for a `RECV` what it answered each item.
#[derive(Clone)]
struct Call {
    from: Option<usize>,
    command: CommandName,
    queues: Vec<QueueId>,
    got: Vec<Got>,
}

/// What a `RECV` answered one queue: the seqs of its entries, or no entries.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Got {
    Entries(Vec<u64>),
    Limited,
    Gone,
    Other,
}

struct FakeQueueServer {
    app: axum::Router,
    /// The router's store, for its hourly sweep.
    store: queue_server::Store,
    db: std::path::PathBuf,
    _dir: tempfile::TempDir,
    /// The server's clock (ms), set to the asking core's wall clock for each request.
    now: Arc<AtomicU64>,
    /// One request at a time, so `now` is the asking core's while the router reads it.
    turn: tokio::sync::Mutex<()>,
    calls: Mutex<Vec<Call>>,
    /// While true, answers to `CREATE` wait after the server stored them (the core may die
    /// before it hears back).
    hold: tokio::sync::watch::Sender<bool>,
    /// While true, `ACK`s are lost on their way: the server never gets them.
    lose_acks: std::sync::atomic::AtomicBool,
    /// While true, `RECV`s wait before the server reads them.
    hold_recv: tokio::sync::watch::Sender<bool>,
    /// While true, `ACK`s wait before the server reads them.
    hold_acks: tokio::sync::watch::Sender<bool>,
}

impl FakeQueueServer {
    fn new(limits: queue_server::Limits) -> Arc<Self> {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("queue.db");
        let store = queue_server::Store::open(&db).unwrap();
        let now = Arc::new(AtomicU64::new(NOW * 1_000));
        let mut state = queue_server::AppState::new(store.clone());
        state.limits = limits;
        let clock = now.clone();
        state.clock = Arc::new(move || clock.load(Ordering::SeqCst));
        state.log = Arc::new(|_: &str| {});
        Arc::new(Self {
            app: queue_server::router(state),
            store,
            db,
            _dir: dir,
            now,
            turn: tokio::sync::Mutex::new(()),
            calls: Mutex::new(Vec::new()),
            hold: tokio::sync::watch::channel(false).0,
            lose_acks: Default::default(),
            hold_recv: tokio::sync::watch::channel(false).0,
            hold_acks: tokio::sync::watch::channel(false).0,
        })
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// Requests `f` sent so far.
    fn calls_from(&self, f: &Fixture) -> Vec<Call> {
        let from = Some(Arc::as_ptr(&f.clock) as usize);
        self.calls()
            .into_iter()
            .filter(|c| c.from == from)
            .collect()
    }

    /// `command` requests `f` sent so far.
    fn sent(&self, f: &Fixture, command: CommandName) -> Vec<Call> {
        self.calls_from(f)
            .into_iter()
            .filter(|c| c.command == command)
            .collect()
    }

    fn hold(&self, held: bool) {
        self.hold.send_replace(held);
    }

    fn hold_recv(&self, held: bool) {
        self.hold_recv.send_replace(held);
    }

    fn hold_acks(&self, held: bool) {
        self.hold_acks.send_replace(held);
    }

    fn lose_acks(&self, lost: bool) {
        self.lose_acks.store(lost, Ordering::SeqCst);
    }

    /// The `RECV`s `f` sent so far.
    fn fetches(&self, f: &Fixture) -> Vec<Call> {
        self.sent(f, CommandName::Recv)
    }

    /// How many entries the server holds in `queue` now (its own database: no `RECV`, which
    /// would spend the cores' shared limit).
    fn held(&self, queue: QueueId) -> usize {
        let c = rusqlite::Connection::open(&self.db).unwrap();
        c.query_row(
            "SELECT count(*) FROM entry WHERE queue_id=?1",
            [queue.as_slice()],
            |r| r.get::<_, i64>(0),
        )
        .unwrap() as usize
    }

    /// The entries `queue` holds now, in seq order (its own database).
    fn stored(&self, queue: QueueId) -> Vec<Vec<u8>> {
        let c = rusqlite::Connection::open(&self.db).unwrap();
        let mut q = c
            .prepare("SELECT ct FROM entry WHERE queue_id=?1 ORDER BY seq")
            .unwrap();
        q.query_map([queue.as_slice()], |r| r.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(|ct| ct.unwrap())
            .collect()
    }

    /// The server's own sweep at `now_ms`: what has been held longer than `ENTRY_TTL_MS` goes.
    async fn sweep(&self, now_ms: u64) {
        queue_server::sweep(&self.store, now_ms).await.unwrap();
    }

    /// The `SEND`s `f` sent so far.
    fn sends(&self, f: &Fixture) -> Vec<Call> {
        self.sent(f, CommandName::Send)
    }

    /// The queue ids the server holds now (its own database).
    fn queues(&self) -> Vec<QueueId> {
        let c = rusqlite::Connection::open(&self.db).unwrap();
        let mut q = c.prepare("SELECT id FROM queue ORDER BY id").unwrap();
        q.query_map([], |r| r.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(|id| id.unwrap().try_into().unwrap())
            .collect()
    }

    async fn call<T: serde::Serialize>(
        &self,
        from: Option<&Arc<FakeClock>>,
        command: CommandName,
        queues: Vec<QueueId>,
        items: Vec<T>,
    ) -> Result<Vec<ItemResult>, QueueError> {
        let index = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(Call {
                from: from.map(|clock| Arc::as_ptr(clock) as usize),
                command,
                queues,
                got: Vec::new(),
            });
            calls.len() - 1
        };
        if command == CommandName::Ack && self.lose_acks.load(Ordering::SeqCst) {
            return Err(QueueError::Network);
        }
        let hold = match command {
            CommandName::Recv => Some(&self.hold_recv),
            CommandName::Ack => Some(&self.hold_acks),
            _ => None,
        };
        if let (Some(hold), Some(_)) = (hold, from) {
            let _ = hold.subscribe().wait_for(|held| !*held).await;
        }
        let body = serde_json::to_vec(&QueueRequest { items }).unwrap();
        let mut request = Request::post(command.path())
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(std::net::SocketAddr::from((
                [192, 0, 2, 7],
                40_000,
            ))));
        let response = {
            let _turn = self.turn.lock().await;
            if let Some(clock) = from {
                self.now
                    .store(clock.unix_seconds() * 1_000, Ordering::SeqCst);
            }
            self.app
                .clone()
                .oneshot(request)
                .await
                .map_err(|_| QueueError::Network)?
        };
        if command == CommandName::Create {
            let _ = self.hold.subscribe().wait_for(|held| !*held).await;
        }
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|_| QueueError::Network)?
            .to_bytes();
        if status != 200 {
            return Err(QueueError::Status(status));
        }
        let response: QueueResponse =
            serde_json::from_slice(&body).map_err(|_| QueueError::Malformed)?;
        if command == CommandName::Recv {
            self.calls.lock().unwrap()[index].got = response
                .results
                .iter()
                .map(|result| match result {
                    ItemResult::Entries { entries } => {
                        Got::Entries(entries.iter().map(|e| e.seq).collect())
                    }
                    ItemResult::Limited => Got::Limited,
                    ItemResult::Gone => Got::Gone,
                    _ => Got::Other,
                })
                .collect();
        }
        Ok(response.results)
    }

    /// The test deletes a queue at the server with its recipient key, as if the server lost it.
    async fn delete_as(&self, queue: QueueId, recipient_secret: &[u8; 32]) -> ItemResult {
        let ts = self.now.load(Ordering::SeqCst);
        let item = DeleteItem {
            queue_id: BASE64URL_NOPAD.encode(&queue),
            ts,
            sig: BASE64URL_NOPAD.encode(&sign(
                recipient_secret,
                &signing_bytes(&Signed::Delete, &queue, ts),
            )),
        };
        self.call(None, CommandName::Delete, vec![queue], vec![item])
            .await
            .unwrap()
            .remove(0)
    }

    /// The test puts `ct` into a queue, as a peer holding its sender key would (Task 10 does it
    /// in the core), at the server's current time.
    async fn send_as(&self, queue: QueueId, sender_secret: &[u8; 32], ct: &[u8]) -> ItemResult {
        let ts = self.now.load(Ordering::SeqCst);
        let sig = sign(
            sender_secret,
            &signing_bytes(&Signed::Send { ct }, &queue, ts),
        );
        let item = SendItem {
            queue_id: BASE64URL_NOPAD.encode(&queue),
            ct: BASE64URL_NOPAD.encode(ct),
            ts,
            sig: BASE64URL_NOPAD.encode(&sig),
        };
        let mut results = self
            .call(None, CommandName::Send, vec![queue], vec![item])
            .await
            .unwrap();
        results.remove(0)
    }

    /// How many entries a queue holds, read with its recipient key (as Task 9's fetch will).
    async fn entries(&self, queue: QueueId, recipient_secret: &[u8; 32]) -> Option<usize> {
        let ts = self.now.load(Ordering::SeqCst);
        let signed = Signed::Recv { since_seq: 0 };
        let item = RecvItem {
            queue_id: BASE64URL_NOPAD.encode(&queue),
            since_seq: 0,
            ts,
            sig: BASE64URL_NOPAD
                .encode(&sign(recipient_secret, &signing_bytes(&signed, &queue, ts))),
        };
        match self
            .call(None, CommandName::Recv, vec![queue], vec![item])
            .await
            .unwrap()
            .remove(0)
        {
            ItemResult::Entries { entries } => Some(entries.len()),
            _ => None,
        }
    }
}

fn queue_ids(items: &[impl HasQueue]) -> Vec<QueueId> {
    items
        .iter()
        .map(|item| queue_wire::decode_array(item.queue_id()).unwrap_or([0; 32]))
        .collect()
}

trait HasQueue {
    fn queue_id(&self) -> &str;
}
macro_rules! has_queue {
    ($($t:ty),*) => {$(impl HasQueue for $t { fn queue_id(&self) -> &str { &self.queue_id } })*};
}
has_queue!(CreateItem, SendItem, RecvItem, AckItem, DeleteItem);

/// The server as the core with `clock` reaches it.
struct QueueFrom {
    server: Arc<FakeQueueServer>,
    clock: Arc<FakeClock>,
}

impl QueueFrom {
    fn call<T: serde::Serialize + HasQueue + Send + 'static>(
        &self,
        command: CommandName,
        items: Vec<T>,
    ) -> QueueFuture {
        let (server, clock) = (self.server.clone(), self.clock.clone());
        Box::pin(async move {
            let queues = queue_ids(&items);
            server.call(Some(&clock), command, queues, items).await
        })
    }
}

impl QueueServer for QueueFrom {
    fn create(&self, items: Vec<CreateItem>) -> QueueFuture {
        self.call(CommandName::Create, items)
    }
    fn send(&self, items: Vec<SendItem>) -> QueueFuture {
        self.call(CommandName::Send, items)
    }
    fn recv(&self, items: Vec<RecvItem>) -> QueueFuture {
        self.call(CommandName::Recv, items)
    }
    fn ack(&self, items: Vec<AckItem>) -> QueueFuture {
        self.call(CommandName::Ack, items)
    }
    fn delete(&self, items: Vec<DeleteItem>) -> QueueFuture {
        self.call(CommandName::Delete, items)
    }
}

/// The queue server every core on `net` reaches from now on.
fn queue_server(net: &Arc<Backend>) -> Arc<FakeQueueServer> {
    queue_server_with(net, queue_server::Limits::default())
}

/// The same, with `limits` instead of the spec's.
fn queue_server_with(net: &Arc<Backend>, limits: queue_server::Limits) -> Arc<FakeQueueServer> {
    let server = FakeQueueServer::new(limits);
    let at = server.clone();
    *net.queue.lock().unwrap() = Some(Arc::new(move |clock: &Arc<FakeClock>| {
        Arc::new(QueueFrom {
            server: at.clone(),
            clock: clock.clone(),
        }) as Arc<dyn QueueServer>
    }));
    server
}

/// A record of `account` listing `devices` that stays current for a week from a little after
/// `NOW`, so a device's wall clock can step days ahead (rotation, deletion).
fn week_record(account: u8, devices: &[u8]) -> SignedPacket {
    let issued = NOW + 300;
    packet(
        account,
        devices,
        account,
        issued,
        issued + 604_800,
        issued * 1_000_000,
    )
}

/// A's devices `seeds` and B1, as `accounts` makes them, with records that last a week.
async fn queue_accounts(seeds: &[u8]) -> (Vec<Fixture>, Fixture, Arc<Backend>, ConversationId) {
    accounts_with(seeds, week_record(A, seeds), week_record(B, &[B1])).await
}

async fn tables(f: &Fixture) -> QueueTables {
    f.account
        .storage_test_fixture()
        .queue_tables()
        .await
        .unwrap()
}

/// `f`'s `Active` queues.
async fn active(f: &Fixture) -> Vec<QueueOwnRow> {
    tables(f)
        .await
        .own
        .into_iter()
        .filter(|q| q.delete_at_ms.is_none())
        .collect()
}

/// `f`'s `Retiring` queues.
async fn retiring(f: &Fixture) -> Vec<QueueOwnRow> {
    tables(f)
        .await
        .own
        .into_iter()
        .filter(|q| q.delete_at_ms.is_some())
        .collect()
}

/// What `f` keeps of the queue `from` announced in the conversation: `None` without a row,
/// `Some(None)` for a withdrawal.
async fn peer_queue(
    f: &Fixture,
    conv: ConversationId,
    from: &Fixture,
) -> Option<Option<(String, QueueId, [u8; 32])>> {
    let from = installation(from).await;
    peer_row(f, conv, from).await.map(|r| r.queue)
}

async fn peer_row(
    f: &Fixture,
    conv: ConversationId,
    from: chat_core::account::InstallationId,
) -> Option<QueuePeerRow> {
    tables(f)
        .await
        .peers
        .into_iter()
        .find(|r| r.conversation == conv && r.installation == from)
}

/// `QueueRef` frames (kind 2) still queued on `f`.
async fn queue_ref_deliveries(f: &Fixture) -> usize {
    push_state(f)
        .await
        .deliveries
        .iter()
        .filter(|d| d.kind == 2)
        .count()
}

/// `QueueRef` frames (kind 2) still queued on `f` for `to`.
async fn queue_ref_deliveries_to(f: &Fixture, to: &Fixture) -> usize {
    let to = installation(to).await;
    push_state(f)
        .await
        .deliveries
        .iter()
        .filter(|d| d.kind == 2 && d.recipient == to)
        .count()
}

async fn verdict(f: &Fixture) -> Verdict {
    f.account.queue_state().await.unwrap().verdict
}

/// `f` uses the queue server at `url`.
fn configure(f: &Fixture, url: &str) {
    f.account.queue_configure(Some(url.into())).unwrap();
}

/// A1 and B1 with a conversation and a queue server, both configured with it; returns once
/// each keeps the other's announced queue and neither has anything left to deliver.
async fn announced() -> (
    Fixture,
    Fixture,
    Arc<Backend>,
    ConversationId,
    Arc<FakeQueueServer>,
) {
    announced_with(&[B1]).await
}

/// `announced`, with B's record listing `b_devices` (only B1 runs).
async fn announced_with(
    b_devices: &[u8],
) -> (
    Fixture,
    Fixture,
    Arc<Backend>,
    ConversationId,
    Arc<FakeQueueServer>,
) {
    let (mut a, b1, net, conv) =
        accounts_with(&[A1], week_record(A, &[A1]), week_record(B, b_devices)).await;
    let a1 = a.remove(0);
    let server = queue_server(&net);
    configure(&a1, QUEUE_URL);
    configure(&b1, QUEUE_URL);
    // Between the two devices that run (a device of B's record that never runs keeps its
    // deliveries).
    settle!(
        [&a1, &b1],
        peer_queue(&b1, conv, &a1).await.flatten().is_some()
            && peer_queue(&a1, conv, &b1).await.flatten().is_some()
            && queue_ref_deliveries_to(&a1, &b1).await == 0
            && queue_ref_deliveries_to(&b1, &a1).await == 0
    );
    (a1, b1, net, conv, server)
}

/// The queue entry of `wire`, a frame exactly as `from` writes it to a device it reaches
/// directly, naming `from` as its sender: what Task 10 puts in a queue.
async fn entry_of(from: &Fixture, wire: Vec<u8>) -> Vec<u8> {
    QueueEntry {
        sender_peer: from.peer,
        sender_install: installation(from).await,
        frame: wire,
    }
    .encode()
}

/// The frame `from`'s attempts write for its text `key` (its outbox wire), sealed now if no
/// attempt sealed it yet (sealing a sealed row gives its ciphertext back).
async fn sealed_wire(from: &Fixture, key: MessageKey) -> Vec<u8> {
    from.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_seal(key, NOW * 1000)
        .await
        .unwrap()
        .unwrap();
    outbox(from, key).await.wire
}

/// The queue entry of `from`'s text `key`.
async fn text_entry(from: &Fixture, key: MessageKey) -> Vec<u8> {
    let wire = sealed_wire(from, key).await;
    entry_of(from, wire).await
}

/// The queue entry of the Commit (`kind` 1) or Welcome (2) `from` has queued for `to` at
/// `epoch`: the frame its delivery writes.
async fn delivery_entry(
    from: &Fixture,
    conv: ConversationId,
    to: &Fixture,
    kind: u8,
    epoch: u64,
) -> Vec<u8> {
    let wire = from
        .account
        .storage_test_fixture()
        .mls_delivery_frame(conv, installation(to).await, kind, epoch)
        .await
        .unwrap()
        .expect("a queued delivery");
    entry_of(from, wire).await
}
