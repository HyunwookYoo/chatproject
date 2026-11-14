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

/// A `PushHandle` the other account's device `installation` sealed in `conversation` (spec
/// 5.2): its row becomes this handle, or goes when the handle is empty (withdrawn). A handle
/// longer than `push_peer` takes changes nothing (the frame is still taken, so its sender
/// stops resending it). Returns whether the row changed.
pub(super) fn store_peer_handle(
    tx: &Transaction<'_>,
    conversation: ConversationId,
    installation: InstallationId,
    handle: &[u8],
    now: i64,
) -> R<bool> {
    let key = (conversation.0.as_slice(), installation.0.as_slice());
    let changed = if handle.is_empty() {
        tx.execute(
            "DELETE FROM push_peer WHERE conversation=?1 AND installation=?2",
            params![key.0, key.1],
        )
    } else if handle.len() > MAX_HANDLE {
        return Ok(false);
    } else {
        tx.execute(
            "INSERT INTO push_peer VALUES(?1,?2,?3,?4) ON CONFLICT(conversation,installation) DO UPDATE SET handle=excluded.handle,received_at_ms=excluded.received_at_ms",
            params![key.0, key.1, handle, now],
        )
    }
    .map_err(sql)?;
    Ok(changed > 0)
}

/// One `push_peer` row, for fixture assertions.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushPeerRow {
    pub conversation: ConversationId,
    pub installation: InstallationId,
    pub handle: Vec<u8>,
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
        .prepare("SELECT conversation,installation,handle,received_at_ms FROM push_peer ORDER BY conversation,installation")
        .map_err(sql)?
        .query_map([], |r| {
            Ok(PushPeerRow {
                conversation: ConversationId(bytes::<32>(r, 0)?),
                installation: InstallationId(bytes::<32>(r, 1)?),
                handle: r.get(2)?,
                received_at_ms: uint(r, 3)?,
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
