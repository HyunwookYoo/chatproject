//! `/q/*` through the real router on a temp SQLite file, with a fake clock.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use queue_server::{AppState, Limits, MAX_CONNECTIONS, Store, router, serve_limits, sweep};
use queue_wire::{
    AckItem, BASE64URL_NOPAD, CommandName, CreateItem, DeleteItem, ENTRY_TTL_MS, Entry,
    IDLE_DELETE_MS, ItemResult, MAX_BODY, MAX_CT, MAX_ITEMS, MAX_UNACKED, QueueId, QueueRequest,
    QueueResponse, RecvItem, SendItem, Signed, TS_SKEW_MS, public_key, sign, signing_bytes,
};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

const T0: u64 = 1_800_000_000_000;
const PEER: [u8; 4] = [192, 0, 2, 10];

fn b64(bytes: &[u8]) -> String {
    BASE64URL_NOPAD.encode(bytes)
}

/// A queue as its two key holders see it.
#[derive(Clone, Copy)]
struct Q {
    id: QueueId,
    recipient: [u8; 32],
    sender: [u8; 32],
}

impl Q {
    fn new(n: u16) -> Self {
        let [hi, lo] = n.to_be_bytes();
        let mut id = [0x51; 32];
        id[0] = hi;
        id[1] = lo;
        let mut recipient = [0x52; 32];
        recipient[0] = hi;
        recipient[1] = lo;
        let mut sender = [0x53; 32];
        sender[0] = hi;
        sender[1] = lo;
        Q {
            id,
            recipient,
            sender,
        }
    }

    fn sig(&self, secret: &[u8; 32], signed: &Signed, ts: u64) -> String {
        b64(&sign(secret, &signing_bytes(signed, &self.id, ts)))
    }

    fn create(&self, ts: u64) -> CreateItem {
        let sender_pub = public_key(&self.sender);
        CreateItem {
            queue_id: b64(&self.id),
            recipient_pub: b64(&public_key(&self.recipient)),
            sender_pub: b64(&sender_pub),
            ts,
            sig: self.sig(
                &self.recipient,
                &Signed::Create {
                    sender_pub: &sender_pub,
                },
                ts,
            ),
        }
    }

    fn send(&self, ct: &[u8], ts: u64) -> SendItem {
        SendItem {
            queue_id: b64(&self.id),
            ct: b64(ct),
            ts,
            sig: self.sig(&self.sender, &Signed::Send { ct }, ts),
        }
    }

    fn recv(&self, since_seq: u64, ts: u64) -> RecvItem {
        RecvItem {
            queue_id: b64(&self.id),
            since_seq,
            ts,
            sig: self.sig(&self.recipient, &Signed::Recv { since_seq }, ts),
        }
    }

    fn ack(&self, upto_seq: u64, ts: u64) -> AckItem {
        AckItem {
            queue_id: b64(&self.id),
            upto_seq,
            ts,
            sig: self.sig(&self.recipient, &Signed::Ack { upto_seq }, ts),
        }
    }

    fn delete(&self, ts: u64) -> DeleteItem {
        DeleteItem {
            queue_id: b64(&self.id),
            ts,
            sig: self.sig(&self.recipient, &Signed::Delete, ts),
        }
    }
}

struct Server {
    app: Router,
    store: Store,
    now: Arc<AtomicU64>,
    log: Arc<Mutex<Vec<String>>>,
    path: PathBuf,
    _dir: Arc<tempfile::TempDir>,
}

fn start(limits: Limits) -> Server {
    let dir = Arc::new(tempfile::tempdir().unwrap());
    let path = dir.path().join("queue.db");
    start_on(dir, path, limits, T0)
}

fn start_on(dir: Arc<tempfile::TempDir>, path: PathBuf, limits: Limits, now: u64) -> Server {
    let store = Store::open(&path).unwrap();
    let now = Arc::new(AtomicU64::new(now));
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut state = AppState::new(store.clone());
    let clock = now.clone();
    state.clock = Arc::new(move || clock.load(Ordering::SeqCst));
    state.limits = limits;
    state.trust_proxy = true;
    let sink = log.clone();
    state.log = Arc::new(move |line: &str| sink.lock().unwrap().push(line.to_owned()));
    Server {
        app: router(state),
        store,
        now,
        log,
        path,
        _dir: dir,
    }
}

impl Server {
    /// The same DB file under a new process-like state: new store, router and buckets.
    fn restart(self) -> Server {
        let Server {
            path, _dir, now, ..
        } = self;
        start_on(_dir, path, Limits::default(), now.load(Ordering::SeqCst))
    }

    fn now(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    fn set_now(&self, ms: u64) {
        self.now.store(ms, Ordering::SeqCst);
    }

    async fn call_from(
        &self,
        ip: Option<&str>,
        path: &str,
        body: Vec<u8>,
    ) -> (StatusCode, Vec<u8>) {
        let mut req = Request::post(path).header(header::CONTENT_TYPE, "application/json");
        if let Some(ip) = ip {
            req = req.header("x-forwarded-for", ip);
        }
        let mut req = req.body(Body::from(body)).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((PEER, 40_000))));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, bytes)
    }

    async fn post_from<T: Serialize>(
        &self,
        ip: Option<&str>,
        cmd: CommandName,
        items: Vec<T>,
    ) -> Vec<ItemResult> {
        let n = items.len();
        let (status, body) = self
            .call_from(
                ip,
                cmd.path(),
                serde_json::to_vec(&QueueRequest { items }).unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let resp: QueueResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp.results.len(), n);
        resp.results
    }

    async fn post<T: Serialize>(&self, cmd: CommandName, items: Vec<T>) -> Vec<ItemResult> {
        self.post_from(None, cmd, items).await
    }

    async fn create(&self, q: &Q) -> ItemResult {
        one(self
            .post(CommandName::Create, vec![q.create(self.now())])
            .await)
    }

    async fn send(&self, q: &Q, ct: &[u8]) -> ItemResult {
        one(self
            .post(CommandName::Send, vec![q.send(ct, self.now())])
            .await)
    }

    async fn recv(&self, q: &Q, since: u64) -> ItemResult {
        one(self
            .post(CommandName::Recv, vec![q.recv(since, self.now())])
            .await)
    }

    /// The entries a RECV from `since` returns; panics on any other result.
    async fn entries(&self, q: &Q, since: u64) -> Vec<Entry> {
        match self.recv(q, since).await {
            ItemResult::Entries { entries } => entries,
            other => panic!("RECV gave {other:?}"),
        }
    }

    async fn ack(&self, q: &Q, upto: u64) -> ItemResult {
        one(self
            .post(CommandName::Ack, vec![q.ack(upto, self.now())])
            .await)
    }

    async fn delete(&self, q: &Q) -> ItemResult {
        one(self
            .post(CommandName::Delete, vec![q.delete(self.now())])
            .await)
    }

    fn log_text(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }

    /// Rows in a table, read through a second connection on the same file.
    fn rows(&self, table: &str) -> i64 {
        let db = rusqlite::Connection::open(&self.path).unwrap();
        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}

fn one(mut results: Vec<ItemResult>) -> ItemResult {
    assert_eq!(results.len(), 1);
    results.remove(0)
}

fn cts(entries: &[Entry]) -> Vec<Vec<u8>> {
    entries
        .iter()
        .map(|e| BASE64URL_NOPAD.decode(e.ct.as_bytes()).unwrap())
        .collect()
}

fn unlimited_sends() -> Limits {
    Limits {
        send_per_minute: 100_000,
        send_burst: 100_000,
        ..Limits::default()
    }
}

#[tokio::test]
async fn create_send_recv_ack_delete_round_trip() {
    let s = start(Limits::default());
    let q = Q::new(1);
    assert_eq!(s.create(&q).await, ItemResult::Ok);
    assert_eq!(s.send(&q, b"first").await, ItemResult::Ok);
    s.set_now(T0 + 5);
    assert_eq!(s.send(&q, b"second").await, ItemResult::Ok);

    let entries = s.entries(&q, 0).await;
    assert_eq!(cts(&entries), vec![b"first".to_vec(), b"second".to_vec()]);
    assert_eq!((entries[0].ts, entries[1].ts), (T0, T0 + 5));
    assert!(entries[0].seq < entries[1].seq);
    assert_eq!(s.entries(&q, entries[0].seq).await, entries[1..].to_vec());

    assert_eq!(s.ack(&q, entries[0].seq).await, ItemResult::Ok);
    assert_eq!(s.entries(&q, 0).await, entries[1..].to_vec());

    assert_eq!(s.delete(&q).await, ItemResult::Ok);
    assert_eq!(s.recv(&q, 0).await, ItemResult::Gone);
    assert_eq!(s.send(&q, b"third").await, ItemResult::Gone);
    assert_eq!(s.ack(&q, 99).await, ItemResult::Gone);
    // DELETE converges: a second one, or one for a queue never made, is `ok`.
    assert_eq!(s.delete(&q).await, ItemResult::Ok);
    assert_eq!(s.delete(&Q::new(2)).await, ItemResult::Ok);
    for table in ["queue", "entry", "acked"] {
        assert_eq!(s.rows(table), 0, "{table}");
    }
}

#[tokio::test]
async fn seq_keeps_rising_after_the_queue_is_drained() {
    // A client asks from the last seq it saw; a drained queue must not restart lower.
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    s.send(&q, b"a").await;
    let first = s.entries(&q, 0).await[0].seq;
    s.ack(&q, first).await;
    // Same millisecond, and then the clock stepping back an hour.
    s.send(&q, b"b").await;
    let second = s.entries(&q, first).await;
    assert_eq!(cts(&second), vec![b"b".to_vec()]);
    s.ack(&q, second[0].seq).await;
    s.set_now(T0 - 3_600_000);
    s.send(&q, b"c").await;
    assert_eq!(
        cts(&s.entries(&q, second[0].seq).await),
        vec![b"c".to_vec()]
    );
}

#[tokio::test]
async fn create_is_idempotent_and_other_keys_exist() {
    let s = start(Limits::default());
    let q = Q::new(1);
    assert_eq!(s.create(&q).await, ItemResult::Ok);
    assert_eq!(s.create(&q).await, ItemResult::Ok);
    s.send(&q, b"kept").await;
    assert_eq!(s.create(&q).await, ItemResult::Ok);

    let other_sender = Q {
        sender: [0x77; 32],
        ..q
    };
    let other_recipient = Q {
        recipient: [0x78; 32],
        ..q
    };
    assert_eq!(s.create(&other_sender).await, ItemResult::Exists);
    assert_eq!(s.create(&other_recipient).await, ItemResult::Exists);

    // The first keys still hold the queue; the others cannot use it.
    assert_eq!(s.send(&other_sender, b"x").await, ItemResult::Bad);
    assert_eq!(s.recv(&other_recipient, 0).await, ItemResult::Bad);
    assert_eq!(cts(&s.entries(&q, 0).await), vec![b"kept".to_vec()]);
    assert_eq!(s.rows("queue"), 1);
}

#[tokio::test]
async fn bad_signature_old_ts_and_wrong_key_are_bad_per_item() {
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    let now = s.now();

    let mut tampered = q.send(b"tampered", now);
    tampered.ct = b64(b"tampereD");
    let by_recipient = SendItem {
        sig: q.sig(&q.recipient, &Signed::Send { ct: b"wrong key" }, now),
        ..q.send(b"wrong key", now)
    };
    let not_base64 = SendItem {
        queue_id: "!".repeat(43),
        ..q.send(b"bad id", now)
    };
    let short_sig = SendItem {
        sig: b64(&[1; 63]),
        ..q.send(b"short sig", now)
    };
    let results = s
        .post(
            CommandName::Send,
            vec![
                q.send(b"good", now),
                tampered,
                q.send(b"old", now - TS_SKEW_MS - 1),
                q.send(b"future", now + TS_SKEW_MS + 1),
                by_recipient,
                not_base64,
                short_sig,
                q.send(b"edge old", now - TS_SKEW_MS),
                q.send(b"edge new", now + TS_SKEW_MS),
            ],
        )
        .await;
    use ItemResult::{Bad, Ok};
    assert_eq!(results, vec![Ok, Bad, Bad, Bad, Bad, Bad, Bad, Ok, Ok]);
    assert_eq!(
        cts(&s.entries(&q, 0).await),
        vec![b"good".to_vec(), b"edge old".to_vec(), b"edge new".to_vec()]
    );

    // RECV, ACK and DELETE signed by the sender key; CREATE with a bad signature.
    let by_sender = RecvItem {
        sig: q.sig(&q.sender, &Signed::Recv { since_seq: 0 }, now),
        ..q.recv(0, now)
    };
    assert_eq!(one(s.post(CommandName::Recv, vec![by_sender]).await), Bad);
    let ack = AckItem {
        sig: q.sig(&q.sender, &Signed::Ack { upto_seq: u64::MAX }, now),
        ..q.ack(u64::MAX, now)
    };
    assert_eq!(one(s.post(CommandName::Ack, vec![ack]).await), Bad);
    let delete = DeleteItem {
        sig: q.sig(&q.sender, &Signed::Delete, now),
        ..q.delete(now)
    };
    assert_eq!(one(s.post(CommandName::Delete, vec![delete]).await), Bad);
    let q2 = Q::new(2);
    let mut create = q2.create(now);
    create.sender_pub = b64(&public_key(&[0x99; 32]));
    assert_eq!(one(s.post(CommandName::Create, vec![create]).await), Bad);
    assert_eq!(s.recv(&q2, 0).await, ItemResult::Gone);
    assert_eq!(
        s.entries(&q, 0).await.len(),
        3,
        "nothing was acked or deleted"
    );
}

#[tokio::test]
async fn same_ciphertext_stored_once() {
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    assert_eq!(s.send(&q, b"once").await, ItemResult::Ok);
    s.set_now(T0 + 1_000);
    // A fresh signature over the same ciphertext, and the same item replayed.
    assert_eq!(s.send(&q, b"once").await, ItemResult::Ok);
    assert_eq!(s.send(&q, b"once").await, ItemResult::Ok);
    assert_eq!(cts(&s.entries(&q, 0).await), vec![b"once".to_vec()]);

    // Another queue keeps its own copy.
    let q2 = Q::new(2);
    s.create(&q2).await;
    assert_eq!(s.send(&q2, b"once").await, ItemResult::Ok);
    assert_eq!(s.entries(&q2, 0).await.len(), 1);
}

#[tokio::test]
async fn send_replayed_after_ack_is_not_stored_again() {
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    let captured = q.send(b"replayed", s.now());
    assert_eq!(
        one(s.post(CommandName::Send, vec![captured.clone()]).await),
        ItemResult::Ok
    );
    let seq = s.entries(&q, 0).await[0].seq;
    assert_eq!(s.ack(&q, seq).await, ItemResult::Ok);

    // Until its signature runs out, the captured SEND is answered and dropped.
    s.set_now(T0 + TS_SKEW_MS);
    assert_eq!(
        one(s.post(CommandName::Send, vec![captured]).await),
        ItemResult::Ok
    );
    assert_eq!(s.entries(&q, 0).await, vec![]);
    sweep(&s.store, s.now()).await.unwrap();
    assert_eq!(s.send(&q, b"replayed").await, ItemResult::Ok);
    assert_eq!(s.entries(&q, 0).await, vec![]);

    // The acked hash lives 2 x TS_SKEW_MS; after that a new SEND of it is a new entry.
    s.set_now(T0 + 2 * TS_SKEW_MS);
    sweep(&s.store, s.now()).await.unwrap();
    assert_eq!(s.rows("acked"), 0);
    assert_eq!(s.send(&q, b"replayed").await, ItemResult::Ok);
    assert_eq!(cts(&s.entries(&q, 0).await), vec![b"replayed".to_vec()]);
}

#[tokio::test]
async fn empty_ct_is_bad() {
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    assert_eq!(s.send(&q, b"").await, ItemResult::Bad);
    assert_eq!(s.entries(&q, 0).await, vec![]);
}

#[tokio::test]
async fn unacked_limit_256_then_full() {
    let s = start(unlimited_sends());
    let q = Q::new(1);
    s.create(&q).await;
    let now = s.now();
    let items: Vec<SendItem> = (0..MAX_UNACKED as u32)
        .map(|i| q.send(&i.to_be_bytes(), now))
        .collect();
    for chunk in items.chunks(MAX_ITEMS) {
        let results = s.post(CommandName::Send, chunk.to_vec()).await;
        assert!(results.iter().all(|r| *r == ItemResult::Ok));
    }
    assert_eq!(s.send(&q, b"one more").await, ItemResult::Full);
    // A copy of a stored entry is still `ok`: it is already there.
    assert_eq!(s.send(&q, &0u32.to_be_bytes()).await, ItemResult::Ok);

    let first = s.entries(&q, 0).await[0].seq;
    s.ack(&q, first).await;
    assert_eq!(s.send(&q, b"one more").await, ItemResult::Ok);
    assert_eq!(s.send(&q, b"and another").await, ItemResult::Full);
    assert_eq!(s.rows("entry"), MAX_UNACKED as i64);
}

#[tokio::test]
async fn ct_over_64k_is_bad() {
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    let now = s.now();
    let results = s
        .post(
            CommandName::Send,
            vec![
                q.send(&vec![1; MAX_CT], now),
                q.send(&vec![2; MAX_CT + 1], now),
            ],
        )
        .await;
    assert_eq!(results, vec![ItemResult::Ok, ItemResult::Bad]);
    assert_eq!(cts(&s.entries(&q, 0).await), vec![vec![1; MAX_CT]]);
}

#[tokio::test]
async fn ack_deletes_and_recv_after_ack_is_empty() {
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    for ct in [b"a", b"b", b"c"] {
        s.send(&q, ct).await;
    }
    let entries = s.entries(&q, 0).await;
    assert_eq!(s.ack(&q, entries[1].seq).await, ItemResult::Ok);
    assert_eq!(s.rows("entry"), 1);
    assert_eq!(s.ack(&q, entries[2].seq).await, ItemResult::Ok);
    assert_eq!(s.entries(&q, 0).await, vec![]);
    assert_eq!(s.rows("entry"), 0);
    // ACK again converges.
    assert_eq!(s.ack(&q, entries[2].seq).await, ItemResult::Ok);
    assert_eq!(s.ack(&q, u64::MAX).await, ItemResult::Ok);
}

#[tokio::test]
async fn entries_expire_after_24h() {
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    s.send(&q, b"old").await;
    s.set_now(T0 + 1);
    s.send(&q, b"newer").await;

    s.set_now(T0 + ENTRY_TTL_MS - 1);
    sweep(&s.store, s.now()).await.unwrap();
    assert_eq!(s.entries(&q, 0).await.len(), 2);

    s.set_now(T0 + ENTRY_TTL_MS);
    let swept = sweep(&s.store, s.now()).await.unwrap();
    assert_eq!(swept.entries, 1);
    assert_eq!(cts(&s.entries(&q, 0).await), vec![b"newer".to_vec()]);
    assert_eq!(s.rows("entry"), 1);
}

/// Spec 9.3: a SEND of a ciphertext the queue still holds (its sender put it in again after
/// its own 24 h) holds it 24 h from that SEND, though the hourly sweep had not run yet. Same
/// entry, same seq: the receiver never took it.
#[tokio::test]
async fn duplicate_send_renews_the_hold() {
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    assert_eq!(s.send(&q, b"kept").await, ItemResult::Ok);
    let seq = s.entries(&q, 0).await[0].seq;

    let again = T0 + ENTRY_TTL_MS + 2 * 60_000;
    s.set_now(again);
    assert_eq!(s.send(&q, b"kept").await, ItemResult::Ok);
    s.set_now(T0 + ENTRY_TTL_MS + 50 * 60_000);
    sweep(&s.store, s.now()).await.unwrap();
    let entries = s.entries(&q, 0).await;
    assert_eq!(cts(&entries), vec![b"kept".to_vec()]);
    assert_eq!(entries[0].seq, seq);

    s.set_now(again + ENTRY_TTL_MS);
    sweep(&s.store, s.now()).await.unwrap();
    assert_eq!(s.rows("entry"), 0);
}

#[tokio::test]
async fn queue_unused_30_days_is_deleted() {
    let s = start(Limits::default());
    let (idle, used) = (Q::new(1), Q::new(2));
    s.create(&idle).await;
    s.create(&used).await;
    s.set_now(T0 + IDLE_DELETE_MS - 1);
    // Any command counts as use; RECV is the one a receiving device sends most.
    s.recv(&used, 0).await;

    s.set_now(T0 + IDLE_DELETE_MS);
    let swept = sweep(&s.store, s.now()).await.unwrap();
    assert_eq!(swept.queues, 1);
    assert_eq!(s.recv(&idle, 0).await, ItemResult::Gone);
    assert_eq!(s.entries(&used, 0).await, vec![]);
    assert_eq!(s.rows("queue"), 1);
}

#[tokio::test]
async fn global_byte_cap_limits_send() {
    let s = start(Limits {
        max_bytes: 400_000,
        ..unlimited_sends()
    });
    let q = Q::new(1);
    s.create(&q).await;
    let mut results = Vec::new();
    for i in 0..10u8 {
        results.push(s.send(&q, &vec![i; MAX_CT]).await);
    }
    let stored = results.iter().take_while(|r| **r == ItemResult::Ok).count();
    assert!((3..10).contains(&stored), "{results:?}");
    assert!(results[stored..].iter().all(|r| *r == ItemResult::Limited));

    // ACK frees the space for new entries.
    let last = s.entries(&q, 0).await.last().unwrap().seq;
    s.ack(&q, last).await;
    assert_eq!(s.send(&q, &vec![42; MAX_CT]).await, ItemResult::Ok);
}

#[tokio::test]
async fn restart_keeps_queues_and_entries() {
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    s.send(&q, b"acked").await;
    s.send(&q, b"kept").await;
    let entries = s.entries(&q, 0).await;
    s.ack(&q, entries[0].seq).await;

    let s = s.restart();
    assert_eq!(s.entries(&q, 0).await, entries[1..].to_vec());
    assert_eq!(s.create(&q).await, ItemResult::Ok);
    // The acked hash survives too, and seq keeps rising.
    assert_eq!(s.send(&q, b"acked").await, ItemResult::Ok);
    s.send(&q, b"after").await;
    let after = s.entries(&q, entries[1].seq).await;
    assert_eq!(cts(&after), vec![b"after".to_vec()]);
}

#[tokio::test]
async fn stored_tables_and_columns_are_exactly_spec_9_5() {
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    s.send(&q, b"x").await;
    let db = rusqlite::Connection::open(&s.path).unwrap();

    let mut stmt = db
        .prepare("SELECT type, name, tbl_name FROM sqlite_master ORDER BY type, name")
        .unwrap();
    let objects: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let want: Vec<(String, String, String)> = [
        ("index", "acked_drop", "acked"),
        ("index", "entry_received", "entry"),
        ("index", "queue_last_used", "queue"),
        ("index", "sqlite_autoindex_acked_1", "acked"),
        ("index", "sqlite_autoindex_entry_1", "entry"),
        ("index", "sqlite_autoindex_entry_2", "entry"),
        ("index", "sqlite_autoindex_queue_1", "queue"),
        ("table", "acked", "acked"),
        ("table", "entry", "entry"),
        ("table", "queue", "queue"),
    ]
    .iter()
    .map(|(a, b, c)| ((*a).to_owned(), (*b).to_owned(), (*c).to_owned()))
    .collect();
    assert_eq!(objects, want);

    // Spec 9.5: queue (id, two public keys, last use), entry (queue id, seq,
    // received time, ciphertext hash, ciphertext), acked hash (queue id, hash,
    // drop time). `ct` is last so reading the other columns skips its pages.
    for (table, columns) in [
        (
            "queue",
            &["id", "recipient_pub", "sender_pub", "last_used_ms"][..],
        ),
        (
            "entry",
            &["queue_id", "seq", "received_ms", "ct_hash", "ct"][..],
        ),
        ("acked", &["queue_id", "ct_hash", "drop_at_ms"][..]),
    ] {
        let mut stmt = db
            .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
            .unwrap();
        let got: Vec<String> = stmt
            .query_map([table], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(got, columns, "{table}");
    }
}

#[tokio::test]
async fn logs_hold_no_ids_keys_sigs_cts_or_ips() {
    let s = start(Limits::default());
    let q = Q::new(1);
    let ct = vec![0xab; 1_234];
    let create = q.create(s.now());
    let send = q.send(&ct, s.now());
    let recv = q.recv(0, s.now());
    let ack = q.ack(u64::MAX, s.now());
    let delete = q.delete(s.now());
    let forbidden = [
        create.queue_id.clone(),
        create.recipient_pub.clone(),
        create.sender_pub.clone(),
        create.sig.clone(),
        send.sig.clone(),
        send.ct[..40].to_owned(),
        recv.sig.clone(),
        ack.sig.clone(),
        delete.sig.clone(),
        "198.51.100.23".to_owned(),
        "192.0.2.10".to_owned(),
        "1234".to_owned(),
    ];
    let ip = Some("198.51.100.23");
    s.post_from(ip, CommandName::Create, vec![create]).await;
    s.post_from(ip, CommandName::Send, vec![send]).await;
    s.post_from(ip, CommandName::Recv, vec![recv]).await;
    s.post_from(ip, CommandName::Ack, vec![ack]).await;
    s.post_from(ip, CommandName::Delete, vec![delete]).await;
    s.call_from(ip, "/q/secret-path-value", b"{}".to_vec())
        .await;
    sweep(&s.store, s.now()).await.unwrap();

    let log = s.log_text();
    for value in &forbidden {
        assert!(!log.contains(value.as_str()), "{value} in log:\n{log}");
    }
    assert!(!log.contains("secret-path-value"), "{log}");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 6, "{log}");
    for (line, path) in lines.iter().zip([
        "/q/create",
        "/q/send",
        "/q/recv",
        "/q/ack",
        "/q/delete",
        "other",
    ]) {
        assert!(
            line.starts_with(&format!("t={T0} path={path} status=")),
            "{line}"
        );
        assert!(line.contains(" ms="), "{line}");
    }
    assert!(lines[1].contains("items=1 ok=1"), "{}", lines[1]);
}

#[tokio::test]
async fn create_rate_limit_500_per_hour_burst_200() {
    let s = start(Limits::default());
    let a = Some("203.0.113.5");
    let items = |from: u16| -> Vec<CreateItem> {
        (from..from + 100).map(|n| Q::new(n).create(T0)).collect()
    };
    for from in [0, 100] {
        let results = s.post_from(a, CommandName::Create, items(from)).await;
        assert!(results.iter().all(|r| *r == ItemResult::Ok));
    }
    let results = s.post_from(a, CommandName::Create, items(200)).await;
    assert!(results.iter().all(|r| *r == ItemResult::Limited));
    assert_eq!(s.rows("queue"), 200);

    // Another client, and the same /64 counting as one.
    let b = Some("2001:db8:5:6::1");
    let results = s.post_from(b, CommandName::Create, items(300)).await;
    assert!(results.iter().all(|r| *r == ItemResult::Ok));
    let b_other = Some("2001:db8:5:6:ffff::9");
    let results = s.post_from(b_other, CommandName::Create, items(400)).await;
    assert_eq!(
        results.iter().filter(|r| **r == ItemResult::Ok).count(),
        100
    );
    let results = s.post_from(b_other, CommandName::Create, items(500)).await;
    assert!(results.iter().all(|r| *r == ItemResult::Limited));

    // 500 an hour: one more every 7.2 s.
    s.set_now(T0 + 7_200);
    let results = s
        .post_from(
            a,
            CommandName::Create,
            vec![Q::new(900).create(T0), Q::new(901).create(T0)],
        )
        .await;
    assert_eq!(results, vec![ItemResult::Ok, ItemResult::Limited]);
}

#[tokio::test]
async fn send_rate_limit_per_queue() {
    let s = start(Limits::default());
    let (q, q2) = (Q::new(1), Q::new(2));
    s.create(&q).await;
    s.create(&q2).await;
    let now = s.now();
    let items: Vec<SendItem> = (0..21u8).map(|i| q.send(&[i], now)).collect();
    let results = s.post(CommandName::Send, items).await;
    assert!(results[..20].iter().all(|r| *r == ItemResult::Ok));
    assert_eq!(results[20], ItemResult::Limited);
    // Another queue has its own budget.
    assert_eq!(s.send(&q2, b"x").await, ItemResult::Ok);
    // 60 a minute: one more every second.
    s.set_now(now + 1_000);
    assert_eq!(s.send(&q, b"later").await, ItemResult::Ok);
    assert_eq!(s.send(&q, b"too soon").await, ItemResult::Limited);
    assert_eq!(s.entries(&q, 0).await.len(), 21);
}

/// A CREATE whose signature does not match its keys.
fn badly_signed_create(q: &Q, ts: u64) -> CreateItem {
    CreateItem {
        sender_pub: b64(&public_key(&[0x99; 32])),
        ..q.create(ts)
    }
}

#[tokio::test]
async fn create_over_the_limit_is_limited_before_the_signature_is_checked() {
    let s = start(Limits::default());
    let a = Some("203.0.113.5");
    // Badly signed items spend the caller's own tokens...
    for from in [0, 100] {
        let items = (from..from + 100)
            .map(|n| badly_signed_create(&Q::new(n), T0))
            .collect();
        let results = s.post_from(a, CommandName::Create, items).await;
        assert!(results.iter().all(|r| *r == ItemResult::Bad));
    }
    // ...and past them an item is `limited`, signed well or not: a `bad`
    // here would mean its signature was checked.
    let results = s
        .post_from(
            a,
            CommandName::Create,
            vec![
                Q::new(200).create(T0),
                badly_signed_create(&Q::new(201), T0),
            ],
        )
        .await;
    assert_eq!(results, vec![ItemResult::Limited, ItemResult::Limited]);
    assert_eq!(s.rows("queue"), 0);
    // Another client still gets its signature checked.
    let b = Some("203.0.113.6");
    let results = s
        .post_from(
            b,
            CommandName::Create,
            vec![badly_signed_create(&Q::new(202), T0)],
        )
        .await;
    assert_eq!(results, vec![ItemResult::Bad]);
}

#[tokio::test]
async fn send_over_the_limit_is_limited_before_the_signature_is_checked() {
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    let now = s.now();
    let forged = |ct: &[u8]| SendItem {
        sig: q.sig(&q.recipient, &Signed::Send { ct }, now),
        ..q.send(ct, now)
    };
    let mut items: Vec<SendItem> = (0..20u8).map(|i| forged(&[i])).collect();
    items.push(q.send(b"good", now));
    items.push(forged(b"forged"));
    let results = s.post(CommandName::Send, items).await;
    assert!(results[..20].iter().all(|r| *r == ItemResult::Bad));
    assert_eq!(results[20..], [ItemResult::Limited, ItemResult::Limited]);
}

#[tokio::test]
async fn recv_rate_limit_per_client() {
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    let a = Some("203.0.113.5");
    let body = |s: &Server| {
        serde_json::to_vec(&QueueRequest {
            items: vec![q.recv(0, s.now())],
        })
        .unwrap()
    };
    for _ in 0..120 {
        let (status, _) = s.call_from(a, "/q/recv", body(&s)).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, _) = s.call_from(a, "/q/recv", body(&s)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    // Refused before the body is read: even an unparsable one gets 429.
    let (status, _) = s.call_from(a, "/q/recv", b"not json".to_vec()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // ACK and other clients keep their own buckets; 120 a minute refills one per 0.5 s.
    assert_eq!(
        one(s
            .post_from(a, CommandName::Ack, vec![q.ack(0, s.now())])
            .await),
        ItemResult::Ok
    );
    let (status, _) = s.call_from(Some("203.0.113.6"), "/q/recv", body(&s)).await;
    assert_eq!(status, StatusCode::OK);
    s.set_now(T0 + 500);
    let (status, _) = s.call_from(a, "/q/recv", body(&s)).await;
    assert_eq!(status, StatusCode::OK);
}

/// Final review I1: anyone can hold a queue id by creating a queue, so `DELETE` gets a
/// per-client request bucket taken before the body is read, as `RECV` and `ACK` have. Past its
/// burst a client's batches of forged signatures get 429 and cost the store worker no verify;
/// another client is served, and the queue is still there.
#[tokio::test]
async fn delete_rate_limit_per_client() {
    let s = start(Limits::default());
    let q = Q::new(1);
    s.create(&q).await;
    let a = Some("203.0.113.5");
    let forged = |s: &Server| {
        let mut item = q.delete(s.now());
        item.sig = q.sig(&q.sender, &Signed::Delete, s.now());
        serde_json::to_vec(&QueueRequest {
            items: vec![item; MAX_ITEMS],
        })
        .unwrap()
    };
    for _ in 0..10 {
        let (status, body) = s.call_from(a, "/q/delete", forged(&s)).await;
        assert_eq!(status, StatusCode::OK);
        let resp: QueueResponse = serde_json::from_slice(&body).unwrap();
        assert!(resp.results.iter().all(|r| *r == ItemResult::Bad));
    }
    let (status, _) = s.call_from(a, "/q/delete", forged(&s)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let (status, _) = s.call_from(a, "/q/delete", b"not json".to_vec()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // The other commands and other clients keep their own buckets; 10 a minute refills one
    // per 6 s.
    assert_eq!(
        one(s
            .post_from(a, CommandName::Send, vec![q.send(b"still here", s.now())])
            .await),
        ItemResult::Ok
    );
    assert_eq!(
        one(s
            .post_from(Some("203.0.113.6"), CommandName::Delete, vec![q.delete(s.now())])
            .await),
        ItemResult::Ok
    );
    assert_eq!(s.rows("queue"), 0);
    s.set_now(T0 + 6_000);
    let (status, _) = s.call_from(a, "/q/delete", forged(&s)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn recv_answer_fits_max_body_and_the_rest_is_limited() {
    let s = start(unlimited_sends());
    let queues: Vec<Q> = (0..MAX_ITEMS as u16).map(Q::new).collect();
    let now = s.now();
    s.post(
        CommandName::Create,
        queues.iter().map(|q| q.create(now)).collect(),
    )
    .await;
    // Two 64 KiB entries in each of 100 queues: 12.8 MiB, far over one answer.
    for i in 0..2u8 {
        let items: Vec<SendItem> = queues
            .iter()
            .map(|q| q.send(&vec![i; MAX_CT], now))
            .collect();
        for chunk in items.chunks(8) {
            s.post(CommandName::Send, chunk.to_vec()).await;
        }
    }
    let body = serde_json::to_vec(&QueueRequest {
        items: queues.iter().map(|q| q.recv(0, now)).collect::<Vec<_>>(),
    })
    .unwrap();
    let (status, answer) = s.call_from(None, "/q/recv", body).await;
    assert_eq!(status, StatusCode::OK);
    assert!(answer.len() <= MAX_BODY, "{} bytes", answer.len());
    let results = serde_json::from_slice::<QueueResponse>(&answer)
        .unwrap()
        .results;

    // Answered queues come first and are non-empty; the rest wait for the next RECV.
    let answered = results
        .iter()
        .take_while(|r| matches!(r, ItemResult::Entries { entries } if !entries.is_empty()))
        .count();
    assert!(answered >= 1);
    assert!(
        results[answered..]
            .iter()
            .all(|r| *r == ItemResult::Limited)
    );
    // A queue whose answer ran out of room still gets the ones that fit.
    let ItemResult::Entries { entries } = &results[answered - 1] else {
        unreachable!()
    };
    let resent = s
        .entries(&queues[answered - 1], entries.last().unwrap().seq)
        .await;
    assert_eq!(entries.len() + resent.len(), 2);
}

#[tokio::test]
async fn request_shape_errors() {
    let s = start(Limits::default());
    let (status, _) = s.call_from(None, "/q/send", b"not json".to_vec()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = s.call_from(None, "/q/send", b"{}".to_vec()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let q = Q::new(1);
    let over = QueueRequest {
        items: vec![q.delete(T0); MAX_ITEMS + 1],
    };
    let (status, _) = s
        .call_from(None, "/q/delete", serde_json::to_vec(&over).unwrap())
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = s.call_from(None, "/q/send", vec![b' '; MAX_BODY + 1]).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let empty = QueueRequest::<SendItem> { items: vec![] };
    assert_eq!(s.post(CommandName::Send, empty.items).await, vec![]);

    let req = Request::get("/q/recv").body(Body::empty()).unwrap();
    assert_eq!(
        s.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
}

/// Reads until the server closes `stream`; `None` if still open after `wait`.
async fn closed_within(stream: &mut tokio::net::TcpStream, wait: Duration) -> Option<Vec<u8>> {
    let mut got = Vec::new();
    tokio::time::timeout(wait, stream.read_to_end(&mut got))
        .await
        .ok()
        .map(|_| got)
}

#[tokio::test]
async fn connections_past_the_cap_are_closed() {
    let s = start(Limits::default());
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(http_guard::serve(listener, s.app.clone(), serve_limits()));
    let head = "POST /q/send HTTP/1.1
Host: q
";
    let mut held = Vec::new();
    for _ in 0..MAX_CONNECTIONS {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(head.as_bytes()).await.unwrap();
        held.push(stream);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut over = tokio::net::TcpStream::connect(addr).await.unwrap();
    let got = closed_within(&mut over, Duration::from_secs(2)).await;
    assert_eq!(
        got.as_deref(),
        Some(&b""[..]),
        "connection past the cap not closed"
    );

    // A freed slot serves again.
    drop(held.pop());
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = br#"{"items":[]}"#;
    let request = format!(
        "POST /q/send HTTP/1.1
Host: q
Content-Length: {}
Connection: close

",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let answer = closed_within(&mut stream, Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        answer.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&answer)
    );
}
