//! The SQLite file (spec 9.5). One connection on one worker thread: every
//! request is one job and one transaction there, so writes never contend and
//! no network wait ever sits inside a transaction.
//!
//! Stored: `queue` (id, two public keys, last use), `entry` (queue id, seq,
//! received time, ciphertext hash, ciphertext) and `acked` (queue id, hash of
//! an acked ciphertext, when to drop it). Nothing else.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use queue_wire::{BASE64URL_NOPAD, Entry, MAX_BODY, MAX_ITEMS, MAX_UNACKED, QueueId, TS_SKEW_MS};
use rusqlite::{Connection, ErrorCode, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};

const SCHEMA_VERSION: i64 = 1;
const SCHEMA: &str = "
CREATE TABLE queue (
    id BLOB NOT NULL PRIMARY KEY,
    recipient_pub BLOB NOT NULL,
    sender_pub BLOB NOT NULL,
    last_used_ms INTEGER NOT NULL
);
CREATE INDEX queue_last_used ON queue (last_used_ms);
CREATE TABLE entry (
    queue_id BLOB NOT NULL,
    seq INTEGER NOT NULL,
    received_ms INTEGER NOT NULL,
    ct_hash BLOB NOT NULL,
    ct BLOB NOT NULL,
    PRIMARY KEY (queue_id, seq),
    UNIQUE (queue_id, ct_hash)
);
CREATE INDEX entry_received ON entry (received_ms);
CREATE TABLE acked (
    queue_id BLOB NOT NULL,
    ct_hash BLOB NOT NULL,
    drop_at_ms INTEGER NOT NULL,
    PRIMARY KEY (queue_id, ct_hash)
);
CREATE INDEX acked_drop ON acked (drop_at_ms);
";

const KEYS: &str = "SELECT recipient_pub, sender_pub FROM queue WHERE id = ?1";
const INSERT_QUEUE: &str = "INSERT INTO queue (id, recipient_pub, sender_pub, last_used_ms)
     VALUES (?1, ?2, ?3, ?4) ON CONFLICT (id) DO NOTHING";
const TOUCH: &str = "UPDATE queue SET last_used_ms = max(last_used_ms, ?2) WHERE id = ?1";
/// The next seq, which is also the queue's new last use: see [`Tx::send`].
const NEXT_SEQ: &str = "UPDATE queue SET last_used_ms = max(?2, last_used_ms + 1) WHERE id = ?1 RETURNING last_used_ms";
/// A SEND of a ciphertext the queue holds renews its hold from now (spec 9.3): its sender put
/// it in again because its own copy of the hold ran out.
const RENEW_ENTRY: &str =
    "UPDATE entry SET received_ms = max(received_ms, ?3) WHERE queue_id = ?1 AND ct_hash = ?2";
const HAS_ACKED: &str =
    "SELECT 1 FROM acked WHERE queue_id = ?1 AND ct_hash = ?2 AND drop_at_ms > ?3";
const COUNT_ENTRIES: &str = "SELECT count(*) FROM entry WHERE queue_id = ?1";
const USED_BYTES: &str = "SELECT (page_count - freelist_count) * page_size
     FROM pragma_page_count, pragma_freelist_count, pragma_page_size";
const INSERT_ENTRY: &str = "INSERT INTO entry (queue_id, seq, received_ms, ct_hash, ct)
     VALUES (?1, ?2, ?3, ?4, ?5)";
const ENTRIES_AFTER: &str =
    "SELECT seq, received_ms, ct FROM entry WHERE queue_id = ?1 AND seq > ?2 ORDER BY seq";
const ACK_ENTRIES: &str = "DELETE FROM entry WHERE queue_id = ?1 AND seq <= ?2 RETURNING ct_hash";
const INSERT_ACKED: &str = "INSERT INTO acked (queue_id, ct_hash, drop_at_ms) VALUES (?1, ?2, ?3)
     ON CONFLICT (queue_id, ct_hash) DO UPDATE SET drop_at_ms = max(drop_at_ms, excluded.drop_at_ms)";
const DELETE_ENTRIES: &str = "DELETE FROM entry WHERE queue_id = ?1";
const DELETE_ACKED: &str = "DELETE FROM acked WHERE queue_id = ?1";
const DELETE_QUEUE: &str = "DELETE FROM queue WHERE id = ?1";
const SWEEP_ENTRIES: &str = "DELETE FROM entry WHERE rowid IN
     (SELECT rowid FROM entry WHERE received_ms <= ?1 LIMIT ?2)";
const SWEEP_ACKED: &str = "DELETE FROM acked WHERE rowid IN
     (SELECT rowid FROM acked WHERE drop_at_ms <= ?1 LIMIT ?2)";
const IDLE_QUEUES: &str = "SELECT id FROM queue WHERE last_used_ms <= ?1 LIMIT ?2";

/// Every statement a request or the sweep runs, for the query-plan test.
#[cfg(test)]
const STATEMENTS: [&str; 18] = [
    KEYS,
    INSERT_QUEUE,
    TOUCH,
    NEXT_SEQ,
    RENEW_ENTRY,
    HAS_ACKED,
    COUNT_ENTRIES,
    USED_BYTES,
    INSERT_ENTRY,
    ENTRIES_AFTER,
    ACK_ENTRIES,
    INSERT_ACKED,
    DELETE_ENTRIES,
    DELETE_ACKED,
    DELETE_QUEUE,
    SWEEP_ENTRIES,
    SWEEP_ACKED,
    IDLE_QUEUES,
];

/// Jobs waiting for the worker. A request waits for a slot within its timeout.
const JOB_QUEUE: usize = 64;
/// An acked hash outlives every signature its SEND could carry (`ts` ± skew).
const ACKED_KEEP_MS: u64 = 2 * TS_SKEW_MS;
/// Room in a RECV answer for the JSON around the entries: every result's
/// envelope and the outer object.
const ANSWER_RESERVE: usize = 64 * MAX_ITEMS + 64;
/// The JSON of one entry around its base64 `ct`: two u64 fields, the keys and
/// the comma before the next entry. Checked against serde in the tests.
const ENTRY_JSON: usize = 64;

/// A RECV answer's budget: the JSON stays within [`MAX_BODY`].
pub(crate) const RECV_BUDGET: usize = MAX_BODY - ANSWER_RESERVE;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StoreError {
    /// SQLite's error code; never a message, which could quote a value.
    Sqlite(ErrorCode),
    /// A non-SQLite failure inside rusqlite (a type conversion).
    Database,
    /// The file holds a schema version this build does not know.
    Schema,
    /// The worker thread is gone.
    Stopped,
    /// The job panicked; its transaction rolled back and the worker goes on.
    Panicked,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Sqlite(code) => write!(f, "sqlite {code:?}"),
            StoreError::Database => f.write_str("database error"),
            StoreError::Schema => f.write_str("unknown schema version"),
            StoreError::Stopped => f.write_str("store worker stopped"),
            StoreError::Panicked => f.write_str("store job panicked"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        match e {
            rusqlite::Error::SqliteFailure(e, _) => StoreError::Sqlite(e.code),
            _ => StoreError::Database,
        }
    }
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// Handle to the worker. Clones share it; the worker ends when the last goes.
#[derive(Clone)]
pub struct Store {
    jobs: mpsc::Sender<Job>,
}

impl Store {
    /// Opens or creates the file and starts the worker.
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        let mut conn = Connection::open(path)?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        // WAL + NORMAL: a process crash loses nothing; an OS crash can lose
        // the last commits, which senders send again after the entry TTL.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.set_prepared_statement_cache_capacity(32);
        migrate(&mut conn)?;
        let (jobs, queue) = mpsc::channel(JOB_QUEUE);
        std::thread::Builder::new()
            .name("queue-store".to_owned())
            .spawn(move || work(conn, queue))
            .map_err(|_| StoreError::Stopped)?;
        Ok(Store { jobs })
    }

    /// Runs `f` in one transaction on the worker: committed if it returns
    /// `Ok`, rolled back otherwise.
    pub(crate) async fn run<R, F>(&self, f: F) -> Result<R, StoreError>
    where
        R: Send + 'static,
        F: FnOnce(&Tx) -> rusqlite::Result<R> + Send + 'static,
    {
        let (reply, answer) = oneshot::channel();
        let job: Job = Box::new(move |conn| {
            let result = catch_unwind(AssertUnwindSafe(|| in_transaction(conn, f)))
                .unwrap_or(Err(StoreError::Panicked));
            // The request may have timed out and gone; nothing to tell then.
            let _ = reply.send(result);
        });
        self.jobs.send(job).await.map_err(|_| StoreError::Stopped)?;
        answer.await.map_err(|_| StoreError::Stopped)?
    }
}

fn work(mut conn: Connection, mut queue: mpsc::Receiver<Job>) {
    while let Some(job) = queue.blocking_recv() {
        job(&mut conn);
    }
}

fn in_transaction<R>(
    conn: &mut Connection,
    f: impl FnOnce(&Tx) -> rusqlite::Result<R>,
) -> Result<R, StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let result = f(&Tx(&tx))?;
    tx.commit()?;
    Ok(result)
}

/// Creates the schema on a new file; refuses a file from an unknown version.
fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    match version {
        0 => {
            tx.execute_batch(SCHEMA)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        SCHEMA_VERSION => {}
        _ => return Err(StoreError::Schema),
    }
    tx.commit()?;
    Ok(())
}

pub(crate) struct Keys {
    pub(crate) recipient_pub: [u8; 32],
    pub(crate) sender_pub: [u8; 32],
}

pub(crate) enum Sent {
    /// Stored, or already there (as an entry or a recently acked one).
    Ok,
    Full,
    /// The file is at its size cap.
    OverCap,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct SweepCounts {
    pub entries: u64,
    pub acked: u64,
    pub queues: u64,
}

impl SweepCounts {
    pub(crate) fn add(&mut self, other: SweepCounts) {
        self.entries += other.entries;
        self.acked += other.acked;
        self.queues += other.queues;
    }

    pub(crate) fn is_empty(&self) -> bool {
        *self == SweepCounts::default()
    }
}

/// One transaction's view of the store: the only SQL in the server.
pub(crate) struct Tx<'a>(&'a Connection);

/// SQLite integers are i64; wire values past i64::MAX mean "everything".
fn int(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn uint(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

impl Tx<'_> {
    pub(crate) fn keys(&self, id: &QueueId) -> rusqlite::Result<Option<Keys>> {
        self.0
            .prepare_cached(KEYS)?
            .query_row([id], |r| {
                Ok(Keys {
                    recipient_pub: r.get(0)?,
                    sender_pub: r.get(1)?,
                })
            })
            .optional()
    }

    /// `true` if the queue now has these keys (new, or made before with
    /// them); `false` if it exists with other keys.
    pub(crate) fn create(&self, id: &QueueId, keys: &Keys, now: u64) -> rusqlite::Result<bool> {
        let added = self.0.prepare_cached(INSERT_QUEUE)?.execute(params![
            id,
            keys.recipient_pub,
            keys.sender_pub,
            int(now)
        ])?;
        if added == 1 {
            return Ok(true);
        }
        let same = self.keys(id)?.is_some_and(|old| {
            old.recipient_pub == keys.recipient_pub && old.sender_pub == keys.sender_pub
        });
        if same {
            self.touch(id, now)?;
        }
        Ok(same)
    }

    fn touch(&self, id: &QueueId, now: u64) -> rusqlite::Result<()> {
        self.0
            .prepare_cached(TOUCH)?
            .execute(params![id, int(now)])?;
        Ok(())
    }

    /// Stores `ct` once per queue; a `ct` it holds is held from `now` again, under the same seq.
    /// A queue's seq is a millisecond stamp that
    /// only rises: `max(now, last_used_ms + 1)`, kept as the new
    /// `last_used_ms`. Since `last_used_ms` never falls, a drained queue or a
    /// clock stepping back still gives a seq above every one handed out.
    pub(crate) fn send(
        &self,
        id: &QueueId,
        ct: &[u8],
        now: u64,
        max_bytes: u64,
    ) -> rusqlite::Result<Sent> {
        let hash: [u8; 32] = Sha256::digest(ct).into();
        let stored = self
            .0
            .prepare_cached(RENEW_ENTRY)?
            .execute(params![id, hash, int(now)])?
            > 0;
        let acked = self
            .0
            .prepare_cached(HAS_ACKED)?
            .exists(params![id, hash, int(now)])?;
        if stored || acked {
            self.touch(id, now)?;
            return Ok(Sent::Ok);
        }
        let count: i64 = self
            .0
            .prepare_cached(COUNT_ENTRIES)?
            .query_row([id], |r| r.get(0))?;
        if uint(count) >= MAX_UNACKED as u64 {
            self.touch(id, now)?;
            return Ok(Sent::Full);
        }
        let used: i64 = self
            .0
            .prepare_cached(USED_BYTES)?
            .query_row([], |r| r.get(0))?;
        if uint(used).saturating_add(ct.len() as u64) > max_bytes {
            self.touch(id, now)?;
            return Ok(Sent::OverCap);
        }
        let seq: i64 = self
            .0
            .prepare_cached(NEXT_SEQ)?
            .query_row(params![id, int(now)], |r| r.get(0))?;
        self.0
            .prepare_cached(INSERT_ENTRY)?
            .execute(params![id, seq, int(now), hash, ct])?;
        Ok(Sent::Ok)
    }

    /// Entries after `since` in seq order, as many as `budget` (JSON bytes)
    /// holds. `None`: not even the first one fits; the client asks again.
    pub(crate) fn recv(
        &self,
        id: &QueueId,
        since: u64,
        now: u64,
        budget: &mut usize,
    ) -> rusqlite::Result<Option<Vec<Entry>>> {
        self.touch(id, now)?;
        let mut stmt = self.0.prepare_cached(ENTRIES_AFTER)?;
        let mut rows = stmt.query(params![id, int(since)])?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next()? {
            let ct = row.get_ref(2)?.as_blob()?;
            let cost = BASE64URL_NOPAD.encode_len(ct.len()) + ENTRY_JSON;
            if cost > *budget {
                return Ok((!entries.is_empty()).then_some(entries));
            }
            *budget -= cost;
            entries.push(Entry {
                seq: uint(row.get(0)?),
                ct: BASE64URL_NOPAD.encode(ct),
                ts: uint(row.get(1)?),
            });
        }
        Ok(Some(entries))
    }

    /// Drops entries up to `upto` and keeps their hashes so a replayed SEND
    /// of one is not stored again.
    pub(crate) fn ack(&self, id: &QueueId, upto: u64, now: u64) -> rusqlite::Result<()> {
        let hashes = self
            .0
            .prepare_cached(ACK_ENTRIES)?
            .query_map(params![id, int(upto)], |r| r.get::<_, [u8; 32]>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let drop_at = int(now.saturating_add(ACKED_KEEP_MS));
        let mut insert = self.0.prepare_cached(INSERT_ACKED)?;
        for hash in hashes {
            insert.execute(params![id, hash, drop_at])?;
        }
        self.touch(id, now)
    }

    pub(crate) fn delete(&self, id: &QueueId) -> rusqlite::Result<()> {
        for sql in [DELETE_ENTRIES, DELETE_ACKED, DELETE_QUEUE] {
            self.0.prepare_cached(sql)?.execute([id])?;
        }
        Ok(())
    }

    /// One bounded step of the sweep: at most `batch` rows of each kind.
    pub(crate) fn sweep(&self, now: u64, batch: usize) -> rusqlite::Result<SweepCounts> {
        let batch = int(batch as u64);
        let expired = int(now.saturating_sub(queue_wire::ENTRY_TTL_MS));
        let entries = self
            .0
            .prepare_cached(SWEEP_ENTRIES)?
            .execute(params![expired, batch])?;
        let acked = self
            .0
            .prepare_cached(SWEEP_ACKED)?
            .execute(params![int(now), batch])?;
        let idle_since = int(now.saturating_sub(queue_wire::IDLE_DELETE_MS));
        let idle = self
            .0
            .prepare_cached(IDLE_QUEUES)?
            .query_map(params![idle_since, batch], |r| r.get::<_, QueueId>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in &idle {
            self.delete(id)?;
        }
        Ok(SweepCounts {
            entries: entries as u64,
            acked: acked as u64,
            queues: idle.len() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = Connection::open(dir.path().join("q.db")).unwrap();
        migrate(&mut conn).unwrap();
        (dir, conn)
    }

    #[test]
    fn every_statement_uses_an_index() {
        let (_dir, conn) = open();
        for sql in STATEMENTS {
            let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let plan: Vec<String> = stmt
                .query_map(
                    params![[0u8; 32], 0, 0, [0u8; 32], [0u8; 32]]
                        .get(..stmt.parameter_count())
                        .unwrap(),
                    |r| r.get(3),
                )
                .unwrap()
                .map(Result::unwrap)
                .collect();
            for step in &plan {
                // A bare `SCAN <table>` reads every row; pragma functions are
                // one-row virtual tables.
                let full_scan = step.starts_with("SCAN ")
                    && !step.contains("INDEX")
                    && !step.starts_with("SCAN pragma_");
                assert!(!full_scan, "{sql}\n{plan:?}");
            }
        }
    }

    #[test]
    fn reopen_keeps_schema_and_unknown_version_is_refused() {
        let (dir, mut conn) = open();
        migrate(&mut conn).unwrap();
        conn.pragma_update(None, "user_version", 9).unwrap();
        drop(conn);
        assert_eq!(
            Store::open(&dir.path().join("q.db")).err(),
            Some(StoreError::Schema)
        );
    }

    #[test]
    fn entry_json_covers_the_largest_entry_around_its_ct() {
        let entry = Entry {
            seq: u64::MAX,
            ct: String::new(),
            ts: u64::MAX,
        };
        let json = serde_json::to_vec(&entry).unwrap();
        assert!(json.len() + ",".len() <= ENTRY_JSON, "{}", json.len());
    }

    #[tokio::test]
    async fn a_panicking_job_is_reported_as_a_panic_and_the_worker_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("q.db")).unwrap();
        let panicked = store
            .run(|_| -> rusqlite::Result<()> { panic!("test panic") })
            .await;
        assert_eq!(panicked.err(), Some(StoreError::Panicked));
        assert_eq!(StoreError::Panicked.to_string(), "store job panicked");
        let id = [7; 32];
        let created = store
            .run(move |tx| {
                let keys = Keys {
                    recipient_pub: [1; 32],
                    sender_pub: [2; 32],
                };
                tx.create(&id, &keys, 0)
            })
            .await;
        assert_eq!(created, Ok(true));
    }

    #[test]
    fn recv_budget_leaves_room_for_the_largest_entry() {
        let one = BASE64URL_NOPAD.encode_len(queue_wire::MAX_CT) + ENTRY_JSON;
        assert!(one <= RECV_BUDGET);
    }
}
