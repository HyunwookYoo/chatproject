//! Membership and fork decisions for a 1:1 conversation's MLS group.
//!
//! Everything here is synchronous and knows nothing about SQL: functions work on one
//! `MlsProvider`, whose in-memory `KvStore` the caller flushes in its SQL transaction when the
//! whole unit of work succeeded (and drops when it did not). The group changes only by by-value
//! Add/Remove Commits; there are no standing proposals.
use std::cmp::Ordering;

use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use sha2::{Digest, Sha256};

use super::MlsProvider;
use super::group::{
    CLASS_NO_GROUP, CLASS_OTHER, LeafVerifier, MAX_MLS_MESSAGE_BYTES, OpenFailure,
    checked_key_package, classify, fail, group_id_of, verify_leaf_node,
};
use super::identity::MlsCredential;
use crate::account::{IdentityRecord, InstallationId};
use crate::{CoreError, ErrorCode};

type R<T> = Result<T, CoreError>;

/// Rejoins allowed per conversation per window.
pub(crate) const REJOIN_LIMIT: u32 = 3;
/// After the limit is reached, no further rejoin until this long has passed (24 hours).
pub(crate) const REJOIN_WINDOW_MS: u64 = 86_400_000;
/// Minimum time between two rejoins of one conversation.
pub(crate) const REJOIN_MIN_GAP_MS: u64 = 60_000;

fn invalid(detail: &'static str) -> CoreError {
    CoreError::new(ErrorCode::InvalidMessage, detail)
}

/// The installation a leaf credential names, if it is one of ours.
fn installation_of(credential: &Credential) -> Option<InstallationId> {
    let basic = BasicCredential::try_from(credential.clone()).ok()?;
    MlsCredential::decode(basic.identity())
        .ok()
        .map(|credential| credential.installation)
}

fn load_group(p: &MlsProvider, group_id: &[u8]) -> R<MlsGroup> {
    MlsGroup::load(p.storage(), &group_id_of(group_id))
        .map_err(|_| invalid("mls group state unreadable"))?
        .ok_or_else(|| CoreError::new(ErrorCode::NotAvailable, "mls group not found"))
}

/// The installations that currently hold a leaf in the group.
pub(crate) fn group_installations(p: &MlsProvider, group_id: &[u8]) -> R<Vec<InstallationId>> {
    load_group(p, group_id)?
        .members()
        .map(|member| {
            installation_of(&member.credential).ok_or_else(|| invalid("invalid mls credential"))
        })
        .collect()
}

/// Binary snapshot of the conversation's group state, for `restore_snapshot`.
pub(crate) fn snapshot(p: &MlsProvider) -> R<Vec<u8>> {
    p.storage().snapshot()
}

/// Puts the group back into the state a snapshot captured. Only one snapshot is ever kept, so
/// this reaches back one epoch at most. The caller flushes afterwards like for any other change.
pub(crate) fn restore_snapshot(p: &MlsProvider, snapshot: &[u8]) -> R<()> {
    p.storage().restore(snapshot)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct MemberDiff {
    pub add: Vec<InstallationId>,
    pub remove: Vec<InstallationId>,
}

/// Compares the group's leaves with the two accounts' current device lists (spec 4.5): a listed
/// device without a leaf is added, a leaf whose device is in neither list is removed.
pub(crate) fn diff_members(
    group_leaves: &[InstallationId],
    local: &IdentityRecord,
    peer: &IdentityRecord,
) -> MemberDiff {
    let mut wanted: Vec<InstallationId> = Vec::new();
    for device in local.devices.iter().chain(peer.devices.iter()) {
        if !wanted.contains(&device.installation_id) {
            wanted.push(device.installation_id);
        }
    }
    let mut diff = MemberDiff::default();
    for installation in &wanted {
        if !group_leaves.contains(installation) {
            diff.add.push(*installation);
        }
    }
    for installation in group_leaves {
        if !wanted.contains(installation) && !diff.remove.contains(installation) {
            diff.remove.push(*installation);
        }
    }
    diff
}

pub(crate) struct Committed {
    /// `MlsMessageOut` TLS of the Commit, for every member that was in the group before.
    pub commit: Vec<u8>,
    /// `MlsMessageOut` TLS of the Welcome for the added members, if any were added.
    pub welcome: Option<Vec<u8>>,
    /// The epoch the Commit has already taken this device to.
    pub epoch: u64,
    /// The group state before the Commit, to keep as `previous_epoch_snapshot`.
    pub previous: Vec<u8>,
}

/// One by-value Commit that adds `add_key_packages` and removes `remove`, merged immediately.
///
/// Every KeyPackage's leaf goes through `verify` first; a rejection (or any other error) creates
/// no Commit and leaves the group as it was. Leaves are identified by the `InstallationId`
/// inside their credential, and a device cannot remove itself.
pub(crate) fn commit_changes(
    p: &MlsProvider,
    signer: &SignatureKeyPair,
    group_id: &[u8],
    add_key_packages: &[Vec<u8>],
    remove: &[InstallationId],
    verify: LeafVerifier<'_>,
) -> R<Committed> {
    if add_key_packages.is_empty() && remove.is_empty() {
        return Err(invalid("mls commit changes nothing"));
    }
    let key_packages = add_key_packages
        .iter()
        .map(|bytes| checked_key_package(p, bytes, verify))
        .collect::<R<Vec<_>>>()?;
    let mut group = load_group(p, group_id)?;
    let mut removed = Vec::new();
    for installation in remove {
        let index = group
            .members()
            .find(|member| installation_of(&member.credential) == Some(*installation))
            .map(|member| member.index)
            .ok_or_else(|| invalid("mls member to remove not found"))?;
        if index == group.own_leaf_index() {
            return Err(invalid("mls device cannot remove itself"));
        }
        if !removed.contains(&index) {
            removed.push(index);
        }
    }
    let previous = snapshot(p)?;
    match build_commit(p, signer, &mut group, key_packages, removed) {
        Ok((commit, welcome, epoch)) => Ok(Committed {
            commit,
            welcome,
            epoch,
            previous,
        }),
        Err(error) => {
            // Staging writes the pending Commit into the store; take it back out.
            let _ = restore_snapshot(p, &previous);
            Err(error)
        }
    }
}

fn build_commit(
    p: &MlsProvider,
    signer: &SignatureKeyPair,
    group: &mut MlsGroup,
    key_packages: Vec<KeyPackage>,
    removed: Vec<LeafNodeIndex>,
) -> R<(Vec<u8>, Option<Vec<u8>>, u64)> {
    let bundle = group
        .commit_builder()
        .force_self_update(true)
        .propose_adds(key_packages)
        .propose_removals(removed)
        .load_psks(p.storage())
        .map_err(|_| invalid("mls commit failed"))?
        .build(p.rand(), p.crypto(), signer, |_| true)
        .map_err(|_| invalid("mls commit failed"))?
        .stage_commit(p)
        .map_err(|_| invalid("mls commit failed"))?;
    let encode = |message: &MlsMessageOut| {
        message
            .tls_serialize_detached()
            .ok()
            .filter(|bytes| bytes.len() <= MAX_MLS_MESSAGE_BYTES)
            .ok_or_else(|| invalid("mls commit too large"))
    };
    let commit = encode(bundle.commit())?;
    let welcome = bundle.to_welcome_msg().as_ref().map(encode).transpose()?;
    group
        .merge_pending_commit(p)
        .map_err(|_| invalid("mls commit merge failed"))?;
    Ok((commit, welcome, group.epoch().as_u64()))
}

#[derive(Debug)]
pub(crate) struct Applied {
    /// The epoch the group is at now.
    pub epoch: u64,
    /// SHA-256 of the Commit's TLS bytes, as `Created::first_commit_hash` is for the first one.
    pub commit_hash: [u8; 32],
    /// The group state before the Commit, to keep as `previous_epoch_snapshot`.
    pub previous: Vec<u8>,
    /// The Commit removed this device; the group is finished here and the caller drops it.
    pub self_removed: bool,
}

/// Applies a received Commit. Only Add and Remove proposals are accepted, and `verify` vets the
/// leaf of every member the Commit adds and of the committer's new leaf before anything is
/// merged. On any failure the group is left exactly as it was.
///
/// `verify` is the caller's own closure, so it can note why it refused (for instance a device
/// that is not in the cached record yet) even though a refusal here is just `CLASS_OTHER`.
pub(crate) fn apply_commit(
    p: &MlsProvider,
    group_id: &[u8],
    commit: &[u8],
    verify: LeafVerifier<'_>,
) -> Result<Applied, OpenFailure> {
    if commit.len() > MAX_MLS_MESSAGE_BYTES {
        return Err(fail(CLASS_OTHER));
    }
    let protocol: ProtocolMessage = MlsMessageIn::tls_deserialize_exact(commit)
        .ok()
        .and_then(|message| message.try_into_protocol_message().ok())
        .ok_or(fail(CLASS_OTHER))?;
    let mut group = MlsGroup::load(p.storage(), &group_id_of(group_id))
        .map_err(|_| fail(CLASS_OTHER))?
        .ok_or(fail(CLASS_NO_GROUP))?;
    if protocol.group_id() != group.group_id() {
        return Err(fail(CLASS_OTHER));
    }
    let previous = snapshot(p).map_err(|_| fail(CLASS_OTHER))?;
    match apply_staged(p, &mut group, protocol, verify) {
        Ok((epoch, self_removed)) => Ok(Applied {
            epoch,
            commit_hash: Sha256::digest(commit).into(),
            previous,
            self_removed,
        }),
        Err(failure) => {
            let _ = restore_snapshot(p, &previous);
            Err(failure)
        }
    }
}

fn apply_staged(
    p: &MlsProvider,
    group: &mut MlsGroup,
    protocol: ProtocolMessage,
    verify: LeafVerifier<'_>,
) -> Result<(u64, bool), OpenFailure> {
    let (message_epoch, current) = (protocol.epoch().as_u64(), group.epoch().as_u64());
    let processed = group
        .process_message(p, protocol)
        .map_err(|error| fail(classify(&error, message_epoch, current)))?;
    let ProcessedMessageContent::StagedCommitMessage(staged) = processed.into_content() else {
        return Err(fail(CLASS_OTHER));
    };
    if staged
        .queued_proposals()
        .any(|queued| !matches!(queued.proposal(), Proposal::Add(_) | Proposal::Remove(_)))
    {
        return Err(fail(CLASS_OTHER));
    }
    for added in staged.add_proposals() {
        verify_leaf_node(added.add_proposal().key_package().leaf_node(), verify)
            .map_err(|_| fail(CLASS_OTHER))?;
    }
    if let Some(leaf) = staged.update_path_leaf_node() {
        verify_leaf_node(leaf, verify).map_err(|_| fail(CLASS_OTHER))?;
    }
    let self_removed = staged.self_removed();
    group
        .merge_staged_commit(p, *staged)
        .map_err(|_| fail(CLASS_OTHER))?;
    Ok((group.epoch().as_u64(), self_removed))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForkDecision {
    /// Stay on this device's epoch; the other side will follow (or rejoin).
    KeepMine,
    /// Restore the previous-epoch snapshot, then apply the other Commit.
    AdoptTheirs,
    /// Drop the group state and rejoin through a Welcome (spec 4.6).
    Rejoin,
}

/// Decides between two Commits, one this device is at (`my_*`) and one it just received
/// (`their_*`), each given as the epoch it leads to and the hash of its TLS bytes.
///
/// Only a fork of the same epoch can be settled, and the smaller hash wins (the same answer on
/// every device). Losing it needs the one stored previous state (`have_previous`) to switch
/// over; without it, or when the epochs differ (the fork is deeper than that one state, or a
/// Commit is missing), the device rejoins.
pub(crate) fn resolve_fork(
    my_epoch: u64,
    my_commit_hash: [u8; 32],
    their_epoch: u64,
    their_commit_hash: [u8; 32],
    have_previous: bool,
) -> ForkDecision {
    if my_epoch != their_epoch {
        return ForkDecision::Rejoin;
    }
    match my_commit_hash.cmp(&their_commit_hash) {
        Ordering::Less | Ordering::Equal => ForkDecision::KeepMine,
        Ordering::Greater if have_previous => ForkDecision::AdoptTheirs,
        Ordering::Greater => ForkDecision::Rejoin,
    }
}

/// Which of two groups created for one conversation wins: the one with the smaller first-Commit
/// hash (`Less` means `a` wins). Age and size play no part, so every device agrees.
pub(crate) fn choose_group(
    a_first_commit_hash: [u8; 32],
    b_first_commit_hash: [u8; 32],
) -> Ordering {
    a_first_commit_hash.cmp(&b_first_commit_hash)
}

/// Whether a conversation may rejoin now: fewer than `REJOIN_LIMIT` rejoins in the window and
/// not before `after_ms`.
pub(crate) fn may_rejoin(count: u32, after_ms: u64, now_ms: u64) -> bool {
    count < REJOIN_LIMIT && now_ms >= after_ms
}

/// `(rejoin_count, rejoin_after_ms)` to store after a rejoin at `now_ms`: the next one waits
/// `REJOIN_MIN_GAP_MS`, and the one that reaches the limit blocks the conversation for
/// `REJOIN_WINDOW_MS`.
pub(crate) fn after_rejoin(count: u32, now_ms: u64) -> (u32, u64) {
    let count = count.saturating_add(1);
    let wait = if count >= REJOIN_LIMIT {
        REJOIN_WINDOW_MS
    } else {
        REJOIN_MIN_GAP_MS
    };
    (count, now_ms.saturating_add(wait))
}

/// The stored count as `may_rejoin` should see it: a conversation that reached the limit
/// starts a fresh window once its block (`after_ms`) has passed.
pub(crate) fn rejoin_count_now(count: u32, after_ms: u64, now_ms: u64) -> u32 {
    if count >= REJOIN_LIMIT && now_ms >= after_ms {
        0
    } else {
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{
        DeviceKeys, DeviceKind, Generation, PeerId, RecordDevice, Secret32, derive_device,
    };
    use crate::message::types::{ConversationId, MessageId};
    use crate::mls::group::{
        CLASS_FUTURE_EPOCH, Created, Opened, create_group, join, new_key_package, open, seal,
    };
    use crate::mls::identity::{mls_signer, verify_leaf};
    use crate::mls::kv::KvStore;
    use crate::mls::payload::{ChatMessage, Content};
    use openmls_basic_credential::SignatureKeyPair;
    use rusqlite::Connection;

    const CONV: ConversationId = ConversationId([5; 32]);

    fn hash(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    fn database() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        c.execute_batch(include_str!("../storage/schema.sql"))
            .unwrap();
        c
    }

    /// One installation of one account, with its own in-memory MLS state.
    struct Dev {
        provider: MlsProvider,
        device: DeviceKeys,
        signer: SignatureKeyPair,
        credential: MlsCredential,
        peer: PeerId,
    }
    impl Dev {
        /// `account` picks the PeerId, `seed` the device.
        fn new(account: u8, seed: u8) -> Self {
            let device = derive_device(Secret32::from([seed; 32])).unwrap();
            let peer = PeerId([account; 32]);
            Self {
                provider: MlsProvider::new(KvStore::load(&database(), CONV.0).unwrap()),
                signer: mls_signer(&device),
                credential: MlsCredential::issue(peer, &device),
                device,
                peer,
            }
        }
        fn installation(&self) -> InstallationId {
            self.device.installation_id()
        }
        fn key_package(&self) -> Vec<u8> {
            new_key_package(&self.provider, &self.signer, &self.credential).unwrap()
        }
        fn create(&self, members: &[Vec<u8>], verify: LeafVerifier<'_>) -> Created {
            create_group(
                &self.provider,
                &self.signer,
                &self.credential,
                CONV,
                members,
                verify,
            )
            .unwrap()
        }
        fn join(&self, welcome: &[u8], verify: LeafVerifier<'_>) -> Vec<u8> {
            join(&self.provider, welcome, verify).unwrap().group_id
        }
        fn commit(
            &self,
            gid: &[u8],
            add: &[Vec<u8>],
            remove: &[InstallationId],
            verify: LeafVerifier<'_>,
        ) -> R<Committed> {
            commit_changes(&self.provider, &self.signer, gid, add, remove, verify)
        }
        fn apply(
            &self,
            gid: &[u8],
            commit: &[u8],
            verify: LeafVerifier<'_>,
        ) -> Result<Applied, OpenFailure> {
            apply_commit(&self.provider, gid, commit, verify)
        }
        fn seal(&self, gid: &[u8], n: u8) -> Vec<u8> {
            seal(&self.provider, &self.signer, gid, &chat(n)).unwrap()
        }
        fn open(&self, gid: &[u8], ciphertext: &[u8]) -> Result<Opened, OpenFailure> {
            open(&self.provider, gid, ciphertext)
        }
        fn epoch(&self, gid: &[u8]) -> u64 {
            load_group(&self.provider, gid).unwrap().epoch().as_u64()
        }
        fn leaves(&self, gid: &[u8]) -> Vec<InstallationId> {
            let mut leaves = group_installations(&self.provider, gid).unwrap();
            leaves.sort_by_key(|id| id.0);
            leaves
        }
    }

    fn chat(n: u8) -> ChatMessage {
        ChatMessage {
            msg_id: MessageId([n; 16]),
            lamport: u64::from(n),
            prev_hash: [n; 32],
            sent_at_ms: 1_700_000_000_000 + u64::from(n),
            content: Content::Text {
                body: format!("message {n}"),
            },
        }
    }

    /// `from` seals message `n`, `to` opens it and sees the same text.
    fn talks(from: &Dev, to: &Dev, gid: &[u8], n: u8) {
        let ct = from.seal(gid, n);
        match to.open(gid, &ct).unwrap() {
            Opened::Application { sender, message } => {
                assert_eq!(sender, from.installation());
                assert_eq!(message, chat(n));
            }
            other => panic!("{other:?}"),
        }
    }

    fn record(devices: &[&Dev]) -> IdentityRecord {
        let mut devices: Vec<RecordDevice> = devices
            .iter()
            .map(|dev| RecordDevice {
                installation_id: dev.installation(),
                transport_public_key: dev.device.transport_public_key(),
                kind: DeviceKind::Mobile,
            })
            .collect();
        devices.sort_by_key(|device| device.installation_id.0);
        IdentityRecord {
            generation: Generation([1; 16]),
            issued_at: 10,
            expires_at: 20,
            devices,
        }
    }

    /// Accepts a leaf only when `verify_leaf` finds it in the record of its account, built from
    /// the devices given for that account.
    fn verifier(devs: &[&Dev]) -> impl Fn(&[u8], &[u8]) -> R<()> + use<> {
        let mut records: Vec<(PeerId, IdentityRecord)> = Vec::new();
        for dev in devs {
            if records.iter().all(|(peer, _)| *peer != dev.peer) {
                let same: Vec<&Dev> = devs
                    .iter()
                    .copied()
                    .filter(|d| d.peer == dev.peer)
                    .collect();
                records.push((dev.peer, record(&same)));
            }
        }
        move |credential, key| {
            let peer = MlsCredential::decode(credential)?.peer;
            let (_, record) = records
                .iter()
                .find(|(candidate, _)| *candidate == peer)
                .ok_or_else(|| invalid("unknown peer"))?;
            verify_leaf(credential, key, peer, record).map(|_| ())
        }
    }

    fn reject_all(_: &[u8], _: &[u8]) -> R<()> {
        Err(CoreError::new(ErrorCode::AuthFailed, "rejected"))
    }

    /// A1 creates the group and adds `others`, which join; returns the group id.
    fn group_of(a: &Dev, others: &[&Dev]) -> Vec<u8> {
        let mut all = vec![a];
        all.extend_from_slice(others);
        let verify = verifier(&all);
        let key_packages: Vec<Vec<u8>> = others.iter().map(|dev| dev.key_package()).collect();
        let created = a.create(&key_packages, &verify);
        for dev in others {
            assert_eq!(dev.join(&created.welcome, &verify), created.group_id);
        }
        created.group_id
    }

    fn record_of(devs: &[&Dev]) -> IdentityRecord {
        record(devs)
    }

    #[test]
    fn diff_adds_new_and_removes_revoked() {
        let (a1, a2, b1, b2, old) = (
            Dev::new(1, 1),
            Dev::new(1, 2),
            Dev::new(2, 3),
            Dev::new(2, 4),
            Dev::new(2, 5),
        );
        let (local, peer) = (record_of(&[&a1, &a2]), record_of(&[&b1, &b2]));
        // A2 and B2 have no leaf yet, and `old` (a revoked device) still has one.
        let leaves = [a1.installation(), b1.installation(), old.installation()];
        let diff = diff_members(&leaves, &local, &peer);
        assert_eq!(
            diff,
            MemberDiff {
                add: vec![a2.installation(), b2.installation()],
                remove: vec![old.installation()],
            }
        );
        // Nothing to do when the group already matches both lists.
        let all = [
            a1.installation(),
            a2.installation(),
            b1.installation(),
            b2.installation(),
        ];
        assert_eq!(diff_members(&all, &local, &peer), MemberDiff::default());
        // An empty group wants everything; a device listed by both accounts is added once.
        let both = diff_members(&[], &local, &local);
        let in_record: Vec<InstallationId> =
            local.devices.iter().map(|d| d.installation_id).collect();
        assert_eq!(both.add, in_record);
        assert!(both.remove.is_empty());
    }

    #[test]
    fn any_member_can_add_other_accounts_device() {
        let (a1, a2, b1) = (Dev::new(1, 1), Dev::new(1, 2), Dev::new(2, 3));
        let gid = group_of(&a1, &[&b1]);
        let verify = verifier(&[&a1, &a2, &b1]);

        // B1 did not create the group, yet it adds A's second device.
        let committed = b1.commit(&gid, &[a2.key_package()], &[], &verify).unwrap();
        assert_eq!(committed.epoch, 2);
        let welcome = committed.welcome.expect("an Add makes a Welcome");
        let applied = a1.apply(&gid, &committed.commit, &verify).unwrap();
        assert_eq!(applied.epoch, 2);
        assert!(!applied.self_removed);
        assert_eq!(applied.commit_hash, hash(&committed.commit));
        assert_eq!(a2.join(&welcome, &verify), gid);

        // All three installations are in the group and talk to each other.
        let mut expected = vec![a1.installation(), a2.installation(), b1.installation()];
        expected.sort_by_key(|id| id.0);
        for dev in [&a1, &a2, &b1] {
            assert_eq!(dev.leaves(&gid), expected);
        }
        talks(&a2, &b1, &gid, 1);
        talks(&b1, &a1, &gid, 2);
        talks(&a1, &a2, &gid, 3);
        // The lists now match the group.
        assert_eq!(
            diff_members(
                &a1.leaves(&gid),
                &record_of(&[&a1, &a2]),
                &record_of(&[&b1])
            ),
            MemberDiff::default()
        );
    }

    #[test]
    fn commit_rejects_unverified_key_package() {
        let (a1, b1, a2) = (Dev::new(1, 1), Dev::new(2, 2), Dev::new(1, 3));
        let gid = group_of(&a1, &[&b1]);
        let before = snapshot(&a1.provider).unwrap();

        let error = a1
            .commit(&gid, &[a2.key_package()], &[], &reject_all)
            .err()
            .expect("an unverified KeyPackage must not be committed");
        assert_eq!(error.code, ErrorCode::AuthFailed);
        assert_eq!(a1.epoch(&gid), 1);
        assert_eq!(snapshot(&a1.provider).unwrap(), before);

        // One bad KeyPackage among good ones stops the whole Commit.
        let c1 = Dev::new(3, 4);
        let only_a = verifier(&[&a1, &a2, &b1]);
        let error = a1
            .commit(&gid, &[a2.key_package(), c1.key_package()], &[], &only_a)
            .err()
            .expect("an unknown account must not be committed");
        assert_eq!(error.code, ErrorCode::InvalidMessage);
        assert_eq!(snapshot(&a1.provider).unwrap(), before);

        // Garbage, nothing to change, an unknown member and removing oneself all fail too.
        let ok = verifier(&[&a1, &a2, &b1]);
        assert!(a1.commit(&gid, &[vec![1, 2, 3]], &[], &ok).is_err());
        assert!(a1.commit(&gid, &[], &[], &ok).is_err());
        assert!(a1.commit(&gid, &[], &[a2.installation()], &ok).is_err());
        assert!(a1.commit(&gid, &[], &[a1.installation()], &ok).is_err());
        assert_eq!(snapshot(&a1.provider).unwrap(), before);
        // The group still works, and the same KeyPackage verifies once allowed.
        assert!(a1.commit(&gid, &[a2.key_package()], &[], &ok).is_ok());
    }

    #[test]
    fn apply_commit_rejects_unverified_added_leaf() {
        let (a1, b1, a2) = (Dev::new(1, 1), Dev::new(2, 2), Dev::new(1, 3));
        let gid = group_of(&a1, &[&b1]);
        let allow = verifier(&[&a1, &a2, &b1]);
        let committed = a1.commit(&gid, &[a2.key_package()], &[], &allow).unwrap();

        let before = snapshot(&b1.provider).unwrap();
        // B's record of A does not list A2 yet.
        let stale = verifier(&[&a1, &b1]);
        let failure = b1.apply(&gid, &committed.commit, &stale).unwrap_err();
        assert_eq!(failure.class, CLASS_OTHER);
        assert_eq!(b1.epoch(&gid), 1);
        assert_eq!(snapshot(&b1.provider).unwrap(), before);
        // Nothing was consumed: once the record catches up the same Commit applies.
        let applied = b1.apply(&gid, &committed.commit, &allow).unwrap();
        assert_eq!(applied.epoch, 2);
        assert_eq!(applied.previous, before);
    }

    #[test]
    fn apply_commit_rejects_what_is_not_a_commit() {
        let (a1, b1) = (Dev::new(1, 1), Dev::new(2, 2));
        let gid = group_of(&a1, &[&b1]);
        let ok = verifier(&[&a1, &b1]);
        let ct = a1.seal(&gid, 1);
        assert_eq!(b1.apply(&gid, &ct, &ok).unwrap_err().class, CLASS_OTHER);
        assert_eq!(b1.apply(&gid, &[], &ok).unwrap_err().class, CLASS_OTHER);
        assert_eq!(
            b1.apply(&[9; 48], &ct, &ok).unwrap_err().class,
            crate::mls::group::CLASS_NO_GROUP
        );
        // A Commit that changes nothing is not made.
        assert!(a1.commit(&gid, &[], &[], &ok).is_err());
        // A Commit for an epoch this device has not reached is reported as missing one.
        let a2 = Dev::new(1, 3);
        let all = verifier(&[&a1, &a2, &b1]);
        let first = a1.commit(&gid, &[a2.key_package()], &[], &all).unwrap();
        let second = a1.commit(&gid, &[], &[a2.installation()], &all).unwrap();
        assert_eq!(
            b1.apply(&gid, &second.commit, &all).unwrap_err().class,
            CLASS_FUTURE_EPOCH
        );
        b1.apply(&gid, &first.commit, &all).unwrap();
        assert_eq!(b1.apply(&gid, &second.commit, &all).unwrap().epoch, 3);
    }

    /// A1 and B1 both commit from epoch 1 (each adds a device of its own account).
    fn competing() -> (Dev, Dev, Vec<u8>, Committed, Committed, Dev, Dev) {
        let (a1, a2, b1, b2) = (
            Dev::new(1, 1),
            Dev::new(1, 2),
            Dev::new(2, 3),
            Dev::new(2, 4),
        );
        let gid = group_of(&a1, &[&b1]);
        let verify = verifier(&[&a1, &a2, &b1, &b2]);
        let by_a = a1.commit(&gid, &[a2.key_package()], &[], &verify).unwrap();
        let by_b = b1.commit(&gid, &[b2.key_package()], &[], &verify).unwrap();
        (a1, b1, gid, by_a, by_b, a2, b2)
    }

    #[test]
    fn same_epoch_fork_smaller_hash_wins_on_both_sides() {
        // Hashes are random, so run enough forks to see both devices win.
        let mut winners = [0u32; 2];
        for _ in 0..12 {
            let (a1, b1, gid, by_a, by_b, a2, b2) = competing();
            assert_eq!((by_a.epoch, by_b.epoch), (2, 2));
            let verify = verifier(&[&a1, &a2, &b1, &b2]);
            let (ha, hb) = (hash(&by_a.commit), hash(&by_b.commit));
            assert_ne!(ha, hb);

            // A1 and B1 each hold their own epoch 2 and receive the other's Commit.
            let a_view = resolve_fork(by_a.epoch, ha, by_b.epoch, hb, true);
            let b_view = resolve_fork(by_b.epoch, hb, by_a.epoch, ha, true);
            let (winner, loser, winner_commit, loser_view, loser_previous, new_device) = if ha < hb
            {
                winners[0] += 1;
                assert_eq!(
                    (a_view, b_view),
                    (ForkDecision::KeepMine, ForkDecision::AdoptTheirs)
                );
                (&a1, &b1, &by_a, b_view, &by_b.previous, &a2)
            } else {
                winners[1] += 1;
                assert_eq!(
                    (b_view, a_view),
                    (ForkDecision::KeepMine, ForkDecision::AdoptTheirs)
                );
                (&b1, &a1, &by_b, a_view, &by_a.previous, &b2)
            };
            assert_eq!(loser_view, ForkDecision::AdoptTheirs);

            restore_snapshot(&loser.provider, loser_previous).unwrap();
            assert_eq!(loser.epoch(&gid), 1);
            let applied = loser.apply(&gid, &winner_commit.commit, &verify).unwrap();
            assert_eq!(applied.epoch, 2);
            assert_eq!(applied.commit_hash, hash(&winner_commit.commit));

            // Both sides are on the winner's branch: its added device can join and everyone talks.
            let welcome = winner_commit.welcome.as_ref().unwrap();
            assert_eq!(new_device.join(welcome, &verify), gid);
            talks(winner, loser, &gid, 1);
            talks(loser, winner, &gid, 2);
            talks(new_device, loser, &gid, 3);
            assert_eq!(winner.leaves(&gid), loser.leaves(&gid));
            assert_eq!(winner.leaves(&gid).len(), 3);
        }
        assert!(winners[0] > 0 && winners[1] > 0, "{winners:?}");
    }

    #[test]
    fn adopt_theirs_restores_previous_then_applies() {
        // B2 receives the larger-hash Commit first, then learns of the smaller one.
        let (a1, a2, b1, b2, b3) = (
            Dev::new(1, 1),
            Dev::new(1, 2),
            Dev::new(2, 3),
            Dev::new(2, 4),
            Dev::new(2, 5),
        );
        let gid = group_of(&a1, &[&b1, &b2]);
        let verify = verifier(&[&a1, &a2, &b1, &b2, &b3]);
        let by_a = a1.commit(&gid, &[a2.key_package()], &[], &verify).unwrap();
        let by_b = b1.commit(&gid, &[b3.key_package()], &[], &verify).unwrap();
        let (ha, hb) = (hash(&by_a.commit), hash(&by_b.commit));
        let (first, second, winner, new_device) = if ha > hb {
            (&by_a, &by_b, &b1, &b3)
        } else {
            (&by_b, &by_a, &a1, &a2)
        };

        let before = snapshot(&b2.provider).unwrap();
        let mine = b2.apply(&gid, &first.commit, &verify).unwrap();
        assert_eq!(mine.epoch, 2);
        assert_eq!(mine.previous, before);

        // The other Commit leads to the same epoch with a smaller hash: switch over.
        let decision = resolve_fork(mine.epoch, mine.commit_hash, 2, hash(&second.commit), true);
        assert_eq!(decision, ForkDecision::AdoptTheirs);
        restore_snapshot(&b2.provider, &mine.previous).unwrap();
        assert_eq!(b2.epoch(&gid), 1);
        assert_eq!(snapshot(&b2.provider).unwrap(), before);
        let adopted = b2.apply(&gid, &second.commit, &verify).unwrap();
        assert_eq!(adopted.epoch, 2);
        assert_eq!(adopted.commit_hash, hash(&second.commit));
        assert_eq!(adopted.previous, before);

        // Messages sent after the winning Commit open on the restored-and-applied device.
        assert_eq!(
            new_device.join(second.welcome.as_ref().unwrap(), &verify),
            gid
        );
        talks(winner, &b2, &gid, 1);
        talks(new_device, &b2, &gid, 2);
        talks(&b2, winner, &gid, 3);
    }

    #[test]
    fn deep_fork_requests_rejoin() {
        let (small, big) = ([1; 32], [2; 32]);
        // Same epoch: the smaller hash wins, and an equal hash is the same Commit.
        assert_eq!(resolve_fork(4, small, 4, big, true), ForkDecision::KeepMine);
        assert_eq!(
            resolve_fork(4, small, 4, big, false),
            ForkDecision::KeepMine
        );
        assert_eq!(
            resolve_fork(4, small, 4, small, true),
            ForkDecision::KeepMine
        );
        assert_eq!(
            resolve_fork(4, big, 4, small, true),
            ForkDecision::AdoptTheirs
        );
        // Losing without a stored previous state cannot be undone: rejoin.
        assert_eq!(resolve_fork(4, big, 4, small, false), ForkDecision::Rejoin);
        // Different epochs mean a fork deeper than one stored state, or a missing Commit.
        for (mine, theirs) in [(5, 3), (5, 4), (3, 5), (4, 5)] {
            assert_eq!(
                resolve_fork(mine, small, theirs, big, true),
                ForkDecision::Rejoin
            );
            assert_eq!(
                resolve_fork(mine, big, theirs, small, true),
                ForkDecision::Rejoin
            );
        }

        // Only one level is kept: after two Commits the snapshot reaches back one epoch.
        let (a1, b1, a2, a3) = (
            Dev::new(1, 1),
            Dev::new(2, 2),
            Dev::new(1, 3),
            Dev::new(1, 4),
        );
        let gid = group_of(&a1, &[&b1]);
        let verify = verifier(&[&a1, &a2, &a3, &b1]);
        let c2 = a1.commit(&gid, &[a2.key_package()], &[], &verify).unwrap();
        let c3 = a1.commit(&gid, &[a3.key_package()], &[], &verify).unwrap();
        b1.apply(&gid, &c2.commit, &verify).unwrap();
        let applied = b1.apply(&gid, &c3.commit, &verify).unwrap();
        assert_eq!(b1.epoch(&gid), 3);
        restore_snapshot(&b1.provider, &applied.previous).unwrap();
        assert_eq!(b1.epoch(&gid), 2);
    }

    #[test]
    fn choose_group_is_symmetric_and_ignores_age() {
        let (small, big) = ([0; 32], [0xff; 32]);
        assert_eq!(choose_group(small, big), Ordering::Less);
        assert_eq!(choose_group(big, small), Ordering::Greater);
        assert_eq!(choose_group(small, small), Ordering::Equal);
        // Every pair agrees with the reversed question, whatever the order they were learned in.
        let hashes: Vec<[u8; 32]> = (0u8..20).map(|n| hash(&[n])).collect();
        for a in &hashes {
            for b in &hashes {
                assert_eq!(choose_group(*a, *b), choose_group(*b, *a).reverse());
            }
        }
        // Two real groups for one conversation: both creators reach the same winner.
        let (a1, b1, c1) = (Dev::new(1, 1), Dev::new(2, 2), Dev::new(2, 3));
        let verify = verifier(&[&a1, &b1, &c1]);
        let from_a = a1.create(&[b1.key_package()], &verify);
        let from_c = c1.create(&[a1.key_package()], &verify);
        assert_ne!(from_a.group_id, from_c.group_id);
        let one = choose_group(from_a.first_commit_hash, from_c.first_commit_hash);
        let other = choose_group(from_c.first_commit_hash, from_a.first_commit_hash);
        assert_eq!(one, other.reverse());
        assert_ne!(one, Ordering::Equal);
    }

    #[test]
    fn removed_device_cannot_open_after_remove_commit() {
        let (a1, b1, b2) = (Dev::new(1, 1), Dev::new(2, 2), Dev::new(2, 3));
        let gid = group_of(&a1, &[&b1, &b2]);
        let verify = verifier(&[&a1, &b1]);

        // B2 was revoked: A1 removes it by its installation.
        let committed = a1.commit(&gid, &[], &[b2.installation()], &verify).unwrap();
        assert!(committed.welcome.is_none());
        assert_eq!(committed.epoch, 2);
        let applied = b1.apply(&gid, &committed.commit, &verify).unwrap();
        assert!(!applied.self_removed);
        let mut expected = vec![a1.installation(), b1.installation()];
        expected.sort_by_key(|id| id.0);
        assert_eq!(a1.leaves(&gid), expected);
        assert_eq!(b1.leaves(&gid), expected);

        let ct = a1.seal(&gid, 1);
        assert!(matches!(
            b1.open(&gid, &ct).unwrap(),
            Opened::Application { .. }
        ));
        // B2 never got the Commit: it is a whole epoch behind and cannot open.
        assert_eq!(b2.open(&gid, &ct).unwrap_err().class, CLASS_FUTURE_EPOCH);
        // Even after it learns of the Commit it is out of the group and still cannot open.
        let seen = b2.apply(&gid, &committed.commit, &verify).unwrap();
        assert!(seen.self_removed);
        assert!(b2.open(&gid, &ct).is_err());
        assert!(b2.open(&gid, &a1.seal(&gid, 2)).is_err());
        // The remaining members are unaffected.
        talks(&b1, &a1, &gid, 3);
    }

    #[test]
    fn rejoin_rate_limited() {
        assert_eq!(
            (REJOIN_LIMIT, REJOIN_WINDOW_MS, REJOIN_MIN_GAP_MS),
            (3, 86_400_000, 60_000)
        );
        // A first rejoin is free; the stored state then spaces the next ones by a minute.
        assert!(may_rejoin(0, 0, 1_000));
        let (count, after) = after_rejoin(0, 1_000);
        assert_eq!((count, after), (1, 61_000));
        assert!(!may_rejoin(count, after, 60_999));
        assert!(may_rejoin(count, after, 61_000));
        let (count, after) = after_rejoin(count, 61_000);
        assert_eq!((count, after), (2, 121_000));
        assert!(!may_rejoin(count, after, 120_999));
        assert!(may_rejoin(count, after, 121_000));
        // The third rejoin uses up the allowance for a day.
        let (count, after) = after_rejoin(count, 121_000);
        assert_eq!((count, after), (3, 121_000 + REJOIN_WINDOW_MS));
        assert!(!may_rejoin(count, after, 121_000 + 1));
        assert!(!may_rejoin(count, after, after - 1));
        assert!(
            !may_rejoin(count, after, after),
            "the count itself still blocks"
        );
        assert!(
            !may_rejoin(7, 0, u64::MAX),
            "a count over the limit never passes"
        );
        // After the day the window starts again.
        assert_eq!(rejoin_count_now(count, after, after - 1), 3);
        let fresh = rejoin_count_now(count, after, after);
        assert_eq!(fresh, 0);
        assert!(may_rejoin(fresh, after, after));
        // Below the limit the count is left alone.
        assert_eq!(rejoin_count_now(2, 500, 1_000_000), 2);
        // No overflow at the ends.
        assert_eq!(after_rejoin(u32::MAX, u64::MAX), (u32::MAX, u64::MAX));
    }
}
