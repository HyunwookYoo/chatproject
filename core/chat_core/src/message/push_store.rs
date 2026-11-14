//! Push rows of the message store (M7, schema v4). Every function runs inside the caller's
//! transaction.
use rusqlite::{Transaction, params};

use super::mls_group::sql;
use super::types::ConversationId;
use crate::CoreError;
use crate::account::InstallationId;

type R<T> = Result<T, CoreError>;

/// Longest handle `push_peer` keeps (its CHECK).
const MAX_HANDLE: usize = 1_024;

/// A `PushHandle` the other account's device `installation` sealed in `conversation` at
/// `sent_at_ms` (its `ChatMessage.sent_at_ms`, spec 5.2): its row becomes this handle, or a
/// withdrawal tombstone (`handle` NULL) when the handle is empty. Only a frame sent later than
/// the stored one replaces it (Ruling R10): an older or equally old one arriving late, through
/// the device log or a hand-over, never brings back a handle that was replaced or withdrawn.
/// A handle longer than `push_peer` takes, or a send time SQLite cannot hold, changes nothing
/// (the frame is still taken, so its sender stops resending it). Returns whether the row
/// changed.
pub(super) fn store_peer_handle(
    tx: &Transaction<'_>,
    conversation: ConversationId,
    installation: InstallationId,
    handle: &[u8],
    sent_at_ms: u64,
    now: i64,
) -> R<bool> {
    let Ok(sent_at_ms) = i64::try_from(sent_at_ms) else {
        return Ok(false);
    };
    if handle.len() > MAX_HANDLE {
        return Ok(false);
    }
    let handle = (!handle.is_empty()).then_some(handle);
    let changed = tx
        .execute(
            "INSERT INTO push_peer VALUES(?1,?2,?3,?4,?5) ON CONFLICT(conversation,installation) DO UPDATE SET handle=excluded.handle,sent_at_ms=excluded.sent_at_ms,received_at_ms=excluded.received_at_ms WHERE excluded.sent_at_ms>push_peer.sent_at_ms",
            params![
                conversation.0.as_slice(),
                installation.0.as_slice(),
                handle,
                sent_at_ms,
                now
            ],
        )
        .map_err(sql)?;
    Ok(changed > 0)
}

/// One `push_peer` row, for fixture assertions.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushPeerRow {
    pub conversation: ConversationId,
    pub installation: InstallationId,
    /// `None`: withdrawn.
    pub handle: Option<Vec<u8>>,
    pub sent_at_ms: u64,
    pub received_at_ms: u64,
}

/// What a fixture sees of the received handles.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushPeerState {
    /// By conversation and installation.
    pub peers: Vec<PushPeerRow>,
    /// `mailbox_notify` rows: texts marked for a notification.
    pub mailbox_notify: u64,
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_peer_state(c: &rusqlite::Connection) -> R<PushPeerState> {
    use super::store::{bytes, uint};
    let peers = c
        .prepare("SELECT conversation,installation,handle,sent_at_ms,received_at_ms FROM push_peer ORDER BY conversation,installation")
        .map_err(sql)?
        .query_map([], |r| {
            Ok(PushPeerRow {
                conversation: ConversationId(bytes::<32>(r, 0)?),
                installation: InstallationId(bytes::<32>(r, 1)?),
                handle: r.get(2)?,
                sent_at_ms: uint(r, 3)?,
                received_at_ms: uint(r, 4)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    let mailbox_notify = c
        .query_row("SELECT count(*) FROM mailbox_notify", [], |r| uint(r, 0))
        .map_err(sql)?;
    Ok(PushPeerState {
        peers,
        mailbox_notify,
    })
}
