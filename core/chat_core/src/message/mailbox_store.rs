//! The device log (spec 5.1, 5.2): every kind 17 frame this device accepted or sent, in the
//! order it processed them, for its other devices to pull. Entries are written inside the
//! transaction that accepted or sealed the frame, so a rolled-back message leaves no entry and a
//! committed one always has its entry.
//!
//! Each other own device has a cursor here (`mailbox_cursor`: the first seq it has not taken)
//! and this device keeps how far it took each of theirs (`mailbox_pull`). Entries every cursor
//! has passed, and entries older than `MAILBOX_RETENTION_MS`, are pruned.
use super::mls_group::sql;
use super::mls_store::{Fault, finish};
use super::store::{Identity, MessageDbReply, bytes, integer, invalid, uint};
use super::types::{ConversationId, MessageId};
use crate::CoreError;
use crate::account::{InstallationId, PeerId};
use crate::mailbox::{
    MAILBOX_RETENTION_MS, PAGE_MAX_ENTRIES,
    wire::{EntryBody, LogEntry, MAX_ENTRY_BODY, page},
};
use crate::mls::wire::MlsFrame;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

type R<T> = Result<T, CoreError>;

/// `mailbox_store.kind`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    /// A kind 17 frame inner body.
    Frame = 1,
    Delivered = 2,
    HandoffStop = 3,
}

pub(crate) struct NewEntry<'a> {
    pub conversation: ConversationId,
    pub sender_peer: PeerId,
    /// The device that originally sent it (its MLS leaf), not the session it came over.
    pub sender_install: InstallationId,
    pub kind: EntryKind,
    /// The message of an App Text, or the one a note is about.
    pub msg_id: Option<MessageId>,
    pub body: &'a [u8],
    pub now: u64,
}

/// Appends one entry; `None` when the same body is already logged, or when it is too large to
/// travel in one `SyncResp` page (`MAX_ENTRY_BODY`), which kind 17 frames never are.
pub(crate) fn record(tx: &Transaction, entry: &NewEntry) -> R<Option<u64>> {
    if entry.body.len() > MAX_ENTRY_BODY {
        return Ok(None);
    }
    let hash: [u8; 32] = Sha256::digest(entry.body).into();
    let added = tx
        .execute(
            "INSERT INTO mailbox_store(conversation,sender_peer,sender_install,kind,msg_id,hash,body,recorded_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(hash) DO NOTHING",
            params![
                entry.conversation.0.as_slice(),
                entry.sender_peer.0.as_slice(),
                entry.sender_install.0.as_slice(),
                entry.kind as i64,
                entry.msg_id.as_ref().map(|id| id.0.as_slice()),
                hash.as_slice(),
                entry.body,
                integer(entry.now)?
            ],
        )
        .map_err(sql)?;
    Ok((added > 0).then(|| tx.last_insert_rowid() as u64))
}

/// Logs a kind 17 frame (its inner body) that this device accepted or sent.
pub(super) fn record_frame(
    tx: &Transaction,
    sender_peer: PeerId,
    sender_install: InstallationId,
    msg_id: Option<MessageId>,
    frame: &MlsFrame,
    now: u64,
) -> R<Option<u64>> {
    let conversation = match frame {
        MlsFrame::App { conversation, .. }
        | MlsFrame::Commit { conversation, .. }
        | MlsFrame::Welcome { conversation, .. } => *conversation,
        _ => return Err(invalid()),
    };
    record(
        tx,
        &NewEntry {
            conversation,
            sender_peer,
            sender_install,
            kind: EntryKind::Frame,
            msg_id,
            body: &frame.inner(),
            now,
        },
    )
}

/// Log, cursor and retention operations, each one SQL transaction.
pub(crate) enum MailboxDbOp {
    /// The page another own device `puller` asked for with `SyncReq { since_seq }`. A device
    /// with no cursor yet starts at the head (Ruling 4); the caller has checked that it is one
    /// of this account's devices and not revoked.
    Page {
        puller: InstallationId,
        since_seq: u64,
        now: u64,
    },
    /// This device processed `holder`'s log up to `next_seq` (exclusive).
    Pulled {
        holder: InstallationId,
        next_seq: u64,
    },
    /// Where the next pull from the holder starts.
    PullFrom(InstallationId),
    /// The account's other devices that are not revoked: new ones get a cursor at the head,
    /// revoked ones lose everything kept for them; then the log is pruned.
    Devices {
        others: Vec<InstallationId>,
        now: u64,
    },
    /// Drops what every cursor passed and what is older than the retention (now).
    Prune(u64),
}
impl MailboxDbOp {
    pub(crate) fn writes(&self) -> bool {
        !matches!(self, Self::PullFrom(_))
    }
}

pub struct MailboxPage {
    /// In `seq` order, as many as one `SyncResp` carries.
    pub entries: Vec<LogEntry>,
    /// Last entry's seq + 1, or (no entries) where the request was moved up to.
    pub next_seq: u64,
    pub more: bool,
}

pub(crate) enum MailboxDbReply {
    Page(MailboxPage),
    Since(u64),
    /// Log entries pruned.
    Count(u64),
}

pub(super) fn mailbox_op(
    c: &mut Connection,
    identity: Identity,
    op: MailboxDbOp,
    fault: Fault,
) -> R<MessageDbReply> {
    let reply = match op {
        MailboxDbOp::Page {
            puller,
            since_seq,
            now,
        } => {
            let tx = c.transaction().map_err(sql)?;
            let page = serve_page(&tx, puller, since_seq, now)?;
            finish(tx, fault)?;
            MailboxDbReply::Page(page)
        }
        MailboxDbOp::Pulled { holder, next_seq } => {
            let tx = c.transaction().map_err(sql)?;
            tx.execute(
                "INSERT INTO mailbox_pull VALUES(?1,?2) ON CONFLICT(installation) DO UPDATE SET since_seq=max(since_seq,excluded.since_seq)",
                params![holder.0.as_slice(), integer(next_seq)?],
            )
            .map_err(sql)?;
            let since = pulled_since(&tx, holder)?;
            finish(tx, fault)?;
            MailboxDbReply::Since(since)
        }
        MailboxDbOp::PullFrom(holder) => MailboxDbReply::Since(pulled_since(c, holder)?),
        MailboxDbOp::Devices { others, now } => {
            // This device never pulls from itself.
            let others: Vec<InstallationId> = others
                .into_iter()
                .filter(|i| *i != identity.installation)
                .collect();
            let tx = c.transaction().map_err(sql)?;
            let head = high_water(&tx)? + 1;
            for device in &others {
                tx.execute(
                    "INSERT INTO mailbox_cursor VALUES(?1,?2) ON CONFLICT(installation) DO NOTHING",
                    params![device.0.as_slice(), integer(head)?],
                )
                .map_err(sql)?;
                tx.execute(
                    "INSERT INTO mailbox_pull VALUES(?1,0) ON CONFLICT(installation) DO NOTHING",
                    params![device.0.as_slice()],
                )
                .map_err(sql)?;
            }
            let known: Vec<[u8; 32]> = {
                let mut q = tx
                    .prepare("SELECT installation FROM mailbox_cursor UNION SELECT installation FROM mailbox_pull UNION SELECT installation FROM mailbox_device UNION SELECT origin_install FROM mailbox_handoff")
                    .map_err(sql)?;
                q.query_map([], |r| bytes::<32>(r, 0))
                    .map_err(sql)?
                    .collect::<Result<_, _>>()
                    .map_err(sql)?
            };
            for revoked in known
                .iter()
                .filter(|i| !others.contains(&InstallationId(**i)))
            {
                for statement in [
                    "DELETE FROM mailbox_cursor WHERE installation=?1",
                    "DELETE FROM mailbox_pull WHERE installation=?1",
                    "DELETE FROM mailbox_device WHERE installation=?1",
                    "DELETE FROM mailbox_handoff WHERE origin_install=?1",
                ] {
                    tx.execute(statement, params![revoked.as_slice()])
                        .map_err(sql)?;
                }
            }
            let pruned = prune(&tx, now)?;
            finish(tx, fault)?;
            MailboxDbReply::Count(pruned)
        }
        MailboxDbOp::Prune(now) => {
            let tx = c.transaction().map_err(sql)?;
            let pruned = prune(&tx, now)?;
            finish(tx, fault)?;
            MailboxDbReply::Count(pruned)
        }
    };
    Ok(MessageDbReply::Mailbox(reply))
}

/// The largest seq ever handed out (`AUTOINCREMENT` never reuses one, even after the log was
/// pruned empty); 0 before the first entry.
pub(super) fn high_water(c: &Connection) -> R<u64> {
    c.query_row(
        "SELECT seq FROM sqlite_sequence WHERE name='mailbox_store'",
        [],
        |r| uint(r, 0),
    )
    .optional()
    .map_err(sql)
    .map(Option::unwrap_or_default)
}

fn pulled_since(c: &Connection, holder: InstallationId) -> R<u64> {
    c.query_row(
        "SELECT since_seq FROM mailbox_pull WHERE installation=?1",
        params![holder.0.as_slice()],
        |r| uint(r, 0),
    )
    .optional()
    .map_err(sql)
    .map(Option::unwrap_or_default)
}

fn serve_page(
    tx: &Transaction,
    puller: InstallationId,
    since_seq: u64,
    now: u64,
) -> R<MailboxPage> {
    let cursor: Option<u64> = tx
        .query_row(
            "SELECT next_seq FROM mailbox_cursor WHERE installation=?1",
            params![puller.0.as_slice()],
            |r| uint(r, 0),
        )
        .optional()
        .map_err(sql)?;
    let Some(cursor) = cursor else {
        // An own device whose link this device has not seen yet: it starts at the head, the
        // way `Devices` starts it.
        let head = high_water(tx)? + 1;
        tx.execute(
            "INSERT INTO mailbox_cursor VALUES(?1,?2)",
            params![puller.0.as_slice(), integer(head)?],
        )
        .map_err(sql)?;
        return Ok(MailboxPage {
            entries: Vec::new(),
            next_seq: head,
            more: false,
        });
    };
    // Nothing below the cursor is served again, and nothing below the oldest kept entry exists
    // any more (retention may have passed this cursor): the page starts at the larger.
    let oldest: Option<i64> = tx
        .query_row("SELECT min(seq) FROM mailbox_store", [], |r| r.get(0))
        .map_err(sql)?;
    let oldest = match oldest {
        Some(seq) => u64::try_from(seq).map_err(|_| invalid())?,
        // Pruned empty: the next entry is the first there is.
        None => high_water(tx)? + 1,
    };
    let from = since_seq.max(cursor).max(oldest);
    let rows: Vec<LogEntry> = {
        let mut q = tx
            .prepare("SELECT seq,conversation,sender_peer,sender_install,kind,body,recorded_at_ms FROM mailbox_store WHERE seq>=?1 ORDER BY seq LIMIT ?2")
            .map_err(sql)?;
        q.query_map(
            params![integer(from)?, PAGE_MAX_ENTRIES as i64 + 1],
            |r| {
                Ok((
                    uint(r, 0)?,
                    bytes::<32>(r, 1)?,
                    bytes::<32>(r, 2)?,
                    bytes::<32>(r, 3)?,
                    uint(r, 4)?,
                    r.get::<_, Vec<u8>>(5)?,
                    uint(r, 6)?,
                ))
            },
        )
        .map_err(sql)?
        .map(|row| {
            let (seq, conversation, peer, install, kind, body, at) = row.map_err(sql)?;
            // Only frames are logged so far; the note kinds get their body codec with the
            // tasks that record them.
            if kind != EntryKind::Frame as u64 {
                return Err(invalid());
            }
            Ok(LogEntry {
                seq,
                conversation: ConversationId(conversation),
                sender_peer: PeerId(peer),
                sender_install: InstallationId(install),
                recorded_at_ms: at,
                body: EntryBody::Frame(body),
            })
        })
        .collect::<R<_>>()?
    };
    let (entries, more) = page(rows.into_iter());
    let next_seq = entries.last().map_or(from, |e| e.seq + 1);
    // `since_seq` says the puller has everything before it.
    if since_seq > cursor {
        tx.execute(
            "UPDATE mailbox_cursor SET next_seq=?2 WHERE installation=?1",
            params![puller.0.as_slice(), integer(since_seq)?],
        )
        .map_err(sql)?;
        prune(tx, now)?;
    }
    Ok(MailboxPage {
        entries,
        next_seq,
        more,
    })
}

/// Spec 5.2: an entry goes once every (not revoked) other device's cursor passed it, or when it
/// is older than the retention; with no other device nothing holds it. Notes waiting for their
/// message and handed-off ciphertexts go after the same 30 days.
fn prune(tx: &Transaction, now: u64) -> R<u64> {
    let before = integer(now.saturating_sub(MAILBOX_RETENTION_MS))?;
    let pruned = tx
        .execute(
            "DELETE FROM mailbox_store WHERE seq<coalesce((SELECT min(next_seq) FROM mailbox_cursor),9223372036854775807) OR recorded_at_ms<?1",
            params![before],
        )
        .map_err(sql)?;
    tx.execute(
        "DELETE FROM mailbox_status_ahead WHERE at_ms<?1",
        params![before],
    )
    .map_err(sql)?;
    tx.execute(
        "DELETE FROM mailbox_handoff WHERE received_at_ms<?1",
        params![before],
    )
    .map_err(sql)?;
    Ok(pruned as u64)
}

// ---- test-hooks fixtures -------------------------------------------------------------------

/// One `mailbox_store` row, for fixture assertions.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxRow {
    pub seq: u64,
    pub conversation: ConversationId,
    pub sender_peer: PeerId,
    pub sender_install: InstallationId,
    pub kind: u8,
    pub msg_id: Option<MessageId>,
    pub body: Vec<u8>,
    pub recorded_at_ms: u64,
}

/// The log and what is kept per other device, for fixture assertions.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxState {
    pub entries: Vec<MailboxRow>,
    /// `(installation, next_seq)`, by installation.
    pub cursors: Vec<(InstallationId, u64)>,
    /// `(installation, since_seq)`, by installation.
    pub pulls: Vec<(InstallationId, u64)>,
    pub devices: u64,
    pub handoffs: u64,
    pub status_ahead: u64,
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_state(c: &Connection) -> R<MailboxState> {
    let entries = {
        let mut q = c
            .prepare("SELECT seq,conversation,sender_peer,sender_install,kind,msg_id,body,recorded_at_ms FROM mailbox_store ORDER BY seq")
            .map_err(sql)?;
        q.query_map([], |r| {
            Ok(MailboxRow {
                seq: uint(r, 0)?,
                conversation: ConversationId(bytes(r, 1)?),
                sender_peer: PeerId(bytes(r, 2)?),
                sender_install: InstallationId(bytes(r, 3)?),
                kind: uint(r, 4)? as u8,
                msg_id: r
                    .get::<_, Option<Vec<u8>>>(5)?
                    .map(|id| MessageId(id.try_into().unwrap())),
                body: r.get(6)?,
                recorded_at_ms: uint(r, 7)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?
    };
    let pairs = |table: &str, column: &str| -> R<Vec<(InstallationId, u64)>> {
        let mut q = c
            .prepare(&format!(
                "SELECT installation,{column} FROM {table} ORDER BY installation"
            ))
            .map_err(sql)?;
        q.query_map([], |r| Ok((InstallationId(bytes(r, 0)?), uint(r, 1)?)))
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)
    };
    let count = |table: &str| -> R<u64> {
        c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| uint(r, 0))
            .map_err(sql)
    };
    Ok(MailboxState {
        entries,
        cursors: pairs("mailbox_cursor", "next_seq")?,
        pulls: pairs("mailbox_pull", "since_seq")?,
        devices: count("mailbox_device")?,
        handoffs: count("mailbox_handoff")?,
        status_ahead: count("mailbox_status_ahead")?,
    })
}

/// Records a `Frame` entry with fixed ids and the given body.
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_record(c: &mut Connection, body: &[u8], now: u64) -> R<Option<u64>> {
    let ids = (
        ConversationId([1; 32]),
        PeerId([2; 32]),
        InstallationId([3; 32]),
    );
    fixture_record_as(c, ids, body, now)
}

/// Records a `Frame` entry of the given conversation, sender account and installation.
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_record_as(
    c: &mut Connection,
    (conversation, sender_peer, sender_install): (ConversationId, PeerId, InstallationId),
    body: &[u8],
    now: u64,
) -> R<Option<u64>> {
    let tx = c.transaction().map_err(sql)?;
    let seq = record(
        &tx,
        &NewEntry {
            conversation,
            sender_peer,
            sender_install,
            kind: EntryKind::Frame,
            msg_id: None,
            body,
            now,
        },
    )?;
    tx.commit().map_err(sql)?;
    Ok(seq)
}

/// A `mailbox_device` row and a `mailbox_handoff` row of `installation`, and one
/// `mailbox_status_ahead` row, all at `at`.
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_seed(c: &Connection, installation: InstallationId, at: u64) -> R<()> {
    let at = integer(at)?;
    let i = installation.0.as_slice();
    c.execute(
        "INSERT INTO mailbox_device VALUES(?1,1,?2)",
        params![i, at],
    )
    .map_err(sql)?;
    c.execute(
        "INSERT INTO mailbox_handoff VALUES(?1,?2,?3,?4,x'01',0,?5,?5)",
        params![i, [1u8; 32].as_slice(), [2u8; 16].as_slice(), [4u8; 32].as_slice(), at],
    )
    .map_err(sql)?;
    c.execute(
        "INSERT INTO mailbox_status_ahead VALUES(?1,?2,?3,1,?4)",
        params![[1u8; 32].as_slice(), [4u8; 32].as_slice(), [2u8; 16].as_slice(), at],
    )
    .map_err(sql)?;
    Ok(())
}
