use super::types::*;
use super::wire::{Frame, ReceiptFrame, ReceiptKind, TextFrame};
use crate::{
    CoreError, ErrorCode,
    account::{InstallationId, PeerId, storage::schema::LocalAccount},
    storage::worker::StorageHandle,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;
type R<T> = Result<T, CoreError>;
#[derive(Clone, Copy)]
pub(crate) struct Identity {
    pub peer: PeerId,
    pub installation: InstallationId,
}
#[derive(Clone)]
pub(crate) struct MessageRepo {
    storage: StorageHandle,
    identity: Identity,
}
pub(crate) enum MessageDbOp {
    Observed(Box<MessageDbOp>),
    MarkRead(ConversationId, Vec<MessageKey>, u64),
    #[cfg(all(feature="test-hooks",debug_assertions))]
    RejectedPrepare(MessageKey, InstallationId, u64, PrepareTransition),
    Due(u64, Vec<PeerId>, [Option<DueCursor>; 2]),
    RecoverScheduler(u64),
    SettleCompleted(MessageKey),
    Defer(MessageKey, bool, u64),
    Cancel(MessageKey),
    Retry(MessageKey, u64),
    ReleasePeer(PeerId, u64),
    ResetUnlaunched(MessageKey, PrepareToken),
    Receive(TextFrame, [u8; 32], u64, u64),
    Prepare(MessageKey, InstallationId, bool, u64, u64, PrepareTransition),
    Target(MessageKey, bool),
    ApplyReceipt(ReceiptFrame, PeerId, u64),
    Applied(ReceiptFrame, PeerId),
    AttemptOutcome(MessageKey, Option<BlockReason>, Option<ErrorCode>, bool),
    Open(PeerId, PeerId),
    Enqueue(PeerId, MessageId, String, u64),
    Messages(ConversationId, Option<MessageCursor>, u32),
    Conversations(Option<ConversationCursor>, u32),
}

// Provenance of one Prepare, not authority to send. Only its SQL execution can
// become eligible; dropping a queued/rejected operation settles Pending too.
#[derive(Clone)]
pub(crate) struct PrepareToken(Arc<Mutex<PrepareState>>);
#[derive(Clone, Copy, PartialEq, Eq)]
enum PrepareState {
    Pending,
    RejectedWithoutMutation,
    EligibleForTransition,
}
pub(crate) struct PrepareTransition(PrepareToken);
impl PrepareToken {
    pub(crate) fn new() -> (Self, PrepareTransition) {
        let token = Self(Arc::new(Mutex::new(PrepareState::Pending)));
        (token.clone(), PrepareTransition(token))
    }
    fn eligible(&self) -> R<bool> {
        match *self.0.lock().unwrap() {
            PrepareState::Pending => Err(CoreError::new(ErrorCode::Busy, "prepare_pending")),
            PrepareState::RejectedWithoutMutation => Ok(false),
            PrepareState::EligibleForTransition => Ok(true),
        }
    }
}
impl PrepareTransition {
    fn eligible(&self) {
        *self.0.0.lock().unwrap() = PrepareState::EligibleForTransition;
    }
}
impl Drop for PrepareTransition {
    fn drop(&mut self) {
        let mut state = self.0.0.lock().unwrap();
        if *state == PrepareState::Pending {
            *state = PrepareState::RejectedWithoutMutation;
        }
    }
}

fn invalid() -> CoreError {
    CoreError::new(ErrorCode::InvalidMessage, "invalid_message")
}
pub(crate) fn sql_error(e: rusqlite::Error) -> CoreError {
    if let rusqlite::Error::SqliteFailure(code, _) = e
        && code.code == rusqlite::ErrorCode::DiskFull
    {
        return CoreError::new(ErrorCode::StorageFull, "message_storage_full");
    }
    CoreError::new(ErrorCode::StorageUncertain, "message_storage_uncertain")
}
fn integer(n: u64) -> R<i64> {
    n.try_into().map_err(|_| invalid())
}
fn limit(n: u32) -> R<i64> {
    if (1..=50).contains(&n) {
        Ok(i64::from(n))
    } else {
        Err(invalid())
    }
}
pub(crate) fn conversation_id(local: PeerId, peer: PeerId) -> R<ConversationId> {
    if local == peer {
        return Err(CoreError::new(
            ErrorCode::SelfConnection,
            "self_conversation",
        ));
    }
    let (a, b) = if local < peer {
        (local, peer)
    } else {
        (peer, local)
    };
    let mut hash = Sha256::new();
    hash.update(b"ChatProject/dm/v1");
    hash.update(a.0);
    hash.update(b.0);
    Ok(ConversationId(hash.finalize().into()))
}
const COLUMNS: &str = "conversation,sender_peer,sender_install,msg_id,lamport,sent_at_ms,body,outgoing,outcome,delivered_at_ms,read_at_ms,cancel_requested,block_reason";
fn uint(row: &Row<'_>, n: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(n)?;
    value.try_into().map_err(|_| rusqlite::Error::InvalidQuery)
}
fn bytes<const N: usize>(row: &Row<'_>, n: usize) -> rusqlite::Result<[u8; N]> {
    let b: Vec<u8> = row.get(n)?;
    b.try_into().map_err(|_| rusqlite::Error::InvalidQuery)
}
fn message_row(row: &Row<'_>) -> rusqlite::Result<MessageView> {
    let outcome: i64 = row.get(8)?;
    let delivered: Option<i64> = row.get(9)?;
    let read: Option<i64> = row.get(10)?;
    let cancel: bool = row.get(11)?;
    let state = if read.is_some() {
        DeliveryState::Read
    } else if delivered.is_some() {
        DeliveryState::Delivered
    } else if cancel {
        DeliveryState::Cancelled
    } else {
        match outcome {
            0 => DeliveryState::Queued,
            1 => DeliveryState::SentDirect,
            2 => DeliveryState::Blocked,
            3 => DeliveryState::Failed,
            _ => return Err(rusqlite::Error::InvalidQuery),
        }
    };
    Ok(MessageView {
        key: MessageKey {
            conversation: ConversationId(bytes(row, 0)?),
            sender: PeerId(bytes(row, 1)?),
            installation: InstallationId(bytes(row, 2)?),
            id: MessageId(bytes(row, 3)?),
        },
        lamport: uint(row, 4)?,
        sent_at_ms: uint(row, 5)?,
        body: row.get(6)?,
        outgoing: row.get(7)?,
        state,
        cancel_requested: cancel,
        block: match row.get::<_, Option<i64>>(12)? {
            None => None,
            Some(1) => Some(BlockReason::Account),
            Some(2) => Some(BlockReason::ContactRequired),
            Some(3) => Some(BlockReason::Storage),
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
    })
}
fn find(c: &Connection, key: MessageKey) -> R<Option<MessageView>> {
    c.query_row(&format!("SELECT {COLUMNS} FROM message WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4"),
        params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],message_row).optional().map_err(sql_error)
}
fn revision(c: &Connection) -> R<u64> {
    c.query_row(
        "SELECT message_revision FROM storage_meta WHERE singleton=1",
        [],
        |r| uint(r, 0),
    )
    .map_err(sql_error)
}
fn bump(tx: &Transaction<'_>) -> R<u64> {
    let next = revision(tx)?
        .checked_add(1)
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or_else(invalid)?;
    tx.execute(
        "UPDATE storage_meta SET message_revision=?1 WHERE singleton=1",
        params![next as i64],
    )
    .map_err(sql_error)?;
    Ok(next)
}
fn ensure_conversation(tx: &Transaction<'_>, id: ConversationId, peer: PeerId) -> R<()> {
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM conversation WHERE id=?1)",
            params![id.0.as_slice()],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    if !exists {
        let rev = bump(tx)?;
        tx.execute(
            "INSERT INTO conversation VALUES(?1,?2,0,?3,?4,0)",
            params![
                id.0.as_slice(),
                peer.0.as_slice(),
                [0u8; 32].as_slice(),
                rev as i64
            ],
        )
        .map_err(sql_error)?;
    }
    Ok(())
}
fn conversation(c: &Connection, id: ConversationId) -> R<ConversationView> {
    let (peer, unread, change): (Vec<u8>, u64, u64) = c
        .query_row(
            "SELECT peer,unread,change_revision FROM conversation WHERE id=?1",
            params![id.0.as_slice()],
            |r| Ok((r.get(0)?, uint(r, 1)?, uint(r, 2)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| CoreError::new(ErrorCode::MessageNotFound, "conversation_not_found"))?;
    let last=c.query_row(&format!("SELECT {COLUMNS} FROM message WHERE conversation=?1 ORDER BY lamport DESC,sender_install DESC,msg_id DESC LIMIT 1"),params![id.0.as_slice()],message_row).optional().map_err(sql_error)?;
    Ok(ConversationView {
        id,
        peer: PeerId(peer.try_into().map_err(|_| invalid())?),
        last,
        unread,
        change_revision: change,
    })
}
pub(crate) fn execute(
    c: &mut Connection,
    identity: Identity,
    op: MessageDbOp,
    #[cfg(all(feature = "test-hooks", debug_assertions))] fault: Option<
        crate::storage::worker::CommitFault,
    >,
) -> R<MessageDbReply> {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    if fault == Some(crate::storage::worker::CommitFault::WriteDenied)
        && op.key(identity)?.is_some()
    {
        c.pragma_update(None, "query_only", true)
            .map_err(sql_error)?;
    }
    match op {
        MessageDbOp::Observed(op) => {
            let before = revision(c)?;
            let key = op.key(identity)?;
            let prior = key.map(|k| find(c, k)).transpose()?.flatten().map(|m| (m.state, m.cancel_requested, m.block));
            let cid = match &*op {
                MessageDbOp::Open(local, peer) => Some(conversation_id(*local, *peer)?),
                MessageDbOp::ReleasePeer(peer, _) => Some(conversation_id(identity.peer, *peer)?),
                MessageDbOp::MarkRead(id, _, _) => Some(*id),
                _ => key.map(|k| k.conversation),
            };
            let reply = execute(c, identity, *op, #[cfg(all(feature="test-hooks",debug_assertions))] fault)?;
            let after = revision(c)?;
            let mut events = Vec::new();
            if after > before && let Some(conversation) = cid {
                events.push(crate::CoreEvent::MessagesChanged { revision: after, conversation });
                if let Some(key) = key && let Some(view) = find(c, key)? {
                    if prior != Some((view.state, view.cancel_requested, view.block)) {
                        events.push(crate::CoreEvent::MessageStateChanged { revision: after, key, state: view.state, cancel_requested: view.cancel_requested });
                    }
                }
            }
            Ok(MessageDbReply::Observed(Box::new(reply), events))
        }
        MessageDbOp::MarkRead(id, keys, now) => mark_read(c, identity, id, keys, now, #[cfg(all(feature="test-hooks",debug_assertions))] fault),
        #[cfg(all(feature="test-hooks",debug_assertions))]
        MessageDbOp::RejectedPrepare(key,receiver,now,transition) => {
            let state:Option<u64>=c.query_row("SELECT dispatch_state FROM outbox WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],|r|uint(r,0)).optional().map_err(sql_error)?;
            if state!=Some(1) {return Err(invalid());}
            prepare(c,identity,key,receiver,false,now,transition,fault)
        }
        MessageDbOp::Retry(key, _) => {
            let peer=target(c,identity,key,false)?;
            release_blocked(c,peer,Some(key),#[cfg(all(feature="test-hooks",debug_assertions))] fault)?;
            Ok(MessageDbReply::Message(find(c,key)?.ok_or_else(missing)?))
        }
        MessageDbOp::ReleasePeer(peer, _) => {
            release_blocked(c,peer,None,#[cfg(all(feature="test-hooks",debug_assertions))] fault)?;
            Ok(MessageDbReply::Done)
        }
        MessageDbOp::Cancel(key) => {
            let tx=c.transaction().map_err(sql_error)?;
            let view=find(&tx,key)?.ok_or_else(missing)?;
            if !view.outgoing || key.sender!=identity.peer || key.installation!=identity.installation {return Err(invalid());}
            if !view.cancel_requested {
                tx.execute("UPDATE message SET cancel_requested=1 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()]).map_err(sql_error)?;
                tx.execute("DELETE FROM outbox WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()]).map_err(sql_error)?;
                let revision=bump(&tx)?;
                tx.execute("UPDATE conversation SET change_revision=?2 WHERE id=?1",params![key.conversation.0.as_slice(),integer(revision)?]).map_err(sql_error)?;
            }
            let view=find(&tx,key)?.unwrap();
            commit(tx,#[cfg(all(feature="test-hooks",debug_assertions))] fault)?;
            Ok(MessageDbReply::Message(view))
        },
        MessageDbOp::Due(now, excluded, cursors) => due(c, identity, now, excluded, cursors),
        MessageDbOp::RecoverScheduler(now) => {
            let tx = c.transaction().map_err(sql_error)?;
            let now = integer(now)?;
            // Called only once by a new Manager before any local launch.
            tx.execute("UPDATE outbox SET dispatch_state=CASE WHEN dispatch_state=1 THEN 0 ELSE dispatch_state END,next_retry_at_ms=?1+min(max(next_retry_at_ms-?1,0),retry_delay_ms,300000)",params![now]).map_err(sql_error)?;
            tx.execute("UPDATE receipt_job SET next_retry_at_ms=?1+min(max(next_retry_at_ms-?1,0),300000)",params![now]).map_err(sql_error)?;
            tx.commit().map_err(sql_error)?;
            Ok(MessageDbReply::Done)
        }
        MessageDbOp::SettleCompleted(k) => {
            // Exact actor-owned completed launch. Never reconstruct a deleted job.
            let tx=c.transaction().map_err(sql_error)?;
            tx.execute("UPDATE outbox SET dispatch_state=0 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND dispatch_state=1",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice()]).map_err(sql_error)?;
            commit(tx,#[cfg(all(feature="test-hooks",debug_assertions))] fault)?;
            Ok(MessageDbReply::Done)
        }
        MessageDbOp::Defer(k, receipt, now) => {
            let tx = c.transaction().map_err(sql_error)?;
            schedule_retry(&tx, k, receipt, now)?;
            commit(tx,#[cfg(all(feature="test-hooks",debug_assertions))] fault)?;
            Ok(MessageDbReply::Done)
        }
        MessageDbOp::ResetUnlaunched(k, provenance) => {
            if k.sender != identity.peer || k.installation != identity.installation {
                return Err(invalid());
            }
            // The FIFO worker has settled the original operation before this read.
            if !provenance.eligible()? {
                return Ok(MessageDbReply::Done);
            }
            let tx = c.transaction().map_err(sql_error)?;
            tx.execute("UPDATE outbox SET dispatch_state=0 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND dispatch_state=1 AND EXISTS(SELECT 1 FROM message m WHERE m.conversation=?1 AND m.sender_peer=?2 AND m.sender_install=?3 AND m.msg_id=?4 AND m.cancel_requested=0 AND m.delivered_at_ms IS NULL)",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice()]).map_err(sql_error)?;
            commit(
                tx,
                #[cfg(all(feature = "test-hooks", debug_assertions))]
                fault,
            )?;
            Ok(MessageDbReply::Done)
        }
        MessageDbOp::Receive(text, hash, now, schedule_now) => receive(
            c,
            identity,
            text,
            hash,
            now,
            schedule_now,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            fault,
        ),
        MessageDbOp::Target(key, receipt) => {
            target(c, identity, key, receipt).map(MessageDbReply::Peer)
        }
        MessageDbOp::Prepare(key, receiver, receipt, _wall, now, transition) => prepare(
            c,
            identity,
            key,
            receiver,
            receipt,
            now,
            transition,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            fault,
        ),
        MessageDbOp::ApplyReceipt(r, peer, now) => apply_receipt(
            c,
            identity,
            r,
            peer,
            now,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            fault,
        ),
        MessageDbOp::Applied(r, peer) => applied(
            c,
            identity,
            r,
            peer,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            fault,
        ),
        MessageDbOp::AttemptOutcome(key, block, failure, written) => attempt_outcome(
            c,
            key,
            block,
            failure,
            written,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            fault,
        ),
        MessageDbOp::Open(local, peer) => {
            if local != identity.peer {
                return Err(CoreError::new(
                    ErrorCode::AccountRestricted,
                    "message_identity_mismatch",
                ));
            }
            let id = conversation_id(local, peer)?;
            let tx = c.transaction().map_err(sql_error)?;
            ensure_conversation(&tx, id, peer)?;
            let view = conversation(&tx, id)?;
            tx.commit().map_err(sql_error)?;
            Ok(MessageDbReply::Conversation(view))
        }
        MessageDbOp::Enqueue(peer, id, body, now) => {
            let cid = conversation_id(identity.peer, peer)?;
            let key = MessageKey {
                conversation: cid,
                sender: identity.peer,
                installation: identity.installation,
                id,
            };
            // Idempotency precedes body/time validation and quotas, including terminal rows.
            if let Some(original) = find(c, key)? {
                return if original.body == body {
                    Ok(MessageDbReply::Message(original))
                } else {
                    Err(CoreError::new(
                        ErrorCode::MessageConflict,
                        "message_id_conflict",
                    ))
                };
            }
            if body.is_empty() {
                return Err(invalid());
            }
            if body.len() > 16384 {
                return Err(CoreError::new(
                    ErrorCode::MessageTooLarge,
                    "message_text_too_large",
                ));
            }
            let now = integer(now)?;
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            if fault == Some(crate::storage::worker::CommitFault::WriteDenied) {
                c.pragma_update(None, "query_only", true)
                    .map_err(sql_error)?;
            }
            let tx = c.transaction().map_err(sql_error)?;
            ensure_conversation(&tx, cid, peer)?;
            let (maximum, prev): (u64, Vec<u8>) = tx
                .query_row(
                    "SELECT lamport_max,local_prev_hash FROM conversation WHERE id=?1",
                    params![cid.0.as_slice()],
                    |r| Ok((uint(r, 0)?, r.get(1)?)),
                )
                .map_err(sql_error)?;
            let lamport = maximum
                .checked_add(1)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(invalid)?;
            let frame = TextFrame {
                key,
                lamport,
                prev_hash: prev.try_into().map_err(|_| invalid())?,
                sent_at_ms: now as u64,
                body,
            };
            let (wire, hash) = frame.encode();
            if wire.len() > 61440 {
                return Err(CoreError::new(
                    ErrorCode::MessageTooLarge,
                    "message_frame_too_large",
                ));
            }
            let (count, bytes): (u64, u64) = tx
                .query_row(
                    "SELECT count(*),coalesce(sum(length(wire)),0) FROM outbox",
                    [],
                    |r| Ok((uint(r, 0)?, uint(r, 1)?)),
                )
                .map_err(sql_error)?;
            if count >= 10_000 || bytes + wire.len() as u64 > 128 * 1024 * 1024 {
                return Err(CoreError::new(ErrorCode::StorageFull, "message_queue_full"));
            }
            insert_outgoing(&tx, &frame, &hash, &wire)?;
            let rev = bump(&tx)?;
            tx.execute("UPDATE conversation SET lamport_max=?2,local_prev_hash=?3,change_revision=?4 WHERE id=?1",params![cid.0.as_slice(),lamport as i64,hash.as_slice(),rev as i64]).map_err(sql_error)?;
            let view = find(&tx, key)?.ok_or_else(invalid)?;
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            if fault == Some(crate::storage::worker::CommitFault::BeforeCommit) {
                return Err(CoreError::new(
                    ErrorCode::StorageUncertain,
                    "message_storage_uncertain",
                ));
            }
            tx.commit().map_err(sql_error)?;
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            if fault == Some(crate::storage::worker::CommitFault::AfterCommit) {
                return Err(CoreError::new(
                    ErrorCode::StorageUncertain,
                    "message_storage_uncertain",
                ));
            }
            Ok(MessageDbReply::Message(view))
        }
        MessageDbOp::Messages(id, before, n) => {
            let take = limit(n)?;
            let (lamport, install, msg) = match before {
                Some(b) => (integer(b.lamport)?, b.installation.0, b.id.0),
                None => (0, [0; 32], [0; 16]),
            };
            let mut q=c.prepare(&format!("SELECT {COLUMNS} FROM message WHERE conversation=?1 AND (?2=0 OR (lamport,sender_install,msg_id)<(?3,?4,?5)) ORDER BY lamport DESC,sender_install DESC,msg_id DESC LIMIT ?6")).map_err(sql_error)?;
            let mut items = q
                .query_map(
                    params![
                        id.0.as_slice(),
                        before.is_some(),
                        lamport,
                        install.as_slice(),
                        msg.as_slice(),
                        take + 1
                    ],
                    message_row,
                )
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            let more = items.len() > n as usize;
            items.truncate(n as usize);
            let next = if more {
                items.last().map(|m| MessageCursor {
                    lamport: m.lamport,
                    installation: m.key.installation,
                    id: m.key.id,
                })
            } else {
                None
            };
            Ok(MessageDbReply::Messages(MessagePage {
                revision: revision(c)?,
                items,
                next,
            }))
        }
        MessageDbOp::Conversations(before, n) => {
            let take = limit(n)?;
            let (change, id) = if let Some(b) = before {
                let current = conversation(c, b.id).map_err(|e| {
                    if e.code == ErrorCode::MessageNotFound {
                        invalid()
                    } else {
                        e
                    }
                })?;
                if current.change_revision != b.change_revision {
                    return Err(invalid());
                }
                (integer(b.change_revision)?, b.id.0)
            } else {
                (0, [0; 32])
            };
            let mut q=c.prepare("SELECT id FROM conversation WHERE (?1=0 OR (change_revision,id)<(?2,?3)) ORDER BY change_revision DESC,id DESC LIMIT ?4").map_err(sql_error)?;
            let ids = q
                .query_map(
                    params![before.is_some(), change, id.as_slice(), take + 1],
                    |r| bytes::<32>(r, 0),
                )
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            let more = ids.len() > n as usize;
            let items = ids
                .into_iter()
                .take(n as usize)
                .map(|id| conversation(c, ConversationId(id)))
                .collect::<R<Vec<_>>>()?;
            let next = if more {
                items.last().map(|c| ConversationCursor {
                    change_revision: c.change_revision,
                    id: c.id,
                })
            } else {
                None
            };
            Ok(MessageDbReply::Conversations(ConversationPage {
                revision: revision(c)?,
                items,
                next,
            }))
        }
    }
}
fn insert_outgoing(tx: &Transaction<'_>, frame: &TextFrame, hash: &[u8; 32], wire: &[u8]) -> R<()> {
    let k = frame.key;
    tx.execute(
        "INSERT INTO message VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8,?9,1,0,NULL,NULL,1,0,NULL,NULL,?10)",
        params![
            k.conversation.0.as_slice(),
            k.sender.0.as_slice(),
            k.installation.0.as_slice(),
            k.id.0.as_slice(),
            integer(frame.lamport)?,
            frame.prev_hash.as_slice(),
            hash.as_slice(),
            integer(frame.sent_at_ms)?,
            frame.body,
            [].as_slice() as &[u8]
        ],
    )
    .map_err(sql_error)?;
    tx.execute(
        "INSERT INTO outbox VALUES(?1,?2,?3,?4,1,?5,0,1000,0,0)",
        params![
            k.conversation.0.as_slice(),
            k.sender.0.as_slice(),
            k.installation.0.as_slice(),
            k.id.0.as_slice(),
            wire
        ],
    )
    .map_err(sql_error)?;
    Ok(())
}
/// Query the actual outcome after reopening before any further message work.
pub(crate) fn reconcile(c: &mut Connection, pending: PendingMessage) -> R<()> {
    let key = pending.key;
    if let Some(m) = find(c, key)? {
        let (prev,hash,wire):(Vec<u8>,Vec<u8>,Option<Vec<u8>>)=c.query_row("SELECT m.prev_hash,m.payload_hash,o.wire FROM message m LEFT JOIN outbox o USING(conversation,sender_peer,sender_install,msg_id) WHERE m.conversation=?1 AND m.sender_peer=?2 AND m.sender_install=?3 AND m.msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(sql_error)?;
        let raw:Vec<u8>=c.query_row("SELECT attempted_receivers FROM message WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],|r|r.get(0)).map_err(sql_error)?;
        receivers(&raw)?;
        if !m.outgoing {
            let processed:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM processed_ct WHERE codec=1 AND sender_peer=?1 AND sender_install=?2 AND hash=?3)",params![key.sender.0.as_slice(),key.installation.0.as_slice(),hash.as_slice()],|r|r.get(0)).map_err(sql_error)?;
            if !processed || wire.is_some() {
                return Err(CoreError::new(
                    ErrorCode::StorageCorrupt,
                    "message_outcome_invalid",
                ));
            }
        }
        let frame = TextFrame {
            key,
            lamport: m.lamport,
            prev_hash: prev.try_into().map_err(|_| invalid())?,
            sent_at_ms: m.sent_at_ms,
            body: m.body,
        };
        let (canonical, digest) = frame.encode();
        if digest.as_slice() != hash
            || wire.as_ref().is_some_and(|w| *w != *canonical)
            || (m.outgoing
                && wire.is_none()
                && matches!(
                    m.state,
                    DeliveryState::Queued | DeliveryState::SentDirect | DeliveryState::Blocked
                ))
        {
            return Err(CoreError::new(
                ErrorCode::StorageCorrupt,
                "message_outcome_invalid",
            ));
        }
    }
    if pending
        .prebyte_attempt
        .as_ref()
        .map(PrepareToken::eligible)
        .transpose()?
        .unwrap_or(false)
    {
        c.execute("UPDATE outbox SET dispatch_state=0 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND dispatch_state=1 AND EXISTS(SELECT 1 FROM message m WHERE m.conversation=?1 AND m.sender_peer=?2 AND m.sender_install=?3 AND m.msg_id=?4 AND m.cancel_requested=0 AND m.delivered_at_ms IS NULL)",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()]).map_err(sql_error)?;
    }
    Ok(())
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub struct MessageFacts {
    pub messages: u64,
    pub jobs: u64,
    pub quota_bytes: u64,
    pub wire: Vec<u8>,
    pub hash: [u8; 32],
    pub prev_hash: [u8; 32],
    pub attempted_receivers: Vec<InstallationId>,
    pub receipt_jobs: u64,
    pub processed: u64,
    pub dispatch_state: Option<u64>,
    pub retry: Option<(u64, u64, u64)>,
    pub receipt_retry: Option<(u64, u64)>,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(crate) enum FixtureOp {
    SaturateAttempts(MessageKey, bool, tokio::sync::oneshot::Sender<R<()>>),
    Counts(tokio::sync::oneshot::Sender<R<(u64, u64, u64, u64)>>),
    Facts(MessageKey, tokio::sync::oneshot::Sender<R<MessageFacts>>),
    Fill(
        MessageKey,
        u64,
        Option<u64>,
        tokio::sync::oneshot::Sender<R<()>>,
    ),
    Delivered(MessageKey, tokio::sync::oneshot::Sender<R<()>>),
    Lamport(ConversationId, u64, tokio::sync::oneshot::Sender<R<()>>),
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(crate) fn fixture(c: &mut Connection, op: FixtureOp) {
    match op {
        FixtureOp::SaturateAttempts(k,receipt,reply) => {
            let table=if receipt {"receipt_job"} else {"outbox"};
            let result=c.execute(&format!("UPDATE {table} SET attempts=4294967295 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4"),params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice()]).map(|_|()).map_err(sql_error);
            let _=reply.send(result);
        }
        FixtureOp::Counts(reply) => {
            let result=c.query_row("SELECT (SELECT count(*) FROM message),(SELECT count(*) FROM processed_ct),(SELECT count(*) FROM receipt_job),message_revision FROM storage_meta WHERE singleton=1",[],|r|Ok((uint(r,0)?,uint(r,1)?,uint(r,2)?,uint(r,3)?))).map_err(sql_error);
            let _ = reply.send(result);
        }
        FixtureOp::Facts(key, tx) => {
            let result = (|| {
                let (messages,jobs,quota_bytes):(u64,u64,u64)=c.query_row("SELECT (SELECT count(*) FROM message),count(*),coalesce(sum(length(wire)),0) FROM outbox",[],|r|Ok((uint(r,0)?,uint(r,1)?,uint(r,2)?))).map_err(sql_error)?;
                let (hash,wire):([u8;32],Vec<u8>)=c.query_row("SELECT payload_hash,coalesce(o.wire,X'') FROM message m LEFT JOIN outbox o USING(conversation,sender_peer,sender_install,msg_id) WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],|r|Ok((bytes(r,0)?,r.get(1)?))).map_err(sql_error)?;
                let (prev_hash,attempted):( [u8;32],Vec<u8>)=c.query_row("SELECT prev_hash,attempted_receivers FROM message WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],|r|Ok((bytes(r,0)?,r.get(1)?))).map_err(sql_error)?;
                let (receipt_jobs,processed):(u64,u64)=c.query_row("SELECT (SELECT count(*) FROM receipt_job),(SELECT count(*) FROM processed_ct)",[],|r|Ok((uint(r,0)?,uint(r,1)?))).map_err(sql_error)?;
                let dispatch_state=c.query_row("SELECT dispatch_state FROM outbox WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],|r|uint(r,0)).optional().map_err(sql_error)?;
                Ok(MessageFacts {
                    prev_hash,
                    attempted_receivers: receivers(&attempted)?,
                    retry: c.query_row("SELECT attempts,retry_delay_ms,next_retry_at_ms FROM outbox WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],|r|Ok((uint(r,0)?,uint(r,1)?,uint(r,2)?))).optional().map_err(sql_error)?,
                    receipt_retry: c.query_row("SELECT attempts,next_retry_at_ms FROM receipt_job WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 ORDER BY kind DESC LIMIT 1",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],|r|Ok((uint(r,0)?,uint(r,1)?))).optional().map_err(sql_error)?,
                    receipt_jobs,
                    processed,
                    dispatch_state,
                    messages,
                    jobs,
                    quota_bytes,
                    wire,
                    hash,
                })
            })();
            let _ = tx.send(result);
        }
        FixtureOp::Fill(key, count, target, reply) => {
            let result = fill(c, key, count, target);
            let _ = reply.send(result);
        }
        FixtureOp::Lamport(id, value, reply) => {
            let result = integer(value).and_then(|v| {
                c.execute(
                    "UPDATE conversation SET lamport_max=?2 WHERE id=?1",
                    params![id.0.as_slice(), v],
                )
                .map(|_| ())
                .map_err(sql_error)
            });
            let _ = reply.send(result);
        }
        FixtureOp::Delivered(key, reply) => {
            let result = (|| {
                let tx = c.transaction().map_err(sql_error)?;
                tx.execute("UPDATE message SET delivered_at_ms=1 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()]).map_err(sql_error)?;
                tx.execute("DELETE FROM outbox WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()]).map_err(sql_error)?;
                let rev = bump(&tx)?;
                tx.execute(
                    "UPDATE conversation SET change_revision=?2 WHERE id=?1",
                    params![key.conversation.0.as_slice(), rev as i64],
                )
                .map_err(sql_error)?;
                tx.commit().map_err(sql_error)
            })();
            let _ = reply.send(result);
        }
    }
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
fn fill(c: &mut Connection, key: MessageKey, count: u64, target: Option<u64>) -> R<()> {
    let tx = c.transaction().map_err(sql_error)?;
    let (mut jobs, mut total): (u64, u64) = tx
        .query_row(
            "SELECT count(*),coalesce(sum(length(wire)),0) FROM outbox",
            [],
            |r| Ok((uint(r, 0)?, uint(r, 1)?)),
        )
        .map_err(sql_error)?;
    let (mut lamport, prev): (u64, Vec<u8>) = tx
        .query_row(
            "SELECT lamport_max,local_prev_hash FROM conversation WHERE id=?1",
            params![key.conversation.0.as_slice()],
            |r| Ok((uint(r, 0)?, r.get(1)?)),
        )
        .map_err(sql_error)?;
    let mut prev: [u8; 32] = prev.try_into().map_err(|_| invalid())?;
    while jobs < count || target.is_some_and(|t| total < t) {
        lamport += 1;
        let mut frame = TextFrame {
            key: MessageKey {
                id: MessageId((lamport as u128 + 1000).to_be_bytes()),
                ..key
            },
            lamport,
            prev_hash: prev,
            sent_at_ms: 1,
            body: "x".repeat(if target.is_some() { 16384 } else { 1 }),
        };
        let (mut wire, mut hash) = frame.encode();
        if let Some(target) = target {
            let remaining = target - total;
            if remaining < wire.len() as u64 {
                let overhead = wire.len() - frame.body.len();
                let expected = (remaining as usize).saturating_sub(overhead);
                let mut found = false;
                for size in expected.saturating_sub(3).max(1)..=(expected + 3).min(16384) {
                    frame.body = "x".repeat(size);
                    (wire, hash) = frame.encode();
                    if wire.len() as u64 == remaining {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Err(invalid());
                }
            }
        }
        insert_outgoing(&tx, &frame, &hash, &wire)?;
        prev = hash;
        jobs += 1;
        total += wire.len() as u64;
    }
    let rev = bump(&tx)?;
    tx.execute(
        "UPDATE conversation SET lamport_max=?2,local_prev_hash=?3,change_revision=?4 WHERE id=?1",
        params![
            key.conversation.0.as_slice(),
            integer(lamport)?,
            prev.as_slice(),
            rev as i64
        ],
    )
    .map_err(sql_error)?;
    tx.commit().map_err(sql_error)
}
pub(crate) enum MessageDbReply {
    Observed(Box<MessageDbReply>, Vec<crate::CoreEvent>),
    Due(
        Vec<super::scheduler::Dispatch>,
        Vec<super::scheduler::Dispatch>,
        [Option<DueCursor>; 2],
    ),
    Receipt(ReceiptFrame),
    Prepared(Zeroizing<Vec<u8>>, Option<ReceiptFrame>),
    Peer(PeerId),
    Done,
    Conversation(ConversationView),
    Message(MessageView),
    Messages(MessagePage),
    Conversations(ConversationPage),
}
impl MessageRepo {
    pub(crate) fn new(storage: StorageHandle, local: &LocalAccount) -> Self {
        Self {
            storage,
            identity: Identity {
                peer: local.keys.peer_id(),
                installation: local.device.installation_id(),
            },
        }
    }
    pub(crate) async fn due_hint(&self, now: u64) -> R<bool> {
        self.storage.due_hint(now).await
    }
    pub(crate) async fn retry_send(&self, key: MessageKey, now_ms: u64) -> R<MessageView> {
        match self.operation(MessageDbOp::Retry(key,now_ms)).await? {
            MessageDbReply::Message(view)=>Ok(view), _=>unreachable!()
        }
    }
    pub(crate) async fn cancel_send(&self, key: MessageKey) -> R<MessageView> {
        match self.operation(MessageDbOp::Cancel(key)).await? {
            MessageDbReply::Message(view)=>Ok(view), _=>unreachable!()
        }
    }
    pub(crate) fn local_peer(&self) -> PeerId {
        self.identity.peer
    }
    pub(crate) async fn open_dm(&self, local: PeerId, peer: PeerId) -> R<ConversationView> {
        match self
            .storage
            .message(self.identity, MessageDbOp::Open(local, peer))
            .await?
        {
            MessageDbReply::Conversation(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    pub(crate) async fn enqueue(
        &self,
        peer: PeerId,
        id: MessageId,
        body: String,
        now_ms: u64,
    ) -> R<MessageView> {
        match self
            .storage
            .message(self.identity, MessageDbOp::Enqueue(peer, id, body, now_ms))
            .await?
        {
            MessageDbReply::Message(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    pub(crate) async fn load_messages(
        &self,
        id: ConversationId,
        before: Option<MessageCursor>,
        limit: u32,
    ) -> R<MessagePage> {
        match self
            .storage
            .message(self.identity, MessageDbOp::Messages(id, before, limit))
            .await?
        {
            MessageDbReply::Messages(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    pub(crate) async fn list_conversations(
        &self,
        before: Option<ConversationCursor>,
        limit: u32,
    ) -> R<ConversationPage> {
        match self
            .storage
            .message(self.identity, MessageDbOp::Conversations(before, limit))
            .await?
        {
            MessageDbReply::Conversations(v) => Ok(v),
            _ => unreachable!(),
        }
    }
}

impl MessageDbOp {
    pub(crate) fn key(&self, identity: Identity) -> R<Option<MessageKey>> {
        Ok(match self {
            Self::Observed(op) => return op.key(identity),
            Self::MarkRead(_, keys, _) => keys.first().copied(),
            Self::Enqueue(peer, id, _, _) => Some(MessageKey {
                conversation: conversation_id(identity.peer, *peer)?,
                sender: identity.peer,
                installation: identity.installation,
                id: *id,
            }),
            Self::Receive(t, _, _, _) => Some(t.key),
            #[cfg(all(feature="test-hooks",debug_assertions))]
            Self::RejectedPrepare(k,_,_,_) => Some(*k),
            Self::Retry(k, _)
            | Self::Cancel(k)
            | Self::SettleCompleted(k)
            | Self::Defer(k, _, _)
            | Self::ResetUnlaunched(k, _)
            | Self::Prepare(k, _, _, _, _, _)
            | Self::AttemptOutcome(k, _, _, _) => Some(*k),
            Self::ApplyReceipt(r, _, _) | Self::Applied(r, _) => Some(r.key),
            _ => None,
        })
    }
}
impl MessageRepo {
    pub(crate) async fn operation(&self, op: MessageDbOp) -> R<MessageDbReply> {
        self.storage.message(self.identity, op).await
    }
}
fn conflict() -> CoreError {
    CoreError::new(ErrorCode::MessageConflict, "message_conflict")
}
fn missing() -> CoreError {
    CoreError::new(ErrorCode::MessageNotFound, "message_not_found")
}
fn receipt_for(
    c: &Connection,
    key: MessageKey,
    recipient: InstallationId,
    kind: ReceiptKind,
) -> R<ReceiptFrame> {
    let hash=c.query_row("SELECT payload_hash FROM message WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],|r|bytes(r,0)).optional().map_err(sql_error)?.ok_or_else(missing)?;
    Ok(ReceiptFrame {
        kind,
        key,
        text_hash: hash,
        recipient,
    })
}
fn commit(
    tx: Transaction<'_>,
    #[cfg(all(feature = "test-hooks", debug_assertions))] fault: Option<
        crate::storage::worker::CommitFault,
    >,
) -> R<()> {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    if fault == Some(crate::storage::worker::CommitFault::BeforeCommit) {
        return Err(CoreError::new(
            ErrorCode::StorageUncertain,
            "message_storage_uncertain",
        ));
    }
    tx.commit().map_err(sql_error)?;
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    if fault == Some(crate::storage::worker::CommitFault::AfterCommit) {
        return Err(CoreError::new(
            ErrorCode::StorageUncertain,
            "message_storage_uncertain",
        ));
    }
    Ok(())
}
fn receive(
    c: &mut Connection,
    identity: Identity,
    text: TextFrame,
    hash: [u8; 32],
    now: u64,
    schedule_now: u64,
    #[cfg(all(feature = "test-hooks", debug_assertions))] fault: Option<
        crate::storage::worker::CommitFault,
    >,
) -> R<MessageDbReply> {
    let k = text.key;
    if conversation_id(identity.peer, k.sender)? != k.conversation {
        return Err(invalid());
    }
    let now = integer(now)?;
    let tx = c.transaction().map_err(sql_error)?;
    if let Some(existing) = find(&tx, k)? {
        let receipt = receipt_for(&tx, k, identity.installation, ReceiptKind::Delivered)?;
        if existing.outgoing || receipt.text_hash != hash {
            return Err(conflict());
        }
        return Ok(MessageDbReply::Receipt(receipt));
    }
    let maximum: Option<u64> = tx
        .query_row(
            "SELECT lamport_max FROM conversation WHERE id=?1",
            params![k.conversation.0.as_slice()],
            |r| uint(r, 0),
        )
        .optional()
        .map_err(sql_error)?;
    if text.lamport
        > maximum
            .unwrap_or(0)
            .saturating_add(1_000_000)
            .min(i64::MAX as u64)
    {
        return Err(invalid());
    }
    let collision:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM message WHERE conversation=?1 AND lamport=?2 AND sender_install=?3 AND msg_id=?4)",params![k.conversation.0.as_slice(),integer(text.lamport)?,k.installation.0.as_slice(),k.id.0.as_slice()],|r|r.get(0)).map_err(sql_error)?;
    if collision {
        return Err(conflict());
    }
    ensure_conversation(&tx, k.conversation, k.sender)?;
    tx.execute(
        "INSERT INTO message VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,0,0,?9,NULL,0,0,NULL,NULL,X'')",
        params![
            k.conversation.0.as_slice(),
            k.sender.0.as_slice(),
            k.installation.0.as_slice(),
            k.id.0.as_slice(),
            integer(text.lamport)?,
            text.prev_hash.as_slice(),
            hash.as_slice(),
            integer(text.sent_at_ms)?,
            now,
            text.body
        ],
    )
    .map_err(sql_error)?;
    tx.execute(
        "INSERT INTO processed_ct VALUES(1,?1,?2,?3,?4)",
        params![
            k.sender.0.as_slice(),
            k.installation.0.as_slice(),
            hash.as_slice(),
            now
        ],
    )
    .map_err(sql_error)?;
    tx.execute(
        "INSERT INTO receipt_job VALUES(?1,?2,?3,?4,?5,1,?6,1)",
        params![
            k.conversation.0.as_slice(),
            k.sender.0.as_slice(),
            k.installation.0.as_slice(),
            k.id.0.as_slice(),
            identity.installation.0.as_slice(),
            integer(schedule_now.checked_add(1000).ok_or_else(invalid)?)?
        ],
    )
    .map_err(sql_error)?;
    let rev = bump(&tx)?;
    tx.execute("UPDATE conversation SET lamport_max=max(lamport_max,?2),unread=unread+1,change_revision=?3 WHERE id=?1",params![k.conversation.0.as_slice(),integer(text.lamport)?,integer(rev)?]).map_err(sql_error)?;
    let receipt = receipt_for(&tx, k, identity.installation, ReceiptKind::Delivered)?;
    commit(
        tx,
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        fault,
    )?;
    Ok(MessageDbReply::Receipt(receipt))
}
fn target(c: &Connection, identity: Identity, key: MessageKey, receipt: bool) -> R<PeerId> {
    let m = find(c, key)?.ok_or_else(missing)?;
    if receipt {
        if m.outgoing || key.sender == identity.peer {
            return Err(invalid());
        }
        Ok(key.sender)
    } else {
        if !m.outgoing || key.sender != identity.peer || key.installation != identity.installation {
            return Err(invalid());
        }
        Ok(conversation(c, key.conversation)?.peer)
    }
}
pub(crate) fn receivers(raw: &[u8]) -> R<Vec<InstallationId>> {
    if raw.len() > 160 || raw.len() % 32 != 0 {
        return Err(invalid());
    }
    let ids: Vec<_> = raw
        .chunks_exact(32)
        .map(|b| InstallationId(b.try_into().unwrap()))
        .collect();
    if ids.windows(2).any(|w| w[0] >= w[1]) {
        return Err(invalid());
    }
    Ok(ids)
}
fn prepare(
    c: &mut Connection,
    identity: Identity,
    k: MessageKey,
    receiver: InstallationId,
    receipt: bool,
    now: u64,
    transition: PrepareTransition,
    #[cfg(all(feature = "test-hooks", debug_assertions))] fault: Option<
        crate::storage::worker::CommitFault,
    >,
) -> R<MessageDbReply> {
    target(c, identity, k, receipt)?;
    let tx = c.transaction().map_err(sql_error)?;
    let prepared = if receipt {
        if receiver != k.installation {
            return Err(invalid());
        }
        let kind:u64=tx.query_row("SELECT kind FROM receipt_job WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND recipient_install=?5 ORDER BY kind DESC LIMIT 1",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice(),identity.installation.0.as_slice()],|r|uint(r,0)).optional().map_err(sql_error)?.ok_or_else(missing)?;
        let r = receipt_for(
            &tx,
            k,
            identity.installation,
            if kind == 2 {
                ReceiptKind::Read
            } else {
                ReceiptKind::Delivered
            },
        )?;
        schedule_retry(&tx, k, true, now)?;
        MessageDbReply::Prepared(Frame::Receipt(r.clone()).envelope(), Some(r))
    } else {
        let (raw,wire,state):(Vec<u8>,Vec<u8>,u64)=tx.query_row("SELECT attempted_receivers,wire,dispatch_state FROM message JOIN outbox USING(conversation,sender_peer,sender_install,msg_id) WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND cancel_requested=0",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,uint(r,2)?))).optional().map_err(sql_error)?.ok_or_else(missing)?;
        if state == 1 {
            return Err(CoreError::new(ErrorCode::Busy, "message_in_flight"));
        }
        let mut allowed = receivers(&raw)?;
        if !allowed.contains(&receiver) {
            if allowed.len() == 5 {
                return Err(CoreError::new(
                    ErrorCode::AccountRestricted,
                    "receiver_limit",
                ));
            }
            allowed.push(receiver);
            allowed.sort();
        }
        let packed: Vec<_> = allowed.iter().flat_map(|i| i.0).collect();
        integer(now)?;
        // All definite no-mutation validation has passed. From here a storage
        // error/fenced reply may hide this attempt's own transition.
        transition.eligible();
        tx.execute("UPDATE message SET attempted_receivers=?5 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice(),packed]).map_err(sql_error)?;
        schedule_retry(&tx, k, false, now)?;
        tx.execute("UPDATE outbox SET dispatch_state=1 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice()]).map_err(sql_error)?;
        MessageDbReply::Prepared(Zeroizing::new(wire), None)
    };
    commit(
        tx,
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        fault,
    )?;
    Ok(prepared)
}
fn apply_receipt(
    c: &mut Connection,
    identity: Identity,
    r: ReceiptFrame,
    peer: PeerId,
    now: u64,
    #[cfg(all(feature = "test-hooks", debug_assertions))] fault: Option<
        crate::storage::worker::CommitFault,
    >,
) -> R<MessageDbReply> {
    let k = r.key;
    let tx = c.transaction().map_err(sql_error)?;
    let m = find(&tx, k)?.ok_or_else(missing)?;
    let expected = receipt_for(&tx, k, r.recipient, r.kind)?;
    if !m.outgoing
        || k.sender != identity.peer
        || k.installation != identity.installation
        || conversation_id(identity.peer, peer)? != k.conversation
        || expected.text_hash != r.text_hash
    {
        return Err(invalid());
    }
    let raw:Vec<u8>=tx.query_row("SELECT attempted_receivers FROM message WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice()],|r|r.get(0)).map_err(sql_error)?;
    if !receivers(&raw)?.contains(&r.recipient) {
        return Err(invalid());
    }
    let changed=tx.execute("UPDATE message SET delivered_at_ms=coalesce(delivered_at_ms,?5),read_at_ms=CASE WHEN ?6=2 THEN coalesce(read_at_ms,?5) ELSE read_at_ms END WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND (delivered_at_ms IS NULL OR (?6=2 AND read_at_ms IS NULL))",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice(),integer(now)?,r.kind.number()]).map_err(sql_error)?;
    tx.execute("DELETE FROM outbox WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice()]).map_err(sql_error)?;
    if changed > 0 {
        let rev = bump(&tx)?;
        tx.execute(
            "UPDATE conversation SET change_revision=?2 WHERE id=?1",
            params![k.conversation.0.as_slice(), integer(rev)?],
        )
        .map_err(sql_error)?;
    }
    commit(
        tx,
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        fault,
    )?;
    Ok(MessageDbReply::Receipt(r))
}
fn applied(
    c: &mut Connection,
    identity: Identity,
    r: ReceiptFrame,
    peer: PeerId,
    #[cfg(all(feature = "test-hooks", debug_assertions))] fault: Option<
        crate::storage::worker::CommitFault,
    >,
) -> R<MessageDbReply> {
    let k = r.key;
    let tx = c.transaction().map_err(sql_error)?;
    let m = find(&tx, k)?.ok_or_else(missing)?;
    let expected = receipt_for(&tx, k, identity.installation, r.kind)?;
    if m.outgoing || k.sender != peer || expected != r {
        return Err(invalid());
    }
    tx.execute("DELETE FROM receipt_job WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND recipient_install=?5 AND kind<=?6",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice(),r.recipient.0.as_slice(),r.kind.number()]).map_err(sql_error)?;
    commit(
        tx,
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        fault,
    )?;
    Ok(MessageDbReply::Done)
}
fn attempt_outcome(
    c: &mut Connection,
    k: MessageKey,
    block: Option<BlockReason>,
    failure: Option<ErrorCode>,
    written: bool,
    #[cfg(all(feature = "test-hooks", debug_assertions))] fault: Option<
        crate::storage::worker::CommitFault,
    >,
) -> R<MessageDbReply> {
    let tx = c.transaction().map_err(sql_error)?;
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM outbox WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4)",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice()],|r|r.get(0)).map_err(sql_error)?;
    if exists {
        let reason = block.map(|b| match b {
            BlockReason::Account => 1,
            BlockReason::ContactRequired => 2,
            BlockReason::Storage => 3,
        });
        let code = failure.map(|c| match c {
            ErrorCode::UnsupportedMessageVersion => "UnsupportedMessageVersion",
            _ => "InvalidMessage",
        });
        tx.execute("UPDATE message SET outcome=CASE WHEN ?5 IS NOT NULL THEN 2 WHEN ?6 IS NOT NULL THEN 3 WHEN ?7 THEN 1 ELSE outcome END,block_reason=?5,failure_code=?6 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice(),reason,code,written]).map_err(sql_error)?;
        if failure.is_some() {
            tx.execute("DELETE FROM outbox WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice()]).map_err(sql_error)?;
        } else {
            tx.execute("UPDATE outbox SET dispatch_state=?5 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice(),if block.is_some() {2} else {0}]).map_err(sql_error)?;
        }
        let rev = bump(&tx)?;
        tx.execute(
            "UPDATE conversation SET change_revision=?2 WHERE id=?1",
            params![k.conversation.0.as_slice(), integer(rev)?],
        )
        .map_err(sql_error)?;
    }
    commit(
        tx,
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        fault,
    )?;
    Ok(MessageDbReply::Done)
}

/// The worker checks the actual SQL account membership before recording a receiver.
/// A caller's installation ID is never itself a membership grant.
pub(crate) fn authorize(c: &Connection, local: &LocalAccount, op: &MessageDbOp) -> R<()> {
    if let MessageDbOp::Observed(op) = op { return authorize(c, local, op); }
    let retry = match op {
        MessageDbOp::Retry(key, now) => Some((target(c,Identity { peer:local.keys.peer_id(),installation:local.device.installation_id() },*key,false)?,*now)),
        MessageDbOp::ReleasePeer(peer, now) => Some((*peer,*now)),
        _=>None,
    };
    if let Some((peer,now))=retry {
        let now=now/1000;
        let own=local.records.accepted.as_ref().filter(|r|
            local.records.conflict.is_none() && local.records.state!=crate::account::PublicationState::Conflict
            && r.seq>=local.records.observed_seq && r.record.generation==local.records.generation
            && r.current(now) && !r.record.is_expired(now)
            && r.record.devices.iter().any(|d|d.installation_id==local.device.installation_id() && d.transport_public_key==local.device.transport_public_key())
            && !local.records.tombstones.iter().any(|t|t.target_installation==local.device.installation_id() || t.target_transport==local.device.transport_public_key()));
        let remote=local.remote.iter().find(|r|r.peer==peer && r.conflict.is_none() && r.accepted.seq>=r.floor
            && r.accepted.current(now) && !r.accepted.record.is_expired(now));
        if own.is_none() || remote.is_none() {return Err(CoreError::new(ErrorCode::AccountRestricted,"retry_not_authorized"));}
        if !local.contacts.iter().any(|c|c.peer==peer) {return Err(CoreError::new(ErrorCode::ContactRequired,"retry_contact_required"));}
    }
    let prepare = match op {
        MessageDbOp::Prepare(key,receiver,receipt,now,_,_) => Some((key,receiver,*receipt,now)),
        #[cfg(all(feature="test-hooks",debug_assertions))]
        MessageDbOp::RejectedPrepare(key,receiver,now,_) => Some((key,receiver,false,now)),
        _=>None,
    };
    if let Some((key,receiver,receipt,now)) = prepare {
        let identity = Identity {
            peer: local.keys.peer_id(),
            installation: local.device.installation_id(),
        };
        let peer = target(c, identity, *key, receipt)?;
        let accepted = local
            .remote
            .iter()
            .find(|r| r.peer == peer)
            .filter(|r| {
                r.conflict.is_none() && r.accepted.seq >= r.floor && r.accepted.current(now / 1000)
            })
            .ok_or_else(|| {
                CoreError::new(ErrorCode::AccountRestricted, "receiver_not_authorized")
            })?;
        if !local.contacts.iter().any(|c| c.peer == peer)
            || !accepted
                .accepted
                .record
                .devices
                .iter()
                .any(|d| d.installation_id == *receiver)
        {
            return Err(CoreError::new(
                ErrorCode::AccountRestricted,
                "receiver_not_authorized",
            ));
        }
    }
    Ok(())
}
#[cfg(test)]
mod receiver_tests {
    use super::*;
    #[test]
    fn prepare_provenance_retains_exact_pending_token_until_operation_drop() {
        let identity = Identity {
            peer: PeerId([1; 32]),
            installation: InstallationId([2; 32]),
        };
        let key = MessageKey {
            conversation: ConversationId([3; 32]),
            sender: identity.peer,
            installation: identity.installation,
            id: MessageId([4; 16]),
        };
        let (token, transition) = PrepareToken::new();
        let op = MessageDbOp::Prepare(key, InstallationId([5; 32]), false, 1, 1, transition);
        let pending = op.pending(identity).unwrap().unwrap();
        let retained = pending.clone();
        assert!(token.eligible().is_err());
        assert!(
            retained
                .prebyte_attempt
                .as_ref()
                .unwrap()
                .eligible()
                .is_err()
        );
        drop(op); // includes queue rejection, cancelled request and worker validation failure
        assert!(!token.eligible().unwrap());
        assert!(!retained.prebyte_attempt.unwrap().eligible().unwrap());
        assert!(retained.key == key);
        let (eligible, transition) = PrepareToken::new();
        transition.eligible();
        drop(transition);
        assert!(eligible.eligible().unwrap());
        assert!(!token.eligible().unwrap()); // one operation cannot confer another's ownership
    }
    #[test]
    fn packed_receivers_require_sorted_distinct_bounded_fixed_width_ids() {
        assert!(receivers(&[]).unwrap().is_empty());
        assert_eq!(receivers(&[1; 32]).unwrap(), vec![InstallationId([1; 32])]);
        let valid: Vec<u8> = (1..=5).flat_map(|n| [n; 32]).collect();
        assert_eq!(receivers(&valid).unwrap().len(), 5);
        for invalid in [
            vec![1; 31],
            vec![1; 64],
            vec![1; 161],
            (1..=6).flat_map(|n| [n; 32]).collect(),
            [vec![2; 32], vec![1; 32]].concat(),
        ] {
            assert!(receivers(&invalid).is_err());
        }
    }
}

#[derive(Clone)]
pub(crate) struct PendingMessage {
    pub key: MessageKey,
    pub prebyte_attempt: Option<PrepareToken>,
}
impl MessageDbOp {
    pub(crate) fn pending(&self, identity: Identity) -> R<Option<PendingMessage>> {
        if let Self::Observed(op) = self { return op.pending(identity); }
        Ok(self.key(identity)?.map(|key| PendingMessage {
            key,
            prebyte_attempt: match self {
                Self::Prepare(_, _, false, _, _, transition) => Some(transition.0.clone()),
                Self::ResetUnlaunched(_, token) => Some(token.clone()),
                _ => None,
            },
        }))
    }
}

fn schedule_retry(tx: &Transaction<'_>, k: MessageKey, receipt: bool, now: u64) -> R<()> {
    let table = if receipt { "receipt_job" } else { "outbox" };
    let filter = "conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4";
    let count = tx.query_row(&format!("SELECT max(attempts) FROM {table} WHERE {filter}"),
        params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice()],
        |r| r.get::<_,Option<i64>>(0)).map_err(sql_error)?;
    let Some(count) = count else { return Ok(()) };
    let count = (count as u64).saturating_add(1).min(u32::MAX as u64);
    let delay = super::scheduler::retry_delay(count);
    let next = integer(now.checked_add(delay).ok_or_else(invalid)?)?;
    let extra = if receipt { String::new() } else { format!(",retry_delay_ms={delay}") };
    tx.execute(&format!("UPDATE {table} SET attempts=?5,next_retry_at_ms=?6{extra} WHERE {filter}"),
        params![k.conversation.0.as_slice(),k.sender.0.as_slice(),k.installation.0.as_slice(),k.id.0.as_slice(),integer(count)?,next]).map_err(sql_error)?;
    Ok(())
}
#[derive(Clone, Copy)]
pub(crate) struct DueCursor {
    at: i64,
    conversation: ConversationId,
    rowid: i64,
}
#[derive(Clone, Copy, Debug)]
enum DueSeek {
    Start,
    Row,
    Conversation,
    Time,
}
fn due_sql(receipt: bool, seek: DueSeek) -> String {
    let (table, index, state) = if receipt {
        ("receipt_job", "receipt_due", "")
    } else {
        ("outbox", "outbox_due", "AND j.dispatch_state=0")
    };
    let after = match seek {
        DueSeek::Start => "",
        DueSeek::Row => "AND j.next_retry_at_ms=?2 AND j.conversation=?3 AND j.rowid>?4",
        DueSeek::Conversation => "AND j.next_retry_at_ms=?2 AND j.conversation>?3",
        DueSeek::Time => "AND j.next_retry_at_ms>?2",
    };
    format!(
        "SELECT j.conversation,j.sender_peer,j.sender_install,j.msg_id,c.peer,j.next_retry_at_ms,j.rowid FROM {table} j INDEXED BY {index} JOIN conversation c ON c.id=j.conversation WHERE j.next_retry_at_ms<=?1 {state} {after} ORDER BY j.next_retry_at_ms,j.conversation,j.rowid LIMIT ?5"
    )
}
struct DueKeys {
    rows: Vec<(MessageKey, PeerId, DueCursor)>,
    #[cfg(test)]
    steps: i32,
}
fn scan_due(c: &Connection, receipt: bool, now: u64, cursor: Option<DueCursor>) -> R<DueKeys> {
    let seeks: &[DueSeek] = if cursor.is_some() {
        &[DueSeek::Row, DueSeek::Conversation, DueSeek::Time]
    } else {
        &[DueSeek::Start]
    };
    let cursor = cursor.unwrap_or(DueCursor {
        at: 0,
        conversation: ConversationId([0; 32]),
        rowid: 0,
    });
    let mut selected = DueKeys {
        rows: Vec::new(),
        #[cfg(test)]
        steps: 0,
    };
    for seek in seeks {
        // Separate equality prefixes let SQLite seek the implicit index rowid.
        // A row-value comparison only seeks its explicit (time,conversation)
        // prefix and would rescan every prior row tied at that prefix.
        let mut stmt = c.prepare(&due_sql(receipt, *seek)).map_err(sql_error)?;
        let rows = stmt
            .query_map(
                params![
                    integer(now)?,
                    cursor.at,
                    cursor.conversation.0.as_slice(),
                    cursor.rowid,
                    16 - selected.rows.len() as i64
                ],
                |r| {
                    Ok((
                        MessageKey {
                            conversation: ConversationId(bytes(r, 0)?),
                            sender: PeerId(bytes(r, 1)?),
                            installation: InstallationId(bytes(r, 2)?),
                            id: MessageId(bytes(r, 3)?),
                        },
                        PeerId(bytes(r, 4)?),
                        DueCursor {
                            at: r.get(5)?,
                            conversation: ConversationId(bytes(r, 0)?),
                            rowid: r.get(6)?,
                        },
                    ))
                },
            )
            .map_err(sql_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_error)?;
        selected.rows.extend(rows);
        #[cfg(test)]
        {
            selected.steps += stmt.get_status(rusqlite::StatementStatus::VmStep);
        }
        if selected.rows.len() == 16 {
            break;
        }
    }
    Ok(selected)
}
// Advisory scheduling presence only: never returns keys, bodies or authority.
pub(crate) fn due_hint(c: &Connection, now: u64) -> R<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM outbox INDEXED BY outbox_due WHERE dispatch_state=0 AND next_retry_at_ms<=?1 LIMIT 1) OR EXISTS(SELECT 1 FROM receipt_job INDEXED BY receipt_due WHERE next_retry_at_ms<=?1 LIMIT 1)",
        params![integer(now)?], |r| r.get(0),
    ).map_err(sql_error)
}
fn due(
    c: &Connection,
    identity: Identity,
    now: u64,
    excluded: Vec<PeerId>,
    mut cursors: [Option<DueCursor>; 2],
) -> R<MessageDbReply> {
    use super::scheduler::Dispatch;
    let mut batches = Vec::new();
    for (kind, receipt) in [false, true].into_iter().enumerate() {
        // Bound indexed key work before reading payloads. Advance over exclusions
        // and duplicate peers too; a full backlog cannot pin selection at its head.
        let rows = scan_due(c, receipt, now, cursors[kind])?.rows;
        cursors[kind] = if rows.len() == 16 {
            rows.last().map(|r| r.2)
        } else {
            None
        };
        let mut jobs: Vec<Dispatch> = Vec::new();
        for (key, peer, _) in rows {
            if excluded.contains(&peer) || jobs.iter().any(|j| j.peer == peer) {
                continue;
            }
            let wire = if receipt {
                let kind:u64=c.query_row("SELECT max(kind) FROM receipt_job WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],|r|uint(r,0)).map_err(sql_error)?;
                Frame::Receipt(receipt_for(c,key,identity.installation,if kind==2 {ReceiptKind::Read} else {ReceiptKind::Delivered})?).envelope()
            } else {
                Zeroizing::new(c.query_row("SELECT wire FROM outbox WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],|r|r.get(0)).map_err(sql_error)?)
            };
            jobs.push(Dispatch {key,peer,wire,ticket:0});
        }
        batches.push(jobs);
    }
    let receipts = batches.pop().unwrap();
    Ok(MessageDbReply::Due(
        batches.pop().unwrap(),
        receipts,
        cursors,
    ))
}

fn release_blocked(c:&mut Connection,peer:PeerId,key:Option<MessageKey>,
    #[cfg(all(feature="test-hooks",debug_assertions))] fault:Option<crate::storage::worker::CommitFault>) -> R<()> {
    let tx=c.transaction().map_err(sql_error)?;
    let condition=if key.is_some() {" AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4"} else {""};
    let mut values:Vec<rusqlite::types::Value>=vec![peer.0.to_vec().into()];
    if let Some(k)=key {values.extend([k.sender.0.to_vec().into(),k.installation.0.to_vec().into(),k.id.0.to_vec().into()]);}
    let filter=format!("conversation=(SELECT id FROM conversation WHERE peer=?1){condition} AND cancel_requested=0 AND delivered_at_ms IS NULL AND outcome=2");
    // Only rows with retained retry work can be released; never recreate terminal payloads.
    let changed=tx.execute(&format!("UPDATE message SET outcome=0,block_reason=NULL WHERE {filter} AND EXISTS(SELECT 1 FROM outbox o WHERE o.conversation=message.conversation AND o.sender_peer=message.sender_peer AND o.sender_install=message.sender_install AND o.msg_id=message.msg_id AND o.dispatch_state=2)"),rusqlite::params_from_iter(values.iter())).map_err(sql_error)?;
    tx.execute(&format!("UPDATE outbox SET dispatch_state=0,next_retry_at_ms=0 WHERE conversation=(SELECT id FROM conversation WHERE peer=?1){condition} AND dispatch_state=2 AND EXISTS(SELECT 1 FROM message m WHERE m.conversation=outbox.conversation AND m.sender_peer=outbox.sender_peer AND m.sender_install=outbox.sender_install AND m.msg_id=outbox.msg_id AND m.outcome=0 AND m.cancel_requested=0 AND m.delivered_at_ms IS NULL)"),rusqlite::params_from_iter(values.iter())).map_err(sql_error)?;
    if changed>0 {
        let rev=bump(&tx)?;
        tx.execute("UPDATE conversation SET change_revision=?2 WHERE peer=?1",params![peer.0.as_slice(),integer(rev)?]).map_err(sql_error)?;
    }
    commit(tx,#[cfg(all(feature="test-hooks",debug_assertions))] fault)
}

#[cfg(test)]
mod due_tests {
    use super::*;
    fn backlog(count: u64) -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../storage/schema.sql"))
            .unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        let tx = c.transaction().unwrap();
        for peer in [1u8, 2] {
            tx.execute(
                "INSERT INTO conversation VALUES(?1,?1,0,?1,0,0)",
                params![[peer; 32].as_slice()],
            )
            .unwrap();
        }
        for n in 0..=count {
            let peer = if n < count { 1u8 } else { 2 };
            let id = (n as u128).to_be_bytes();
            tx.execute("INSERT INTO message VALUES(?1,?1,?1,?2,?3,?1,?1,0,0,'body',0,0,NULL,NULL,0,0,NULL,NULL,X'')",params![[peer;32].as_slice(),id.as_slice(),n as i64]).unwrap();
            tx.execute(
                "INSERT INTO receipt_job VALUES(?1,?1,?1,?2,?1,1,0,0)",
                params![[peer; 32].as_slice(), id.as_slice()],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO outbox VALUES(?1,?1,?1,?2,1,X'01',0,0,0,0)",
                params![[peer; 32].as_slice(), id.as_slice()],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        c
    }
    #[test]
    fn indexed_due_selection_work_is_bounded_before_payload_reads() {
        let c = backlog(8192);
        for receipt in [true, false] {
            for cursor in [
                None,
                Some(DueCursor {
                    at: 0,
                    conversation: ConversationId([1; 32]),
                    rowid: 8000,
                }),
                Some(DueCursor {
                    at: 0,
                    conversation: ConversationId([1; 32]),
                    rowid: 8192,
                }),
                Some(DueCursor {
                    at: 0,
                    conversation: ConversationId([2; 32]),
                    rowid: 8193,
                }),
            ] {
                let selected = scan_due(&c, receipt, 0, cursor).unwrap();
                eprintln!(
                    "native sqlite {} receipt={receipt} cursor_row={:?} rows={} VMsteps={}",
                    rusqlite::version(),
                    cursor.map(|c| c.rowid),
                    selected.rows.len(),
                    selected.steps
                );
                assert!(
                    selected.steps < 2000,
                    "selection must bound native VM work before payload reads: {}",
                    selected.steps
                );
                assert!(selected.rows.len() <= 16);
                let (_, steps) = measured_due(&c, 0, vec![], [cursor, cursor]);
                eprintln!("whole due including payload native progress steps={steps}");
                assert!(steps < 3000);

                for seek in [
                    DueSeek::Start,
                    DueSeek::Row,
                    DueSeek::Conversation,
                    DueSeek::Time,
                ] {
                    let sql = due_sql(receipt, seek);
                    let mut plan = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
                    let plan: Vec<String> = plan
                        .query_map(params![0, 0, [1u8; 32].as_slice(), 8000, 16], |r| r.get(3))
                        .unwrap()
                        .collect::<Result<_, _>>()
                        .unwrap();
                    eprintln!("receipt={receipt} seek={seek:?} plan={plan:?}");
                    assert!(
                        plan.iter()
                            .all(|p| !p.contains("TEMP B-TREE") && !p.starts_with("SCAN"))
                    );
                    assert!(plan.iter().any(|p| p.contains(if receipt {
                        "receipt_due"
                    } else {
                        "outbox_due"
                    })));
                }
            }
        }
    }
    // Read-only native VM observation of the actual due operation including
    // payload lookups. The callback always continues; no feature/schema change.
    fn measured_due(
        c: &Connection,
        now: u64,
        excluded: Vec<PeerId>,
        cursors: [Option<DueCursor>; 2],
    ) -> (MessageDbReply, u64) {
        unsafe extern "C" fn count(p: *mut std::ffi::c_void) -> std::ffi::c_int {
            unsafe {
                *(p as *mut u64) += 1;
            }
            0
        }
        let mut steps = 0u64;
        // The synchronous connection cannot outlive this stack counter; remove
        // the observer before inspecting/unwrap of the operation result.
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                c.handle(),
                1,
                Some(count),
                (&mut steps as *mut u64).cast(),
            );
        }
        let result = due(
            c,
            Identity {
                peer: PeerId([3; 32]),
                installation: InstallationId([3; 32]),
            },
            now,
            excluded,
            cursors,
        );
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(c.handle(), 0, None, std::ptr::null_mut());
        }
        (result.unwrap(), steps)
    }
    #[test]
    fn due_hint_native_work_is_bounded_for_empty_future_blocked_and_due_rows() {
        unsafe extern "C" fn count(p: *mut std::ffi::c_void) -> std::ffi::c_int {
            unsafe {
                *(p as *mut u64) += 1;
            }
            0
        }
        let c = backlog(8192);
        let measure = |expected| {
            let mut steps = 0u64;
            unsafe {
                rusqlite::ffi::sqlite3_progress_handler(
                    c.handle(),
                    1,
                    Some(count),
                    (&mut steps as *mut u64).cast(),
                );
            }
            let result = due_hint(&c, 0);
            unsafe {
                rusqlite::ffi::sqlite3_progress_handler(c.handle(), 0, None, std::ptr::null_mut());
            }
            assert_eq!(result.unwrap(), expected);
            eprintln!("due_hint expected={expected} native_VM_steps={steps}");
            assert!(steps < 200, "presence hint must seek, never scan backlog");
        };
        measure(true);
        c.execute("UPDATE outbox SET dispatch_state=2", []).unwrap();
        c.execute("UPDATE receipt_job SET next_retry_at_ms=1", [])
            .unwrap();
        measure(false);
        c.execute("UPDATE outbox SET dispatch_state=0,next_retry_at_ms=1", [])
            .unwrap();
        measure(false);
        c.execute(
            "UPDATE outbox SET next_retry_at_ms=0 WHERE rowid=(SELECT max(rowid) FROM outbox)",
            [],
        )
        .unwrap();
        measure(true);
        c.execute("UPDATE outbox SET dispatch_state=1", []).unwrap();
        measure(false);
        c.execute("UPDATE receipt_job SET next_retry_at_ms=0 WHERE rowid=(SELECT max(rowid) FROM receipt_job)",[]).unwrap();
        measure(true);
        c.execute("DELETE FROM receipt_job", []).unwrap();
        c.execute("DELETE FROM outbox", []).unwrap();
        measure(false);
        c.execute_batch("DROP INDEX receipt_due").unwrap();
        assert!(due_hint(&c, 0).is_err(), "probe errors must propagate");
    }

    #[test]
    fn due_later_time_and_full_payload_batch_remain_bounded() {
        let c = backlog(8192);
        for table in ["outbox", "receipt_job"] {
            c.execute(
                &format!("UPDATE {table} SET next_retry_at_ms=5 WHERE conversation=?1"),
                params![[2u8; 32].as_slice()],
            )
            .unwrap();
        }
        let cursor = Some(DueCursor {
            at: 0,
            conversation: ConversationId([1; 32]),
            rowid: 8192,
        });
        for receipt in [false, true] {
            assert!(scan_due(&c, receipt, 4, cursor).unwrap().rows.is_empty());
            let selected = scan_due(&c, receipt, 5, cursor).unwrap();
            assert_eq!(selected.rows.len(), 1);
            assert_eq!(selected.rows[0].1, PeerId([2; 32]));
            assert!(selected.steps < 2000);
            eprintln!(
                "later-time receipt={receipt} native VMsteps={}",
                selected.steps
            );
        }
        for peer in 3u8..=18 {
            c.execute(
                "INSERT INTO conversation VALUES(?1,?1,0,?1,0,0)",
                params![[peer; 32].as_slice()],
            )
            .unwrap();
            c.execute("INSERT INTO message VALUES(?1,?1,?1,?2,0,?1,?1,0,0,'body',0,0,NULL,NULL,0,0,NULL,NULL,X'')",params![[peer;32].as_slice(),[peer;16].as_slice()]).unwrap();
            c.execute(
                "INSERT INTO receipt_job VALUES(?1,?1,?1,?2,?1,1,0,0)",
                params![[peer; 32].as_slice(), [peer; 16].as_slice()],
            )
            .unwrap();
            c.execute(
                "INSERT INTO outbox VALUES(?1,?1,?1,?2,1,X'01',0,0,0,0)",
                params![[peer; 32].as_slice(), [peer; 16].as_slice()],
            )
            .unwrap();
        }
        let (MessageDbReply::Due(text, receipts, _), steps) =
            measured_due(&c, 5, vec![], [cursor, cursor])
        else {
            unreachable!()
        };
        assert_eq!(text.len(), 16);
        assert_eq!(receipts.len(), 16);
        eprintln!("32 payload candidates native progress steps={steps}");
        assert!(steps < 5000);
    }
    #[test]
    fn excluded_backlog_continues_to_other_peer_and_wraps() {
        let c = backlog(8192);
        let mut cursors = [None, None];
        for page in 0..=512 {
            let (MessageDbReply::Due(text, receipts, next), steps) =
                measured_due(&c, 0, vec![PeerId([1; 32])], cursors)
            else {
                unreachable!()
            };
            assert!(steps < 3000);
            assert!(text.len() + receipts.len() <= 32);
            if page < 512 {
                assert!(text.is_empty() && receipts.is_empty());
            } else {
                assert_eq!(text.len(), 1);
                assert_eq!(receipts.len(), 1);
                assert_eq!(receipts[0].peer, PeerId([2; 32]));
            }
            cursors = next;
        }
        assert!(cursors.iter().all(Option::is_none));
        let (MessageDbReply::Due(text, receipts, _), steps) = measured_due(&c, 0, vec![], cursors)
        else {
            unreachable!()
        };
        assert!(steps < 3000);
        assert_eq!(text.len(), 1);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].peer, PeerId([1; 32]));
    }
}

fn mark_read(
    c: &mut Connection,
    identity: Identity,
    id: ConversationId,
    keys: Vec<MessageKey>,
    now: u64,
    #[cfg(all(feature = "test-hooks", debug_assertions))] fault: Option<
        crate::storage::worker::CommitFault,
    >,
) -> R<MessageDbReply> {
    if keys.len() > 50 {
        return Err(invalid());
    }
    let now = integer(now)?;
    let tx = c.transaction().map_err(sql_error)?;
    let current = conversation(&tx, id)?;
    // Validate the whole supplied visible set before changing any fact or job.
    for key in &keys {
        if key.conversation != id {
            return Err(invalid());
        }
        let view = find(&tx, *key)?.ok_or_else(missing)?;
        if view.outgoing || key.sender == identity.peer || key.sender != current.peer {
            return Err(invalid());
        }
    }
    let mut changed = 0usize;
    for key in keys {
        let k = key;
        let count = tx.execute("UPDATE message SET seen_local=1 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND seen_local=0", params![k.conversation.0.as_slice(), k.sender.0.as_slice(), k.installation.0.as_slice(), k.id.0.as_slice()]).map_err(sql_error)?;
        if count == 0 {
            continue;
        }
        changed += count;
        // Read subsumes Delivered. Repeating mark_read never resets an existing obligation's backoff.
        tx.execute("DELETE FROM receipt_job WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND recipient_install=?5 AND kind=1", params![k.conversation.0.as_slice(), k.sender.0.as_slice(), k.installation.0.as_slice(), k.id.0.as_slice(), identity.installation.0.as_slice()]).map_err(sql_error)?;
        tx.execute(
            "INSERT OR IGNORE INTO receipt_job VALUES(?1,?2,?3,?4,?5,2,?6,0)",
            params![
                k.conversation.0.as_slice(),
                k.sender.0.as_slice(),
                k.installation.0.as_slice(),
                k.id.0.as_slice(),
                identity.installation.0.as_slice(),
                now
            ],
        )
        .map_err(sql_error)?;
    }
    if changed > 0 {
        let rev = bump(&tx)?;
        tx.execute(
            "UPDATE conversation SET unread=unread-?2,change_revision=?3 WHERE id=?1",
            params![id.0.as_slice(), changed as i64, integer(rev)?],
        )
        .map_err(sql_error)?;
    }
    commit(
        tx,
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        fault,
    )?;
    Ok(MessageDbReply::Done)
}
