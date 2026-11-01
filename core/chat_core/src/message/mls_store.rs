//! MLS operations of the message store. Each one is a single SQL transaction that carries the
//! MLS state it changed (`mls_kv`, `mls_conversation`) together with the `message`, `outbox` and
//! `processed_ct` rows, so a crash or a rolled-back transaction can never leave a ratchet that
//! moved without the message it opened, or the reverse.
//!
//! A `KvStore` is loaded per operation and `flush`ed into the operation's transaction only on
//! success. `flush` clears its dirty set before the caller commits, so after any failure the
//! store (and its provider) is dropped; the next operation loads a fresh one.
use super::mls_group::{check_own, group_row, signer_of, sql};
use super::store::{
    Identity, MessageDbReply, bump, bytes, commit, ensure_conversation, find, integer, invalid,
    missing, receipt_for, schedule_retry, target, uint,
};
use super::types::*;
use super::wire::ReceiptKind;
use crate::mls::{
    MlsProvider,
    group::{self, OpenFailure, Opened},
    kv::KvStore,
    payload::{ChatMessage, Content},
    wire::{MlsFrame, MlsReply},
};
use crate::{
    CoreError, ErrorCode,
    account::{InstallationId, PeerId, storage::schema::LocalAccount},
};
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider as _;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

type R<T> = Result<T, CoreError>;
/// The injected commit fault of test-hook builds; nothing in production builds.
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) type Fault = Option<crate::storage::worker::CommitFault>;
#[cfg(not(all(feature = "test-hooks", debug_assertions)))]
pub(super) type Fault = ();
pub(super) fn finish(tx: Transaction<'_>, fault: Fault) -> R<()> {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    return commit(tx, fault);
    #[cfg(not(all(feature = "test-hooks", debug_assertions)))]
    {
        let () = fault;
        commit(tx)
    }
}

/// Rows of group-independent labels (KeyPackages, signature keys) live in this scope.
pub(super) const GLOBAL: [u8; 32] = [0; 32];
/// Largest complete frame (M4 transport limit).
pub(super) const MAX_FRAME: usize = 61_440;
/// A message that was re-encrypted this many times is given up on.
const REENCRYPT_CAP: u64 = 3;
const TEXT_LIMIT: usize = 16_384;
/// The outbox holds at most this many wire bytes (M4 rule), sealed frames included.
const OUTBOX_BYTES: u64 = 128 * 1024 * 1024;
/// Upper bound of what sealing adds to a padded plaintext bucket (MLS framing, AEAD tag,
/// signature, kind 17 frame): the largest bucket plus this is the frame limit.
const SEAL_OVERHEAD: usize = MAX_FRAME - crate::mls::payload::MAX_BUCKET;
/// CBOR bytes of a text `ChatMessage` other than its body (array, version, msg_id, lamport,
/// prev_hash, sent_at, content header, body string header), at their largest.
const TEXT_HEAD: usize = 76;

/// Outbox bytes a row awaiting encryption (codec 3) reserves: the frame its body will seal to,
/// at most (the bucket it pads to plus `SEAL_OVERHEAD`). Accounting this at enqueue time keeps
/// a later seal from finding the outbox full.
pub(super) fn reserved_bytes(body_len: usize) -> u64 {
    let bucket = crate::mls::payload::BUCKETS
        .into_iter()
        .find(|bucket| *bucket > body_len + TEXT_HEAD)
        .unwrap_or(crate::mls::payload::MAX_BUCKET);
    (bucket + SEAL_OVERHEAD).min(MAX_FRAME) as u64
}

/// `(rows, bytes)` of the outbox for its 10,000 row / 128 MiB caps: a sealed row counts its
/// frame, a row awaiting encryption what `reserved_bytes` reserves for it.
pub(super) fn outbox_usage(c: &Connection) -> R<(u64, u64)> {
    let mut reserve = String::from("CASE");
    for bucket in crate::mls::payload::BUCKETS {
        reserve.push_str(&format!(
            " WHEN length(CAST(m.body AS BLOB))+{TEXT_HEAD}<{bucket} THEN {}",
            (bucket + SEAL_OVERHEAD).min(MAX_FRAME)
        ));
    }
    reserve.push_str(&format!(" ELSE {MAX_FRAME} END"));
    c.query_row(
        &format!("SELECT count(*),coalesce(sum(CASE WHEN o.codec=3 THEN {reserve} ELSE length(o.wire) END),0) FROM outbox o JOIN message m USING(conversation,sender_peer,sender_install,msg_id)"),
        [],
        |r| Ok((uint(r, 0)?, uint(r, 1)?)),
    )
    .map_err(sql)
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
thread_local! {
    /// Lowered outbox byte cap for tests, per storage writer thread.
    static OUTBOX_CAP: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}
pub(super) fn outbox_cap() -> u64 {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    if let Some(cap) = OUTBOX_CAP.with(|c| c.get()) {
        return cap;
    }
    OUTBOX_BYTES
}
/// Replacing `old_len` wire bytes of one row by `new_len` must keep the whole outbox within the
/// cap. Checked inside the sealing transaction, so a refusal writes nothing.
fn check_outbox_cap(tx: &Transaction<'_>, old_len: u64, new_len: u64) -> R<()> {
    let total: u64 = tx
        .query_row(
            "SELECT coalesce(sum(length(wire)),0) FROM outbox",
            [],
            |r| uint(r, 0),
        )
        .map_err(sql)?;
    if total.saturating_sub(old_len).saturating_add(new_len) > outbox_cap() {
        return Err(CoreError::new(ErrorCode::StorageFull, "message_queue_full"));
    }
    Ok(())
}
/// SHA-256 of the MLS ciphertext inside a stored kind 17 `App` frame.
fn wire_ciphertext_hash(wire: &[u8]) -> R<[u8; 32]> {
    let inner = crate::account::auth::payload(wire, 17).map_err(|_| invalid())?;
    match MlsFrame::decode(&inner).map_err(|_| invalid())? {
        MlsFrame::App { ciphertext, .. } => Ok(Sha256::digest(&ciphertext).into()),
        _ => Err(invalid()),
    }
}

struct Outgoing {
    codec: i64,
    reencrypts: u64,
    dispatch_state: u64,
    wire: Vec<u8>,
    message: ChatMessage,
}
fn load_outgoing(tx: &Transaction<'_>, key: MessageKey) -> R<Outgoing> {
    tx.query_row(
        "SELECT o.codec,o.reencrypts,m.lamport,m.prev_hash,m.sent_at_ms,m.body,o.dispatch_state,o.wire FROM message m JOIN outbox o USING(conversation,sender_peer,sender_install,msg_id) WHERE m.conversation=?1 AND m.sender_peer=?2 AND m.sender_install=?3 AND m.msg_id=?4 AND m.outgoing=1 AND m.cancel_requested=0",
        params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice()],
        |r| Ok(Outgoing {
            codec: r.get(0)?,
            reencrypts: uint(r, 1)?,
            dispatch_state: uint(r, 6)?,
            wire: r.get(7)?,
            message: ChatMessage {
                msg_id: key.id,
                lamport: uint(r, 2)?,
                prev_hash: bytes(r, 3)?,
                sent_at_ms: uint(r, 4)?,
                content: Content::Text { body: r.get(5)? },
            },
        }),
    )
    .optional()
    .map_err(sql)?
    .ok_or_else(missing)
}

/// Seals `message`, remembers the ciphertext hash (so this device recognises it if it comes
/// back) and returns the hash and the complete kind 17 frame.
#[allow(clippy::too_many_arguments)]
fn seal_one(
    tx: &Transaction<'_>,
    provider: &MlsProvider,
    signer: &SignatureKeyPair,
    identity: Identity,
    key: MessageKey,
    group_id: &[u8],
    message: &ChatMessage,
    now: u64,
) -> R<([u8; 32], Vec<u8>)> {
    let ciphertext = group::seal(provider, signer, group_id, message)?;
    let frame = MlsFrame::App {
        conversation: key.conversation,
        ciphertext: ciphertext.clone(),
    }
    .encode();
    if frame.len() > MAX_FRAME {
        return Err(CoreError::new(
            ErrorCode::MessageTooLarge,
            "message_frame_too_large",
        ));
    }
    let hash: [u8; 32] = Sha256::digest(&ciphertext).into();
    tx.execute(
        "INSERT OR IGNORE INTO processed_ct VALUES(2,?1,?2,?3,?4)",
        params![
            identity.peer.0.as_slice(),
            identity.installation.0.as_slice(),
            hash.as_slice(),
            integer(now)?
        ],
    )
    .map_err(sql)?;
    Ok((hash, frame.to_vec()))
}

pub(super) fn seal(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    key: MessageKey,
    now: u64,
    fault: Fault,
) -> R<MessageDbReply> {
    check_own(identity, key)?;
    let tx = c.transaction().map_err(sql)?;
    let outgoing = load_outgoing(&tx, key)?;
    match outgoing.codec {
        3 => {}
        // Already sealed: report the ciphertext it carries, never seal twice.
        2 => {
            return Ok(MessageDbReply::Sealed {
                ct_hash: wire_ciphertext_hash(&outgoing.wire)?,
            });
        }
        _ => return Err(invalid()),
    }
    let Some(group) = group_row(&tx, key.conversation)? else {
        return Ok(MessageDbReply::NeedGroup(key.conversation));
    };
    let provider = MlsProvider::new(KvStore::load(&tx, key.conversation.0)?);
    let (signer, _) = signer_of(local);
    let (hash, frame) = seal_one(
        &tx,
        &provider,
        &signer,
        identity,
        key,
        &group.group_id,
        &outgoing.message,
        now,
    )?;
    check_outbox_cap(&tx, outgoing.wire.len() as u64, frame.len() as u64)?;
    store_sealed(&tx, key, &frame)?;
    provider.storage().flush(&tx, key.conversation.0)?;
    finish(tx, fault)?;
    Ok(MessageDbReply::Sealed { ct_hash: hash })
}

/// Never touches `message.payload_hash`: for a promoted M4 row it is the canonical M4 text hash
/// that M4 receipts are checked against. The own ciphertext hash lives in `processed_ct`.
fn store_sealed(tx: &Transaction<'_>, key: MessageKey, frame: &[u8]) -> R<()> {
    tx.execute(
        "UPDATE outbox SET codec=2,wire=?5,waiting_peer_update=0 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
        params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice(), frame],
    )
    .map_err(sql)?;
    Ok(())
}

/// Seals every waiting (codec 3) row of the conversation in lamport order, in one transaction:
/// the batch is atomic. If any row would push the outbox over its byte cap the whole batch fails
/// with `StorageFull` and nothing (rows, ratchet, markers) is written.
pub(super) fn seal_pending(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    conversation: ConversationId,
    now: u64,
    fault: Fault,
) -> R<MessageDbReply> {
    let tx = c.transaction().map_err(sql)?;
    let Some(group) = group_row(&tx, conversation)? else {
        return Ok(MessageDbReply::NeedGroup(conversation));
    };
    let ids: Vec<[u8; 16]> = {
        let mut q = tx
            .prepare("SELECT o.msg_id FROM outbox o JOIN message m USING(conversation,sender_peer,sender_install,msg_id) WHERE o.conversation=?1 AND o.sender_peer=?2 AND o.sender_install=?3 AND o.codec=3 AND m.outgoing=1 AND m.cancel_requested=0 ORDER BY m.lamport,o.msg_id")
            .map_err(sql)?;
        q.query_map(
            params![
                conversation.0.as_slice(),
                identity.peer.0.as_slice(),
                identity.installation.0.as_slice()
            ],
            |r| bytes::<16>(r, 0),
        )
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?
    };
    if ids.is_empty() {
        return Ok(MessageDbReply::Count(0));
    }
    let provider = MlsProvider::new(KvStore::load(&tx, conversation.0)?);
    let (signer, _) = signer_of(local);
    for id in &ids {
        let key = MessageKey {
            conversation,
            sender: identity.peer,
            installation: identity.installation,
            id: MessageId(*id),
        };
        let outgoing = load_outgoing(&tx, key)?;
        let (_, frame) = seal_one(
            &tx,
            &provider,
            &signer,
            identity,
            key,
            &group.group_id,
            &outgoing.message,
            now,
        )?;
        check_outbox_cap(&tx, outgoing.wire.len() as u64, frame.len() as u64)?;
        store_sealed(&tx, key, &frame)?;
        // A row that waited for the group (backing off meanwhile) can go out now.
        tx.execute(
            "UPDATE outbox SET next_retry_at_ms=min(next_retry_at_ms,?5) WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
            params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice(), integer(now)?],
        )
        .map_err(sql)?;
    }
    provider.storage().flush(&tx, conversation.0)?;
    finish(tx, fault)?;
    Ok(MessageDbReply::Count(ids.len() as u64))
}

/// Seals a sent message again after the receiver could not open the first ciphertext. The
/// message id, lamport and everything else stay; only the ciphertext is new. The third time
/// the message is given up on (a user `Retry` starts the count over).
pub(super) fn reencrypt(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    key: MessageKey,
    now: u64,
    fault: Fault,
) -> R<MessageDbReply> {
    check_own(identity, key)?;
    let tx = c.transaction().map_err(sql)?;
    let outgoing = load_outgoing(&tx, key)?;
    if outgoing.codec != 2 {
        return Err(invalid());
    }
    // An attempt is in flight: its outcome has not been recorded yet, and a new ciphertext
    // (or giving the message up) must not race it.
    if outgoing.dispatch_state == 1 {
        return Err(CoreError::new(ErrorCode::Busy, "message_in_flight"));
    }
    if outgoing.reencrypts >= REENCRYPT_CAP {
        return Ok(MessageDbReply::Done);
    }
    let Some(group) = group_row(&tx, key.conversation)? else {
        return Ok(MessageDbReply::NeedGroup(key.conversation));
    };
    let provider = MlsProvider::new(KvStore::load(&tx, key.conversation.0)?);
    let (signer, _) = signer_of(local);
    let (_, frame) = seal_one(
        &tx,
        &provider,
        &signer,
        identity,
        key,
        &group.group_id,
        &outgoing.message,
        now,
    )?;
    check_outbox_cap(&tx, outgoing.wire.len() as u64, frame.len() as u64)?;
    let count = outgoing.reencrypts + 1;
    let capped = count >= REENCRYPT_CAP;
    tx.execute(
        // A given-up message is parked (2) so it is never due; otherwise it is due right away.
        "UPDATE outbox SET wire=?5,reencrypts=?6,dispatch_state=?7,next_retry_at_ms=?8 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
        params![
            key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice(),
            frame, integer(count)?, if capped { 2 } else { 0 }, integer(now)?
        ],
    )
    .map_err(sql)?;
    if capped {
        tx.execute(
            "UPDATE message SET outcome=3,failure_code='InvalidMessage' WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
            params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice()],
        )
        .map_err(sql)?;
        let revision = bump(&tx)?;
        tx.execute(
            "UPDATE conversation SET change_revision=?2 WHERE id=?1",
            params![key.conversation.0.as_slice(), integer(revision)?],
        )
        .map_err(sql)?;
    }
    provider.storage().flush(&tx, key.conversation.0)?;
    finish(tx, fault)?;
    Ok(MessageDbReply::Done)
}

/// The user's `Retry` of a message that was given up on after too many re-encryptions: the
/// count starts over and the sealed ciphertext it still holds becomes sendable again.
pub(super) fn revive_capped(c: &mut Connection, key: MessageKey, fault: Fault) -> R<()> {
    let tx = c.transaction().map_err(sql)?;
    let changed = tx
        .execute(
            "UPDATE message SET outcome=0,failure_code=NULL WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND outcome=3 AND cancel_requested=0 AND delivered_at_ms IS NULL AND EXISTS(SELECT 1 FROM outbox o WHERE o.conversation=message.conversation AND o.sender_peer=message.sender_peer AND o.sender_install=message.sender_install AND o.msg_id=message.msg_id AND o.codec=2 AND o.reencrypts>=3)",
            params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice()],
        )
        .map_err(sql)?;
    if changed > 0 {
        tx.execute(
            "UPDATE outbox SET reencrypts=0,dispatch_state=0,next_retry_at_ms=0 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
            params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice()],
        )
        .map_err(sql)?;
        let revision = bump(&tx)?;
        tx.execute(
            "UPDATE conversation SET change_revision=?2 WHERE id=?1",
            params![key.conversation.0.as_slice(), integer(revision)?],
        )
        .map_err(sql)?;
    }
    finish(tx, fault)
}

pub(super) fn mark_waiting(
    c: &mut Connection,
    identity: Identity,
    key: MessageKey,
    waiting: bool,
    fault: Fault,
) -> R<MessageDbReply> {
    check_own(identity, key)?;
    let tx = c.transaction().map_err(sql)?;
    tx.execute(
        "UPDATE outbox SET waiting_peer_update=?5 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
        params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice(), waiting],
    )
    .map_err(sql)?;
    finish(tx, fault)?;
    Ok(MessageDbReply::Done)
}

/// Whether a received message came over MLS (its payload hash is a remembered MLS ciphertext
/// hash of its sender), so its receipts go back over MLS too. Messages received from an M4
/// peer (codec 1) keep the M4 kind 16 `Receipt`.
pub(super) fn received_over_mls(c: &Connection, key: MessageKey) -> R<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM message m JOIN processed_ct p ON p.codec=2 AND p.sender_peer=m.sender_peer AND p.sender_install=m.sender_install AND p.hash=m.payload_hash WHERE m.conversation=?1 AND m.sender_peer=?2 AND m.sender_install=?3 AND m.msg_id=?4 AND m.outgoing=0)",
        params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice()],
        |r| r.get(0),
    )
    .map_err(sql)
}

/// The `Prepare` of a receipt job for a message received over MLS: the receipt (`Read` after
/// `mark_read`) is sealed as an MLS `Content::Receipt` for the message, in the same
/// transaction that moves the job's retry time, and sent as a kind 17 `App` frame. Each attempt
/// seals afresh (a receipt has no outbox row to keep a ciphertext in); the receiver applies a
/// receipt idempotently. The returned `ReceiptFrame` names the job the `Ack` settles.
#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_receipt(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    key: MessageKey,
    receiver: InstallationId,
    wall: u64,
    now: u64,
    fault: Fault,
) -> R<MessageDbReply> {
    target(c, identity, key, true)?;
    if receiver != key.installation {
        return Err(invalid());
    }
    let tx = c.transaction().map_err(sql)?;
    let kind: u64 = tx
        .query_row(
            "SELECT kind FROM receipt_job WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND recipient_install=?5 ORDER BY kind DESC LIMIT 1",
            params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice(), identity.installation.0.as_slice()],
            |r| uint(r, 0),
        )
        .optional()
        .map_err(sql)?
        .ok_or_else(missing)?;
    let receipt = receipt_for(
        &tx,
        key,
        identity.installation,
        if kind == 2 {
            ReceiptKind::Read
        } else {
            ReceiptKind::Delivered
        },
    )?;
    let Some(group) = group_row(&tx, key.conversation)? else {
        return Ok(MessageDbReply::NeedGroup(key.conversation));
    };
    let (lamport, prev): (u64, Vec<u8>) = tx
        .query_row(
            "SELECT lamport_max,local_prev_hash FROM conversation WHERE id=?1",
            params![key.conversation.0.as_slice()],
            |r| Ok((uint(r, 0)?, r.get(1)?)),
        )
        .map_err(sql)?;
    schedule_retry(&tx, key, true, now)?;
    // A receipt is not a message of its own: an id derived from what it acknowledges.
    let mut id = Sha256::new();
    id.update(b"ChatProject/m5/receipt-id/v1");
    id.update(key.conversation.0);
    id.update(key.sender.0);
    id.update(key.installation.0);
    id.update(key.id.0);
    id.update([kind as u8]);
    id.update(wall.to_be_bytes());
    let id: [u8; 32] = id.finalize().into();
    let message = ChatMessage {
        msg_id: MessageId(id[..16].try_into().map_err(|_| invalid())?),
        lamport,
        prev_hash: prev.try_into().map_err(|_| invalid())?,
        sent_at_ms: wall,
        content: Content::Receipt {
            kind: receipt.kind,
            targets: vec![key.id],
        },
    };
    let provider = MlsProvider::new(KvStore::load(&tx, key.conversation.0)?);
    let (signer, _) = signer_of(local);
    let (_, frame) = seal_one(
        &tx,
        &provider,
        &signer,
        identity,
        key,
        &group.group_id,
        &message,
        wall,
    )?;
    provider.storage().flush(&tx, key.conversation.0)?;
    finish(tx, fault)?;
    Ok(MessageDbReply::Prepared(
        zeroize::Zeroizing::new(frame),
        Some(receipt),
    ))
}

/// The synchronous `Ack` of a sent `App` frame: the recipient stored the message (spec: the
/// same meaning as an M4 `Receipt`), so it is `Delivered` and leaves the outbox. The recipient
/// must be one this message was prepared for. Never undoes `Read`.
pub(super) fn acked(
    c: &mut Connection,
    identity: Identity,
    key: MessageKey,
    recipient: InstallationId,
    now: u64,
    fault: Fault,
) -> R<MessageDbReply> {
    check_own(identity, key)?;
    let tx = c.transaction().map_err(sql)?;
    let message = find(&tx, key)?.ok_or_else(missing)?;
    let raw: Vec<u8> = tx
        .query_row(
            "SELECT attempted_receivers FROM message WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
            params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if !message.outgoing || !super::store::receivers(&raw)?.contains(&recipient) {
        return Err(invalid());
    }
    let changed = tx
        .execute(
            "UPDATE message SET delivered_at_ms=?5 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND delivered_at_ms IS NULL",
            params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice(), integer(now)?],
        )
        .map_err(sql)?;
    tx.execute(
        "DELETE FROM outbox WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
        params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice()],
    )
    .map_err(sql)?;
    if changed > 0 {
        let revision = bump(&tx)?;
        tx.execute(
            "UPDATE conversation SET change_revision=?2 WHERE id=?1",
            params![key.conversation.0.as_slice(), integer(revision)?],
        )
        .map_err(sql)?;
    }
    finish(tx, fault)?;
    Ok(MessageDbReply::Done)
}

/// Test-hook builds only: the M4-peer fixture (a core that speaks only kind 16) puts the
/// canonical M4 text frame of a row awaiting encryption into its `wire`, so its `Prepare` sends
/// that plaintext frame the way a released M4 app does. Production builds have no such path.
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn m4_encode(
    c: &mut Connection,
    identity: Identity,
    key: MessageKey,
) -> R<MessageDbReply> {
    check_own(identity, key)?;
    let Some(message) = find(c, key)? else {
        return Err(missing());
    };
    let prev: Vec<u8> = c
        .query_row(
            "SELECT prev_hash FROM message WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
            params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    let (wire, _) = super::wire::TextFrame {
        key,
        lamport: message.lamport,
        prev_hash: prev.try_into().map_err(|_| invalid())?,
        sent_at_ms: message.sent_at_ms,
        body: message.body,
    }
    .encode();
    c.execute(
        "UPDATE outbox SET wire=?5 WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND codec=3 AND wire=X''",
        params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice(), wire.as_slice()],
    )
    .map_err(sql)?;
    Ok(MessageDbReply::Done)
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
thread_local! {
    /// How often the MLS engine's `open` ran on this (the storage writer) thread.
    static OPEN_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}
fn open_counted(
    p: &MlsProvider,
    group_id: &[u8],
    ciphertext: &[u8],
) -> Result<Opened, OpenFailure> {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    OPEN_CALLS.with(|n| n.set(n.get() + 1));
    group::open(p, group_id, ciphertext)
}

fn decrypt_failed(class: u8, epoch: u64) -> MessageDbReply {
    MessageDbReply::Opened(MlsReply::DecryptFailed { class, epoch })
}

/// Opens one received application ciphertext and stores the message it carries, in one
/// transaction. `sender_peer`/`sender_install` are what the authenticated transport says; the
/// MLS credential must agree.
///
/// * the device's own ciphertext coming back, or one already in `processed_ct`, is acknowledged
///   without opening (a replay would only fail with secret reuse);
/// * a ciphertext that cannot be opened changes nothing and answers `DecryptFailed`;
/// * an undecodable plaintext or unknown content still advances the ratchet and is
///   acknowledged, so its sender stops retrying;
/// * a `Receipt` is applied to this device's sent messages and acknowledged;
/// * a text M4 would refuse advances the ratchet but stores nothing and is `Rejected` (the
///   sender gives the message up, as for an M4 `Reject`);
/// * the same message id again (a re-encrypted resend) stores nothing new.
#[allow(clippy::too_many_arguments)]
pub(super) fn open_app(
    c: &mut Connection,
    identity: Identity,
    sender_peer: PeerId,
    sender_install: InstallationId,
    conversation: ConversationId,
    ciphertext: Vec<u8>,
    now: u64,
    fault: Fault,
) -> R<MessageDbReply> {
    let ct_hash: [u8; 32] = Sha256::digest(&ciphertext).into();
    let ack = MessageDbReply::Opened(MlsReply::Ack { ct_hash });
    if sender_install == identity.installation {
        return Ok(ack);
    }
    if super::store::conversation_id(identity.peer, sender_peer)? != conversation {
        return Err(invalid());
    }
    let now = integer(now)?;
    let tx = c.transaction().map_err(sql)?;
    let seen: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM processed_ct WHERE codec=2 AND sender_peer=?1 AND sender_install=?2 AND hash=?3)",
            params![sender_peer.0.as_slice(), sender_install.0.as_slice(), ct_hash.as_slice()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if seen {
        return Ok(ack);
    }
    let Some(group) = group_row(&tx, conversation)? else {
        return Ok(decrypt_failed(group::CLASS_NO_GROUP, 0));
    };
    let provider = MlsProvider::new(KvStore::load(&tx, conversation.0)?);
    let opened = match open_counted(&provider, &group.group_id, &ciphertext) {
        Ok(opened) => opened,
        Err(failure) => return Ok(decrypt_failed(failure.class, group.epoch)),
    };
    let message = match opened {
        Opened::Application { sender, message } => {
            if sender != sender_install {
                return Err(invalid());
            }
            Some(message)
        }
        Opened::Undecodable { sender } => {
            if sender != sender_install {
                return Err(invalid());
            }
            None
        }
        // A Commit is applied through the group protocol (with its members vetted), never here.
        Opened::Commit { .. } => return Ok(decrypt_failed(group::CLASS_OTHER, group.epoch)),
    };
    let mut stored = Stored::Yes;
    if let Some(message) = message {
        stored = match &message.content {
            Content::Text { body } => store_text(
                &tx,
                sender_peer,
                sender_install,
                conversation,
                &message,
                body,
                &ct_hash,
                now,
            )?,
            Content::Receipt { kind, targets } => {
                apply_receipt(&tx, identity, conversation, *kind, targets, now)?;
                Stored::Yes
            }
            // Content this version does not know is dropped (spec 5.1 `Custom` fallback).
            Content::Custom { .. } => Stored::Yes,
        };
    }
    tx.execute(
        "INSERT INTO processed_ct VALUES(2,?1,?2,?3,?4)",
        params![
            sender_peer.0.as_slice(),
            sender_install.0.as_slice(),
            ct_hash.as_slice(),
            now
        ],
    )
    .map_err(sql)?;
    tx.execute(
        "UPDATE mls_conversation SET peer_mls=1 WHERE conversation=?1 AND peer_mls=0",
        params![conversation.0.as_slice()],
    )
    .map_err(sql)?;
    provider.storage().flush(&tx, conversation.0)?;
    finish(tx, fault)?;
    Ok(match stored {
        Stored::Yes => ack,
        Stored::Rejected => MessageDbReply::Rejected,
    })
}

/// Whether a decrypted application message was taken (stored, already stored, or nothing to
/// store) or refused the way M4 refuses a text frame.
enum Stored {
    Yes,
    Rejected,
}

/// Applies a received MLS `Receipt` to this device's own sent messages (M4 `apply_receipt`
/// rules): `Read` implies delivered, neither is ever undone, and a delivered message leaves the
/// outbox. Targets that are not this device's sent messages are ignored.
fn apply_receipt(
    tx: &Transaction<'_>,
    identity: Identity,
    conversation: ConversationId,
    kind: ReceiptKind,
    targets: &[MessageId],
    now: i64,
) -> R<()> {
    let read = kind == ReceiptKind::Read;
    let mut changed = 0;
    for id in targets {
        let key = params![
            conversation.0.as_slice(),
            identity.peer.0.as_slice(),
            identity.installation.0.as_slice(),
            id.0.as_slice()
        ];
        let outgoing: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM message WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND outgoing=1)",
                key,
                |r| r.get(0),
            )
            .map_err(sql)?;
        if !outgoing {
            continue;
        }
        changed += tx
            .execute(
                "UPDATE message SET delivered_at_ms=coalesce(delivered_at_ms,?5),read_at_ms=CASE WHEN ?6 THEN coalesce(read_at_ms,?5) ELSE read_at_ms END WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND (delivered_at_ms IS NULL OR (?6 AND read_at_ms IS NULL))",
                params![
                    conversation.0.as_slice(),
                    identity.peer.0.as_slice(),
                    identity.installation.0.as_slice(),
                    id.0.as_slice(),
                    now,
                    read
                ],
            )
            .map_err(sql)?;
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
            params![conversation.0.as_slice(), integer(revision)?],
        )
        .map_err(sql)?;
    }
    Ok(())
}

/// Stores a received text the way M4 `receive` does (same sender/conversation rules, idempotent
/// on the logical key and body). What M4 would refuse (an empty or too long body, an
/// out-of-range lamport or time, a different body or position for a known id) is `Rejected`:
/// nothing is stored, but the ratchet still moves, and the sender is told it failed for good.
#[allow(clippy::too_many_arguments)]
fn store_text(
    tx: &Transaction<'_>,
    sender_peer: PeerId,
    sender_install: InstallationId,
    conversation: ConversationId,
    message: &ChatMessage,
    body: &str,
    ct_hash: &[u8; 32],
    now: i64,
) -> R<Stored> {
    let key = MessageKey {
        conversation,
        sender: sender_peer,
        installation: sender_install,
        id: message.msg_id,
    };
    let (lamport, sent_at) = (
        i64::try_from(message.lamport),
        i64::try_from(message.sent_at_ms),
    );
    let (Ok(lamport), Ok(sent_at)) = (lamport, sent_at) else {
        return Ok(Stored::Rejected);
    };
    if body.is_empty() || body.len() > TEXT_LIMIT {
        return Ok(Stored::Rejected);
    }
    if let Some(existing) = find(tx, key)? {
        if existing.outgoing || existing.body != *body {
            return Ok(Stored::Rejected);
        }
        return Ok(Stored::Yes);
    }
    let maximum: Option<u64> = tx
        .query_row(
            "SELECT lamport_max FROM conversation WHERE id=?1",
            params![conversation.0.as_slice()],
            |r| uint(r, 0),
        )
        .optional()
        .map_err(sql)?;
    if message.lamport
        > maximum
            .unwrap_or(0)
            .saturating_add(1_000_000)
            .min(i64::MAX as u64)
    {
        return Ok(Stored::Rejected);
    }
    let collision: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM message WHERE conversation=?1 AND lamport=?2 AND sender_install=?3 AND msg_id=?4)",
            params![conversation.0.as_slice(), lamport, sender_install.0.as_slice(), key.id.0.as_slice()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if collision {
        return Ok(Stored::Rejected);
    }
    ensure_conversation(tx, conversation, sender_peer)?;
    tx.execute(
        "INSERT INTO message VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,0,0,?9,NULL,0,0,NULL,NULL,X'')",
        params![
            conversation.0.as_slice(),
            sender_peer.0.as_slice(),
            sender_install.0.as_slice(),
            key.id.0.as_slice(),
            lamport,
            message.prev_hash.as_slice(),
            ct_hash.as_slice(),
            sent_at,
            now,
            body
        ],
    )
    .map_err(sql)?;
    let revision = bump(tx)?;
    tx.execute(
        "UPDATE conversation SET lamport_max=max(lamport_max,?2),unread=unread+1,change_revision=?3 WHERE id=?1",
        params![conversation.0.as_slice(), lamport, integer(revision)?],
    )
    .map_err(sql)?;
    Ok(Stored::Yes)
}

// ---- test-hooks fixtures -------------------------------------------------------------------

/// Outbox row of one message, for fixture assertions.
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub struct MlsOutbox {
    pub codec: Option<i64>,
    pub wire: Vec<u8>,
    pub reencrypts: u64,
    pub waiting_peer_update: bool,
    pub dispatch_state: u64,
}
/// MLS state of one conversation, for fixture assertions.
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub struct MlsState {
    /// SHA-256 over every `mls_kv` row (key and value), in key order.
    pub kv_digest: [u8; 32],
    pub kv_rows: u64,
    /// `processed_ct` rows of codec 2.
    pub processed_mls: u64,
    /// Calls of the MLS `open` on this database's writer thread.
    pub open_calls: u64,
    pub group_epoch: Option<u64>,
    pub peer_mls: Option<bool>,
    pub group_id: Option<Vec<u8>>,
    pub first_commit_hash: Option<[u8; 32]>,
    pub last_commit_hash: Option<[u8; 32]>,
    pub own_commit_unacked: Option<bool>,
    pub rejoin_count: Option<u32>,
    /// Installations holding a leaf in the stored group.
    pub members: Vec<InstallationId>,
    /// Equal on two devices only when they share the same branch of the same epoch.
    pub epoch_authenticator: Option<Vec<u8>>,
    /// Queued deliveries of the conversation: recipient, kind (1 Commit, 2 Welcome), epoch.
    pub deliveries: Vec<(InstallationId, u8, u64)>,
    /// Held Commits/Welcomes of the conversation.
    pub holds: u64,
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_outbox_cap(cap: Option<u64>) {
    OUTBOX_CAP.with(|c| c.set(cap));
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_promote(c: &Connection, key: MessageKey) -> R<()> {
    let changed = c
        .execute(
            // Rows are enqueued awaiting encryption already; this only clears a test wire.
            "UPDATE outbox SET codec=3,wire=X'' WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND codec=3",
            params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice()],
        )
        .map_err(sql)?;
    if changed == 1 { Ok(()) } else { Err(missing()) }
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_outbox(c: &Connection, key: MessageKey) -> R<MlsOutbox> {
    let row = c
        .query_row(
            "SELECT codec,wire,reencrypts,waiting_peer_update,dispatch_state FROM outbox WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4",
            params![key.conversation.0.as_slice(), key.sender.0.as_slice(), key.installation.0.as_slice(), key.id.0.as_slice()],
            |r| Ok(MlsOutbox {
                codec: Some(r.get(0)?),
                wire: r.get(1)?,
                reencrypts: uint(r, 2)?,
                waiting_peer_update: r.get(3)?,
                dispatch_state: uint(r, 4)?,
            }),
        )
        .optional()
        .map_err(sql)?;
    Ok(row.unwrap_or(MlsOutbox {
        codec: None,
        wire: Vec::new(),
        reencrypts: 0,
        waiting_peer_update: false,
        dispatch_state: 0,
    }))
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_state(c: &Connection, conversation: ConversationId) -> R<MlsState> {
    let mut digest = Sha256::new();
    let mut rows = 0u64;
    {
        let mut q = c
            .prepare("SELECT key,value FROM mls_kv ORDER BY key")
            .map_err(sql)?;
        let mut cursor = q.query([]).map_err(sql)?;
        while let Some(row) = cursor.next().map_err(sql)? {
            let (key, value): (Vec<u8>, Vec<u8>) =
                (row.get(0).map_err(sql)?, row.get(1).map_err(sql)?);
            digest.update((key.len() as u64).to_be_bytes());
            digest.update(&key);
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(&value);
            rows += 1;
        }
    }
    let processed_mls: u64 = c
        .query_row("SELECT count(*) FROM processed_ct WHERE codec=2", [], |r| {
            uint(r, 0)
        })
        .map_err(sql)?;
    type Group = (u64, bool, Vec<u8>, [u8; 32], Option<Vec<u8>>, bool, u64);
    let group: Option<Group> = c
        .query_row(
            "SELECT epoch,peer_mls,group_id,first_commit_hash,last_commit_hash,own_commit_unacked,rejoin_count FROM mls_conversation WHERE conversation=?1",
            params![conversation.0.as_slice()],
            |r| {
                Ok((
                    uint(r, 0)?,
                    r.get(1)?,
                    r.get(2)?,
                    bytes(r, 3)?,
                    r.get(4)?,
                    r.get(5)?,
                    uint(r, 6)?,
                ))
            },
        )
        .optional()
        .map_err(sql)?;
    let (members, epoch_authenticator) = match &group {
        Some(g) => {
            let provider = MlsProvider::new(KvStore::load(c, conversation.0)?);
            (
                crate::mls::membership::group_installations(&provider, &g.2)?,
                Some(crate::mls::membership::epoch_authenticator(
                    &provider, &g.2,
                )?),
            )
        }
        None => (Vec::new(), None),
    };
    let deliveries = c
        .prepare("SELECT recipient_install,kind,epoch FROM mls_delivery WHERE conversation=?1 ORDER BY recipient_install,epoch,kind")
        .map_err(sql)?
        .query_map(params![conversation.0.as_slice()], |r| {
            Ok((InstallationId(bytes(r, 0)?), r.get(1)?, uint(r, 2)?))
        })
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    let holds = c
        .query_row(
            "SELECT count(*) FROM mls_hold WHERE conversation=?1",
            params![conversation.0.as_slice()],
            |r| uint(r, 0),
        )
        .map_err(sql)?;
    Ok(MlsState {
        kv_digest: digest.finalize().into(),
        kv_rows: rows,
        processed_mls,
        open_calls: OPEN_CALLS.with(|n| n.get()),
        group_epoch: group.as_ref().map(|g| g.0),
        peer_mls: group.as_ref().map(|g| g.1),
        group_id: group.as_ref().map(|g| g.2.clone()),
        first_commit_hash: group.as_ref().map(|g| g.3),
        last_commit_hash: group
            .as_ref()
            .and_then(|g| g.4.clone())
            .and_then(|h| h.try_into().ok()),
        own_commit_unacked: group.as_ref().map(|g| g.5),
        rejoin_count: group.as_ref().map(|g| g.6 as u32),
        members,
        epoch_authenticator,
        deliveries,
        holds,
    })
}

/// Test-only: lets the next own Commit be made although the last one is unacknowledged.
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_clear_commit_flag(c: &Connection, conversation: ConversationId) -> R<()> {
    c.execute(
        "UPDATE mls_conversation SET own_commit_unacked=0 WHERE conversation=?1",
        params![conversation.0.as_slice()],
    )
    .map_err(sql)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Secret32, derive_device};
    use crate::message::store::conversation_id;
    use crate::mls::identity::{MlsCredential, mls_signer};
    use openmls::prelude::{GroupId, MlsGroup, tls_codec::Serialize as _};

    fn database() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        c.execute_batch(include_str!("../storage/schema.sql"))
            .unwrap();
        c
    }
    fn rows(c: &Connection) -> Vec<(Vec<u8>, Vec<u8>)> {
        c.prepare("SELECT key,value FROM mls_kv ORDER BY key")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
    fn count(c: &Connection, sql: &str) -> i64 {
        c.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    /// An authenticated application message whose plaintext is not a `ChatMessage` (Ruling 8):
    /// nothing is stored, but the ratchet moved and the sender is told it arrived, and a repeat
    /// is recognised before it can fail as a replay.
    #[test]
    fn undecodable_plaintext_advances_the_ratchet_and_is_acknowledged() {
        let (a_device, b_device) = (
            derive_device(Secret32::from([1; 32])).unwrap(),
            derive_device(Secret32::from([2; 32])).unwrap(),
        );
        let (a_peer, b_peer) = (PeerId([1; 32]), PeerId([2; 32]));
        let conversation = conversation_id(b_peer, a_peer).unwrap();
        let (a_signer, a_credential) = (
            mls_signer(&a_device),
            MlsCredential::issue(a_peer, &a_device),
        );
        let (b_signer, b_credential) = (
            mls_signer(&b_device),
            MlsCredential::issue(b_peer, &b_device),
        );
        let any = |_: &[u8], _: &[u8]| -> R<()> { Ok(()) };

        let a_connection = database();
        let mut b_connection = database();
        let a = MlsProvider::new(KvStore::load(&a_connection, conversation.0).unwrap());
        let b = MlsProvider::new(KvStore::load(&b_connection, conversation.0).unwrap());
        let key_package = group::new_key_package(&b, &b_signer, &b_credential).unwrap();
        let created = group::create_group(
            &a,
            &a_signer,
            &a_credential,
            conversation,
            &[key_package],
            &any,
        )
        .unwrap();
        let joined = group::join(&b, &created.welcome, &any).unwrap();
        {
            let tx = b_connection.transaction().unwrap();
            tx.execute(
                "INSERT INTO conversation VALUES(?1,?2,0,?3,0,0)",
                params![
                    conversation.0.as_slice(),
                    a_peer.0.as_slice(),
                    [0u8; 32].as_slice()
                ],
            )
            .unwrap();
            b.storage().flush(&tx, conversation.0).unwrap();
            tx.execute(
                "INSERT INTO mls_conversation(conversation,group_id,first_commit_hash,epoch,own_commit_unacked,rejoin_count,rejoin_after_ms,peer_mls) VALUES(?1,?2,?3,?4,0,0,0,1)",
                params![
                    conversation.0.as_slice(),
                    joined.group_id,
                    created.first_commit_hash.as_slice(),
                    joined.epoch as i64
                ],
            )
            .unwrap();
            tx.commit().unwrap();
        }

        let mut group = MlsGroup::load(a.storage(), &GroupId::from_slice(&created.group_id))
            .unwrap()
            .unwrap();
        let ciphertext = group
            .create_message(&a, &a_signer, b"this is not a chat message")
            .unwrap()
            .tls_serialize_detached()
            .unwrap();
        let before = rows(&b_connection);
        let identity = Identity {
            peer: b_peer,
            installation: b_device.installation_id(),
        };
        let ack = |c: &mut Connection| {
            open_app(
                c,
                identity,
                a_peer,
                a_device.installation_id(),
                conversation,
                ciphertext.clone(),
                5,
                Fault::default(),
            )
            .unwrap()
        };

        let reply = ack(&mut b_connection);
        assert!(matches!(
            reply,
            MessageDbReply::Opened(MlsReply::Ack { .. })
        ));
        assert_eq!(count(&b_connection, "SELECT count(*) FROM message"), 0);
        assert_eq!(
            count(
                &b_connection,
                "SELECT count(*) FROM processed_ct WHERE codec=2"
            ),
            1
        );
        let after = rows(&b_connection);
        assert_ne!(after, before, "the ratchet must have moved");

        let reply = ack(&mut b_connection);
        assert!(matches!(
            reply,
            MessageDbReply::Opened(MlsReply::Ack { .. })
        ));
        assert_eq!(rows(&b_connection), after);
    }
}
