//! Queue rows of the message store (M8, schema v5): this device's queues (`queue_own`, spec 6.2)
//! and the queues other devices announced (`queue_peer`, spec 7.2). The `QueueRef` frames this
//! device seals ride `push_delivery` as kind 2 (`FrameKind::QueueRef`), sent like the push
//! handles' frames. `Apply` and `Settle` write one transaction per queue (as push's `Seal`
//! writes one per conversation), so a queue that fails holds up no other; `store_peer_ref` runs
//! inside the caller's.
//!
//! Sending (spec 8): any frame a direct send would write to a device also goes into that
//! device's queue while no direct send reaches it. The frames are the bytes the direct sends
//! keep: a text's outbox `wire` (or, on a mailbox, a handed-off `mailbox_handoff` ciphertext), a
//! receipt's `mls_receipt` wire, the `Commit`s and `Welcome`s of `mls_delivery`, and the
//! `PushHandle` and `QueueRef` frames of `push_delivery`. `queue_delivery` has a row per device
//! and frame hash while that device's queue holds the frame (until `expires_at_ms`, the
//! server's 24 h); while it lasts, and the device's queue is still that one, the frame does not
//! go in again. Expired rows are swept (`Sweep`); a frame that no longer waits is never read
//! again, so its row needs no other delete.
use queue_wire::{QueueId, RETIRE_MS};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use zeroize::Zeroizing;

use super::mailbox_store::STOPPED;
use super::mls_group::{PENDING_LIMIT, sql};
use super::mls_store::{Fault, finish};
use super::push_store::{FrameKind, Routing, recipients};
use super::store::{Identity, MessageDbReply, bytes, integer, uint};
use super::types::{ConversationId, MessageKey};
use crate::account::{InstallationId, PeerId, storage::schema::LocalAccount};
use crate::mailbox::verdict::QueueMode;
use crate::mls::payload::{MAX_SERVER_URL, QueueRefBody, queue_ref_message};
use crate::mls::wire::MlsFrame;
use crate::{CoreError, ErrorCode};

type R<T> = Result<T, CoreError>;

/// Most `queue_delivery` rows one `Sweep` drops.
pub(crate) const SWEEP_MAX: i64 = 256;

/// The devices a `SEND` is for (spec 8.1): those no direct send reaches now.
pub(crate) enum Unreached {
    /// The attempt of text or receipt `key` did not reach `devices`: the frames they still
    /// lack in its conversation, then its own. `key` names this device's text, a text another
    /// of my devices handed off to this mailbox, or (`receipt`) a received message whose
    /// receipt this device owes.
    Message {
        key: MessageKey,
        receipt: bool,
        devices: Vec<InstallationId>,
    },
    /// Every device but `reached` (those with a session): the frames still queued for it, in
    /// every conversation.
    Standing { reached: Vec<InstallationId> },
}

/// One `SEND` request (spec 8.1): every device it goes to.
pub(crate) struct ToSend {
    pub targets: Vec<Target>,
}

/// One device's queue, and the frames that go into it, in the order a direct attempt carries
/// them: the `Commit`s and `Welcome`s still queued for it, its `PushHandle` and `QueueRef`
/// frames, then the text or receipt. Each frame is the complete kind 17 envelope a direct send
/// writes, with the installation that sealed it (this device, or a handed-off text's origin).
pub(crate) struct Target {
    pub conversation: ConversationId,
    pub installation: InstallationId,
    pub queue_id: QueueId,
    pub sender_secret: Zeroizing<[u8; 32]>,
    pub frames: Vec<(InstallationId, Vec<u8>)>,
}

/// A frame put into a device's queue: `queue_delivery`'s key and the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Queued {
    pub conversation: ConversationId,
    pub installation: InstallationId,
    pub queue_id: QueueId,
    pub frame_hash: [u8; 32],
}

/// The `queue_delivery` key of `frame`.
pub(crate) fn frame_hash(frame: &[u8]) -> [u8; 32] {
    Sha256::digest(frame).into()
}

/// A queue's state (spec 6.2): `Active` (at most one per conversation), or `Retiring`, still
/// fetched until `delete_at_ms` (wall) and then deleted at the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueState {
    Active,
    Retiring { delete_at_ms: u64 },
}

/// One `queue_own` row. `confirmed`: the server took its `CREATE` and its `QueueRef` was queued
/// for the peer, both while it was `Active` (one transaction, `confirm`).
pub(crate) struct OwnQueue {
    pub id: QueueId,
    pub conversation: ConversationId,
    pub recipient_secret: Zeroizing<[u8; 32]>,
    pub sender_secret: Zeroizing<[u8; 32]>,
    /// Wall time it was made; its `QueueRef` is sent at this time.
    pub created_at_ms: u64,
    pub state: QueueState,
    pub confirmed: bool,
}

/// A queue to make: its id and keys, chosen by the actor and written before any `CREATE`.
pub(crate) struct NewQueue {
    pub id: QueueId,
    pub conversation: ConversationId,
    pub recipient_secret: Zeroizing<[u8; 32]>,
    pub sender_secret: Zeroizing<[u8; 32]>,
}

/// A queue a fetch asks (spec 6.4): an `Active` one the server took, or a `Retiring` one.
/// `last_seq` is the last entry the server took an `ACK` for, so a fetch asks for what comes
/// after it: whatever was stored here but not acknowledged comes again (and is taken once).
pub(crate) struct Fetchable {
    pub id: QueueId,
    pub conversation: ConversationId,
    /// The conversation's peer account: what its entries are opened as coming from.
    pub peer: PeerId,
    pub recipient_secret: Zeroizing<[u8; 32]>,
    pub last_seq: u64,
}

/// What a reconcile looks at.
pub(crate) struct QueueView {
    pub mode: QueueMode,
    /// The conversations with a group, in id order.
    pub conversations: Vec<ConversationId>,
    pub own: Vec<OwnQueue>,
}

pub(crate) enum QueueDbOp {
    Load,
    /// Whether this device has an `Active` queue the server took (`QueueStateView.using_queue`).
    Using,
    /// The local half of a reconcile: `retire` goes `Retiring` (its withdrawal sealed for each
    /// current device of the peer when `true`), then `new` rows are written, and `forget` rows
    /// are dropped without a request. `devices`: each peer account's
    /// current devices; `wall` the time frames and states are stamped at, `now` the scheduler
    /// time their deliveries are due at.
    Apply {
        retire: Vec<(QueueId, bool)>,
        new: Vec<NewQueue>,
        forget: Vec<QueueId>,
        devices: Vec<(PeerId, Vec<InstallationId>)>,
        wall: u64,
        now: u64,
    },
    /// What the server answered: `created` took their `CREATE` (an `Active` one gets its
    /// `QueueRef` for `server_url` sealed and is confirmed; the next `RECV` starts at seq 0),
    /// `refused` ids belong to other keys there (an unconfirmed row goes, so a new queue is made),
    /// `deleted` took their `DELETE` (the rows go).
    Settle {
        created: Vec<QueueId>,
        refused: Vec<QueueId>,
        deleted: Vec<QueueId>,
        server_url: String,
        devices: Vec<(PeerId, Vec<InstallationId>)>,
        wall: u64,
        now: u64,
    },
    /// The mode and the queues a fetch asks, in conversation and creation order.
    Fetchable,
    /// What a fetch's `ACK`s and `RECV`s said, in one transaction: `acked` took an `ACK` up to
    /// that seq (`last_seq` only rises), `gone` are unknown at the server. A `gone` `Active`
    /// queue goes `Retiring` with no withdrawal (its frame time still counts for the next
    /// one's), so the reconcile makes a fresh queue; a `gone` `Retiring` one is dropped. Its
    /// `server_confirmed` and `last_seq` are never reset: the same id created again would be
    /// announced at the same frame time, which peers drop as no newer.
    Fetched {
        acked: Vec<(QueueId, u64)>,
        gone: Vec<QueueId>,
        wall: u64,
    },
    /// The `SEND` to the devices of `unreached`: what each still lacks, read before a message
    /// attempt's outcome is stored (an acknowledged text's row goes with it). Nothing with the
    /// mode "never", for a text or receipt no longer waiting, or for a device without a live
    /// queue on `server_url`; no frame its queue holds at `wall`.
    ToSend {
        unreached: Unreached,
        server_url: String,
        wall: u64,
    },
    /// A `SEND` came back: the queues of `taken` hold those frames until `expires_at`; the
    /// queues of `gone` are unknown at the server: not used again until that device announces
    /// another queue.
    Sent {
        taken: Vec<Queued>,
        gone: Vec<Queued>,
        expires_at: u64,
    },
    /// Drops `queue_delivery` rows expired at `wall`, `SWEEP_MAX` at most.
    Sweep {
        wall: u64,
    },
}

impl QueueDbOp {
    pub(crate) fn writes(&self) -> bool {
        !matches!(
            self,
            Self::Load | Self::Using | Self::Fetchable | Self::ToSend { .. }
        )
    }
}

pub(crate) enum QueueDbReply {
    Loaded(QueueView),
    Using(bool),
    /// Withdrawals sealed, and the `new` queues written: only those go to the server.
    Applied {
        withdrawn: u32,
        written: Vec<QueueId>,
    },
    /// `QueueRef`s sealed, and created `Active` queues whose `QueueRef` could not be sealed yet
    /// (no group, no known device of the peer, or a fault in its conversation): they stay
    /// unconfirmed for a later run.
    Settled {
        announced: u32,
        unsealed: u32,
    },
    Fetchable {
        mode: QueueMode,
        queues: Vec<Fetchable>,
    },
    /// `Active` queues that went `Retiring` because the server lost them: new ones are due.
    Fetched {
        replaced: u32,
    },
    ToSend(Option<ToSend>),
    Sent,
    /// Rows dropped: `SWEEP_MAX` means there may be more.
    Swept(i64),
}

pub(super) fn queue_op(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    op: QueueDbOp,
    fault: Fault,
) -> R<MessageDbReply> {
    let reply = match op {
        QueueDbOp::Load => QueueDbReply::Loaded(load(c)?),
        QueueDbOp::Using => QueueDbReply::Using(
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM queue_own WHERE state=0 AND server_confirmed=1)",
                [],
                |r| r.get(0),
            )
            .map_err(sql)?,
        ),
        QueueDbOp::Apply {
            retire,
            new,
            forget,
            devices,
            wall,
            now,
        } => {
            let seal = Sealer {
                routing: Routing {
                    identity,
                    local,
                    wall,
                    now,
                },
                devices: &devices,
            };
            // One transaction per queue (as push seals one per conversation): a queue that
            // fails is left as it was and planned again, and the others go on.
            let mut withdrawn = 0;
            for (id, withdraw) in retire {
                if let Ok(true) = alone(c, fault, |tx| retire_one(tx, &seal, id, withdraw)) {
                    withdrawn += 1;
                }
            }
            let written = new
                .into_iter()
                .filter(|queue| alone(c, fault, |tx| insert(tx, queue, wall)).is_ok())
                .map(|queue| queue.id)
                .collect();
            for id in forget {
                // Left for the next run when it fails.
                let _ = alone(c, fault, |tx| {
                    tx.execute(
                        "DELETE FROM queue_own WHERE queue_id=?1",
                        params![id.as_slice()],
                    )
                    .map_err(sql)
                });
            }
            QueueDbReply::Applied { withdrawn, written }
        }
        QueueDbOp::Settle {
            created,
            refused,
            deleted,
            server_url,
            devices,
            wall,
            now,
        } => {
            let seal = Sealer {
                routing: Routing {
                    identity,
                    local,
                    wall,
                    now,
                },
                devices: &devices,
            };
            let (mut announced, mut unsealed) = (0, 0);
            for id in created {
                match alone(c, fault, |tx| confirm(tx, &seal, id, &server_url)) {
                    Ok(None) => {}
                    Ok(Some(true)) => announced += 1,
                    // It stays unconfirmed, created and sealed again by a later run; the other
                    // queues are confirmed meanwhile.
                    Ok(Some(false)) | Err(_) => unsealed += 1,
                }
            }
            // Plain deletes, no seal: one transaction, and its failure is the op's. Skipping one
            // would leave a refused row that the actor, told nothing failed, re-creates at once.
            let tx = c.transaction().map_err(sql)?;
            for id in refused {
                tx.execute(
                    "DELETE FROM queue_own WHERE queue_id=?1 AND server_confirmed=0",
                    params![id.as_slice()],
                )
                .map_err(sql)?;
            }
            for id in deleted {
                tx.execute(
                    "DELETE FROM queue_own WHERE queue_id=?1 AND state=1",
                    params![id.as_slice()],
                )
                .map_err(sql)?;
            }
            finish(tx, fault)?;
            QueueDbReply::Settled {
                announced,
                unsealed,
            }
        }
        QueueDbOp::Fetchable => QueueDbReply::Fetchable {
            mode: mode(c)?,
            queues: fetchable(c)?,
        },
        QueueDbOp::Fetched { acked, gone, wall } => {
            // Plain updates and deletes, no seal: one transaction for the whole fetch step.
            let tx = c.transaction().map_err(sql)?;
            for (id, seq) in acked {
                tx.execute(
                    "UPDATE queue_own SET last_seq=max(last_seq,?2) WHERE queue_id=?1",
                    params![id.as_slice(), integer(seq)?],
                )
                .map_err(sql)?;
            }
            let mut replaced = 0;
            for id in gone {
                if retire(&tx, id, wall)?.is_some() {
                    replaced += 1;
                } else {
                    tx.execute(
                        "DELETE FROM queue_own WHERE queue_id=?1 AND state=1",
                        params![id.as_slice()],
                    )
                    .map_err(sql)?;
                }
            }
            finish(tx, fault)?;
            QueueDbReply::Fetched { replaced }
        }
        QueueDbOp::ToSend {
            unreached,
            server_url,
            wall,
        } => QueueDbReply::ToSend(to_send(c, identity, unreached, &server_url, wall)?),
        QueueDbOp::Sent {
            taken,
            gone,
            expires_at,
        } => {
            let tx = c.transaction().map_err(sql)?;
            for q in taken {
                tx.execute(
                    "INSERT INTO queue_delivery VALUES(?1,?2,?3,?4) ON CONFLICT(recipient_install,frame_hash) DO UPDATE SET queue_id=excluded.queue_id,expires_at_ms=excluded.expires_at_ms",
                    params![
                        q.installation.0.as_slice(),
                        q.frame_hash.as_slice(),
                        q.queue_id.as_slice(),
                        integer(expires_at)?
                    ],
                )
                .map_err(sql)?;
            }
            // Only the queue that answered: a newer one announced meanwhile stays live.
            for q in gone {
                tx.execute(
                    "UPDATE queue_peer SET dead=1 WHERE conversation=?1 AND installation=?2 AND queue_id=?3",
                    params![
                        q.conversation.0.as_slice(),
                        q.installation.0.as_slice(),
                        q.queue_id.as_slice()
                    ],
                )
                .map_err(sql)?;
            }
            finish(tx, fault)?;
            QueueDbReply::Sent
        }
        QueueDbOp::Sweep { wall } => {
            let tx = c.transaction().map_err(sql)?;
            let swept = tx
                .execute(
                    "DELETE FROM queue_delivery WHERE rowid IN (SELECT rowid FROM queue_delivery WHERE expires_at_ms<=?1 LIMIT ?2)",
                    params![integer(wall)?, SWEEP_MAX],
                )
                .map_err(sql)?;
            finish(tx, fault)?;
            QueueDbReply::Swept(swept as i64)
        }
    };
    Ok(MessageDbReply::Queue(reply))
}

/// The frame of text `key` while it still waits for delivery, as a direct attempt writes it:
/// this device's own sealed outbox row, or the ciphertext another of my devices handed off to
/// this mailbox (not one stopped because the peer could not open it).
fn text(c: &Connection, identity: Identity, key: MessageKey) -> R<Option<Vec<u8>>> {
    if key.sender != identity.peer {
        return Ok(None);
    }
    if key.installation == identity.installation {
        return c
            .query_row(
                "SELECT o.wire FROM outbox o JOIN message m USING(conversation,sender_peer,sender_install,msg_id) WHERE o.conversation=?1 AND o.sender_peer=?2 AND o.sender_install=?3 AND o.msg_id=?4 AND o.codec=2 AND m.delivered_at_ms IS NULL AND m.cancel_requested=0 AND m.outcome<>3",
                params![
                    key.conversation.0.as_slice(),
                    key.sender.0.as_slice(),
                    key.installation.0.as_slice(),
                    key.id.0.as_slice()
                ],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql);
    }
    let ciphertext: Option<Vec<u8>> = c
        .query_row(
            "SELECT ciphertext FROM mailbox_handoff WHERE origin_install=?1 AND conversation=?2 AND msg_id=?3 AND next_retry_at_ms<?4",
            params![
                key.installation.0.as_slice(),
                key.conversation.0.as_slice(),
                key.id.0.as_slice(),
                STOPPED
            ],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    Ok(ciphertext.map(|ciphertext| {
        MlsFrame::App {
            conversation: key.conversation,
            ciphertext,
        }
        .encode()
        .to_vec()
    }))
}

/// The `Read` (2) or else `Delivered` (1) receipt this device still owes for received message
/// `key`, as its attempts send it: the `mls_receipt` wire of its open `receipt_job`. A receipt
/// that was never sealed (no attempt yet, no group) is not there.
fn receipt(c: &Connection, identity: Identity, key: MessageKey) -> R<Option<Vec<u8>>> {
    c.query_row(
        "SELECT r.wire FROM mls_receipt r JOIN receipt_job j ON j.conversation=r.conversation AND j.sender_peer=r.sender_peer AND j.sender_install=r.sender_install AND j.msg_id=r.msg_id AND j.kind=r.kind WHERE r.conversation=?1 AND r.sender_peer=?2 AND r.sender_install=?3 AND r.msg_id=?4 AND j.recipient_install=?5 ORDER BY r.kind DESC LIMIT 1",
        params![
            key.conversation.0.as_slice(),
            key.sender.0.as_slice(),
            key.installation.0.as_slice(),
            key.id.0.as_slice(),
            identity.installation.0.as_slice()
        ],
        |r| r.get(0),
    )
    .optional()
    .map_err(sql)
}

/// See `QueueDbOp::ToSend`. Six reads whatever the number of devices and conversations: the
/// mode, the targets, the text or receipt, the frames their queues hold, and the `mls_delivery`
/// and `push_delivery` frames queued for them. A message's read covers its conversation
/// only; the standing one every conversation (a range over the whole id space). With no live
/// queue to put into (most conversations), it stops after the second, small read (review
/// minor 5).
fn to_send(
    c: &Connection,
    identity: Identity,
    unreached: Unreached,
    server_url: &str,
    wall: u64,
) -> R<Option<ToSend>> {
    if mode(c)? == QueueMode::Never {
        return Ok(None);
    }
    // The conversations read, the devices kept (all when `None`) and left out, and the
    // message whose frame goes last.
    let (range, only, but, message) = match unreached {
        Unreached::Message {
            key,
            receipt,
            devices,
        } => {
            let conversation = key.conversation.0;
            (
                (conversation, conversation),
                Some(devices),
                Vec::new(),
                Some((key, receipt)),
            )
        }
        Unreached::Standing { reached } => (([0; 32], [0xff; 32]), None, reached, None),
    };
    let (lo, hi) = (range.0.as_slice(), range.1.as_slice());
    let mut targets: Vec<Target> = c
        .prepare("SELECT conversation,installation,queue_id,sender_secret FROM queue_peer WHERE conversation BETWEEN ?1 AND ?2 AND server_url=?3 AND queue_id IS NOT NULL AND dead=0 ORDER BY conversation,installation")
        .map_err(sql)?
        .query_map(params![lo, hi, server_url], |r| {
            Ok(Target {
                conversation: ConversationId(bytes(r, 0)?),
                installation: InstallationId(bytes(r, 1)?),
                queue_id: bytes(r, 2)?,
                sender_secret: Zeroizing::new(bytes(r, 3)?),
                frames: Vec::new(),
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    targets.retain(|t| {
        only.as_ref().is_none_or(|only| only.contains(&t.installation))
            && !but.contains(&t.installation)
    });
    if targets.is_empty() {
        return Ok(None);
    }
    let last = match message {
        Some((key, true)) => match receipt(c, identity, key)? {
            // Sealed here.
            Some(frame) => Some((identity.installation, frame)),
            None => return Ok(None),
        },
        Some((key, false)) => match text(c, identity, key)? {
            // Sealed by its origin: this device, or the one that handed it off to this mailbox.
            Some(frame) => Some((key.installation, frame)),
            None => return Ok(None),
        },
        None => None,
    };
    // What each queue holds now: a row for the device's present queue that has not expired.
    let held: BTreeSet<(InstallationId, [u8; 32])> = c
        .prepare("SELECT d.recipient_install,d.frame_hash FROM queue_delivery d JOIN queue_peer p ON p.installation=d.recipient_install AND p.queue_id=d.queue_id WHERE p.conversation BETWEEN ?1 AND ?2 AND d.expires_at_ms>?3")
        .map_err(sql)?
        .query_map(params![lo, hi, integer(wall)?], |r| {
            Ok((InstallationId(bytes(r, 0)?), bytes(r, 1)?))
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    let me = identity.installation;
    let installations: Vec<InstallationId> = targets.iter().map(|t| t.installation).collect();
    let mut add = |conversation: ConversationId, installation: InstallationId, frame: (InstallationId, Vec<u8>), limit: Option<i64>| {
        let Some(target) = targets
            .iter_mut()
            .find(|t| t.conversation == conversation && t.installation == installation)
        else {
            return;
        };
        if limit.is_some_and(|limit| target.frames.len() as i64 >= limit)
            || held.contains(&(installation, frame_hash(&frame.1)))
        {
            return;
        }
        target.frames.push(frame);
    };
    // The order a direct attempt carries them (`pending_deliveries`): `Commit`s and `Welcome`s
    // by epoch, Welcome first, at most `PENDING_LIMIT`; so a text never comes before its
    // epoch's Commit.
    let mut ahead = c
        .prepare("SELECT m.conversation,m.recipient_install,m.body FROM mls_delivery m JOIN queue_peer p ON p.conversation=m.conversation AND p.installation=m.recipient_install WHERE m.conversation BETWEEN ?1 AND ?2 AND p.server_url=?3 AND p.queue_id IS NOT NULL AND p.dead=0 ORDER BY m.conversation,m.recipient_install,m.epoch,m.kind DESC")
        .map_err(sql)?;
    for row in ahead
        .query_map(params![lo, hi, server_url], |r| {
            Ok((
                ConversationId(bytes(r, 0)?),
                InstallationId(bytes(r, 1)?),
                r.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(sql)?
    {
        let (conversation, installation, body) = row.map_err(sql)?;
        add(conversation, installation, (me, body), Some(PENDING_LIMIT));
    }
    let mut routing = c
        .prepare("SELECT d.conversation,d.recipient_install,d.wire FROM push_delivery d JOIN queue_peer p ON p.conversation=d.conversation AND p.installation=d.recipient_install WHERE d.conversation BETWEEN ?1 AND ?2 AND p.server_url=?3 AND p.queue_id IS NOT NULL AND p.dead=0 ORDER BY d.conversation,d.recipient_install,d.kind")
        .map_err(sql)?;
    for row in routing
        .query_map(params![lo, hi, server_url], |r| {
            Ok((
                ConversationId(bytes(r, 0)?),
                InstallationId(bytes(r, 1)?),
                r.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(sql)?
    {
        let (conversation, installation, wire) = row.map_err(sql)?;
        add(conversation, installation, (me, wire), None);
    }
    if let Some(frame) = last {
        let conversation = ConversationId(range.0);
        for installation in installations {
            add(conversation, installation, frame.clone(), None);
        }
    }
    targets.retain(|t| !t.frames.is_empty());
    Ok((!targets.is_empty()).then_some(ToSend { targets }))
}

/// `f` in a transaction of its own: committed when it returns, rolled back when it fails.
fn alone<T>(c: &mut Connection, fault: Fault, f: impl FnOnce(&Transaction<'_>) -> R<T>) -> R<T> {
    let tx = c.transaction().map_err(sql)?;
    let out = f(&tx)?;
    finish(tx, fault)?;
    Ok(out)
}

/// The `Active` queue `id` goes `Retiring`; its withdrawal is sealed when `withdraw`. Whether
/// one was.
fn retire_one(tx: &Transaction<'_>, seal: &Sealer<'_>, id: QueueId, withdraw: bool) -> R<bool> {
    let Some((conversation, at)) = retire(tx, id, seal.routing.wall)? else {
        return Ok(false);
    };
    Ok(withdraw && seal.queue_ref(tx, conversation, at, None)?)
}

/// The `Active` queue `id` goes `Retiring` at `wall` (or just after the conversation's last
/// frame), to be deleted `RETIRE_MS` later: its conversation and that time, `None` when it is
/// not `Active`.
fn retire(tx: &Transaction<'_>, id: QueueId, wall: u64) -> R<Option<(ConversationId, u64)>> {
    let Some(conversation) = active_conversation(tx, &id)? else {
        return Ok(None);
    };
    let at = wall.max(floor(tx, conversation)?.saturating_add(1));
    tx.execute(
        "UPDATE queue_own SET state=1,delete_at_ms=?2 WHERE queue_id=?1 AND state=0",
        params![id.as_slice(), integer(at.saturating_add(RETIRE_MS))?],
    )
    .map_err(sql)?;
    Ok(Some((conversation, at)))
}

/// A new `Active` queue's row with its keys, unconfirmed, made at `wall` (or just after the
/// conversation's last frame).
fn insert(tx: &Transaction<'_>, queue: &NewQueue, wall: u64) -> R<()> {
    let at = wall.max(floor(tx, queue.conversation)?.saturating_add(1));
    tx.execute(
        "INSERT INTO queue_own VALUES(?1,?2,?3,?4,?5,0,NULL,0,0)",
        params![
            queue.id.as_slice(),
            queue.conversation.0.as_slice(),
            queue.recipient_secret.as_slice(),
            queue.sender_secret.as_slice(),
            integer(at)?
        ],
    )
    .map_err(sql)?;
    Ok(())
}

/// The server took the `CREATE` of `id`: an unconfirmed `Active` queue gets its `QueueRef` for
/// `server_url` sealed and is confirmed (its next `RECV` starts at seq 0). `None`: no such
/// queue (dropped, or confirmed before); `Some(false)`: it cannot be sealed now and stays
/// unconfirmed.
fn confirm(
    tx: &Transaction<'_>,
    seal: &Sealer<'_>,
    id: QueueId,
    server_url: &str,
) -> R<Option<bool>> {
    let row = tx
        .query_row(
            "SELECT conversation,sender_secret,created_at_ms FROM queue_own WHERE queue_id=?1 AND state=0 AND server_confirmed=0",
            params![id.as_slice()],
            |r| {
                Ok((
                    ConversationId(bytes(r, 0)?),
                    Zeroizing::new(bytes::<32>(r, 1)?),
                    uint(r, 2)?,
                ))
            },
        )
        .optional()
        .map_err(sql)?;
    let Some((conversation, sender_secret, created_at_ms)) = row else {
        return Ok(None);
    };
    let body = QueueRefBody {
        server_url: server_url.to_owned(),
        queue_id: id,
        sender_secret,
    };
    if !seal.queue_ref(tx, conversation, created_at_ms, Some(body))? {
        return Ok(Some(false));
    }
    tx.execute(
        "UPDATE queue_own SET server_confirmed=1,last_seq=0 WHERE queue_id=?1",
        params![id.as_slice()],
    )
    .map_err(sql)?;
    Ok(Some(true))
}

fn mode(c: &Connection) -> R<QueueMode> {
    let mode = c
        .query_row("SELECT mode FROM queue_local WHERE id=1", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(sql)?;
    match mode {
        0 => Ok(QueueMode::Auto),
        1 => Ok(QueueMode::Always),
        2 => Ok(QueueMode::Never),
        _ => Err(CoreError::new(ErrorCode::StorageCorrupt, "queue_mode")),
    }
}

/// Every queue a fetch asks, with its conversation's peer, in one query.
fn fetchable(c: &Connection) -> R<Vec<Fetchable>> {
    c.prepare("SELECT q.queue_id,q.conversation,v.peer,q.recipient_secret,q.last_seq FROM queue_own q JOIN conversation v ON v.id=q.conversation WHERE q.state=1 OR q.server_confirmed=1 ORDER BY q.conversation,q.created_at_ms")
        .map_err(sql)?
        .query_map([], |r| {
            Ok(Fetchable {
                id: bytes(r, 0)?,
                conversation: ConversationId(bytes(r, 1)?),
                peer: PeerId(bytes(r, 2)?),
                recipient_secret: Zeroizing::new(bytes(r, 3)?),
                last_seq: uint(r, 4)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)
}

fn load(c: &Connection) -> R<QueueView> {
    let mode = mode(c)?;
    let conversations = c
        .prepare("SELECT m.conversation FROM mls_conversation m JOIN conversation v ON v.id=m.conversation ORDER BY m.conversation")
        .map_err(sql)?
        .query_map([], |r| Ok(ConversationId(bytes(r, 0)?)))
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    let own = c
        .prepare("SELECT queue_id,conversation,recipient_secret,sender_secret,created_at_ms,state,delete_at_ms,server_confirmed FROM queue_own ORDER BY conversation,created_at_ms")
        .map_err(sql)?
        .query_map([], |r| {
            let state = match (r.get::<_, i64>(5)?, r.get::<_, Option<i64>>(6)?) {
                (0, None) => QueueState::Active,
                (1, Some(at)) => QueueState::Retiring {
                    delete_at_ms: u64::try_from(at).map_err(|_| rusqlite::Error::InvalidQuery)?,
                },
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(OwnQueue {
                id: bytes(r, 0)?,
                conversation: ConversationId(bytes(r, 1)?),
                recipient_secret: Zeroizing::new(bytes(r, 2)?),
                sender_secret: Zeroizing::new(bytes(r, 3)?),
                created_at_ms: uint(r, 4)?,
                state,
                confirmed: r.get(7)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    Ok(QueueView {
        mode,
        conversations,
        own,
    })
}

/// The conversation of `id` while it is `Active`.
fn active_conversation(tx: &Transaction<'_>, id: &QueueId) -> R<Option<ConversationId>> {
    tx.query_row(
        "SELECT conversation FROM queue_own WHERE queue_id=?1 AND state=0",
        params![id.as_slice()],
        |r| Ok(ConversationId(bytes(r, 0)?)),
    )
    .optional()
    .map_err(sql)
}

/// The latest `QueueRef` frame time this device's queues of `conversation` stand for (each
/// queue's announcement, and each retirement's withdrawal): the next frame is sent later
/// (Ruling R11, as `PushHandle` frames are). 0 without a row.
fn floor(tx: &Transaction<'_>, conversation: ConversationId) -> R<u64> {
    let (created, retired): (Option<i64>, Option<i64>) = tx
        .query_row(
            "SELECT max(created_at_ms),max(delete_at_ms) FROM queue_own WHERE conversation=?1",
            params![conversation.0.as_slice()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sql)?;
    let created = created.and_then(|at| u64::try_from(at).ok()).unwrap_or(0);
    let retired = retired
        .and_then(|at| u64::try_from(at).ok())
        .map_or(0, |at| at.saturating_sub(RETIRE_MS));
    Ok(created.max(retired))
}

/// Seals this device's `QueueRef` frames for the conversations' peers.
struct Sealer<'a> {
    routing: Routing<'a>,
    /// Each peer account's current devices.
    devices: &'a [(PeerId, Vec<InstallationId>)],
}

impl Sealer<'_> {
    /// Seals `queue` (`None`: a withdrawal) sent at `sent_at_ms` in the conversation's group and
    /// queues it for each current device of the peer (a newer frame replaces what is still
    /// queued); my other devices take it from the device log. False when it cannot be sealed
    /// now: no group, no known device of the peer, or a group that cannot seal.
    fn queue_ref(
        &self,
        tx: &Transaction<'_>,
        conversation: ConversationId,
        sent_at_ms: u64,
        queue: Option<QueueRefBody>,
    ) -> R<bool> {
        let peer: Option<[u8; 32]> = tx
            .query_row(
                "SELECT peer FROM conversation WHERE id=?1",
                params![conversation.0.as_slice()],
                |r| bytes(r, 0),
            )
            .optional()
            .map_err(sql)?;
        let Some(recipients) = peer.and_then(|peer| recipients(self.devices, peer)) else {
            return Ok(false);
        };
        let message = queue_ref_message(self.routing.identity.installation, sent_at_ms, queue);
        self.routing
            .seal(tx, conversation, &message, FrameKind::QueueRef, recipients)
    }
}

/// A `QueueRef` device `installation` (of either account: my other devices send in the
/// conversation too, spec 7.1) sealed in `conversation` at `sent_at_ms`, in the transaction
/// that takes its frame. Its row becomes that queue, or a withdrawal tombstone, but only when
/// the frame was sent later than the stored one (Ruling R10, as `push_peer`): a late older
/// frame never brings back a queue that was replaced or withdrawn. A queue is taken only on
/// `accepted`, the queue server this device uses (spec 7.2: a release build's own address); one
/// on any other server, or any queue while this device has none configured, changes nothing.
/// A new queue clears `dead`. Returns whether the row changed.
pub(super) fn store_peer_ref(
    tx: &Transaction<'_>,
    conversation: ConversationId,
    installation: InstallationId,
    queue: Option<&QueueRefBody>,
    sent_at_ms: u64,
    now: i64,
    accepted: Option<&str>,
) -> R<bool> {
    let Ok(sent_at_ms) = i64::try_from(sent_at_ms) else {
        return Ok(false);
    };
    if let Some(queue) = queue
        && (Some(queue.server_url.as_str()) != accepted || queue.server_url.len() > MAX_SERVER_URL)
    {
        return Ok(false);
    }
    let changed = tx
        .execute(
            "INSERT INTO queue_peer VALUES(?1,?2,?3,?4,?5,?6,?7,0) ON CONFLICT(conversation,installation) DO UPDATE SET server_url=excluded.server_url,queue_id=excluded.queue_id,sender_secret=excluded.sender_secret,sent_at_ms=excluded.sent_at_ms,received_at_ms=excluded.received_at_ms,dead=0 WHERE excluded.sent_at_ms>queue_peer.sent_at_ms",
            params![
                conversation.0.as_slice(),
                installation.0.as_slice(),
                queue.map(|q| q.server_url.as_str()),
                queue.map(|q| q.queue_id.as_slice()),
                queue.map(|q| q.sender_secret.as_slice()),
                sent_at_ms,
                now
            ],
        )
        .map_err(sql)?;
    Ok(changed > 0)
}

/// One `queue_own` row, for fixture assertions.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueOwnRow {
    pub queue_id: QueueId,
    pub conversation: ConversationId,
    pub recipient_secret: [u8; 32],
    pub sender_secret: [u8; 32],
    pub created_at_ms: u64,
    /// `None` while `Active`; the wall time it is deleted at while `Retiring`.
    pub delete_at_ms: Option<u64>,
    pub last_seq: u64,
    pub server_confirmed: bool,
}

/// One `queue_peer` row, for fixture assertions.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuePeerRow {
    pub conversation: ConversationId,
    pub installation: InstallationId,
    /// `None`: withdrawn (a tombstone).
    pub queue: Option<(String, QueueId, [u8; 32])>,
    pub sent_at_ms: u64,
    pub dead: bool,
}

/// One `queue_delivery` row, for fixture assertions.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueDeliveryRow {
    pub recipient: InstallationId,
    pub frame_hash: [u8; 32],
    pub queue_id: QueueId,
    pub expires_at_ms: u64,
}

/// What a fixture sees of the queue tables.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueTables {
    /// By conversation, then creation.
    pub own: Vec<QueueOwnRow>,
    /// By conversation and installation.
    pub peers: Vec<QueuePeerRow>,
    /// By recipient, then frame hash.
    pub deliveries: Vec<QueueDeliveryRow>,
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_state(c: &Connection) -> R<QueueTables> {
    let own = c
        .prepare("SELECT queue_id,conversation,recipient_secret,sender_secret,created_at_ms,delete_at_ms,last_seq,server_confirmed FROM queue_own ORDER BY conversation,created_at_ms")
        .map_err(sql)?
        .query_map([], |r| {
            Ok(QueueOwnRow {
                queue_id: bytes(r, 0)?,
                conversation: ConversationId(bytes(r, 1)?),
                recipient_secret: bytes(r, 2)?,
                sender_secret: bytes(r, 3)?,
                created_at_ms: uint(r, 4)?,
                delete_at_ms: r.get::<_, Option<i64>>(5)?.map(|at| at as u64),
                last_seq: uint(r, 6)?,
                server_confirmed: r.get(7)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    let peers = c
        .prepare("SELECT conversation,installation,server_url,queue_id,sender_secret,sent_at_ms,dead FROM queue_peer ORDER BY conversation,installation")
        .map_err(sql)?
        .query_map([], |r| {
            let queue = match (
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<Vec<u8>>>(3)?,
                r.get::<_, Option<Vec<u8>>>(4)?,
            ) {
                (Some(url), Some(id), Some(secret)) => Some((
                    url,
                    id.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?,
                    secret.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?,
                )),
                _ => None,
            };
            Ok(QueuePeerRow {
                conversation: ConversationId(bytes(r, 0)?),
                installation: InstallationId(bytes(r, 1)?),
                queue,
                sent_at_ms: uint(r, 5)?,
                dead: r.get(6)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    let deliveries = c
        .prepare("SELECT recipient_install,frame_hash,queue_id,expires_at_ms FROM queue_delivery ORDER BY recipient_install,frame_hash")
        .map_err(sql)?
        .query_map([], |r| {
            Ok(QueueDeliveryRow {
                recipient: InstallationId(bytes(r, 0)?),
                frame_hash: bytes(r, 1)?,
                queue_id: bytes(r, 2)?,
                expires_at_ms: uint(r, 3)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    Ok(QueueTables {
        own,
        peers,
        deliveries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../storage/schema.sql"))
            .unwrap();
        c
    }

    const URL: &str = "https://queue.example";

    fn body(n: u8) -> QueueRefBody {
        QueueRefBody {
            server_url: URL.into(),
            queue_id: [n; 32],
            sender_secret: [n.wrapping_add(1); 32].into(),
        }
    }

    /// (queue id, sent_at) of the row, `None` id for a tombstone.
    fn row(c: &Connection) -> Option<(Option<Vec<u8>>, i64)> {
        c.query_row("SELECT queue_id,sent_at_ms FROM queue_peer", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .unwrap()
    }

    #[test]
    fn newer_frames_win_and_a_tombstone_stays() {
        let mut c = fresh();
        let (conv, from) = (ConversationId([7; 32]), InstallationId([8; 32]));
        let mut take = |queue: Option<QueueRefBody>, sent_at: u64| {
            let tx = c.transaction().unwrap();
            let changed =
                store_peer_ref(&tx, conv, from, queue.as_ref(), sent_at, 5, Some(URL)).unwrap();
            tx.commit().unwrap();
            changed
        };
        assert!(take(Some(body(1)), 1_000));
        assert!(take(Some(body(2)), 2_000));
        // An older or equally old frame never replaces the stored one.
        assert!(!take(Some(body(1)), 1_000));
        assert!(!take(Some(body(3)), 2_000));
        assert!(take(None, 3_000));
        assert!(!take(Some(body(2)), 2_000));
        assert_eq!(row(&c), Some((None, 3_000)));
    }

    #[test]
    fn a_queue_on_another_server_or_with_none_configured_is_ignored() {
        let mut c = fresh();
        let (conv, from) = (ConversationId([7; 32]), InstallationId([8; 32]));
        let tx = c.transaction().unwrap();
        let mut foreign = body(1);
        foreign.server_url = "https://other.example".into();
        assert!(!store_peer_ref(&tx, conv, from, Some(&foreign), 1_000, 5, Some(URL)).unwrap());
        assert!(!store_peer_ref(&tx, conv, from, Some(&body(1)), 1_000, 5, None).unwrap());
        // A withdrawal needs no server: it only stops use of what was taken before.
        assert!(store_peer_ref(&tx, conv, from, None, 2_000, 5, None).unwrap());
        tx.commit().unwrap();
        assert_eq!(row(&c), Some((None, 2_000)));
    }

    /// Review minor 1: a `queue_delivery` row keeps a frame out of a device's queue only while
    /// that queue is still the device's and the row has not expired. With the mode "never", or
    /// for a device a session reaches, nothing goes.
    #[test]
    fn a_frame_goes_again_into_a_new_queue_or_after_its_hold() {
        let c = fresh();
        let (conv, to) = (ConversationId([7; 32]), InstallationId([8; 32]));
        let identity = Identity {
            peer: PeerId([1; 32]),
            installation: InstallationId([2; 32]),
        };
        let commit = b"commit".to_vec();
        c.execute(
            "INSERT INTO mls_delivery VALUES(?1,?2,1,3,?3,0,0)",
            params![conv.0.as_slice(), to.0.as_slice(), commit],
        )
        .unwrap();
        let announce = |queue: u8| {
            c.execute(
                "INSERT OR REPLACE INTO queue_peer VALUES(?1,?2,?3,?4,?5,0,0,0)",
                params![
                    conv.0.as_slice(),
                    to.0.as_slice(),
                    URL,
                    [queue; 32].as_slice(),
                    [9u8; 32].as_slice()
                ],
            )
            .unwrap();
        };
        let hold = |queue: u8, until: i64| {
            c.execute(
                "INSERT OR REPLACE INTO queue_delivery VALUES(?1,?2,?3,?4)",
                params![
                    to.0.as_slice(),
                    frame_hash(&commit).as_slice(),
                    [queue; 32].as_slice(),
                    until
                ],
            )
            .unwrap();
        };
        let sends = |reached: Vec<InstallationId>, wall: u64| {
            to_send(&c, identity, Unreached::Standing { reached }, URL, wall)
                .unwrap()
                .map(|send| {
                    send.targets
                        .into_iter()
                        .map(|t| (t.queue_id[0], t.frames))
                        .collect::<Vec<_>>()
                })
        };
        let one = |queue: u8| Some(vec![(queue, vec![(identity.installation, commit.clone())])]);
        announce(1);
        assert_eq!(sends(vec![], 10), one(1));
        assert_eq!(sends(vec![to], 10), None);
        hold(1, 100);
        assert_eq!(sends(vec![], 10), None);
        assert_eq!(sends(vec![], 100), one(1));
        // The device announced another queue: the row for the old one holds nothing back.
        announce(2);
        assert_eq!(sends(vec![], 10), one(2));
        c.execute("UPDATE queue_local SET mode=2", []).unwrap();
        assert_eq!(sends(vec![], 10), None);
    }
}
