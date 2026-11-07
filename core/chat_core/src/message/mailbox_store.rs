//! The device log (spec 5.1, 5.2): every kind 17 frame this device accepted or sent, in the
//! order it processed them, for its other devices to pull. Entries are written inside the
//! transaction that accepted or sealed the frame, so a rolled-back message leaves no entry and a
//! committed one always has its entry.
//!
//! Each other own device has a cursor here (`mailbox_cursor`: the first seq it has not taken)
//! and this device keeps how far it took each of theirs (`mailbox_pull`). Entries every cursor
//! has passed, and entries older than `MAILBOX_RETENTION_MS`, are pruned.
//!
//! As a mailbox (spec 7), this device also keeps the ciphertexts its other devices handed over
//! (`mailbox_handoff`) with their retry state; how each attempt ended goes into the log as a
//! note for the device that handed it over.
use super::mls_group::sql;
use super::mls_store::{Fault, finish, store_plain};
use super::scheduler::retry_delay_capped;
use super::store::{
    Identity, MessageDbReply, bump, bytes, conversation_id, integer, invalid, uint,
};
use super::types::{ConversationId, MessageId};
use crate::CoreError;
use crate::account::{InstallationId, PeerId};
use crate::mailbox::{
    MAILBOX_MAX_DELAY_MS, MAILBOX_RETENTION_MS, PAGE_MAX_ENTRIES,
    wire::{
        EntryBody, HandoffEntry, LogEntry, MAX_ENTRY_BODY, PlainRecord, decode_note, note_body,
        page,
    },
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

/// Logs a note (`Delivered`) about message `msg_id` that `sender_install` of `sender_peer` sent.
pub(super) fn record_note(
    tx: &Transaction,
    conversation: ConversationId,
    sender_peer: PeerId,
    sender_install: InstallationId,
    body: &EntryBody,
    now: u64,
) -> R<Option<u64>> {
    let msg_id = match body {
        EntryBody::Delivered { msg_id, .. } | EntryBody::HandoffStop { msg_id, .. } => *msg_id,
        EntryBody::Frame(_) => return Err(invalid()),
    };
    let kind = match body {
        EntryBody::HandoffStop { .. } => EntryKind::HandoffStop,
        _ => EntryKind::Delivered,
    };
    let bytes = note_body(body).ok_or_else(invalid)?;
    record(
        tx,
        &NewEntry {
            conversation,
            sender_peer,
            sender_install,
            kind,
            msg_id: Some(msg_id),
            body: &bytes,
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
    /// A note pulled from another own device's log, applied (and logged here) at `now`.
    ApplyNote { entry: LogEntry, now: u64 },
    /// The plaintext record of entry `seq` of this device's log, for another own device that
    /// could not open it (spec 6.3; the caller checked the device): only an App Text entry
    /// whose message is still here has one.
    Plain { seq: u64 },
    /// A plaintext record another own device handed over for an entry of its log this device
    /// could not open, whose ciphertext hashes to `ct_hash`; `peer` is the conversation's other
    /// account. Stored like an opened text (with its `processed_ct` row), without log entry or
    /// receipt.
    StorePlain {
        record: PlainRecord,
        peer: PeerId,
        ct_hash: [u8; 32],
        now: u64,
    },
    /// This account's devices recorded as mailboxes (`mailbox_device.mailbox = 1`), the only
    /// ones unsent ciphertexts are handed to (spec 7, Ruling 12).
    Mailboxes,
    /// The conversations where this device has messages awaiting encryption (codec 3), to be
    /// sealed (`SealPending`) before a handoff.
    Unsealed,
    /// The sealed ciphertexts (codec 2) of this device's messages that are not delivered,
    /// cancelled, failed or waiting for the peer's update, in send order (spec 7).
    HandoffRows,
    /// Ciphertexts own device `origin` handed over, each due at `now`. Another ciphertext of a
    /// message already here replaces the one kept (its attempts start over); the same one again
    /// changes nothing. Entries whose conversation is not this account's with `peer` are not
    /// taken.
    TakeHandoff {
        origin: InstallationId,
        entries: Vec<HandoffEntry>,
        now: u64,
    },
    /// Handed-off ciphertexts due at `now` (at most `HANDOFF_DUE_MAX`).
    HandoffDue(u64),
    /// A session with `peer` is ready: its handed-off ciphertexts are due at `now`.
    HandoffWake { peer: PeerId, now: u64 },
    /// How the attempt of the ciphertext hashing to `ct_hash` that `origin` handed over for
    /// message `msg_id` ended; `now` is scheduler time, `wall` the time notes carry.
    HandoffResult {
        origin: InstallationId,
        conversation: ConversationId,
        msg_id: MessageId,
        ct_hash: [u8; 32],
        outcome: HandoffOutcome,
        now: u64,
        wall: u64,
    },
}
impl MailboxDbOp {
    pub(crate) fn writes(&self) -> bool {
        !matches!(
            self,
            Self::PullFrom(_)
                | Self::Plain { .. }
                | Self::Mailboxes
                | Self::Unsealed
                | Self::HandoffRows
                | Self::HandoffDue(_)
        )
    }
}

/// How one attempt of a handed-off ciphertext ended.
pub(crate) enum HandoffOutcome {
    /// The recipient `receiver` stored it: the row goes, and a `Delivered` note tells the origin.
    Acked { receiver: InstallationId },
    /// The recipient could not open it: it is not tried again, and a `HandoffStop` note tells
    /// the origin, which alone re-encrypts.
    Stop { class: u8, epoch: u64 },
    /// No answer, `Busy`, no session: tried again after the back-off (capped at 30 s).
    Retry,
}

/// Most handed-off ciphertexts one `HandoffDue` returns.
pub(crate) const HANDOFF_DUE_MAX: usize = 32;
/// `mailbox_handoff.next_retry_at_ms` of a ciphertext the recipient could not open: never due
/// again, kept so the same ciphertext handed over again is not tried again (until the origin
/// hands over a new one, or the row is pruned).
const STOPPED: i64 = i64::MAX;

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
    Plain(Option<PlainRecord>),
    /// The handed-over text was stored just now (false: already here, or not taken).
    Stored(bool),
    Installations(Vec<InstallationId>),
    Conversations(Vec<ConversationId>),
    Handoff(Vec<HandoffEntry>),
    /// Due handed-off ciphertexts with the device that handed each over, and when the next of
    /// the others is due (`None`: none is).
    HandoffDue {
        jobs: Vec<(InstallationId, HandoffEntry)>,
        next: Option<u64>,
    },
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
        MailboxDbOp::ApplyNote { entry, now } => {
            let tx = c.transaction().map_err(sql)?;
            let changed = apply_note(&tx, identity, &entry, now)?;
            finish(tx, fault)?;
            MailboxDbReply::Count(changed)
        }
        MailboxDbOp::Prune(now) => {
            let tx = c.transaction().map_err(sql)?;
            let pruned = prune(&tx, now)?;
            finish(tx, fault)?;
            MailboxDbReply::Count(pruned)
        }
        MailboxDbOp::Plain { seq } => MailboxDbReply::Plain(plain_record(c, seq)?),
        MailboxDbOp::StorePlain {
            record,
            peer,
            ct_hash,
            now,
        } => {
            let tx = c.transaction().map_err(sql)?;
            let stored = store_plain(&tx, identity, &record, peer, &ct_hash, now)?;
            finish(tx, fault)?;
            MailboxDbReply::Stored(stored)
        }
        MailboxDbOp::Mailboxes => MailboxDbReply::Installations(
            column(c, "SELECT installation FROM mailbox_device WHERE mailbox=1 ORDER BY installation", [])?
                .into_iter()
                .map(InstallationId)
                .collect(),
        ),
        MailboxDbOp::Unsealed => MailboxDbReply::Conversations(
            column(
                c,
                "SELECT DISTINCT conversation FROM outbox WHERE sender_peer=?1 AND sender_install=?2 AND codec=3",
                params![identity.peer.0.as_slice(), identity.installation.0.as_slice()],
            )?
            .into_iter()
            .map(ConversationId)
            .collect(),
        ),
        MailboxDbOp::HandoffRows => MailboxDbReply::Handoff(handoff_rows(c, identity)?),
        MailboxDbOp::TakeHandoff {
            origin,
            entries,
            now,
        } => {
            let tx = c.transaction().map_err(sql)?;
            let mut taken = 0;
            for entry in entries {
                if conversation_id(identity.peer, entry.peer).ok() != Some(entry.conversation) {
                    continue;
                }
                taken += tx
                    .execute(
                        "INSERT INTO mailbox_handoff VALUES(?1,?2,?3,?4,?5,0,?6,?6) ON CONFLICT(origin_install,conversation,msg_id) DO UPDATE SET peer=excluded.peer,ciphertext=excluded.ciphertext,attempts=0,next_retry_at_ms=excluded.next_retry_at_ms,received_at_ms=excluded.received_at_ms WHERE ciphertext<>excluded.ciphertext",
                        params![
                            origin.0.as_slice(),
                            entry.conversation.0.as_slice(),
                            entry.msg_id.0.as_slice(),
                            entry.peer.0.as_slice(),
                            entry.ciphertext,
                            integer(now)?
                        ],
                    )
                    .map_err(sql)? as u64;
            }
            finish(tx, fault)?;
            MailboxDbReply::Count(taken)
        }
        MailboxDbOp::HandoffDue(now) => handoff_due(c, now)?,
        MailboxDbOp::HandoffWake { peer, now } => {
            let tx = c.transaction().map_err(sql)?;
            let woken = tx
                .execute(
                    "UPDATE mailbox_handoff SET next_retry_at_ms=?2 WHERE peer=?1 AND next_retry_at_ms>?2 AND next_retry_at_ms<?3",
                    params![peer.0.as_slice(), integer(now)?, STOPPED],
                )
                .map_err(sql)?;
            finish(tx, fault)?;
            MailboxDbReply::Count(woken as u64)
        }
        MailboxDbOp::HandoffResult {
            origin,
            conversation,
            msg_id,
            ct_hash,
            outcome,
            now,
            wall,
        } => {
            let tx = c.transaction().map_err(sql)?;
            let changed = handoff_result(
                &tx,
                identity,
                (origin, conversation, msg_id),
                ct_hash,
                outcome,
                now,
                wall,
            )?;
            finish(tx, fault)?;
            MailboxDbReply::Count(changed)
        }
    };
    Ok(MessageDbReply::Mailbox(reply))
}

/// The 32-byte values of the first column of `query`.
fn column(c: &Connection, query: &str, values: impl rusqlite::Params) -> R<Vec<[u8; 32]>> {
    let mut q = c.prepare(query).map_err(sql)?;
    q.query_map(values, |r| bytes::<32>(r, 0))
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)
}

fn handoff_rows(c: &Connection, identity: Identity) -> R<Vec<HandoffEntry>> {
    let mut q = c
        .prepare("SELECT o.conversation,v.peer,o.msg_id,o.wire FROM outbox o JOIN message m USING(conversation,sender_peer,sender_install,msg_id) JOIN conversation v ON v.id=o.conversation WHERE o.sender_peer=?1 AND o.sender_install=?2 AND o.codec=2 AND o.waiting_peer_update=0 AND m.outgoing=1 AND m.delivered_at_ms IS NULL AND m.cancel_requested=0 AND m.outcome<>3 ORDER BY m.lamport,o.msg_id")
        .map_err(sql)?;
    let rows = q
        .query_map(
            params![
                identity.peer.0.as_slice(),
                identity.installation.0.as_slice()
            ],
            |r| {
                Ok((
                    bytes::<32>(r, 0)?,
                    bytes::<32>(r, 1)?,
                    bytes::<16>(r, 2)?,
                    r.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    // The row holds the complete kind 17 `App` frame; the mailbox gets its ciphertext.
    Ok(rows
        .into_iter()
        .filter_map(|(conversation, peer, msg_id, wire)| {
            let inner = crate::account::auth::payload(&wire, 17).ok()?;
            match MlsFrame::decode(&inner).ok()? {
                MlsFrame::App {
                    conversation: id,
                    ciphertext,
                } if id.0 == conversation => Some(HandoffEntry {
                    conversation: id,
                    peer: PeerId(peer),
                    msg_id: MessageId(msg_id),
                    ciphertext,
                }),
                _ => None,
            }
        })
        .collect())
}

fn handoff_due(c: &Connection, now: u64) -> R<MailboxDbReply> {
    let jobs: Vec<(InstallationId, HandoffEntry)> = {
        let mut q = c
            .prepare("SELECT origin_install,conversation,msg_id,peer,ciphertext FROM mailbox_handoff WHERE next_retry_at_ms<=?1 ORDER BY next_retry_at_ms LIMIT ?2")
            .map_err(sql)?;
        q.query_map(params![integer(now)?, HANDOFF_DUE_MAX as i64], |r| {
            Ok((
                InstallationId(bytes(r, 0)?),
                HandoffEntry {
                    conversation: ConversationId(bytes(r, 1)?),
                    msg_id: MessageId(bytes(r, 2)?),
                    peer: PeerId(bytes(r, 3)?),
                    ciphertext: r.get(4)?,
                },
            ))
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?
    };
    let next = if jobs.len() == HANDOFF_DUE_MAX {
        Some(now)
    } else {
        c.query_row(
            "SELECT min(next_retry_at_ms) FROM mailbox_handoff WHERE next_retry_at_ms>?1 AND next_retry_at_ms<?2",
            params![integer(now)?, STOPPED],
            |r| r.get::<_, Option<i64>>(0),
        )
        .map_err(sql)?
        .map(|at| at as u64)
    };
    Ok(MailboxDbReply::HandoffDue { jobs, next })
}

/// Spec 7: an `Ack` delivers the message (whichever ciphertext of it was acked) and is noted
/// for the origin; a decrypt failure of the ciphertext kept stops it and is noted for the
/// origin; anything else backs it off. A result about a ciphertext the origin replaced since
/// changes nothing else. Returns the rows changed.
fn handoff_result(
    tx: &Transaction,
    identity: Identity,
    (origin, conversation, msg_id): (InstallationId, ConversationId, MessageId),
    ct_hash: [u8; 32],
    outcome: HandoffOutcome,
    now: u64,
    wall: u64,
) -> R<u64> {
    let key = params![
        origin.0.as_slice(),
        conversation.0.as_slice(),
        msg_id.0.as_slice()
    ];
    let kept: Option<(Vec<u8>, u64)> = tx
        .query_row(
            "SELECT ciphertext,attempts FROM mailbox_handoff WHERE origin_install=?1 AND conversation=?2 AND msg_id=?3",
            key,
            |r| Ok((r.get(0)?, uint(r, 1)?)),
        )
        .optional()
        .map_err(sql)?;
    let attempts = match kept {
        Some((ciphertext, attempts)) if Sha256::digest(&ciphertext).as_slice() == ct_hash => {
            attempts
        }
        _ if matches!(outcome, HandoffOutcome::Acked { .. }) => 0,
        // Replaced (or gone) meanwhile: the result is of a ciphertext nobody tries any more.
        _ => return Ok(0),
    };
    let note = |body: EntryBody| -> R<()> {
        record_note(tx, conversation, identity.peer, origin, &body, wall).map(|_| ())
    };
    match outcome {
        HandoffOutcome::Acked { receiver } => {
            let body = EntryBody::Delivered {
                msg_id,
                receiver,
                at_ms: wall,
            };
            let entry = LogEntry {
                seq: 0,
                conversation,
                sender_peer: identity.peer,
                sender_install: origin,
                recorded_at_ms: wall,
                body: body.clone(),
            };
            // This device's copy of the message is delivered too (that also drops the row);
            // the note is logged even if the copy already was, for the origin.
            let changed = apply_note(tx, identity, &entry, wall)?;
            note(body)?;
            Ok(changed)
        }
        HandoffOutcome::Stop { class, epoch } => {
            tx.execute(
                "UPDATE mailbox_handoff SET next_retry_at_ms=?4 WHERE origin_install=?1 AND conversation=?2 AND msg_id=?3",
                params![origin.0.as_slice(), conversation.0.as_slice(), msg_id.0.as_slice(), STOPPED],
            )
            .map_err(sql)?;
            note(EntryBody::HandoffStop {
                msg_id,
                class,
                epoch,
            })?;
            Ok(1)
        }
        HandoffOutcome::Retry => {
            let attempts = attempts.saturating_add(1).min(u64::from(u32::MAX));
            let next = now.saturating_add(retry_delay_capped(attempts, MAILBOX_MAX_DELAY_MS));
            tx.execute(
                "UPDATE mailbox_handoff SET attempts=?4,next_retry_at_ms=?5 WHERE origin_install=?1 AND conversation=?2 AND msg_id=?3",
                params![
                    origin.0.as_slice(),
                    conversation.0.as_slice(),
                    msg_id.0.as_slice(),
                    integer(attempts)?,
                    integer(next)?
                ],
            )
            .map_err(sql)?;
            Ok(1)
        }
    }
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
        .filter_map(|row| {
            let (seq, conversation, peer, install, kind, body, at) = match row {
                Ok(row) => row,
                Err(error) => return Some(Err(sql(error))),
            };
            let body = if kind == EntryKind::Frame as u64 {
                EntryBody::Frame(body)
            } else {
                // A note row this version cannot read back is skipped rather than holding
                // up every entry after it.
                decode_note(kind, &body).ok()?
            };
            Some(Ok(LogEntry {
                seq,
                conversation: ConversationId(conversation),
                sender_peer: PeerId(peer),
                sender_install: InstallationId(install),
                recorded_at_ms: at,
                body,
            }))
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

/// A note pulled from another own device's log (spec 6.4, 7). `Delivered`: the message of this
/// account it names is delivered (never undone; `Read` stays), and this device's own outbox row
/// of it is done; a message not here yet keeps the delivery in `mailbox_status_ahead` for when it
/// comes. What changed something here is logged here too, so a third own device gets it from
/// this one; a repeat changes nothing and is not logged again. A ciphertext of the message handed
/// over to this device is not tried any more. `HandoffStop` is the origin's alone to act on (the
/// account actor does, Ruling 1): nothing here. Returns the message rows changed.
fn apply_note(tx: &Transaction, identity: Identity, entry: &LogEntry, now: u64) -> R<u64> {
    let EntryBody::Delivered { msg_id, at_ms, .. } = entry.body else {
        return Ok(0);
    };
    // Only this account's own sent messages have a delivery to learn.
    if entry.sender_peer != identity.peer {
        return Ok(0);
    }
    tx.execute(
        "DELETE FROM mailbox_handoff WHERE origin_install=?1 AND conversation=?2 AND msg_id=?3",
        params![
            entry.sender_install.0.as_slice(),
            entry.conversation.0.as_slice(),
            msg_id.0.as_slice()
        ],
    )
    .map_err(sql)?;
    let key = params![
        entry.conversation.0.as_slice(),
        entry.sender_peer.0.as_slice(),
        entry.sender_install.0.as_slice(),
        msg_id.0.as_slice()
    ];
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM message WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND outgoing=1)",
            key,
            |r| r.get(0),
        )
        .map_err(sql)?;
    let (changed, new) = if exists {
        let changed = tx
            .execute(
                "UPDATE message SET delivered_at_ms=?5 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND outgoing=1 AND delivered_at_ms IS NULL",
                params![
                    entry.conversation.0.as_slice(),
                    entry.sender_peer.0.as_slice(),
                    entry.sender_install.0.as_slice(),
                    msg_id.0.as_slice(),
                    integer(at_ms)?
                ],
            )
            .map_err(sql)?;
        // This device's own message, delivered by another device it was handed to.
        if entry.sender_install == identity.installation {
            tx.execute(
                "DELETE FROM outbox WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
                key,
            )
            .map_err(sql)?;
        }
        if changed > 0 {
            let revision = bump(tx)?;
            tx.execute(
                "UPDATE conversation SET change_revision=?2 WHERE id=?1",
                params![entry.conversation.0.as_slice(), integer(revision)?],
            )
            .map_err(sql)?;
        }
        (changed as u64, changed > 0)
    } else {
        let ahead = tx
            .execute(
                "INSERT OR IGNORE INTO mailbox_status_ahead VALUES(?1,?2,?3,2,?4)",
                params![
                    entry.conversation.0.as_slice(),
                    entry.sender_peer.0.as_slice(),
                    msg_id.0.as_slice(),
                    integer(at_ms)?
                ],
            )
            .map_err(sql)?;
        (0, ahead > 0)
    };
    if new {
        record_note(
            tx,
            entry.conversation,
            entry.sender_peer,
            entry.sender_install,
            &entry.body,
            now,
        )?;
    }
    Ok(changed)
}

/// The text logged as entry `seq` (only an App Text entry names its message), from its message
/// row: `None` for any other entry, an entry no longer kept, or a message no longer here (its
/// conversation was deleted).
fn plain_record(c: &Connection, seq: u64) -> R<Option<PlainRecord>> {
    let Some((conversation, sender_peer, sender_install, msg_id)) = c
        .query_row(
            "SELECT conversation,sender_peer,sender_install,msg_id FROM mailbox_store WHERE seq=?1 AND kind=1 AND msg_id IS NOT NULL",
            params![integer(seq)?],
            |r| {
                Ok((
                    bytes::<32>(r, 0)?,
                    bytes::<32>(r, 1)?,
                    bytes::<32>(r, 2)?,
                    bytes::<16>(r, 3)?,
                ))
            },
        )
        .optional()
        .map_err(sql)?
    else {
        return Ok(None);
    };
    c.query_row(
        "SELECT lamport,prev_hash,sent_at_ms,body FROM message WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
        params![
            conversation.as_slice(),
            sender_peer.as_slice(),
            sender_install.as_slice(),
            msg_id.as_slice()
        ],
        |r| {
            Ok(PlainRecord {
                conversation: ConversationId(conversation),
                sender_peer: PeerId(sender_peer),
                sender_install: InstallationId(sender_install),
                msg_id: MessageId(msg_id),
                lamport: uint(r, 0)?,
                prev_hash: bytes(r, 1)?,
                sent_at_ms: uint(r, 2)?,
                // Text, the only kind of message an entry names.
                kind: 1,
                body: r.get(3)?,
            })
        },
    )
    .optional()
    .map_err(sql)
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
    /// The `mailbox_handoff` rows, by origin, conversation and message.
    pub handoff_rows: Vec<HandoffRow>,
}

/// One `mailbox_handoff` row, for fixture assertions.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffRow {
    pub origin: InstallationId,
    pub conversation: ConversationId,
    pub msg_id: MessageId,
    pub peer: PeerId,
    pub ciphertext: Vec<u8>,
    pub attempts: u64,
    /// `i64::MAX` once the recipient could not open it.
    pub next_retry_at_ms: u64,
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
    let handoff_rows = {
        let mut q = c
            .prepare("SELECT origin_install,conversation,msg_id,peer,ciphertext,attempts,next_retry_at_ms FROM mailbox_handoff ORDER BY origin_install,conversation,msg_id")
            .map_err(sql)?;
        q.query_map([], |r| {
            Ok(HandoffRow {
                origin: InstallationId(bytes(r, 0)?),
                conversation: ConversationId(bytes(r, 1)?),
                msg_id: MessageId(bytes(r, 2)?),
                peer: PeerId(bytes(r, 3)?),
                ciphertext: r.get(4)?,
                attempts: uint(r, 5)?,
                next_retry_at_ms: uint(r, 6)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?
    };
    Ok(MailboxState {
        entries,
        cursors: pairs("mailbox_cursor", "next_seq")?,
        pulls: pairs("mailbox_pull", "since_seq")?,
        devices: count("mailbox_device")?,
        handoffs: count("mailbox_handoff")?,
        status_ahead: count("mailbox_status_ahead")?,
        handoff_rows,
    })
}

/// Records `installation` as a mailbox device (or not) at `at`, the way its `Announce` will
/// (Task 10); until then tests mark mailboxes with this (Ruling 12).
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_mark(
    c: &Connection,
    installation: InstallationId,
    mailbox: bool,
    at: u64,
) -> R<()> {
    c.execute(
        "INSERT INTO mailbox_device VALUES(?1,?2,?3) ON CONFLICT(installation) DO UPDATE SET mailbox=excluded.mailbox,announced_at_ms=excluded.announced_at_ms",
        params![installation.0.as_slice(), mailbox, integer(at)?],
    )
    .map_err(sql)?;
    Ok(())
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
