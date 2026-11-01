//! MLS operations of the message store. Each one is a single SQL transaction that carries the
//! MLS state it changed (`mls_kv`, `mls_conversation`) together with the `message`, `outbox` and
//! `processed_ct` rows, so a crash or a rolled-back transaction can never leave a ratchet that
//! moved without the message it opened, or the reverse.
//!
//! A `KvStore` is loaded per operation and `flush`ed into the operation's transaction only on
//! success. `flush` clears its dirty set before the caller commits, so after any failure the
//! store (and its provider) is dropped; the next operation loads a fresh one.
use super::store::{
    Identity, MessageDbReply, bump, bytes, commit, conflict, ensure_conversation, find, integer,
    invalid, missing, uint,
};
use super::types::*;
use crate::mls::{
    MlsProvider,
    group::{self, OpenFailure, Opened},
    identity::{MlsCredential, mls_signer, verify_leaf},
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
type Fault = Option<crate::storage::worker::CommitFault>;
#[cfg(not(all(feature = "test-hooks", debug_assertions)))]
type Fault = ();
fn finish(tx: Transaction<'_>, fault: Fault) -> R<()> {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    return commit(tx, fault);
    #[cfg(not(all(feature = "test-hooks", debug_assertions)))]
    {
        let () = fault;
        commit(tx)
    }
}

/// Rows of group-independent labels (KeyPackages, signature keys) live in this scope.
const GLOBAL: [u8; 32] = [0; 32];
/// Largest complete frame (M4 transport limit).
const MAX_FRAME: usize = 61_440;
/// A message that was re-encrypted this many times is given up on.
const REENCRYPT_CAP: u64 = 3;
const TEXT_LIMIT: usize = 16_384;
/// The outbox holds at most this many wire bytes (M4 rule), sealed frames included.
const OUTBOX_BYTES: u64 = 128 * 1024 * 1024;

#[cfg(all(feature = "test-hooks", debug_assertions))]
thread_local! {
    /// Lowered outbox byte cap for tests, per storage writer thread.
    static OUTBOX_CAP: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}
fn outbox_cap() -> u64 {
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

/// Changes to a conversation's group. Task 7 drives these from the group protocol; the
/// variants it needs beyond the ones implemented here answer `NotAvailable` until then.
pub(crate) enum GroupOp {
    /// A fresh KeyPackage for this installation (its private keys go to the global scope).
    NewKeyPackage,
    /// Creates the group for the conversation and adds `members` (peer KeyPackages).
    Create {
        peer: PeerId,
        members: Vec<Vec<u8>>,
        now: u64,
    },
    /// Joins through a Welcome from `peer`.
    Join {
        peer: PeerId,
        welcome: Vec<u8>,
        first_commit_hash: [u8; 32],
        now: u64,
    },
    /// Not available until Task 7.
    ApplyCommit { commit: Vec<u8>, now: u64 },
    /// Not available until Task 7.
    RestorePrevious,
    /// Forgets the group: its `mls_conversation` row and every `mls_kv` row of the conversation.
    Drop,
}

pub(crate) enum GroupReply {
    KeyPackage(Vec<u8>),
    Created {
        group_id: Vec<u8>,
        first_commit_hash: [u8; 32],
        commit: Vec<u8>,
        welcome: Vec<u8>,
    },
    Joined {
        epoch: u64,
    },
    Done,
}

fn not_available() -> CoreError {
    CoreError::new(ErrorCode::NotAvailable, "mls_operation_not_available")
}
fn group_exists() -> CoreError {
    CoreError::new(ErrorCode::Busy, "mls_group_exists")
}
fn auth_failed() -> CoreError {
    CoreError::new(ErrorCode::AuthFailed, "mls_leaf_not_trusted")
}

struct GroupRow {
    group_id: Vec<u8>,
    epoch: u64,
}
fn group_row(c: &Connection, conversation: ConversationId) -> R<Option<GroupRow>> {
    c.query_row(
        "SELECT group_id,epoch FROM mls_conversation WHERE conversation=?1",
        params![conversation.0.as_slice()],
        |r| {
            Ok(GroupRow {
                group_id: r.get(0)?,
                epoch: uint(r, 1)?,
            })
        },
    )
    .optional()
    .map_err(super::store::sql_error)
}
fn sql(e: rusqlite::Error) -> CoreError {
    super::store::sql_error(e)
}

fn signer_of(local: &LocalAccount) -> (SignatureKeyPair, MlsCredential) {
    (
        mls_signer(&local.device),
        MlsCredential::issue(local.keys.peer_id(), &local.device),
    )
}

/// Checks an MLS leaf against the account record this device holds for the leaf's peer: its own
/// accepted record, or the cached record of the conversation's peer. Anyone else is refused.
fn leaf_verifier<'a>(local: &'a LocalAccount, peer: PeerId) -> impl Fn(&[u8], &[u8]) -> R<()> + 'a {
    move |credential, signature_key| {
        let decoded = MlsCredential::decode(credential).map_err(|_| auth_failed())?;
        let record = if decoded.peer == local.keys.peer_id() {
            local.records.accepted.as_ref().map(|r| &r.record)
        } else if decoded.peer == peer {
            local
                .remote
                .iter()
                .find(|r| r.peer == peer)
                .map(|r| &r.accepted.record)
        } else {
            None
        }
        .ok_or_else(auth_failed)?;
        verify_leaf(credential, signature_key, decoded.peer, record).map(|_| ())
    }
}

fn check_own(identity: Identity, key: MessageKey) -> R<()> {
    if key.sender != identity.peer || key.installation != identity.installation {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn group_op(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    conversation: ConversationId,
    op: GroupOp,
    fault: Fault,
) -> R<MessageDbReply> {
    let (signer, credential) = signer_of(local);
    match op {
        GroupOp::NewKeyPackage => {
            let tx = c.transaction().map_err(sql)?;
            let provider = MlsProvider::new(KvStore::load(&tx, GLOBAL)?);
            let key_package = group::new_key_package(&provider, &signer, &credential)?;
            provider.storage().flush(&tx, GLOBAL)?;
            finish(tx, fault)?;
            Ok(MessageDbReply::Group(GroupReply::KeyPackage(key_package)))
        }
        GroupOp::Create {
            peer,
            members,
            now: _,
        } => {
            if super::store::conversation_id(identity.peer, peer)? != conversation {
                return Err(invalid());
            }
            let tx = c.transaction().map_err(sql)?;
            if group_row(&tx, conversation)?.is_some() {
                return Err(group_exists());
            }
            ensure_conversation(&tx, conversation, peer)?;
            let provider = MlsProvider::new(KvStore::load(&tx, conversation.0)?);
            let verify = leaf_verifier(local, peer);
            let created = group::create_group(
                &provider,
                &signer,
                &credential,
                conversation,
                &members,
                &verify,
            )?;
            provider.storage().flush(&tx, conversation.0)?;
            // `create_group` merges exactly one Commit, so the new group is at epoch 1.
            insert_group_row(
                &tx,
                conversation,
                &created.group_id,
                created.first_commit_hash,
                1,
                false,
            )?;
            finish(tx, fault)?;
            Ok(MessageDbReply::Group(GroupReply::Created {
                group_id: created.group_id,
                first_commit_hash: created.first_commit_hash,
                commit: created.commit,
                welcome: created.welcome,
            }))
        }
        GroupOp::Join {
            peer,
            welcome,
            first_commit_hash,
            now: _,
        } => {
            if super::store::conversation_id(identity.peer, peer)? != conversation {
                return Err(invalid());
            }
            let tx = c.transaction().map_err(sql)?;
            if group_row(&tx, conversation)?.is_some() {
                return Err(group_exists());
            }
            ensure_conversation(&tx, conversation, peer)?;
            let provider = MlsProvider::new(KvStore::load(&tx, conversation.0)?);
            let verify = leaf_verifier(local, peer);
            // Joining consumes the KeyPackage in the in-memory store: on any error the provider
            // and transaction are dropped, so the stored KeyPackage stays usable.
            let joined = group::join(&provider, &welcome, &verify)?;
            provider.storage().flush(&tx, conversation.0)?;
            insert_group_row(
                &tx,
                conversation,
                &joined.group_id,
                first_commit_hash,
                joined.epoch,
                true,
            )?;
            finish(tx, fault)?;
            Ok(MessageDbReply::Group(GroupReply::Joined {
                epoch: joined.epoch,
            }))
        }
        GroupOp::Drop => {
            let tx = c.transaction().map_err(sql)?;
            tx.execute(
                "DELETE FROM mls_kv WHERE scope=?1",
                params![conversation.0.as_slice()],
            )
            .map_err(sql)?;
            tx.execute(
                "DELETE FROM mls_conversation WHERE conversation=?1",
                params![conversation.0.as_slice()],
            )
            .map_err(sql)?;
            finish(tx, fault)?;
            Ok(MessageDbReply::Group(GroupReply::Done))
        }
        // Task 7 (membership protocol).
        GroupOp::ApplyCommit { .. } | GroupOp::RestorePrevious => Err(not_available()),
    }
}

fn insert_group_row(
    tx: &Transaction<'_>,
    conversation: ConversationId,
    group_id: &[u8],
    first_commit_hash: [u8; 32],
    epoch: u64,
    peer_mls: bool,
) -> R<()> {
    tx.execute(
        "INSERT INTO mls_conversation(conversation,group_id,first_commit_hash,epoch,previous_epoch_snapshot,own_commit_unacked,rejoin_count,rejoin_after_ms,peer_mls) VALUES(?1,?2,?3,?4,NULL,0,0,0,?5)",
        params![
            conversation.0.as_slice(),
            group_id,
            first_commit_hash.as_slice(),
            integer(epoch)?,
            peer_mls
        ],
    )
    .map_err(sql)?;
    Ok(())
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
/// * a decrypted message that carries nothing storable (undecodable, not text, out of range)
///   still advances the ratchet and is acknowledged, so its sender stops retrying;
/// * the same message id again (a re-encrypted resend) stores nothing new.
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
    if let Some(message) = message {
        store_text(
            &tx,
            sender_peer,
            sender_install,
            conversation,
            &message,
            &ct_hash,
            now,
        )?;
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
    Ok(ack)
}

/// Stores a received text the way M4 `receive` does (same sender/conversation rules, idempotent
/// on the logical key and body, a different body is a conflict). Content that can never be stored
/// (not text, out of range) is skipped without error, so the caller still acknowledges it.
fn store_text(
    tx: &Transaction<'_>,
    sender_peer: PeerId,
    sender_install: InstallationId,
    conversation: ConversationId,
    message: &ChatMessage,
    ct_hash: &[u8; 32],
    now: i64,
) -> R<()> {
    let Content::Text { body } = &message.content else {
        // Receipts and other content are applied by the message protocol (Task 8).
        return Ok(());
    };
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
        return Ok(());
    };
    if body.is_empty() || body.len() > TEXT_LIMIT {
        return Ok(());
    }
    if let Some(existing) = find(tx, key)? {
        if existing.outgoing || existing.body != *body {
            return Err(conflict());
        }
        return Ok(());
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
        return Ok(());
    }
    let collision: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM message WHERE conversation=?1 AND lamport=?2 AND sender_install=?3 AND msg_id=?4)",
            params![conversation.0.as_slice(), lamport, sender_install.0.as_slice(), key.id.0.as_slice()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if collision {
        return Err(conflict());
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
    Ok(())
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
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_outbox_cap(cap: Option<u64>) {
    OUTBOX_CAP.with(|c| c.set(cap));
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(super) fn fixture_promote(c: &Connection, key: MessageKey) -> R<()> {
    let changed = c
        .execute(
            "UPDATE outbox SET codec=3,wire=X'' WHERE conversation=?1 AND sender_peer=?2 AND sender_install=?3 AND msg_id=?4 AND codec=1",
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
    let group = c
        .query_row(
            "SELECT epoch,peer_mls FROM mls_conversation WHERE conversation=?1",
            params![conversation.0.as_slice()],
            |r| Ok((uint(r, 0)?, r.get::<_, bool>(1)?)),
        )
        .optional()
        .map_err(sql)?;
    Ok(MlsState {
        kv_digest: digest.finalize().into(),
        kv_rows: rows,
        processed_mls,
        open_calls: OPEN_CALLS.with(|n| n.get()),
        group_epoch: group.map(|g| g.0),
        peer_mls: group.map(|g| g.1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Secret32, derive_device};
    use crate::message::store::conversation_id;
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
            insert_group_row(
                &tx,
                conversation,
                &joined.group_id,
                created.first_commit_hash,
                joined.epoch,
                true,
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
