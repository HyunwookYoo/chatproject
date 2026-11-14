//! Push rows of the message store (M7, schema v4): the handles this device has (`push_local`,
//! `push_tag`), the ones others announced (`push_peer`, `push_own`) and the per-device delivery
//! of sealed `PushHandle` frames (`push_delivery`, the `mls_delivery` way). The `pub(super)`
//! helpers run inside the caller's transaction; each `PushDbOp` is its own transaction (one per
//! conversation for `Seal`).
use push_wire::{
    BASE64URL_NOPAD, PUSH_CIPHER_MAX, Platform, PushItem, PushItemKind, PushRequest,
    RENEW_AFTER_MS, Tag,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::mls_group::{Delivery, group_row, signer_of, sql};
use super::mls_store::{Fault, finish, seal_one};
use super::store::{Identity, MessageDbReply, bytes, integer, uint};
use super::types::{ConversationId, MessageId, MessageKey};
use crate::account::{InstallationId, PeerId, storage::schema::LocalAccount};
use crate::mailbox::wire::HandoffEntry;
use crate::mls::{
    MlsProvider, kv::KvStore, payload::push_handle_message, push_ct::strip_group_id,
    wire::MlsFrame,
};
use crate::push::PushStatus;
use crate::{CoreError, ErrorCode};
use openmls_traits::OpenMlsProvider as _;

type R<T> = Result<T, CoreError>;

/// Most due `push_delivery` rows one `DueDeliveries` returns.
const DUE_MAX: i64 = 16;
/// Most unreachable recipients one `UnreachableDue` returns.
const UNREACHABLE_MAX: i64 = 64;

/// This device's `push_local` row.
#[derive(Clone)]
pub(crate) struct PushLocal {
    pub enabled: bool,
    pub platform: Option<Platform>,
    pub token: Option<String>,
    pub own_handle: Option<Vec<u8>>,
}

/// The handles one renewal asks the proxy for: the conversations (with their tags) and the
/// own-device tag, under `platform` and `token`.
pub(crate) struct RenewPlan {
    pub platform: Platform,
    pub token: String,
    pub due: Vec<(ConversationId, Tag)>,
    pub own: Option<Tag>,
}

/// Handles the proxy issued for one renewal under `platform` and `token`.
pub(crate) struct Issued {
    pub platform: Platform,
    pub token: String,
    /// Wall time they are stored as issued at (each conversation: at least its last + 1).
    pub now: u64,
    pub handles: Vec<(ConversationId, Tag, Vec<u8>)>,
    pub own: Option<(Tag, Vec<u8>)>,
}

/// A due `push_delivery` row.
pub(crate) struct PushDue {
    pub conversation: ConversationId,
    pub recipient: InstallationId,
    pub msg_id: MessageId,
    pub wire: Vec<u8>,
}

/// The push decision of a text's first attempt (spec 6.1), made inside the transaction that
/// records the attempt's outcome: what the actor knows, and the `/push` decided on.
pub(crate) struct FirstAttempt {
    /// The peer account's current devices.
    pub devices: Vec<InstallationId>,
    /// The devices the attempt reached: the one that acknowledged the text, and those sent a
    /// copy of it.
    pub reached: Vec<InstallationId>,
    /// Set by `decide`: the push to send, if any.
    pub batch: Option<PushBatch>,
}

/// The conversation a received push's tag names (spec 5.3): one with a group, whose current
/// handle carries that tag.
pub(crate) struct Tagged {
    pub conversation: ConversationId,
    pub peer: PeerId,
    pub group_id: Vec<u8>,
}

/// One `/push` request, and for each of its items (same order) whose handle it carries, so a
/// `gone` result withdraws exactly that handle. `conversation`: the peers' handles of that
/// conversation (`push_peer`); `None`: my own devices' handles (`push_own`).
pub(crate) struct PushBatch {
    pub conversation: Option<ConversationId>,
    pub targets: Vec<(InstallationId, Vec<u8>)>,
    pub request: PushRequest,
}

impl FirstAttempt {
    /// Decides once per text, in the caller's transaction: nothing while push is off here or
    /// before the text is sealed; otherwise `pushed = 1`, and each current device of the peer
    /// with a handle that the attempt did not reach gets the text's ciphertext without its group
    /// id (`cipher`, or `wake` when that is over `PUSH_CIPHER_MAX`). A ciphertext that cannot be
    /// stripped is not pushed, then or later: a push is never tried again.
    pub(super) fn decide(&mut self, tx: &Transaction<'_>, key: MessageKey) -> R<()> {
        let id = params![
            key.conversation.0.as_slice(),
            key.sender.0.as_slice(),
            key.installation.0.as_slice(),
            key.id.0.as_slice()
        ];
        let row: Option<(bool, i64, Vec<u8>)> = tx
            .query_row(
                "SELECT pushed,codec,wire FROM outbox WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
                id,
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(sql)?;
        let Some((false, 2, wire)) = row else {
            return Ok(());
        };
        if !load_local(tx)?.enabled {
            return Ok(());
        }
        tx.execute(
            "UPDATE outbox SET pushed=1 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
            id,
        )
        .map_err(sql)?;
        let Some(stripped) = stripped_ciphertext(&wire) else {
            return Ok(());
        };
        let targets: Vec<(InstallationId, Vec<u8>)> = {
            let mut q = tx
                .prepare("SELECT installation,handle FROM push_peer WHERE conversation=?1 AND handle IS NOT NULL ORDER BY installation")
                .map_err(sql)?;
            q.query_map(params![key.conversation.0.as_slice()], |r| {
                Ok((InstallationId(bytes(r, 0)?), r.get(1)?))
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?
        };
        let targets: Vec<_> = targets
            .into_iter()
            .filter(|(i, _)| self.devices.contains(i) && !self.reached.contains(i))
            .collect();
        if targets.is_empty() {
            return Ok(());
        }
        let kind = if stripped.len() <= PUSH_CIPHER_MAX {
            PushItemKind::Cipher {
                e: BASE64URL_NOPAD.encode(&stripped),
            }
        } else {
            PushItemKind::Wake
        };
        self.batch = Some(batch(Some(key.conversation), targets, kind));
        Ok(())
    }
}

/// The MLS ciphertext of a stored kind 17 `App` frame, its group id stripped (spec 5.3).
fn stripped_ciphertext(wire: &[u8]) -> Option<Vec<u8>> {
    let inner = crate::account::auth::payload(wire, 17).ok()?;
    let MlsFrame::App { ciphertext, .. } = MlsFrame::decode(&inner).ok()? else {
        return None;
    };
    strip_group_id(&ciphertext).ok()
}

/// One request with an item of `kind` for each target's handle.
fn batch(
    conversation: Option<ConversationId>,
    targets: Vec<(InstallationId, Vec<u8>)>,
    kind: PushItemKind,
) -> PushBatch {
    let items = targets
        .iter()
        .map(|(_, handle)| PushItem {
            handle: BASE64URL_NOPAD.encode(handle),
            kind: kind.clone(),
        })
        .collect();
    PushBatch {
        conversation,
        targets,
        request: PushRequest { items },
    }
}

pub(crate) enum PushDbOp {
    Local,
    /// The platform `push_configure` gave. A change drops the token and every handle.
    Configure(Option<Platform>),
    /// A new FCM/APNs token. A change (taken only while push is on) drops every handle so the
    /// next renewal issues them all again (spec 5.1).
    SetToken { platform: Platform, token: String },
    /// Push on, or off at `now` (wall): off drops the token and the handles and queues a
    /// withdrawal for every conversation that had one (spec 8).
    SetEnabled { enabled: bool, now: u64 },
    /// What is due at `now` (wall), after giving every conversation with a group a tag. `own`:
    /// the account has another device, so an Android device keeps an own-device handle.
    Renew { now: u64, own: bool },
    StoreIssued(Box<Issued>),
    /// Seals a `PushHandle` (or a withdrawal) for every conversation whose current state is not
    /// announced yet, queued for each current device of its peer (`devices`); `wall` is the
    /// frame time of the log, `now` the scheduler time the deliveries are due at.
    Seal {
        devices: Vec<(PeerId, Vec<InstallationId>)>,
        wall: u64,
        now: u64,
    },
    DueDeliveries {
        now: u64,
        recipients: Vec<InstallationId>,
    },
    UnreachableDue { now: u64 },
    Delivered {
        conversation: ConversationId,
        recipient: InstallationId,
        msg_id: MessageId,
        outcome: Delivery,
        now: u64,
    },
    /// The queued frames for a mailbox (M6 spec 7), one per conversation and frame; marked
    /// handed off.
    HandOff,
    /// Drops what revoked or unlisted devices announced and what is queued for them: `own` are
    /// this account's other current devices, `peers` each known account's current ones.
    Prune {
        own: Vec<InstallationId>,
        peers: Vec<(PeerId, Vec<InstallationId>)>,
    },
    Status,
    /// The `read` push for those of my devices in `offline` (no session now) that announced an
    /// own-device handle (spec 7); none while push is off here.
    ReadClear { offline: Vec<InstallationId> },
    /// The proxy answered `gone` for these handles: a peer's (of `conversation`) becomes a
    /// withdrawal that keeps its send time (Ruling R10), my own device's goes. A handle replaced
    /// meanwhile stays.
    Gone {
        conversation: Option<ConversationId>,
        gone: Vec<(InstallationId, Vec<u8>)>,
    },
    /// These own texts (each with its peer's current devices) go to a mailbox before their
    /// first attempt had a result: their push is decided now as if no device was reached, in
    /// one transaction (spec 6.1: the mailbox never pushes, and this device may be suspended
    /// before its next attempt). Replies `Batches`.
    HandedOff {
        texts: Vec<(MessageKey, Vec<InstallationId>)>,
    },
    /// The conversation of a received push's tag, if this device has a current handle with it
    /// (a withdrawn one, or one of a conversation gone, names none). Replies `Tagged`.
    Tagged(Tag),
    /// Whether a received text is seen here already (`None`: not stored). Replies `Seen`.
    Seen(MessageKey),
    /// A "새 메시지" notification alerts for a push of the conversation (Ruling R13): marks it,
    /// and replies `Generic` with its unread count and whether one alerted already.
    Generic(ConversationId),
}

impl PushDbOp {
    pub(crate) fn writes(&self) -> bool {
        !matches!(
            self,
            Self::Local
                | Self::DueDeliveries { .. }
                | Self::UnreachableDue { .. }
                | Self::Status
                | Self::ReadClear { .. }
                | Self::Tagged(_)
                | Self::Seen(_)
        )
    }
}

pub(crate) enum PushDbReply {
    Local(PushLocal),
    Count(u64),
    /// `None`: push is off, or this device has no platform or token.
    Plan(Option<RenewPlan>),
    Sealed {
        announced: u32,
        withdrawn: u32,
    },
    /// Due rows of the given recipients, when the next row is due (`None`: no later one), and
    /// whether any due row (of any recipient) is left.
    Due {
        rows: Vec<PushDue>,
        next: Option<u64>,
        left: bool,
    },
    /// Due rows' recipients with the account of their conversation.
    Unreachable(Vec<(ConversationId, InstallationId, PeerId)>),
    Handoff(Vec<HandoffEntry>),
    /// Some conversation has a state not sealed yet.
    Pruned {
        unsealed: bool,
    },
    Status(PushStatus),
    /// The push to send, if any.
    Batch(Option<PushBatch>),
    /// The pushes to send, one per message.
    Batches(Vec<PushBatch>),
    Tagged(Option<Tagged>),
    Seen(Option<bool>),
    Generic { unread: u64, pending: bool },
}

pub(super) fn push_op(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    op: PushDbOp,
    fault: Fault,
) -> R<MessageDbReply> {
    let reply = match op {
        PushDbOp::Local => PushDbReply::Local(load_local(c)?),
        PushDbOp::Configure(platform) => {
            let tx = c.transaction().map_err(sql)?;
            let stored = load_local(&tx)?.platform;
            if stored == platform {
                return Ok(MessageDbReply::Push(PushDbReply::Count(0)));
            }
            tx.execute(
                "UPDATE push_local SET platform=?1,token=NULL,own_handle=NULL WHERE id=1",
                params![platform.map(Platform::to_byte)],
            )
            .map_err(sql)?;
            tx.execute("UPDATE push_tag SET handle=NULL", []).map_err(sql)?;
            finish(tx, fault)?;
            PushDbReply::Count(1)
        }
        PushDbOp::SetToken { platform, token } => {
            if token.is_empty() || token.len() > push_wire::MAX_TOKEN {
                return Err(CoreError::new(ErrorCode::InvalidConfig, "push_token"));
            }
            let tx = c.transaction().map_err(sql)?;
            let stored = load_local(&tx)?;
            // Off: the token is not kept (spec 8); the app sets it again when push is on.
            if !stored.enabled
                || (stored.platform == Some(platform) && stored.token.as_ref() == Some(&token))
            {
                return Ok(MessageDbReply::Push(PushDbReply::Count(0)));
            }
            tx.execute(
                "UPDATE push_local SET platform=?1,token=?2,own_handle=NULL WHERE id=1",
                params![platform.to_byte(), token],
            )
            .map_err(sql)?;
            // `issued_at_ms` stays: it is the floor of the next frame time (Ruling R11).
            tx.execute("UPDATE push_tag SET handle=NULL", []).map_err(sql)?;
            finish(tx, fault)?;
            PushDbReply::Count(1)
        }
        PushDbOp::SetEnabled { enabled, now } => {
            let tx = c.transaction().map_err(sql)?;
            if load_local(&tx)?.enabled == enabled {
                return Ok(MessageDbReply::Push(PushDbReply::Count(0)));
            }
            let mut withdrawn = 0;
            if enabled {
                tx.execute("UPDATE push_local SET enabled=1 WHERE id=1", [])
                    .map_err(sql)?;
            } else {
                tx.execute(
                    "UPDATE push_local SET enabled=0,token=NULL,own_handle=NULL WHERE id=1",
                    [],
                )
                .map_err(sql)?;
                // Every conversation that ever had a handle gets an empty one, sent later than
                // anything sent before (Ruling R11); one never issued has nothing to withdraw.
                withdrawn = tx
                    .execute(
                        "UPDATE push_tag SET handle=NULL,issued_at_ms=max(?1,issued_at_ms+1),announced=0 WHERE issued_at_ms IS NOT NULL",
                        params![integer(now)?],
                    )
                    .map_err(sql)?;
            }
            finish(tx, fault)?;
            PushDbReply::Count(withdrawn as u64)
        }
        PushDbOp::Renew { now, own } => PushDbReply::Plan(renew(c, now, own, fault)?),
        PushDbOp::StoreIssued(issued) => PushDbReply::Count(store_issued(c, &issued, fault)?),
        PushDbOp::Seal { devices, wall, now } => {
            let (announced, withdrawn) = seal(c, identity, local, &devices, wall, now, fault)?;
            PushDbReply::Sealed {
                announced,
                withdrawn,
            }
        }
        PushDbOp::DueDeliveries { now, recipients } => due(c, now, &recipients)?,
        PushDbOp::UnreachableDue { now } => {
            let mut q = c
                .prepare("SELECT DISTINCT d.conversation,d.recipient_install,v.peer FROM push_delivery d JOIN conversation v ON v.id=d.conversation WHERE d.next_retry_at_ms<=?1 ORDER BY d.recipient_install,d.conversation LIMIT ?2")
                .map_err(sql)?;
            let rows = q
                .query_map(params![integer(now)?, UNREACHABLE_MAX], |r| {
                    Ok((
                        ConversationId(bytes(r, 0)?),
                        InstallationId(bytes(r, 1)?),
                        PeerId(bytes(r, 2)?),
                    ))
                })
                .map_err(sql)?
                .collect::<Result<_, _>>()
                .map_err(sql)?;
            PushDbReply::Unreachable(rows)
        }
        PushDbOp::Delivered {
            conversation,
            recipient,
            msg_id,
            outcome,
            now,
        } => {
            let tx = c.transaction().map_err(sql)?;
            let key = params![
                conversation.0.as_slice(),
                recipient.0.as_slice(),
                msg_id.0.as_slice()
            ];
            // A frame replaced meanwhile (a newer handle) is another row: left as it is.
            let changed = match outcome {
                Delivery::Acked | Delivery::Settled => tx
                    .execute(
                        "DELETE FROM push_delivery WHERE conversation=?1 AND recipient_install=?2 AND msg_id=?3",
                        key,
                    )
                    .map_err(sql)?,
                Delivery::Retry => {
                    let attempts: Option<u64> = tx
                        .query_row(
                            "SELECT attempts FROM push_delivery WHERE conversation=?1 AND recipient_install=?2 AND msg_id=?3",
                            key,
                            |r| uint(r, 0),
                        )
                        .optional()
                        .map_err(sql)?;
                    match attempts {
                        Some(attempts) => {
                            // The M4 receipt back-off: 1 s doubling to 5 min.
                            let attempts = attempts.saturating_add(1).min(u64::from(u32::MAX));
                            let next = now.saturating_add(super::scheduler::retry_delay(attempts));
                            tx.execute(
                                "UPDATE push_delivery SET attempts=?4,next_retry_at_ms=?5 WHERE conversation=?1 AND recipient_install=?2 AND msg_id=?3",
                                params![
                                    conversation.0.as_slice(),
                                    recipient.0.as_slice(),
                                    msg_id.0.as_slice(),
                                    integer(attempts)?,
                                    integer(next)?
                                ],
                            )
                            .map_err(sql)?
                        }
                        None => 0,
                    }
                }
            };
            finish(tx, fault)?;
            PushDbReply::Count(changed as u64)
        }
        PushDbOp::HandOff => PushDbReply::Handoff(hand_off(c, fault)?),
        PushDbOp::Prune { own, peers } => {
            prune(c, &own, &peers, fault)?;
            // A state not sealed for want of a known device may be sealed now.
            let unsealed = c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM push_tag WHERE announced=0)",
                    [],
                    |r| r.get(0),
                )
                .map_err(sql)?;
            PushDbReply::Pruned { unsealed }
        }
        PushDbOp::Status => {
            let local = load_local(c)?;
            let (handles, oldest) = c
                .query_row(
                    "SELECT count(*),min(issued_at_ms) FROM push_tag WHERE handle IS NOT NULL",
                    [],
                    |r| Ok((uint(r, 0)?, r.get::<_, Option<i64>>(1)?)),
                )
                .map_err(sql)?;
            PushDbReply::Status(PushStatus {
                enabled: local.enabled,
                platform: local.platform,
                handles: u32::try_from(handles).unwrap_or(u32::MAX),
                oldest_issued_at_ms: oldest.and_then(|at| u64::try_from(at).ok()),
            })
        }
        PushDbOp::ReadClear { offline } => {
            let targets: Vec<(InstallationId, Vec<u8>)> = if load_local(c)?.enabled {
                let mut q = c
                    .prepare("SELECT installation,handle FROM push_own ORDER BY installation")
                    .map_err(sql)?;
                q.query_map([], |r| Ok((InstallationId(bytes(r, 0)?), r.get(1)?)))
                    .map_err(sql)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(sql)?
            } else {
                Vec::new()
            };
            let targets: Vec<_> = targets
                .into_iter()
                .filter(|(i, _)| offline.contains(i))
                .collect();
            PushDbReply::Batch(
                (!targets.is_empty()).then(|| batch(None, targets, PushItemKind::Read)),
            )
        }
        PushDbOp::Gone { conversation, gone } => {
            let tx = c.transaction().map_err(sql)?;
            let mut changed = 0;
            for (installation, handle) in &gone {
                changed += match conversation {
                    Some(conversation) => tx.execute(
                        "UPDATE push_peer SET handle=NULL WHERE conversation=?1 AND installation=?2 AND handle=?3",
                        params![conversation.0.as_slice(), installation.0.as_slice(), handle],
                    ),
                    None => tx.execute(
                        "DELETE FROM push_own WHERE installation=?1 AND handle=?2",
                        params![installation.0.as_slice(), handle],
                    ),
                }
                .map_err(sql)?;
            }
            finish(tx, fault)?;
            PushDbReply::Count(changed as u64)
        }
        PushDbOp::HandedOff { texts } => {
            let tx = c.transaction().map_err(sql)?;
            let mut batches = Vec::new();
            for (key, devices) in texts {
                let mut first = FirstAttempt {
                    devices,
                    reached: Vec::new(),
                    batch: None,
                };
                first.decide(&tx, key)?;
                batches.extend(first.batch);
            }
            finish(tx, fault)?;
            PushDbReply::Batches(batches)
        }
        PushDbOp::Tagged(tag) => PushDbReply::Tagged(
            c.query_row(
                "SELECT p.conversation,v.peer,m.group_id FROM push_tag p JOIN conversation v ON v.id=p.conversation JOIN mls_conversation m ON m.conversation=p.conversation WHERE p.tag=?1 AND p.handle IS NOT NULL",
                params![tag.as_slice()],
                |r| {
                    Ok(Tagged {
                        conversation: ConversationId(bytes(r, 0)?),
                        peer: PeerId(bytes(r, 1)?),
                        group_id: r.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(sql)?,
        ),
        PushDbOp::Seen(key) => PushDbReply::Seen(
            c.query_row(
                "SELECT seen_local FROM message WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
                params![
                    key.conversation.0.as_slice(),
                    key.sender.0.as_slice(),
                    key.installation.0.as_slice(),
                    key.id.0.as_slice()
                ],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?,
        ),
        PushDbOp::Generic(conversation) => {
            let tx = c.transaction().map_err(sql)?;
            let id = params![conversation.0.as_slice()];
            let unread = tx
                .query_row("SELECT unread FROM conversation WHERE id=?1", id, |r| uint(r, 0))
                .optional()
                .map_err(sql)?
                .unwrap_or(0);
            let pending = generic_pending(&tx, conversation)?;
            tx.execute("UPDATE push_tag SET generic=1 WHERE conversation=?1", id)
                .map_err(sql)?;
            finish(tx, fault)?;
            PushDbReply::Generic { unread, pending }
        }
    };
    Ok(MessageDbReply::Push(reply))
}

fn load_local(c: &Connection) -> R<PushLocal> {
    c.query_row(
        "SELECT enabled,platform,token,own_handle FROM push_local WHERE id=1",
        [],
        |r| {
            Ok(PushLocal {
                enabled: r.get(0)?,
                platform: r
                    .get::<_, Option<u8>>(1)?
                    .and_then(Platform::from_byte),
                token: r.get(2)?,
                own_handle: r.get(3)?,
            })
        },
    )
    .map_err(sql)
}

fn renew(c: &mut Connection, now: u64, own: bool, fault: Fault) -> R<Option<RenewPlan>> {
    let tx = c.transaction().map_err(sql)?;
    let local = load_local(&tx)?;
    let (true, Some(platform), Some(token)) = (local.enabled, local.platform, local.token) else {
        return Ok(None);
    };
    // Spec 5.1: every conversation with a group gets a tag of its own (never announced yet).
    let untagged: Vec<[u8; 32]> = {
        let mut q = tx
            .prepare("SELECT m.conversation FROM mls_conversation m WHERE NOT EXISTS(SELECT 1 FROM push_tag p WHERE p.conversation=m.conversation)")
            .map_err(sql)?;
        q.query_map([], |r| bytes::<32>(r, 0))
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?
    };
    for conversation in untagged {
        let mut tag: Tag = [0; 16];
        getrandom::fill(&mut tag).map_err(|_| CoreError::new(ErrorCode::NotAvailable, "push_tag"))?;
        tx.execute(
            "INSERT INTO push_tag VALUES(?1,?2,NULL,NULL,1,0)",
            params![conversation.as_slice(), tag.as_slice()],
        )
        .map_err(sql)?;
    }
    // Issued at or before this is due again; before `RENEW_AFTER_MS` of wall time nothing is.
    let renew_at = now
        .checked_sub(RENEW_AFTER_MS)
        .map(integer)
        .transpose()?
        .unwrap_or(-1);
    let due = {
        let mut q = tx
            .prepare("SELECT p.conversation,p.tag FROM push_tag p JOIN mls_conversation m ON m.conversation=p.conversation WHERE p.handle IS NULL OR p.issued_at_ms IS NULL OR p.issued_at_ms<=?1 ORDER BY p.conversation")
            .map_err(sql)?;
        q.query_map(params![renew_at], |r| {
            Ok((ConversationId(bytes(r, 0)?), bytes::<16>(r, 1)?))
        })
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?
    };
    // The own-device handle is for read clearing, which is Android only (spec 5.2, 7).
    let own = if own && platform == Platform::Fcm {
        tx.query_row(
            "SELECT own_tag FROM push_local WHERE id=1 AND (own_handle IS NULL OR own_issued_at_ms IS NULL OR own_issued_at_ms<=?1)",
            params![renew_at],
            |r| bytes::<16>(r, 0),
        )
        .optional()
        .map_err(sql)?
    } else {
        None
    };
    finish(tx, fault)?;
    Ok(Some(RenewPlan {
        platform,
        token,
        due,
        own,
    }))
}

/// Stores what the proxy issued, only while push is still on under the same platform and token
/// (a reply that comes back after push went off, or after the token changed, is dropped).
fn store_issued(c: &mut Connection, issued: &Issued, fault: Fault) -> R<u64> {
    let tx = c.transaction().map_err(sql)?;
    let current: bool = tx
        .query_row(
            "SELECT enabled=1 AND platform=?1 AND token=?2 FROM push_local WHERE id=1",
            params![issued.platform.to_byte(), issued.token],
            |r| r.get::<_, Option<bool>>(0),
        )
        .map_err(sql)?
        .unwrap_or(false);
    if !current {
        return Ok(0);
    }
    let now = integer(issued.now)?;
    let fits = |handle: &[u8]| (1..=MAX_HANDLE).contains(&handle.len());
    let mut stored = 0;
    for (conversation, tag, handle) in &issued.handles {
        if !fits(handle) {
            continue;
        }
        stored += tx
            .execute(
                "UPDATE push_tag SET handle=?3,issued_at_ms=max(?4,coalesce(issued_at_ms+1,0)),announced=0 WHERE conversation=?1 AND tag=?2",
                params![conversation.0.as_slice(), tag.as_slice(), handle, now],
            )
            .map_err(sql)?;
    }
    if let Some((tag, handle)) = &issued.own
        && fits(handle)
    {
        stored += tx
            .execute(
                "UPDATE push_local SET own_handle=?2,own_issued_at_ms=max(?3,coalesce(own_issued_at_ms+1,0)) WHERE id=1 AND own_tag=?1",
                params![tag.as_slice(), handle, now],
            )
            .map_err(sql)?;
    }
    finish(tx, fault)?;
    Ok(stored as u64)
}

#[allow(clippy::too_many_arguments)]
fn seal(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    devices: &[(PeerId, Vec<InstallationId>)],
    wall: u64,
    now: u64,
    fault: Fault,
) -> R<(u32, u32)> {
    type Pending = ([u8; 32], [u8; 32], Option<Vec<u8>>, Option<u64>);
    let pending: Vec<Pending> = {
        let mut q = c
            .prepare("SELECT p.conversation,v.peer,p.handle,p.issued_at_ms FROM push_tag p JOIN conversation v ON v.id=p.conversation JOIN mls_conversation m ON m.conversation=p.conversation WHERE p.announced=0 ORDER BY p.conversation")
            .map_err(sql)?;
        q.query_map([], |r| {
            Ok((
                bytes::<32>(r, 0)?,
                bytes::<32>(r, 1)?,
                r.get(2)?,
                r.get::<_, Option<i64>>(3)?.and_then(|at| u64::try_from(at).ok()),
            ))
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?
    };
    let (mut announced, mut withdrawn) = (0, 0);
    for (conversation, peer, handle, issued_at_ms) in pending {
        let conversation = ConversationId(conversation);
        // No device of the peer known yet: sealed once one is (the ratchet does not move).
        let Some(recipients) = devices
            .iter()
            .find(|(p, _)| p.0 == peer)
            .map(|(_, r)| r)
            .filter(|r| !r.is_empty())
        else {
            continue;
        };
        let Some(issued_at_ms) = issued_at_ms else {
            continue;
        };
        let handle_sent = handle.is_some();
        let tx = c.transaction().map_err(sql)?;
        let Some(group) = group_row(&tx, conversation)? else {
            continue;
        };
        let provider = MlsProvider::new(KvStore::load_scope(&tx, conversation.0)?);
        let (signer, _) = signer_of(local);
        let message =
            push_handle_message(identity.installation, issued_at_ms, handle.unwrap_or_default());
        let key = MessageKey {
            conversation,
            sender: identity.peer,
            installation: identity.installation,
            id: message.msg_id,
        };
        let frame = match seal_one(&tx, &provider, &signer, identity, key, &group.group_id, &message, wall) {
            Ok((_, frame)) => frame,
            // A group that cannot seal now: tried again with the next pass.
            Err(error) if error.code == ErrorCode::InvalidMessage => continue,
            Err(error) => return Err(error),
        };
        // One frame per recipient: a newer one replaces what is still queued (plan 2).
        for recipient in recipients {
            tx.execute(
                "INSERT OR REPLACE INTO push_delivery VALUES(?1,?2,?3,?4,0,?5,0)",
                params![
                    conversation.0.as_slice(),
                    recipient.0.as_slice(),
                    message.msg_id.0.as_slice(),
                    frame,
                    integer(now)?
                ],
            )
            .map_err(sql)?;
        }
        tx.execute(
            "UPDATE push_tag SET announced=1 WHERE conversation=?1",
            params![conversation.0.as_slice()],
        )
        .map_err(sql)?;
        provider.storage().flush(&tx, conversation.0)?;
        finish(tx, fault)?;
        if handle_sent {
            announced += 1;
        } else {
            withdrawn += 1;
        }
    }
    Ok((announced, withdrawn))
}

fn due(c: &Connection, now: u64, recipients: &[InstallationId]) -> R<PushDbReply> {
    let now_value = integer(now)?;
    let rows = if recipients.is_empty() {
        Vec::new()
    } else {
        let reachable = vec!["?"; recipients.len()].join(",");
        let mut q = c
            .prepare(&format!("SELECT conversation,recipient_install,msg_id,wire FROM push_delivery WHERE next_retry_at_ms<=? AND recipient_install IN ({reachable}) ORDER BY next_retry_at_ms,conversation,recipient_install LIMIT {DUE_MAX}"))
            .map_err(sql)?;
        let mut values: Vec<rusqlite::types::Value> = vec![now_value.into()];
        values.extend(recipients.iter().map(|r| r.0.to_vec().into()));
        q.query_map(rusqlite::params_from_iter(values.iter()), |r| {
            Ok(PushDue {
                conversation: ConversationId(bytes(r, 0)?),
                recipient: InstallationId(bytes(r, 1)?),
                msg_id: MessageId(bytes(r, 2)?),
                wire: r.get(3)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?
    };
    let (next, left) = c
        .query_row(
            "SELECT (SELECT min(next_retry_at_ms) FROM push_delivery WHERE next_retry_at_ms>?1),EXISTS(SELECT 1 FROM push_delivery WHERE next_retry_at_ms<=?1)",
            params![now_value],
            |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, bool>(1)?)),
        )
        .map_err(sql)?;
    Ok(PushDbReply::Due {
        rows,
        next: next.and_then(|at| u64::try_from(at).ok()),
        left,
    })
}

/// A queued frame as `hand_off` reads it: conversation, peer account, msg_id, frame.
type QueuedFrame = ([u8; 32], [u8; 32], [u8; 16], Vec<u8>);

fn hand_off(c: &mut Connection, fault: Fault) -> R<Vec<HandoffEntry>> {
    let tx = c.transaction().map_err(sql)?;
    let rows: Vec<QueuedFrame> = {
        let mut q = tx
            .prepare("SELECT d.conversation,v.peer,d.msg_id,min(d.wire) FROM push_delivery d JOIN conversation v ON v.id=d.conversation GROUP BY d.conversation,d.msg_id ORDER BY d.conversation,d.msg_id")
            .map_err(sql)?;
        q.query_map([], |r| {
            Ok((bytes(r, 0)?, bytes(r, 1)?, bytes(r, 2)?, r.get(3)?))
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?
    };
    tx.execute("UPDATE push_delivery SET handed_off=1 WHERE handed_off=0", [])
        .map_err(sql)?;
    finish(tx, fault)?;
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

fn prune(
    c: &mut Connection,
    own: &[InstallationId],
    peers: &[(PeerId, Vec<InstallationId>)],
    fault: Fault,
) -> R<u64> {
    let tx = c.transaction().map_err(sql)?;
    let mut dropped = 0;
    let owned: Vec<[u8; 32]> = {
        let mut q = tx.prepare("SELECT installation FROM push_own").map_err(sql)?;
        q.query_map([], |r| bytes::<32>(r, 0))
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?
    };
    for installation in owned.iter().filter(|i| !own.iter().any(|o| o.0 == **i)) {
        dropped += tx
            .execute(
                "DELETE FROM push_own WHERE installation=?1",
                params![installation.as_slice()],
            )
            .map_err(sql)?;
    }
    // A revoked device never comes back (its frames are refused as not current): its rows go.
    for (table, column) in [
        ("push_peer", "installation"),
        ("push_delivery", "recipient_install"),
    ] {
        let rows: Vec<([u8; 32], [u8; 32], [u8; 32])> = {
            let mut q = tx
                .prepare(&format!("SELECT t.conversation,t.{column},v.peer FROM {table} t JOIN conversation v ON v.id=t.conversation"))
                .map_err(sql)?;
            q.query_map([], |r| Ok((bytes(r, 0)?, bytes(r, 1)?, bytes(r, 2)?)))
                .map_err(sql)?
                .collect::<Result<_, _>>()
                .map_err(sql)?
        };
        for (conversation, installation, peer) in rows {
            let gone = peers
                .iter()
                .find(|(p, _)| p.0 == peer)
                .is_some_and(|(_, current)| !current.iter().any(|i| i.0 == installation));
            if gone {
                dropped += tx
                    .execute(
                        &format!("DELETE FROM {table} WHERE conversation=?1 AND {column}=?2"),
                        params![conversation.as_slice(), installation.as_slice()],
                    )
                    .map_err(sql)?;
            }
        }
    }
    // Nothing to drop (the usual case): nothing is committed.
    if dropped > 0 {
        finish(tx, fault)?;
    }
    Ok(dropped as u64)
}

/// Own device `installation` announced its own-device handle (`None`: it has none), in the
/// transaction recording its `Announce` (spec 5.2).
pub(super) fn store_own_handle(
    tx: &Transaction<'_>,
    installation: InstallationId,
    handle: Option<&[u8]>,
) -> R<()> {
    match handle.filter(|h| (1..=MAX_HANDLE).contains(&h.len())) {
        Some(handle) => tx.execute(
            "INSERT INTO push_own VALUES(?1,?2) ON CONFLICT(installation) DO UPDATE SET handle=excluded.handle",
            params![installation.0.as_slice(), handle],
        ),
        None => tx.execute(
            "DELETE FROM push_own WHERE installation=?1",
            params![installation.0.as_slice()],
        ),
    }
    .map_err(sql)?;
    Ok(())
}

/// A mailbox delivered this device's `PushHandle` frame `msg_id` (its `Delivered` note, M6 spec
/// 7): the peer account has it, so it is no longer queued for any of its devices.
pub(super) fn drop_delivered(
    tx: &Transaction<'_>,
    conversation: ConversationId,
    msg_id: MessageId,
) -> R<()> {
    tx.execute(
        "DELETE FROM push_delivery WHERE conversation=?1 AND msg_id=?2",
        params![conversation.0.as_slice(), msg_id.0.as_slice()],
    )
    .map_err(sql)?;
    Ok(())
}

/// Ruling R13: a "새 메시지" notification alerted for a push of the conversation and it was not
/// read up since; until it is, its new texts update that notification without alerting.
pub(super) fn generic_pending(c: &Connection, conversation: ConversationId) -> R<bool> {
    Ok(c.query_row(
        "SELECT generic FROM push_tag WHERE conversation=?1",
        params![conversation.0.as_slice()],
        |r| r.get::<_, bool>(0),
    )
    .optional()
    .map_err(sql)?
    .unwrap_or(false))
}

/// In the transaction of a read of the conversation (here, synced from another of my devices,
/// or a text stored already read): once nothing is unread, a generic push notification no
/// longer keeps its new texts from alerting (Ruling R13). Other frames never clear it: a wake's
/// generic notification shows before its text is stored, while `unread` may still be 0.
pub(super) fn read_up(tx: &Transaction<'_>, conversation: ConversationId) -> R<()> {
    tx.execute(
        "UPDATE push_tag SET generic=0 WHERE conversation=?1 AND generic=1 AND (SELECT unread FROM conversation WHERE id=?1)=0",
        params![conversation.0.as_slice()],
    )
    .map_err(sql)?;
    Ok(())
}

/// Longest handle `push_peer`, `push_tag`, `push_own` and a kind 18 `Announce` take (their
/// CHECKs).
pub(crate) const MAX_HANDLE: usize = 1_024;

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

/// One `push_tag` row, for fixture assertions.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTagRow {
    pub conversation: ConversationId,
    pub tag: Tag,
    pub handle: Option<Vec<u8>>,
    pub issued_at_ms: Option<u64>,
    pub announced: bool,
}

/// One `push_delivery` row, for fixture assertions.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushDeliveryRow {
    pub conversation: ConversationId,
    pub recipient: InstallationId,
    pub msg_id: MessageId,
    pub attempts: u64,
    pub handed_off: bool,
}

/// What a fixture sees of this device's own handles and their delivery.
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushState {
    pub enabled: bool,
    pub platform: Option<Platform>,
    pub token: Option<String>,
    pub own_tag: Tag,
    pub own_handle: Option<Vec<u8>>,
    pub own_issued_at_ms: Option<u64>,
    /// By conversation.
    pub tags: Vec<PushTagRow>,
    /// `push_own`, by installation.
    pub own: Vec<(InstallationId, Vec<u8>)>,
    /// By conversation and recipient.
    pub deliveries: Vec<PushDeliveryRow>,
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_state(c: &rusqlite::Connection) -> R<PushState> {
    let local = load_local(c)?;
    let (own_tag, own_issued_at_ms) = c
        .query_row(
            "SELECT own_tag,own_issued_at_ms FROM push_local WHERE id=1",
            [],
            |r| Ok((bytes::<16>(r, 0)?, r.get::<_, Option<i64>>(1)?)),
        )
        .map_err(sql)?;
    let tags = c
        .prepare("SELECT conversation,tag,handle,issued_at_ms,announced FROM push_tag ORDER BY conversation")
        .map_err(sql)?
        .query_map([], |r| {
            Ok(PushTagRow {
                conversation: ConversationId(bytes(r, 0)?),
                tag: bytes(r, 1)?,
                handle: r.get(2)?,
                issued_at_ms: r.get::<_, Option<i64>>(3)?.map(|at| at as u64),
                announced: r.get(4)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    let own = c
        .prepare("SELECT installation,handle FROM push_own ORDER BY installation")
        .map_err(sql)?
        .query_map([], |r| Ok((InstallationId(bytes(r, 0)?), r.get(1)?)))
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    let deliveries = c
        .prepare("SELECT conversation,recipient_install,msg_id,attempts,handed_off FROM push_delivery ORDER BY conversation,recipient_install")
        .map_err(sql)?
        .query_map([], |r| {
            Ok(PushDeliveryRow {
                conversation: ConversationId(bytes(r, 0)?),
                recipient: InstallationId(bytes(r, 1)?),
                msg_id: MessageId(bytes(r, 2)?),
                attempts: uint(r, 3)?,
                handed_off: r.get(4)?,
            })
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?;
    Ok(PushState {
        enabled: local.enabled,
        platform: local.platform,
        token: local.token,
        own_tag,
        own_handle: local.own_handle,
        own_issued_at_ms: own_issued_at_ms.map(|at| at as u64),
        tags,
        own,
        deliveries,
    })
}
