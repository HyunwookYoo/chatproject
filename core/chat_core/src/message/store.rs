use super::types::*;
use super::wire::TextFrame;
use crate::{
    CoreError, ErrorCode,
    account::{InstallationId, PeerId, storage::schema::LocalAccount},
    storage::worker::StorageHandle,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use sha2::{Digest, Sha256};
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
    Open(PeerId, PeerId),
    Enqueue(PeerId, MessageId, String, u64),
    Messages(ConversationId, Option<MessageCursor>, u32),
    Conversations(Option<ConversationCursor>, u32),
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
    match op {
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
        "INSERT INTO outbox VALUES(?1,?2,?3,?4,1,?5,0,1000,?6,0)",
        params![
            k.conversation.0.as_slice(),
            k.sender.0.as_slice(),
            k.installation.0.as_slice(),
            k.id.0.as_slice(),
            wire,
            integer(frame.sent_at_ms)?
        ],
    )
    .map_err(sql_error)?;
    Ok(())
}
/// Query the actual outcome after reopening before any further message work.
pub(crate) fn reconcile(c: &Connection, key: MessageKey) -> R<()> {
    if let Some(m) = find(c, key)? {
        let (prev,hash,wire):(Vec<u8>,Vec<u8>,Option<Vec<u8>>)=c.query_row("SELECT m.prev_hash,m.payload_hash,o.wire FROM message m LEFT JOIN outbox o USING(conversation,sender_peer,sender_install,msg_id) WHERE m.conversation=?1 AND m.sender_peer=?2 AND m.sender_install=?3 AND m.msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(sql_error)?;
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
            || (wire.is_none()
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
    Ok(())
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub struct MessageFacts {
    pub messages: u64,
    pub jobs: u64,
    pub quota_bytes: u64,
    pub wire: Vec<u8>,
    pub hash: [u8; 32],
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(crate) enum FixtureOp {
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
        FixtureOp::Facts(key, tx) => {
            let result = (|| {
                let (messages,jobs,quota_bytes):(u64,u64,u64)=c.query_row("SELECT (SELECT count(*) FROM message),count(*),coalesce(sum(length(wire)),0) FROM outbox",[],|r|Ok((uint(r,0)?,uint(r,1)?,uint(r,2)?))).map_err(sql_error)?;
                let (hash,wire):([u8;32],Vec<u8>)=c.query_row("SELECT payload_hash,coalesce(o.wire,X'') FROM message m LEFT JOIN outbox o USING(conversation,sender_peer,sender_install,msg_id) WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",params![key.conversation.0.as_slice(),key.sender.0.as_slice(),key.installation.0.as_slice(),key.id.0.as_slice()],|r|Ok((bytes(r,0)?,r.get(1)?))).map_err(sql_error)?;
                Ok(MessageFacts {
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
