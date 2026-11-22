//! Queue rows of the message store (M8, schema v5): this device's queues (`queue_own`, spec 6.2)
//! and the queues other devices announced (`queue_peer`, spec 7.2). The `QueueRef` frames this
//! device seals ride `push_delivery` as kind 2 (`FrameKind::QueueRef`), sent like the push
//! handles' frames. Each `QueueDbOp` is one transaction; `store_peer_ref` runs inside the
//! caller's.
use queue_wire::{QueueId, RETIRE_MS};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use zeroize::Zeroizing;

use super::mls_group::{group_row, signer_of, sql};
use super::mls_store::{Fault, finish, seal_one};
use super::push_store::FrameKind;
use super::store::{Identity, MessageDbReply, bytes, integer, uint};
use super::types::{ConversationId, MessageKey};
use crate::account::{InstallationId, PeerId, storage::schema::LocalAccount};
use crate::mailbox::verdict::QueueMode;
use crate::mls::{
    MlsProvider,
    kv::KvStore,
    payload::{MAX_SERVER_URL, QueueRefBody, queue_ref_message},
};
use crate::{CoreError, ErrorCode};
use openmls_traits::OpenMlsProvider as _;

type R<T> = Result<T, CoreError>;

/// A queue's state (spec 6.2): `Active` (at most one per conversation), or `Retiring`, still
/// fetched until `delete_at_ms` (wall) and then deleted at the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueState {
    Active,
    Retiring { delete_at_ms: u64 },
}

/// One `queue_own` row. For an `Active` queue `confirmed` says the server took its `CREATE` and
/// its `QueueRef` is queued for the peer; for a `Retiring` one, that the server took it.
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
    /// The local half of a reconcile, in one transaction: `retire` goes `Retiring` (its
    /// withdrawal sealed for each current device of the peer when `true`), then `new` rows are
    /// written, and `forget` rows are dropped without a request. `devices`: each peer account's
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
}

impl QueueDbOp {
    pub(crate) fn writes(&self) -> bool {
        !matches!(self, Self::Load | Self::Using)
    }
}

pub(crate) enum QueueDbReply {
    Loaded(QueueView),
    Using(bool),
    /// Withdrawals sealed.
    Applied {
        withdrawn: u32,
    },
    /// `QueueRef`s sealed, and created `Active` queues whose `QueueRef` could not be sealed yet
    /// (no group or no known device of the peer): they stay unconfirmed for a later run.
    Settled {
        announced: u32,
        unsealed: u32,
    },
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
            let tx = c.transaction().map_err(sql)?;
            let seal = Sealer {
                identity,
                local,
                devices: &devices,
                wall,
                now,
            };
            let mut withdrawn = 0;
            for (id, withdraw) in retire {
                let Some(conversation) = active_conversation(&tx, &id)? else {
                    continue;
                };
                let at = wall.max(floor(&tx, conversation)?.saturating_add(1));
                tx.execute(
                    "UPDATE queue_own SET state=1,delete_at_ms=?2 WHERE queue_id=?1 AND state=0",
                    params![id.as_slice(), integer(at.saturating_add(RETIRE_MS))?],
                )
                .map_err(sql)?;
                if withdraw && seal.queue_ref(&tx, conversation, at, None)? {
                    withdrawn += 1;
                }
            }
            for queue in new {
                let at = wall.max(floor(&tx, queue.conversation)?.saturating_add(1));
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
            }
            for id in forget {
                tx.execute(
                    "DELETE FROM queue_own WHERE queue_id=?1",
                    params![id.as_slice()],
                )
                .map_err(sql)?;
            }
            finish(tx, fault)?;
            QueueDbReply::Applied { withdrawn }
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
            let tx = c.transaction().map_err(sql)?;
            let seal = Sealer {
                identity,
                local,
                devices: &devices,
                wall,
                now,
            };
            let (mut announced, mut unsealed) = (0, 0);
            for id in created {
                let row = tx
                    .query_row(
                        "SELECT conversation,sender_secret,created_at_ms,state,server_confirmed FROM queue_own WHERE queue_id=?1",
                        params![id.as_slice()],
                        |r| {
                            Ok((
                                ConversationId(bytes(r, 0)?),
                                Zeroizing::new(bytes::<32>(r, 1)?),
                                uint(r, 2)?,
                                r.get::<_, i64>(3)?,
                                r.get::<_, bool>(4)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(sql)?;
                let Some((conversation, sender_secret, created_at_ms, state, confirmed)) = row
                else {
                    continue;
                };
                if confirmed {
                    continue;
                }
                if state == 0 {
                    let body = QueueRefBody {
                        server_url: server_url.clone(),
                        queue_id: id,
                        sender_secret: *sender_secret,
                    };
                    if !seal.queue_ref(&tx, conversation, created_at_ms, Some(body))? {
                        unsealed += 1;
                        continue;
                    }
                    announced += 1;
                }
                tx.execute(
                    "UPDATE queue_own SET server_confirmed=1,last_seq=0 WHERE queue_id=?1",
                    params![id.as_slice()],
                )
                .map_err(sql)?;
            }
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
    };
    Ok(MessageDbReply::Queue(reply))
}

fn load(c: &Connection) -> R<QueueView> {
    let mode = c
        .query_row("SELECT mode FROM queue_local WHERE id=1", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_err(sql)?;
    let mode = match mode {
        0 => QueueMode::Auto,
        1 => QueueMode::Always,
        2 => QueueMode::Never,
        _ => return Err(CoreError::new(ErrorCode::StorageCorrupt, "queue_mode")),
    };
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
    identity: Identity,
    local: &'a LocalAccount,
    devices: &'a [(PeerId, Vec<InstallationId>)],
    wall: u64,
    now: u64,
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
        let Some(recipients) = peer.and_then(|peer| {
            self.devices
                .iter()
                .find(|(p, _)| p.0 == peer)
                .map(|(_, r)| r)
                .filter(|r| !r.is_empty())
        }) else {
            return Ok(false);
        };
        let Some(group) = group_row(tx, conversation)? else {
            return Ok(false);
        };
        let provider = MlsProvider::new(KvStore::load_scope(tx, conversation.0)?);
        let (signer, _) = signer_of(self.local);
        let message = queue_ref_message(self.identity.installation, sent_at_ms, queue);
        let key = MessageKey {
            conversation,
            sender: self.identity.peer,
            installation: self.identity.installation,
            id: message.msg_id,
        };
        let frame = match seal_one(
            tx,
            &provider,
            &signer,
            self.identity,
            key,
            &group.group_id,
            &message,
            self.wall,
        ) {
            Ok((_, frame)) => frame,
            Err(error) if error.code == ErrorCode::InvalidMessage => return Ok(false),
            Err(error) => return Err(error),
        };
        for recipient in recipients {
            tx.execute(
                "INSERT OR REPLACE INTO push_delivery VALUES(?1,?2,?3,?4,?5,0,?6,0)",
                params![
                    conversation.0.as_slice(),
                    recipient.0.as_slice(),
                    FrameKind::QueueRef.value(),
                    message.msg_id.0.as_slice(),
                    frame,
                    integer(self.now)?
                ],
            )
            .map_err(sql)?;
        }
        provider.storage().flush(tx, conversation.0)?;
        Ok(true)
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

/// What a fixture sees of the queue tables.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueTables {
    /// By conversation, then creation.
    pub own: Vec<QueueOwnRow>,
    /// By conversation and installation.
    pub peers: Vec<QueuePeerRow>,
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
    Ok(QueueTables { own, peers })
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
            sender_secret: [n.wrapping_add(1); 32],
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
        drop(take);
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
}
