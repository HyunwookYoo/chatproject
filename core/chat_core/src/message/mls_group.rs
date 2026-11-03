//! The group lifecycle half of the MLS store (spec 4.4-4.6): creating, joining and changing a
//! conversation's group, the per-installation Commit/Welcome delivery queue (`mls_delivery`) and
//! the holds for devices missing from a cached record (`mls_hold`). Like the rest of
//! `mls_store`, every operation is one SQL transaction carrying the MLS state it changed.
use super::mls_store::{Fault, GLOBAL, MAX_FRAME, NO_FAULT, finish};
use super::store::{Identity, MessageDbReply, bytes, ensure_conversation, integer, invalid, uint};
use super::types::*;
use crate::account::{
    IdentityRecord, InstallationId, PeerId, parse_stored_record, storage::schema::LocalAccount,
};
use crate::mls::{
    MlsProvider, group,
    identity::{MlsCredential, mls_signer, verify_leaf},
    kv::KvStore,
    membership::{self, ForkDecision},
    wire::MlsFrame,
};
use crate::{CoreError, ErrorCode};
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider as _;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::cmp::Ordering;

type R<T> = Result<T, CoreError>;

/// `mls_delivery.kind`: a Commit for a member that was in the group before it.
pub(crate) const DELIVER_COMMIT: u8 = 1;
/// `mls_delivery.kind`: a Welcome for a member the Commit added.
pub(crate) const DELIVER_WELCOME: u8 = 2;
/// Held Commits/Welcomes are capped per conversation and in total; the oldest go first.
const HOLDS_PER_CONVERSATION: i64 = 32;
const HOLDS_TOTAL: i64 = 1024;
const HOLD_COMMIT: u8 = 1;
const HOLD_WELCOME: u8 = 2;
const HOLD_JOIN_REQUEST: u8 = 3;
/// `[tag][first_commit_hash 32][sender peer 32][conversation peer 32]` before the frame body.
const HOLD_HEADER: usize = 97;

/// Changes to a conversation's group, each one SQL transaction (spec 4.4-4.6). `now` is the
/// scheduler time used for delivery retries; `wall` (and `now` in `Rejoin`) is wall-clock time
/// for the rejoin limit, which outlives the process.
pub(crate) enum GroupOp {
    /// A fresh KeyPackage for this installation (its private keys go to the global scope).
    NewKeyPackage,
    /// The group's epoch and first-Commit hash, if the conversation has a group.
    Info,
    /// Creates the group and adds `members` (peer KeyPackages); queues a Welcome for each.
    Create {
        peer: PeerId,
        members: Vec<Vec<u8>>,
        /// `(rejoin_count, rejoin_after_ms)` carried over from a group this device left.
        rejoin: Option<(u32, u64)>,
        now: u64,
    },
    /// Joins through a Welcome. With a group already present the smaller first-Commit hash
    /// wins (spec 4.4) and switching to it counts as a rejoin (rate limited, Ruling 15); a
    /// newer Welcome for the same group replaces the stale state, and a different Welcome for
    /// the epoch this device joined at means two members added it on competing branches: it
    /// rejoins.
    Join {
        peer: PeerId,
        sender_peer: PeerId,
        sender_install: InstallationId,
        welcome: Vec<u8>,
        first_commit_hash: [u8; 32],
        rejoin: Option<(u32, u64)>,
        /// SHA-256 of the Welcome this device joined the current group with, if known.
        joined_with: Option<[u8; 32]>,
        now: u64,
        wall: u64,
    },
    /// Whether a Welcome from this account's own device `sender_install` may bring in a
    /// conversation with someone who is not a contact (spec 4.4a). Read only: the Welcome is
    /// staged in memory and nothing is kept, so the KeyPackage stays usable for the real join.
    OwnWelcome {
        sender_install: InstallationId,
        welcome: Vec<u8>,
    },
    /// A `JoinRequest` from `sender_peer`'s installation named in `key_package`: an Add (and
    /// Remove of its old leaf) Commit. A device missing from the record is held and looked up.
    JoinRequest {
        peer: PeerId,
        sender_peer: PeerId,
        key_package: Vec<u8>,
        now: u64,
    },
    /// A received Commit leading to `epoch`: applied, held, or settled as a fork (spec 4.5).
    ApplyCommit {
        peer: PeerId,
        sender_peer: PeerId,
        epoch: u64,
        commit: Vec<u8>,
        now: u64,
    },
    /// What `diff_members` says the group needs, and whether an own Commit is unacknowledged.
    Plan { peer: PeerId },
    /// One own Commit adding `add` and removing `remove`; queues it for every other member
    /// and a Welcome for every added one.
    Commit {
        peer: PeerId,
        add: Vec<Vec<u8>>,
        remove: Vec<InstallationId>,
        now: u64,
    },
    /// One own Commit with no proposals, only a path update (C1): every member moves to a new
    /// epoch whose sender ratchets all start over. Refused (`Busy`) while an own Commit is
    /// unacknowledged, like any own Commit.
    Update { peer: PeerId, now: u64 },
    /// Leaves the group to rejoin it (spec 4.6), if the rate limit allows.
    Rejoin { now: u64 },
    /// Due Commit/Welcome deliveries to the given (reachable) recipients, the oldest epoch of
    /// each recipient first. Rows of other recipients are not even read, so any number of
    /// unreachable recipients cannot crowd out the reachable ones.
    DueDeliveries {
        now: u64,
        recipients: Vec<InstallationId>,
    },
    /// Every queued delivery of the conversation to `recipient`, in the order it must get them
    /// (whatever their retry time): sent ahead of an application message to that recipient so
    /// it holds the group state the message needs.
    Pending { recipient: InstallationId },
    /// The result of one delivery attempt.
    Delivered {
        recipient: InstallationId,
        kind: u8,
        epoch: u64,
        outcome: Delivery,
        now: u64,
    },
    /// Accounts whose record lacked a device that a held Commit/Welcome needs.
    HoldPeers,
    /// Processes the holds waiting on `lookup`'s record again; with `drop_unknown` (a fresh
    /// lookup found no newer list) the ones still unknown are dropped, and a dropped Commit or
    /// Welcome means this device cannot follow the group any more: it rejoins (spec 4.6).
    Reprocess {
        lookup: PeerId,
        drop_unknown: bool,
        now: u64,
        wall: u64,
    },
    /// Every conversation with a group, and its peer.
    Groups,
    /// Whether the conversation's peer account ever spoke MLS here (`mls_peer`); with `mark`,
    /// records that it just did (an accepted Commit or Welcome from it). With `peer`, a leaf of
    /// one of that account's devices in the conversation's group counts too (evidence for the
    /// M4-refusal decision, never for the downgrade guard).
    PeerSpokeMls { mark: bool, peer: Option<PeerId> },
    /// Forgets the group: its row, `mls_kv` rows, deliveries and holds.
    Drop,
}

impl GroupOp {
    /// Whether the operation can write (and so takes an injected commit fault in tests); the
    /// read-only ones run on every scheduler tick.
    pub(crate) fn writes(&self) -> bool {
        !matches!(
            self,
            Self::Info
                | Self::OwnWelcome { .. }
                | Self::Plan { .. }
                | Self::DueDeliveries { .. }
                | Self::Pending { .. }
                | Self::HoldPeers
                | Self::Groups
                | Self::PeerSpokeMls { mark: false, .. }
        )
    }
}

/// How a delivery attempt ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// The recipient acknowledged it.
    Acked,
    /// The recipient settled it otherwise (refused, or a fork it resolved); never resent.
    Settled,
    /// Try again after the back-off.
    Retry,
}

/// What became of a received Commit or Welcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Applied now or before, or a Welcome for a group this device keeps.
    Ack,
    /// A Welcome was joined.
    Joined,
    /// A same-epoch fork was lost: the previous epoch was restored and this Commit applied.
    Adopted,
    /// A leaf is not in `PeerId`'s record yet; held until the record is looked up again.
    Held(PeerId),
    /// Not applied; `class` is the `DecryptFailed` class and `epoch` this device's epoch.
    Failed { class: u8, epoch: u64 },
    /// This device cannot follow the group any more: rejoin (spec 4.6).
    Rejoin { epoch: u64 },
    /// Not now (a group switch the rejoin limit does not allow yet): the sender tries again.
    Later,
    /// The Commit removed this device; the group is gone here.
    Removed,
}

pub(crate) struct GroupInfo {
    pub epoch: u64,
    pub first_commit_hash: [u8; 32],
}

pub(crate) struct Plan {
    /// An own Commit has no acknowledgement yet; no other Commit may be made.
    pub unacked: bool,
    pub add: Vec<InstallationId>,
    pub remove: Vec<InstallationId>,
}

pub(crate) struct DueDelivery {
    pub conversation: ConversationId,
    pub recipient: InstallationId,
    pub kind: u8,
    pub epoch: u64,
    /// The complete kind 17 frame.
    pub body: Vec<u8>,
}

pub(crate) enum Committed {
    Done {
        epoch: u64,
    },
    /// No group, or an own Commit is still unacknowledged.
    Busy,
    Held(PeerId),
}

/// What vetting a Welcome from this account's own device decided (spec 4.4a).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnWelcome {
    /// Every leaf checks out: the group is with this peer, whose account is the contact to add.
    Peer(PeerId),
    /// A device of the account is missing from the record this device has: look it up again.
    Held(PeerId),
    /// Not a conversation this account's device may bring in.
    Refused,
}

pub(crate) enum GroupReply {
    KeyPackage(Vec<u8>),
    Created {
        group_id: Vec<u8>,
        first_commit_hash: [u8; 32],
        commit: Vec<u8>,
        welcome: Vec<u8>,
    },
    Info(Option<GroupInfo>),
    Outcome(Outcome),
    OwnWelcome(OwnWelcome),
    Plan(Option<Plan>),
    Committed(Committed),
    /// The stored `(rejoin_count, rejoin_after_ms)` after the rejoin, or `None` when refused.
    Rejoin(Option<(u32, u64)>),
    Due(Vec<DueDelivery>),
    Peers(Vec<PeerId>),
    /// Holds that were settled: conversation, its peer, and what became of the item.
    Reprocessed(Vec<(ConversationId, PeerId, Outcome)>),
    Groups(Vec<(ConversationId, PeerId)>),
    Flag(bool),
    Done,
}

fn group_exists() -> CoreError {
    CoreError::new(ErrorCode::Busy, "mls_group_exists")
}
fn auth_failed() -> CoreError {
    CoreError::new(ErrorCode::AuthFailed, "mls_leaf_not_trusted")
}
fn too_large() -> CoreError {
    CoreError::new(ErrorCode::MessageTooLarge, "mls_frame_too_large")
}

pub(super) struct GroupRow {
    pub group_id: Vec<u8>,
    pub epoch: u64,
}
pub(super) fn group_row(c: &Connection, conversation: ConversationId) -> R<Option<GroupRow>> {
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
pub(super) fn sql(e: rusqlite::Error) -> CoreError {
    super::store::sql_error(e)
}

/// The whole `mls_conversation` row.
struct Row {
    group_id: Vec<u8>,
    first_commit_hash: [u8; 32],
    epoch: u64,
    previous: Option<Vec<u8>>,
    last_commit_hash: Option<[u8; 32]>,
    unacked: bool,
    rejoin: (u32, u64),
    peer_mls: bool,
}
fn load_row(c: &Connection, conversation: ConversationId) -> R<Option<Row>> {
    c.query_row(
        "SELECT group_id,first_commit_hash,epoch,previous_epoch_snapshot,last_commit_hash,own_commit_unacked,rejoin_count,rejoin_after_ms,peer_mls FROM mls_conversation WHERE conversation=?1",
        params![conversation.0.as_slice()],
        |r| {
            let last: Option<Vec<u8>> = r.get(4)?;
            Ok(Row {
                group_id: r.get(0)?,
                first_commit_hash: bytes(r, 1)?,
                epoch: uint(r, 2)?,
                previous: r.get(3)?,
                last_commit_hash: last
                    .map(|hash| hash.try_into().map_err(|_| rusqlite::Error::InvalidQuery))
                    .transpose()?,
                unacked: r.get(5)?,
                rejoin: (
                    uint(r, 6)?.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?,
                    uint(r, 7)?,
                ),
                peer_mls: r.get(8)?,
            })
        },
    )
    .optional()
    .map_err(sql)
}
fn store_row(tx: &Transaction<'_>, conversation: ConversationId, row: &Row) -> R<()> {
    tx.execute(
        "INSERT OR REPLACE INTO mls_conversation(conversation,group_id,first_commit_hash,epoch,previous_epoch_snapshot,own_commit_unacked,rejoin_count,rejoin_after_ms,peer_mls,last_commit_hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            conversation.0.as_slice(),
            row.group_id,
            row.first_commit_hash.as_slice(),
            integer(row.epoch)?,
            row.previous,
            row.unacked,
            i64::from(row.rejoin.0),
            integer(row.rejoin.1)?,
            row.peer_mls,
            row.last_commit_hash.as_ref().map(|hash| hash.as_slice()),
        ],
    )
    .map_err(sql)?;
    Ok(())
}

/// Removes everything this device keeps for the conversation's group, including its
/// conversation-scope `mls_kv` rows (the global KeyPackage rows stay).
fn forget(tx: &Transaction<'_>, conversation: ConversationId) -> R<()> {
    for statement in [
        "DELETE FROM mls_kv WHERE scope=?1",
        "DELETE FROM mls_delivery WHERE conversation=?1",
        "DELETE FROM mls_hold WHERE conversation=?1",
        // Receipts sealed in this group: sealed again in the next one.
        "DELETE FROM mls_receipt WHERE conversation=?1",
        "DELETE FROM mls_conversation WHERE conversation=?1",
    ] {
        tx.execute(statement, params![conversation.0.as_slice()])
            .map_err(sql)?;
    }
    Ok(())
}

/// Sent messages that no device acknowledged yet go back to "awaiting encryption", so they are
/// sealed again in the group this device moves to (`SealPending`).
fn unseal_unacked(tx: &Transaction<'_>, conversation: ConversationId) -> R<()> {
    tx.execute(
        "UPDATE outbox SET codec=3,wire=x'' WHERE conversation=?1 AND codec=2 AND EXISTS(SELECT 1 FROM message m WHERE m.conversation=outbox.conversation AND m.sender_peer=outbox.sender_peer AND m.sender_install=outbox.sender_install AND m.msg_id=outbox.msg_id AND m.delivered_at_ms IS NULL AND m.cancel_requested=0 AND m.outcome<>3)",
        params![conversation.0.as_slice()],
    )
    .map_err(sql)?;
    Ok(())
}

fn deliver(
    tx: &Transaction<'_>,
    conversation: ConversationId,
    recipient: InstallationId,
    kind: u8,
    epoch: u64,
    body: &[u8],
    now: u64,
) -> R<()> {
    if body.is_empty() || body.len() > MAX_FRAME {
        return Err(too_large());
    }
    tx.execute(
        "INSERT OR REPLACE INTO mls_delivery VALUES(?1,?2,?3,?4,?5,0,?6)",
        params![
            conversation.0.as_slice(),
            recipient.0.as_slice(),
            kind,
            integer(epoch)?,
            body,
            integer(now)?
        ],
    )
    .map_err(sql)?;
    Ok(())
}
fn drop_deliveries_to(
    tx: &Transaction<'_>,
    conversation: ConversationId,
    recipient: InstallationId,
) -> R<()> {
    tx.execute(
        "DELETE FROM mls_delivery WHERE conversation=?1 AND recipient_install=?2",
        params![conversation.0.as_slice(), recipient.0.as_slice()],
    )
    .map_err(sql)?;
    Ok(())
}
fn commit_frame(conversation: ConversationId, epoch: u64, commit: &[u8]) -> Vec<u8> {
    MlsFrame::Commit {
        conversation,
        epoch,
        commit: commit.to_vec(),
    }
    .encode()
    .to_vec()
}
fn welcome_frame(conversation: ConversationId, welcome: &[u8], sender: InstallationId) -> Vec<u8> {
    MlsFrame::Welcome {
        conversation,
        welcome: welcome.to_vec(),
        sender_install: sender,
    }
    .encode()
    .to_vec()
}

fn key_package_installation(bytes: &[u8]) -> R<InstallationId> {
    let info = group::inspect_key_package(bytes)?;
    Ok(MlsCredential::decode(&info.credential)
        .map_err(|_| auth_failed())?
        .installation)
}

pub(super) fn signer_of(local: &LocalAccount) -> (SignatureKeyPair, MlsCredential) {
    (
        mls_signer(&local.device),
        MlsCredential::issue(local.keys.peer_id(), &local.device),
    )
}

/// The device lists a leaf may come from: this account's (without revoked devices) and the
/// conversation peer's, each re-parsed from its signed packet for exactly that account
/// (Ruling 6). The cached packets were accepted under the M3 time rules already, so only their
/// signature and account are checked here (`parse_stored_record`).
struct Records {
    own_peer: PeerId,
    own: IdentityRecord,
    peer: PeerId,
    remote: IdentityRecord,
}
fn records(local: &LocalAccount, peer: PeerId) -> R<Records> {
    let own_peer = local.keys.peer_id();
    let signed = local.records.accepted.as_ref().ok_or_else(auth_failed)?;
    let mut own = parse_stored_record(&signed.packet, own_peer)?;
    own.devices.retain(|device| {
        !local.records.tombstones.iter().any(|t| {
            t.target_installation == device.installation_id
                || t.target_transport == device.transport_public_key
        })
    });
    let cached = local
        .remote
        .iter()
        .find(|r| r.peer == peer)
        .ok_or_else(|| CoreError::new(ErrorCode::RecordUnavailable, "mls_peer_record_missing"))?;
    Ok(Records {
        own_peer,
        own,
        peer,
        remote: parse_stored_record(&cached.accepted.packet, peer)?,
    })
}

/// The leaf check of one operation. A leaf is checked against the record of the account its
/// credential names (this device's or the conversation peer's); any other account is refused.
/// OpenMLS reports only that `verify` said no, so why is kept here (Ruling 10): a device that
/// is merely missing from a record is held and looked up again, anything else is rejected.
struct Vetting<'a> {
    records: &'a Records,
    missing: Cell<Option<PeerId>>,
    refused: Cell<bool>,
}
impl<'a> Vetting<'a> {
    fn new(records: &'a Records) -> Self {
        Self {
            records,
            missing: Cell::new(None),
            refused: Cell::new(false),
        }
    }
    fn check(&self, credential: &[u8], signature_key: &[u8]) -> R<()> {
        let result = MlsCredential::decode(credential)
            .map_err(|_| auth_failed())
            .and_then(|decoded| {
                let record = if decoded.peer == self.records.own_peer {
                    &self.records.own
                } else if decoded.peer == self.records.peer {
                    &self.records.remote
                } else {
                    return Err(auth_failed());
                };
                verify_leaf(credential, signature_key, decoded.peer, record)
                    .map(|_| ())
                    .inspect_err(|error| {
                        if error.code == ErrorCode::RecordUnavailable {
                            self.missing.set(Some(decoded.peer));
                        }
                    })
            });
        if result
            .as_ref()
            .is_err_and(|error| error.code != ErrorCode::RecordUnavailable)
        {
            self.refused.set(true);
        }
        result
    }
    /// The account whose record lacked a leaf, when that was the only reason for a refusal.
    fn held(&self) -> Option<PeerId> {
        if self.refused.get() {
            None
        } else {
            self.missing.get()
        }
    }
}

pub(super) fn check_own(identity: Identity, key: MessageKey) -> R<()> {
    if key.sender != identity.peer || key.installation != identity.installation {
        return Err(invalid());
    }
    Ok(())
}

fn check_conversation(identity: Identity, peer: PeerId, conversation: ConversationId) -> R<()> {
    if super::store::conversation_id(identity.peer, peer)? != conversation {
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
    let reply = match op {
        GroupOp::NewKeyPackage => {
            let (signer, credential) = signer_of(local);
            let tx = c.transaction().map_err(sql)?;
            let provider = MlsProvider::new(KvStore::load(&tx, GLOBAL)?);
            let key_package = group::new_key_package(&provider, &signer, &credential)?;
            provider.storage().flush(&tx, GLOBAL)?;
            finish(tx, fault)?;
            GroupReply::KeyPackage(key_package)
        }
        GroupOp::Info => GroupReply::Info(load_row(c, conversation)?.map(|row| GroupInfo {
            epoch: row.epoch,
            first_commit_hash: row.first_commit_hash,
        })),
        GroupOp::Create {
            peer,
            members,
            rejoin,
            now,
        } => create(
            c,
            identity,
            local,
            conversation,
            peer,
            &members,
            rejoin,
            now,
            fault,
        )?,
        GroupOp::Join {
            peer,
            sender_peer,
            sender_install,
            welcome,
            first_commit_hash,
            rejoin,
            joined_with,
            now,
            wall,
        } => GroupReply::Outcome(join(
            c,
            identity,
            local,
            conversation,
            Arrival {
                peer,
                sender_peer,
                hold: true,
                now,
                wall,
            },
            Welcome {
                sender_install,
                welcome: &welcome,
                first_commit_hash,
                rejoin,
                joined_with,
            },
            fault,
        )?),
        GroupOp::OwnWelcome {
            sender_install,
            welcome,
        } => GroupReply::OwnWelcome(vet_own_welcome(
            c,
            identity,
            local,
            conversation,
            sender_install,
            &welcome,
        )?),
        GroupOp::ApplyCommit {
            peer,
            sender_peer,
            epoch,
            commit,
            now,
        } => GroupReply::Outcome(apply(
            c,
            identity,
            local,
            conversation,
            Arrival {
                peer,
                sender_peer,
                hold: true,
                now,
                wall: now,
            },
            epoch,
            &commit,
            fault,
        )?),
        GroupOp::JoinRequest {
            peer,
            sender_peer,
            key_package,
            now,
        } => GroupReply::Committed(join_request(
            c,
            identity,
            local,
            conversation,
            Arrival {
                peer,
                sender_peer,
                hold: true,
                now,
                wall: now,
            },
            &key_package,
            fault,
        )?),
        GroupOp::Plan { peer } => GroupReply::Plan(plan(c, identity, local, conversation, peer)?),
        GroupOp::Commit {
            peer,
            add,
            remove,
            now,
        } => GroupReply::Committed(own_commit(
            c,
            identity,
            local,
            conversation,
            peer,
            &add,
            &remove,
            false,
            now,
            fault,
        )?),
        GroupOp::Update { peer, now } => GroupReply::Committed(own_commit(
            c,
            identity,
            local,
            conversation,
            peer,
            &[],
            &[],
            true,
            now,
            fault,
        )?),
        GroupOp::Rejoin { now } => {
            let tx = c.transaction().map_err(sql)?;
            let Some(row) = load_row(&tx, conversation)? else {
                return Ok(MessageDbReply::Group(GroupReply::Rejoin(None)));
            };
            let Some(counters) = rejoin_allowed(row.rejoin, now) else {
                return Ok(MessageDbReply::Group(GroupReply::Rejoin(None)));
            };
            forget(&tx, conversation)?;
            unseal_unacked(&tx, conversation)?;
            finish(tx, fault)?;
            GroupReply::Rejoin(Some(counters))
        }
        GroupOp::DueDeliveries { now, recipients } => {
            GroupReply::Due(due_deliveries(c, now, &recipients)?)
        }
        GroupOp::Pending { recipient } => {
            GroupReply::Due(pending_deliveries(c, conversation, recipient)?)
        }
        GroupOp::Delivered {
            recipient,
            kind,
            epoch,
            outcome,
            now,
        } => {
            delivered(c, conversation, recipient, kind, epoch, outcome, now, fault)?;
            GroupReply::Done
        }
        GroupOp::HoldPeers => {
            let mut q = c
                .prepare("SELECT DISTINCT sender_peer FROM mls_hold ORDER BY sender_peer")
                .map_err(sql)?;
            let peers = q
                .query_map([], |r| bytes::<32>(r, 0))
                .map_err(sql)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql)?;
            GroupReply::Peers(peers.into_iter().map(PeerId).collect())
        }
        GroupOp::Reprocess {
            lookup,
            drop_unknown,
            now,
            wall,
        } => GroupReply::Reprocessed(reprocess(
            c,
            identity,
            local,
            lookup,
            drop_unknown,
            now,
            wall,
        )?),
        GroupOp::Groups => {
            let mut q = c
                .prepare("SELECT g.conversation,c.peer FROM mls_conversation g JOIN conversation c ON c.id=g.conversation ORDER BY g.conversation")
                .map_err(sql)?;
            let groups = q
                .query_map([], |r| Ok((bytes::<32>(r, 0)?, bytes::<32>(r, 1)?)))
                .map_err(sql)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql)?;
            GroupReply::Groups(
                groups
                    .into_iter()
                    .map(|(id, peer)| (ConversationId(id), PeerId(peer)))
                    .collect(),
            )
        }
        GroupOp::PeerSpokeMls { mark, peer } => {
            if mark {
                let tx = c.transaction().map_err(sql)?;
                super::mls_store::mark_peer_mls(&tx, conversation)?;
                finish(tx, fault)?;
            }
            let spoke: bool = c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM mls_peer WHERE conversation=?1)",
                    params![conversation.0.as_slice()],
                    |r| r.get(0),
                )
                .map_err(sql)?;
            GroupReply::Flag(spoke || peer_has_leaf(c, local, conversation, peer)?)
        }
        GroupOp::Drop => {
            let tx = c.transaction().map_err(sql)?;
            forget(&tx, conversation)?;
            finish(tx, fault)?;
            GroupReply::Done
        }
    };
    Ok(MessageDbReply::Group(reply))
}

/// Ruling 15: the count is first brought up to date (`rejoin_count_now`), then checked; the
/// stored values after an allowed rejoin are returned.
fn rejoin_allowed(stored: (u32, u64), now: u64) -> Option<(u32, u64)> {
    let count = membership::rejoin_count_now(stored.0, stored.1, now);
    membership::may_rejoin(count, stored.1, now).then(|| membership::after_rejoin(count, now))
}

#[allow(clippy::too_many_arguments)]
fn create(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    conversation: ConversationId,
    peer: PeerId,
    members: &[Vec<u8>],
    rejoin: Option<(u32, u64)>,
    now: u64,
    fault: Fault,
) -> R<GroupReply> {
    check_conversation(identity, peer, conversation)?;
    let records = records(local, peer)?;
    let (signer, credential) = signer_of(local);
    // A KeyPackage whose leaf does not check out is left out (that device joins later)
    // instead of failing the whole group.
    let members: Vec<Vec<u8>> = members
        .iter()
        .filter(|kp| {
            group::inspect_key_package(kp).is_ok_and(|info| {
                Vetting::new(&records)
                    .check(&info.credential, &info.signature_key)
                    .is_ok()
            })
        })
        .cloned()
        .collect();
    let added = members
        .iter()
        .map(|kp| key_package_installation(kp))
        .collect::<R<Vec<_>>>()?;
    let tx = c.transaction().map_err(sql)?;
    if group_row(&tx, conversation)?.is_some() {
        return Err(group_exists());
    }
    ensure_conversation(&tx, conversation, peer)?;
    // Leftover state of an abandoned group must not mix with the new one (Ruling 11).
    tx.execute(
        "DELETE FROM mls_kv WHERE scope=?1",
        params![conversation.0.as_slice()],
    )
    .map_err(sql)?;
    let provider = MlsProvider::new(KvStore::load(&tx, conversation.0)?);
    let vetting = Vetting::new(&records);
    let verify = |credential: &[u8], key: &[u8]| vetting.check(credential, key);
    let created = group::create_group(
        &provider,
        &signer,
        &credential,
        conversation,
        &members,
        &verify,
    )?;
    let epoch = membership::epoch_of(&provider, &created.group_id)?;
    provider.storage().flush(&tx, conversation.0)?;
    store_row(
        &tx,
        conversation,
        &Row {
            group_id: created.group_id.clone(),
            first_commit_hash: created.first_commit_hash,
            epoch,
            previous: None,
            last_commit_hash: Some(created.first_commit_hash),
            // Nobody else was in the group to fork it; the Welcomes need no acknowledgement
            // before the next Commit.
            unacked: false,
            rejoin: rejoin.unwrap_or((0, 0)),
            peer_mls: false,
        },
    )?;
    let frame = welcome_frame(conversation, &created.welcome, identity.installation);
    for installation in added {
        deliver(
            &tx,
            conversation,
            installation,
            DELIVER_WELCOME,
            epoch,
            &frame,
            now,
        )?;
    }
    finish(tx, fault)?;
    Ok(GroupReply::Created {
        group_id: created.group_id,
        first_commit_hash: created.first_commit_hash,
        commit: created.commit,
        welcome: created.welcome,
    })
}

/// Who a received Commit/Welcome is from, and whether a missing device holds it.
struct Arrival {
    /// The conversation's peer.
    peer: PeerId,
    /// The account of the installation that sent it.
    sender_peer: PeerId,
    /// Store it in `mls_hold` when a device is missing (false while reprocessing a hold).
    hold: bool,
    now: u64,
    /// Wall-clock time for the rejoin limit.
    wall: u64,
}
struct Welcome<'a> {
    sender_install: InstallationId,
    welcome: &'a [u8],
    first_commit_hash: [u8; 32],
    rejoin: Option<(u32, u64)>,
    joined_with: Option<[u8; 32]>,
}

fn hold_body(
    tag: u8,
    first_commit_hash: [u8; 32],
    arrival: &Arrival,
    frame_inner: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(HOLD_HEADER + frame_inner.len());
    body.push(tag);
    body.extend_from_slice(&first_commit_hash);
    body.extend_from_slice(&arrival.sender_peer.0);
    body.extend_from_slice(&arrival.peer.0);
    body.extend_from_slice(frame_inner);
    body
}

/// Keeps a Commit/Welcome until `lookup`'s record is fetched again; the oldest holds beyond the
/// caps are dropped. A body too large for the table is not held (it is refused instead).
fn hold(
    c: &mut Connection,
    conversation: ConversationId,
    lookup: PeerId,
    body: &[u8],
    now: u64,
) -> R<bool> {
    if body.len() > MAX_FRAME {
        return Ok(false);
    }
    let tx = c.transaction().map_err(sql)?;
    let hash: [u8; 32] = Sha256::digest(body).into();
    tx.execute(
        "INSERT OR IGNORE INTO mls_hold VALUES(?1,?2,?3,?4,?5)",
        params![
            conversation.0.as_slice(),
            lookup.0.as_slice(),
            hash.as_slice(),
            body,
            integer(now)?
        ],
    )
    .map_err(sql)?;
    tx.execute(
        "DELETE FROM mls_hold WHERE conversation=?1 AND rowid NOT IN (SELECT rowid FROM mls_hold WHERE conversation=?1 ORDER BY held_at_ms DESC,rowid DESC LIMIT ?2)",
        params![conversation.0.as_slice(), HOLDS_PER_CONVERSATION],
    )
    .map_err(sql)?;
    tx.execute(
        "DELETE FROM mls_hold WHERE rowid NOT IN (SELECT rowid FROM mls_hold ORDER BY held_at_ms DESC,rowid DESC LIMIT ?1)",
        params![HOLDS_TOTAL],
    )
    .map_err(sql)?;
    tx.commit().map_err(sql)?;
    Ok(true)
}

/// Spec 4.4a: whether a Welcome from `sender_install`, a device of this account, may bring in a
/// conversation with an account that is not a contact. Nothing is kept. The Welcome is staged
/// in memory to read its leaves; then every leaf is checked the way a join checks it (`Vetting`):
/// this account's leaves against its own record, the peer's against the record of exactly that
/// peer (Ruling 6). The peer is whoever else has leaves in the group, and the conversation must
/// be the one between the two accounts. The sending device must be one of the group's leaves.
fn vet_own_welcome(
    c: &Connection,
    identity: Identity,
    local: &LocalAccount,
    conversation: ConversationId,
    sender_install: InstallationId,
    welcome: &[u8],
) -> R<OwnWelcome> {
    let leaves = std::cell::RefCell::new(Vec::<(Vec<u8>, Vec<u8>)>::new());
    let provider = MlsProvider::new(KvStore::load(c, conversation.0)?);
    let collect = |credential: &[u8], key: &[u8]| {
        leaves
            .borrow_mut()
            .push((credential.to_vec(), key.to_vec()));
        Ok(())
    };
    // The provider is dropped without being flushed.
    if group::join(&provider, welcome, &collect).is_err() {
        return Ok(OwnWelcome::Refused);
    }
    let leaves = leaves.into_inner();
    let mut peers = Vec::new();
    for (credential, _) in &leaves {
        let Ok(decoded) = MlsCredential::decode(credential) else {
            return Ok(OwnWelcome::Refused);
        };
        if decoded.peer != identity.peer && !peers.contains(&decoded.peer) {
            peers.push(decoded.peer);
        }
    }
    let [peer] = peers[..] else {
        return Ok(OwnWelcome::Refused);
    };
    if check_conversation(identity, peer, conversation).is_err() {
        return Ok(OwnWelcome::Refused);
    }
    let records = match records(local, peer) {
        Ok(records) => records,
        Err(error) if error.code == ErrorCode::RecordUnavailable => {
            return Ok(OwnWelcome::Held(peer));
        }
        Err(error) => return Err(error),
    };
    let vetting = Vetting::new(&records);
    let mut verified = true;
    let mut sender_leaf = false;
    for (credential, key) in &leaves {
        if vetting.check(credential, key).is_err() {
            verified = false;
        } else if MlsCredential::decode(credential)
            .is_ok_and(|d| d.peer == identity.peer && d.installation == sender_install)
        {
            sender_leaf = true;
        }
    }
    if !verified {
        return Ok(vetting.held().map_or(OwnWelcome::Refused, OwnWelcome::Held));
    }
    Ok(if sender_leaf {
        OwnWelcome::Peer(peer)
    } else {
        OwnWelcome::Refused
    })
}

fn join(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    conversation: ConversationId,
    arrival: Arrival,
    welcome: Welcome<'_>,
    fault: Fault,
) -> R<Outcome> {
    check_conversation(identity, arrival.peer, conversation)?;
    let records = records(local, arrival.peer)?;
    let vetting = Vetting::new(&records);
    let joined = {
        let tx = c.transaction().map_err(sql)?;
        let existing = load_row(&tx, conversation)?;
        if let Some(old) = &existing
            && old.first_commit_hash != welcome.first_commit_hash
            && membership::choose_group(old.first_commit_hash, welcome.first_commit_hash)
                != Ordering::Greater
        {
            // This device's group wins (spec 4.4); the other side joins it instead.
            return Ok(Outcome::Ack);
        }
        let replacing = existing
            .as_ref()
            .is_some_and(|old| old.first_commit_hash != welcome.first_commit_hash);
        // Leaving this device's group for the winning one is a rejoin: it goes through the
        // same limit (Ruling 15) and is counted. Refused for now, the sender tries again later.
        let counters = match &existing {
            Some(old) if replacing => match rejoin_allowed(old.rejoin, arrival.wall) {
                Some(counters) => Some(counters),
                None => return Ok(Outcome::Later),
            },
            Some(old) => Some(old.rejoin),
            None => welcome.rejoin,
        };
        ensure_conversation(&tx, conversation, arrival.peer)?;
        // The old state (another group, or this group's stale state) goes before the new is
        // loaded, so the snapshot and rows that follow belong to the new group only.
        forget(&tx, conversation)?;
        let provider = MlsProvider::new(KvStore::load(&tx, conversation.0)?);
        let verify = |credential: &[u8], key: &[u8]| vetting.check(credential, key);
        match group::join(&provider, welcome.welcome, &verify) {
            Ok(joined) => {
                if let Some(old) = &existing
                    && !replacing
                    && joined.epoch <= old.epoch
                {
                    // Another Welcome for the very epoch this device joined at, and no Commit
                    // seen since: two members added it on competing branches, and it cannot
                    // tell which one wins. Rejoining puts it on the branch that does.
                    let hash: [u8; 32] = Sha256::digest(welcome.welcome).into();
                    if joined.epoch == old.epoch
                        && old.last_commit_hash.is_none()
                        && welcome.joined_with.is_some_and(|first| first != hash)
                    {
                        return Ok(Outcome::Rejoin { epoch: old.epoch });
                    }
                    // An older (or the same) Welcome for the group this device is already in.
                    return Ok(Outcome::Ack);
                }
                provider.storage().flush(&tx, conversation.0)?;
                store_row(
                    &tx,
                    conversation,
                    &Row {
                        group_id: joined.group_id,
                        first_commit_hash: welcome.first_commit_hash,
                        epoch: joined.epoch,
                        previous: None,
                        // The Commit that made this epoch was not seen here.
                        last_commit_hash: None,
                        unacked: false,
                        rejoin: counters.unwrap_or((0, 0)),
                        peer_mls: arrival.sender_peer != identity.peer
                            || existing.as_ref().is_some_and(|old| old.peer_mls),
                    },
                )?;
                if replacing {
                    unseal_unacked(&tx, conversation)?;
                }
                finish(tx, fault)?;
                true
            }
            Err(_) => false,
        }
    };
    if joined {
        return Ok(Outcome::Joined);
    }
    let Some(lookup) = vetting.held() else {
        return Ok(Outcome::Failed {
            class: group::CLASS_OTHER,
            epoch: 0,
        });
    };
    if arrival.hold {
        let inner = MlsFrame::Welcome {
            conversation,
            welcome: welcome.welcome.to_vec(),
            sender_install: welcome.sender_install,
        }
        .inner();
        let body = hold_body(HOLD_WELCOME, welcome.first_commit_hash, &arrival, &inner);
        if !hold(c, conversation, lookup, &body, arrival.now)? {
            return Ok(Outcome::Failed {
                class: group::CLASS_OTHER,
                epoch: 0,
            });
        }
    }
    Ok(Outcome::Held(lookup))
}

#[allow(clippy::too_many_arguments)]
fn apply(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    conversation: ConversationId,
    arrival: Arrival,
    epoch: u64,
    commit: &[u8],
    fault: Fault,
) -> R<Outcome> {
    check_conversation(identity, arrival.peer, conversation)?;
    let records = records(local, arrival.peer)?;
    let vetting = Vetting::new(&records);
    let verify = |credential: &[u8], key: &[u8]| vetting.check(credential, key);
    let hash: [u8; 32] = Sha256::digest(commit).into();
    let tx = c.transaction().map_err(sql)?;
    let Some(row) = load_row(&tx, conversation)? else {
        return Ok(Outcome::Failed {
            class: group::CLASS_NO_GROUP,
            epoch: 0,
        });
    };
    if row.last_commit_hash == Some(hash) {
        return Ok(Outcome::Ack);
    }
    // A Commit of another group of this conversation is not a fork of this one.
    if membership::message_group_id(commit).as_deref() != Some(row.group_id.as_slice()) {
        return Ok(Outcome::Failed {
            class: group::CLASS_NO_GROUP,
            epoch: row.epoch,
        });
    }
    // This device is past that epoch already (typically a resend after a lost Ack): nothing
    // to decide, and nothing it says can move this device.
    if epoch < row.epoch {
        return Ok(Outcome::Failed {
            class: group::CLASS_PAST_EPOCH,
            epoch: row.epoch,
        });
    }
    let provider = MlsProvider::new(KvStore::load(&tx, conversation.0)?);
    let before = membership::group_installations(&provider, &row.group_id)?;
    if let Ok(applied) = membership::apply_commit(&provider, &row.group_id, commit, &verify) {
        if applied.self_removed {
            forget(&tx, conversation)?;
            finish(tx, fault)?;
            return Ok(Outcome::Removed);
        }
        settle_applied(
            &tx,
            &provider,
            conversation,
            &row,
            &before,
            applied,
            hash,
            arrival.sender_peer != identity.peer,
            None,
        )?;
        finish(tx, fault)?;
        return Ok(Outcome::Ack);
    }
    if let Some(lookup) = vetting.held() {
        drop(provider);
        drop(tx);
        if arrival.hold {
            let inner = MlsFrame::Commit {
                conversation,
                epoch,
                commit: commit.to_vec(),
            }
            .inner();
            let body = hold_body(HOLD_COMMIT, [0; 32], &arrival, &inner);
            if !hold(c, conversation, lookup, &body, arrival.now)? {
                return Ok(Outcome::Failed {
                    class: group::CLASS_OTHER,
                    epoch: row.epoch,
                });
            }
        }
        return Ok(Outcome::Held(lookup));
    }
    let refused = Outcome::Failed {
        class: group::CLASS_OTHER,
        epoch: row.epoch,
    };
    if vetting.refused.get() {
        return Ok(refused);
    }
    if epoch > row.epoch {
        // Ahead of this device: an earlier Commit is missing, or it follows a Commit of this
        // epoch that this device has not adopted yet. Either way the sender keeps it and tries
        // again (class 2), and it applies once this device has caught up.
        return Ok(Outcome::Failed {
            class: group::CLASS_FUTURE_EPOCH,
            epoch: row.epoch,
        });
    }
    // A competing Commit for the epoch this device is at (spec 4.5). It decides nothing until
    // it is processed against the state before this epoch: only a real Commit of the same
    // group can win, and a lost one leaves this device exactly as it was.
    let (Some(mine), Some(previous)) = (row.last_commit_hash, row.previous.as_deref()) else {
        return Ok(refused);
    };
    if membership::resolve_fork(row.epoch, mine, epoch, hash, true) != ForkDecision::AdoptTheirs {
        return Ok(refused);
    }
    membership::restore_snapshot(&provider, previous)?;
    let before = membership::group_installations(&provider, &row.group_id)?;
    let checked = Vetting::new(&records);
    let verify = |credential: &[u8], key: &[u8]| checked.check(credential, key);
    match membership::apply_commit(&provider, &row.group_id, commit, &verify) {
        Ok(applied) if applied.epoch == row.epoch => {
            if applied.self_removed {
                forget(&tx, conversation)?;
                finish(tx, fault)?;
                return Ok(Outcome::Removed);
            }
            settle_applied(
                &tx,
                &provider,
                conversation,
                &row,
                &before,
                applied,
                hash,
                arrival.sender_peer != identity.peer,
                Some(epoch),
            )?;
            finish(tx, fault)?;
            Ok(Outcome::Adopted)
        }
        // Not a valid Commit for this epoch: the transaction is dropped, nothing changes.
        _ => Ok(refused),
    }
}

/// Stores an applied Commit: the new epoch, the state before it as the one kept snapshot, its
/// hash for later forks, no more deliveries to the installations it removed, and no own Commit
/// waiting any more. With `abandoned` (a lost fork) the deliveries of the abandoned branch go
/// too.
#[allow(clippy::too_many_arguments)]
fn settle_applied(
    tx: &Transaction<'_>,
    provider: &MlsProvider,
    conversation: ConversationId,
    row: &Row,
    before: &[InstallationId],
    applied: membership::Applied,
    hash: [u8; 32],
    from_peer: bool,
    abandoned: Option<u64>,
) -> R<()> {
    provider.storage().flush(tx, conversation.0)?;
    let after = membership::group_installations(provider, &row.group_id)?;
    for removed in before.iter().filter(|i| !after.contains(i)) {
        drop_deliveries_to(tx, conversation, *removed)?;
    }
    if let Some(epoch) = abandoned {
        tx.execute(
            "DELETE FROM mls_delivery WHERE conversation=?1 AND epoch>=?2",
            params![conversation.0.as_slice(), integer(epoch)?],
        )
        .map_err(sql)?;
    }
    store_row(
        tx,
        conversation,
        &Row {
            group_id: row.group_id.clone(),
            first_commit_hash: row.first_commit_hash,
            epoch: applied.epoch,
            previous: Some(applied.previous),
            last_commit_hash: Some(hash),
            // A Commit applied on top of this device's state (or replacing its lost one) means
            // its own last Commit is settled: whoever committed had processed it, or it lost.
            unacked: false,
            rejoin: row.rejoin,
            peer_mls: row.peer_mls || from_peer,
        },
    )
}

fn plan(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    conversation: ConversationId,
    peer: PeerId,
) -> R<Option<Plan>> {
    check_conversation(identity, peer, conversation)?;
    let records = records(local, peer)?;
    let Some(row) = load_row(c, conversation)? else {
        return Ok(None);
    };
    if row.unacked {
        return Ok(Some(Plan {
            unacked: true,
            add: Vec::new(),
            remove: Vec::new(),
        }));
    }
    let provider = MlsProvider::new(KvStore::load(c, conversation.0)?);
    let leaves = membership::group_installations(&provider, &row.group_id)?;
    let diff = membership::diff_members(&leaves, &records.own, &records.remote);
    Ok(Some(Plan {
        unacked: row.unacked,
        add: diff.add,
        remove: diff
            .remove
            .into_iter()
            .filter(|installation| *installation != identity.installation)
            .collect(),
    }))
}

/// A `JoinRequest`: the asking installation (named by its KeyPackage) is added, replacing its
/// old leaf if it has one. When its account's record does not list it yet, the request is held
/// like a Commit and the record looked up again (spec 4.2).
fn join_request(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    conversation: ConversationId,
    arrival: Arrival,
    key_package: &[u8],
    fault: Fault,
) -> R<Committed> {
    let asker = key_package_installation(key_package)?;
    let committed = own_commit(
        c,
        identity,
        local,
        conversation,
        arrival.peer,
        &[key_package.to_vec()],
        &[asker],
        false,
        arrival.now,
        fault,
    )?;
    if let Committed::Held(lookup) = committed
        && arrival.hold
    {
        let inner = MlsFrame::JoinRequest {
            conversation,
            key_package: key_package.to_vec(),
        }
        .inner();
        let body = hold_body(HOLD_JOIN_REQUEST, [0; 32], &arrival, &inner);
        if !hold(c, conversation, lookup, &body, arrival.now)? {
            return Ok(Committed::Busy);
        }
    }
    Ok(committed)
}

#[allow(clippy::too_many_arguments)]
fn own_commit(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    conversation: ConversationId,
    peer: PeerId,
    add: &[Vec<u8>],
    remove: &[InstallationId],
    update: bool,
    now: u64,
    fault: Fault,
) -> R<Committed> {
    check_conversation(identity, peer, conversation)?;
    let records = records(local, peer)?;
    let (signer, _) = signer_of(local);
    let added = add
        .iter()
        .map(|kp| key_package_installation(kp))
        .collect::<R<Vec<_>>>()?;
    let tx = c.transaction().map_err(sql)?;
    let Some(row) = load_row(&tx, conversation)? else {
        return Ok(Committed::Busy);
    };
    if row.unacked {
        return Ok(Committed::Busy);
    }
    let provider = MlsProvider::new(KvStore::load(&tx, conversation.0)?);
    let before = membership::group_installations(&provider, &row.group_id)?;
    // Only current leaves other than this device can be removed (a joining installation that
    // has no leaf yet is just added).
    let remove: Vec<InstallationId> = remove
        .iter()
        .copied()
        .filter(|i| before.contains(i) && *i != identity.installation)
        .collect();
    let empty = add.is_empty() && remove.is_empty();
    if empty && !update {
        return Ok(Committed::Done { epoch: row.epoch });
    }
    let remove = remove.as_slice();
    let vetting = Vetting::new(&records);
    let verify = |credential: &[u8], key: &[u8]| vetting.check(credential, key);
    let made = if empty {
        membership::commit_update(&provider, &signer, &row.group_id)
    } else {
        membership::commit_changes(&provider, &signer, &row.group_id, add, remove, &verify)
    };
    let committed =
        match made {
            Ok(committed) => committed,
            Err(error) => {
                return match vetting.held() {
                    Some(lookup) => Ok(Committed::Held(lookup)),
                    None => Err(error),
                };
            }
        };
    provider.storage().flush(&tx, conversation.0)?;
    let hash: [u8; 32] = Sha256::digest(&committed.commit).into();
    // A removed installation gets nothing more (and an installation that rejoins is removed and
    // added again: it gets the Welcome only).
    for removed in remove {
        drop_deliveries_to(&tx, conversation, *removed)?;
    }
    let members: Vec<InstallationId> = before
        .into_iter()
        .filter(|i| *i != identity.installation && !remove.contains(i))
        .collect();
    let frame = commit_frame(conversation, committed.epoch, &committed.commit);
    for member in &members {
        deliver(
            &tx,
            conversation,
            *member,
            DELIVER_COMMIT,
            committed.epoch,
            &frame,
            now,
        )?;
    }
    if let Some(welcome) = &committed.welcome {
        let frame = welcome_frame(conversation, welcome, identity.installation);
        for installation in &added {
            deliver(
                &tx,
                conversation,
                *installation,
                DELIVER_WELCOME,
                committed.epoch,
                &frame,
                now,
            )?;
        }
    }
    store_row(
        &tx,
        conversation,
        &Row {
            group_id: row.group_id.clone(),
            first_commit_hash: row.first_commit_hash,
            epoch: committed.epoch,
            previous: Some(committed.previous),
            last_commit_hash: Some(hash),
            // Only members that were in the group before can fork it.
            unacked: !members.is_empty(),
            rejoin: row.rejoin,
            peer_mls: row.peer_mls,
        },
    )?;
    finish(tx, fault)?;
    Ok(Committed::Done {
        epoch: committed.epoch,
    })
}

fn due_deliveries(c: &Connection, now: u64, recipients: &[InstallationId]) -> R<Vec<DueDelivery>> {
    if recipients.is_empty() {
        return Ok(Vec::new());
    }
    // Only recipients this device can reach now are read: the rows of unreachable ones stay
    // as they are (no attempt counted) and can never fill the batch. One recipient of one
    // conversation gets its rows in epoch order (a Welcome before the Commits that follow it);
    // a later row never overtakes an earlier one that waits.
    let reachable = vec!["?"; recipients.len()].join(",");
    let mut q = c
        .prepare(&format!("SELECT d.conversation,d.recipient_install,d.kind,d.epoch,d.body FROM mls_delivery d WHERE d.next_retry_at_ms<=? AND d.recipient_install IN ({reachable}) AND NOT EXISTS(SELECT 1 FROM mls_delivery e WHERE e.conversation=d.conversation AND e.recipient_install=d.recipient_install AND (e.epoch<d.epoch OR (e.epoch=d.epoch AND e.kind>d.kind))) ORDER BY d.next_retry_at_ms,d.conversation,d.recipient_install LIMIT 16"))
        .map_err(sql)?;
    let mut values: Vec<rusqlite::types::Value> = vec![integer(now)?.into()];
    values.extend(recipients.iter().map(|r| r.0.to_vec().into()));
    q.query_map(rusqlite::params_from_iter(values.iter()), |r| {
        Ok(DueDelivery {
            conversation: ConversationId(bytes(r, 0)?),
            recipient: InstallationId(bytes(r, 1)?),
            kind: r.get(2)?,
            epoch: uint(r, 3)?,
            body: r.get(4)?,
        })
    })
    .map_err(sql)?
    .collect::<Result<_, _>>()
    .map_err(sql)
}

/// Whether one of `peer`'s listed devices holds a leaf in the conversation's group.
fn peer_has_leaf(
    c: &Connection,
    local: &LocalAccount,
    conversation: ConversationId,
    peer: Option<PeerId>,
) -> R<bool> {
    let Some(peer) = peer else {
        return Ok(false);
    };
    let Some(row) = load_row(c, conversation)? else {
        return Ok(false);
    };
    let Some(record) = local.remote.iter().find(|r| r.peer == peer) else {
        return Ok(false);
    };
    let provider = MlsProvider::new(KvStore::load(c, conversation.0)?);
    let members = membership::group_installations(&provider, &row.group_id)?;
    Ok(record
        .accepted
        .record
        .devices
        .iter()
        .any(|d| members.contains(&d.installation_id)))
}

/// At most this many queued deliveries go ahead of one application message.
const PENDING_LIMIT: i64 = 8;

fn pending_deliveries(
    c: &Connection,
    conversation: ConversationId,
    recipient: InstallationId,
) -> R<Vec<DueDelivery>> {
    let mut q = c
        .prepare("SELECT kind,epoch,body FROM mls_delivery WHERE conversation=?1 AND recipient_install=?2 ORDER BY epoch,kind DESC LIMIT ?3")
        .map_err(sql)?;
    q.query_map(
        params![
            conversation.0.as_slice(),
            recipient.0.as_slice(),
            PENDING_LIMIT
        ],
        |r| {
            Ok(DueDelivery {
                conversation,
                recipient,
                kind: r.get(0)?,
                epoch: uint(r, 1)?,
                body: r.get(2)?,
            })
        },
    )
    .map_err(sql)?
    .collect::<Result<_, _>>()
    .map_err(sql)
}

#[allow(clippy::too_many_arguments)]
fn delivered(
    c: &mut Connection,
    conversation: ConversationId,
    recipient: InstallationId,
    kind: u8,
    epoch: u64,
    outcome: Delivery,
    now: u64,
    fault: Fault,
) -> R<()> {
    let tx = c.transaction().map_err(sql)?;
    let epoch_value = integer(epoch)?;
    let key = params![
        conversation.0.as_slice(),
        recipient.0.as_slice(),
        kind,
        epoch_value
    ];
    match outcome {
        Delivery::Acked | Delivery::Settled => {
            tx.execute(
                "DELETE FROM mls_delivery WHERE conversation=?1 AND recipient_install=?2 AND kind=?3 AND epoch=?4",
                key,
            )
            .map_err(sql)?;
            // An acknowledgement up to the current epoch confirms the own Commit; so does the
            // last Commit delivery of the current epoch being settled.
            if let Some(current) = load_row(&tx, conversation)?.map(|row| row.epoch)
                && epoch <= current
            {
                let open: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM mls_delivery WHERE conversation=?1 AND kind=?2 AND epoch=?3)",
                        params![conversation.0.as_slice(), DELIVER_COMMIT, integer(current)?],
                        |r| r.get(0),
                    )
                    .map_err(sql)?;
                if outcome == Delivery::Acked || !open {
                    tx.execute(
                        "UPDATE mls_conversation SET own_commit_unacked=0 WHERE conversation=?1",
                        params![conversation.0.as_slice()],
                    )
                    .map_err(sql)?;
                }
            }
        }
        Delivery::Retry => {
            let attempts: Option<u64> = tx
                .query_row(
                    "SELECT attempts FROM mls_delivery WHERE conversation=?1 AND recipient_install=?2 AND kind=?3 AND epoch=?4",
                    key,
                    |r| uint(r, 0),
                )
                .optional()
                .map_err(sql)?;
            if let Some(attempts) = attempts {
                // The M4 receipt back-off: 1 s doubling to 5 min, kept indefinitely.
                let attempts = attempts.saturating_add(1).min(u64::from(u32::MAX));
                let next = now.saturating_add(super::scheduler::retry_delay(attempts));
                tx.execute(
                    "UPDATE mls_delivery SET attempts=?5,next_retry_at_ms=?6 WHERE conversation=?1 AND recipient_install=?2 AND kind=?3 AND epoch=?4",
                    params![
                        conversation.0.as_slice(),
                        recipient.0.as_slice(),
                        kind,
                        integer(epoch)?,
                        integer(attempts)?,
                        integer(next)?
                    ],
                )
                .map_err(sql)?;
            }
        }
    }
    finish(tx, fault)
}

/// Processes the holds waiting on `lookup`'s record again, each in its own transaction.
fn reprocess(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    lookup: PeerId,
    drop_unknown: bool,
    now: u64,
    wall: u64,
) -> R<Vec<(ConversationId, PeerId, Outcome)>> {
    let held: Vec<([u8; 32], [u8; 32], Vec<u8>)> = {
        let mut q = c
            .prepare("SELECT conversation,body_hash,body FROM mls_hold WHERE sender_peer=?1 ORDER BY held_at_ms,rowid")
            .map_err(sql)?;
        q.query_map(params![lookup.0.as_slice()], |r| {
            Ok((bytes(r, 0)?, bytes(r, 1)?, r.get(2)?))
        })
        .map_err(sql)?
        .collect::<Result<_, _>>()
        .map_err(sql)?
    };
    let mut settled = Vec::new();
    for (conversation, hash, body) in held {
        let conversation = ConversationId(conversation);
        let outcome = match reprocess_one(c, identity, local, conversation, &body, now, wall) {
            // Storage trouble leaves the hold for the next pass.
            Err(error) if error.code == ErrorCode::StorageUncertain => return Err(error),
            outcome => outcome,
        };
        let still_unknown = matches!(outcome, Ok(Outcome::Held(_)));
        if still_unknown && !drop_unknown {
            continue;
        }
        c.execute(
            "DELETE FROM mls_hold WHERE conversation=?1 AND body_hash=?2",
            params![conversation.0.as_slice(), hash.as_slice()],
        )
        .map_err(sql)?;
        let Ok(outcome) = outcome else { continue };
        let peer = PeerId(body[65..97].try_into().map_err(|_| invalid())?);
        if still_unknown {
            // A Commit or Welcome this device had to refuse for good: it can no longer follow
            // the group and rejoins (spec 4.6). A refused JoinRequest needs nothing more; the
            // asking device asks again.
            if body[0] != HOLD_JOIN_REQUEST {
                settled.push((conversation, peer, Outcome::Rejoin { epoch: 0 }));
            }
        } else {
            settled.push((conversation, peer, outcome));
        }
    }
    Ok(settled)
}

fn reprocess_one(
    c: &mut Connection,
    identity: Identity,
    local: &LocalAccount,
    conversation: ConversationId,
    body: &[u8],
    now: u64,
    wall: u64,
) -> R<Outcome> {
    if body.len() < HOLD_HEADER {
        return Err(invalid());
    }
    let tag = body[0];
    let first_commit_hash: [u8; 32] = body[1..33].try_into().map_err(|_| invalid())?;
    let arrival = Arrival {
        sender_peer: PeerId(body[33..65].try_into().map_err(|_| invalid())?),
        peer: PeerId(body[65..97].try_into().map_err(|_| invalid())?),
        hold: false,
        now,
        wall,
    };
    match (tag, MlsFrame::decode(&body[HOLD_HEADER..])?) {
        (
            HOLD_WELCOME,
            MlsFrame::Welcome {
                conversation: id,
                welcome,
                sender_install,
            },
        ) if id == conversation => join(
            c,
            identity,
            local,
            conversation,
            arrival,
            Welcome {
                sender_install,
                welcome: &welcome,
                first_commit_hash,
                rejoin: None,
                joined_with: None,
            },
            NO_FAULT,
        ),
        (
            HOLD_COMMIT,
            MlsFrame::Commit {
                conversation: id,
                epoch,
                commit,
            },
        ) if id == conversation => apply(
            c,
            identity,
            local,
            conversation,
            arrival,
            epoch,
            &commit,
            NO_FAULT,
        ),
        (
            HOLD_JOIN_REQUEST,
            MlsFrame::JoinRequest {
                conversation: id,
                key_package,
            },
        ) if id == conversation => {
            Ok(
                match join_request(
                    c,
                    identity,
                    local,
                    conversation,
                    arrival,
                    &key_package,
                    NO_FAULT,
                )? {
                    Committed::Done { .. } => Outcome::Ack,
                    Committed::Held(lookup) => Outcome::Held(lookup),
                    // The asking device asks again when its wait is over.
                    Committed::Busy => Outcome::Failed {
                        class: group::CLASS_OTHER,
                        epoch: 0,
                    },
                },
            )
        }
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{
        DeviceKeys, DeviceKind, Generation, RecordDevice, Secret32, derive_account, derive_device,
        record_packet,
    };

    fn device(seed: u8) -> DeviceKeys {
        derive_device(Secret32::from([seed; 32])).unwrap()
    }
    fn listing(devices: &[&DeviceKeys]) -> IdentityRecord {
        let mut devices: Vec<RecordDevice> = devices
            .iter()
            .map(|d| RecordDevice {
                installation_id: d.installation_id(),
                transport_public_key: d.transport_public_key(),
                kind: DeviceKind::Mobile,
            })
            .collect();
        devices.sort_by_key(|d| d.installation_id);
        IdentityRecord {
            generation: Generation([1; 16]),
            issued_at: 1_000,
            expires_at: 2_000,
            devices,
        }
    }
    fn leaf(peer: PeerId, device: &DeviceKeys) -> (Vec<u8>, Vec<u8>) {
        (
            MlsCredential::issue(peer, device).encode(),
            mls_signer(device).public().to_vec(),
        )
    }

    /// Ruling 6: a leaf is only ever checked against the record of the account its credential
    /// names. Account C lists B's device in its own record; B's valid credential must not pass
    /// because of C's record, and C's record never parses as B's.
    #[test]
    fn credential_is_checked_only_against_its_own_accounts_record() {
        let (a, b, c) = (device(1), device(2), device(3));
        let keys_c = derive_account(&Secret32::from([30; 32])).unwrap();
        let (peer_a, peer_b, peer_c) = (PeerId([10; 32]), PeerId([20; 32]), keys_c.peer_id());
        // C's signed record copies B's device next to its own.
        let record_c = listing(&[&b, &c]);
        let packet_c = record_packet(&keys_c, &record_c, 1).unwrap();
        assert!(parse_stored_record(&packet_c, peer_c).is_ok());
        assert!(
            parse_stored_record(&packet_c, peer_b).is_err(),
            "another account's packet is never B's record"
        );
        let (credential_b, key_b) = leaf(peer_b, &b);
        // On its own, verify_leaf would accept B's leaf against C's record (the hazard).
        assert!(verify_leaf(&credential_b, &key_b, peer_b, &record_c).is_ok());

        // In a conversation between A and C, B's credential is refused outright.
        let records = Records {
            own_peer: peer_a,
            own: listing(&[&a]),
            peer: peer_c,
            remote: record_c.clone(),
        };
        let vetting = Vetting::new(&records);
        let error = vetting.check(&credential_b, &key_b).unwrap_err();
        assert_eq!(error.code, ErrorCode::AuthFailed);
        assert_eq!(vetting.held(), None, "refused, not held for a lookup");

        // The same leaf in a conversation with B is checked against B's own record only: a
        // record that does not list the device holds it, a matching one accepts it.
        let records = Records {
            own_peer: peer_a,
            own: listing(&[&a]),
            peer: peer_b,
            remote: listing(&[&c]),
        };
        let vetting = Vetting::new(&records);
        let error = vetting.check(&credential_b, &key_b).unwrap_err();
        assert_eq!(error.code, ErrorCode::RecordUnavailable);
        assert_eq!(vetting.held(), Some(peer_b));
        let records = Records {
            own_peer: peer_a,
            own: listing(&[&a]),
            peer: peer_b,
            remote: listing(&[&b]),
        };
        assert!(Vetting::new(&records).check(&credential_b, &key_b).is_ok());
    }
}
