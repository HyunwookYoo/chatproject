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
use chat_core::queue::{QueueError, QueueFuture, QueueServer};
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
/// the command, and the queue of each item.
#[derive(Clone)]
struct Call {
    from: Option<usize>,
    command: CommandName,
    queues: Vec<QueueId>,
}

struct FakeQueueServer {
    app: axum::Router,
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
}

impl FakeQueueServer {
    fn new(limits: queue_server::Limits) -> Arc<Self> {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("queue.db");
        let store = queue_server::Store::open(&db).unwrap();
        let now = Arc::new(AtomicU64::new(NOW * 1_000));
        let mut state = queue_server::AppState::new(store);
        state.limits = limits;
        let clock = now.clone();
        state.clock = Arc::new(move || clock.load(Ordering::SeqCst));
        state.log = Arc::new(|_: &str| {});
        Arc::new(Self {
            app: queue_server::router(state),
            db,
            _dir: dir,
            now,
            turn: tokio::sync::Mutex::new(()),
            calls: Mutex::new(Vec::new()),
            hold: tokio::sync::watch::channel(false).0,
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
        self.calls.lock().unwrap().push(Call {
            from: from.map(|clock| Arc::as_ptr(clock) as usize),
            command,
            queues,
        });
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
        Ok(response.results)
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

async fn verdict(f: &Fixture) -> Verdict {
    f.account.queue_state().await.unwrap().verdict
}

/// `f` uses the queue server at `url`.
fn configure(f: &Fixture, url: &str) {
    f.account.queue_configure(Some(url.into())).unwrap();
}
