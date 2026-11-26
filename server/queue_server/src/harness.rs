//! Debug-only read of the queue file for the m8 end-to-end scenario (cargo feature
//! `harness-counts`): how many queues it holds and how many entries each holds, most first.
//! Counts only: no queue id, key or ciphertext. The harness runs it beside the server it
//! started (`queue_server --harness-counts`, `QUEUE_DB`); it opens the file read-only, which
//! WAL allows while the server writes.

#[cfg(not(debug_assertions))]
compile_error!("the harness-counts feature is for debug builds only");

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::StoreError;

/// The entries each queue holds, most first (a queue with none counts as 0).
pub fn held_counts(path: &Path) -> Result<Vec<u64>, StoreError> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = conn.prepare(
        "SELECT COUNT(entry.seq) FROM queue LEFT JOIN entry ON entry.queue_id = queue.id \
         GROUP BY queue.id ORDER BY 1 DESC",
    )?;
    let counts = statement
        .query_map([], |row| row.get::<_, i64>(0))?
        .map(|count| count.map(|n| n.unsigned_abs()))
        .collect::<Result<_, _>>()?;
    Ok(counts)
}

/// The line `--harness-counts` prints: `{"queues":N,"entries":[...]}`.
pub fn counts_line(entries: &[u64]) -> String {
    serde_json::json!({ "queues": entries.len(), "entries": entries }).to_string()
}
