#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared M4 harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Task 7: the group lifecycle protocol between real cores (one per installation).
include!("support/mls_network.rs");

fn sorted(
    mut ids: Vec<chat_core::account::InstallationId>,
) -> Vec<chat_core::account::InstallationId> {
    ids.sort_by_key(|id| id.0);
    ids
}

#[tokio::test]
async fn first_send_creates_group_and_peer_joins() {
    let (ra, rb) = (record(A, &[A1], 0), record(B, &[B1], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    a1.account.connect_account(b1.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;

    // No group anywhere yet: the first call starts asking and creating.
    assert_ne!(ensure(&a1, b1.peer).await, GroupState::Ready);
    settle!(
        [&a1, &b1],
        agreed(&a1, &b1, conv).await && state(&a1, conv).await.deliveries.is_empty()
    );
    assert_eq!(ensure(&a1, b1.peer).await, GroupState::Ready);
    let (x, y) = (state(&a1, conv).await, state(&b1, conv).await);
    assert_eq!(x.group_epoch, Some(1));
    let both = sorted(vec![installation(&a1).await, installation(&b1).await]);
    assert_eq!(sorted(x.members.clone()), both);
    assert_eq!(sorted(y.members.clone()), both);
    // The joiner learned the creator's first-Commit hash (the Welcome does not carry it).
    assert_eq!(x.first_commit_hash, y.first_commit_hash);
    assert!(acked(&talk(&a1, &b1, conv, 1).await));
    assert!(acked(&talk(&b1, &a1, conv, 2).await));
}

#[tokio::test]
async fn offline_peer_device_joins_later_via_query() {
    let (ra, rb) = (record(A, &[A1], 0), record(B, &[B1, B2], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    b1.account.connect_account(a1.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;
    ensure(&a1, b1.peer).await;
    settle!([&a1, &b1], agreed(&a1, &b1, conv).await);
    assert_eq!(state(&a1, conv).await.members.len(), 2);

    // B2 was offline when the group was made. It comes online and asks before creating.
    let b2 = device(B, B2, &rb, &net).await;
    link(&b2, &a1).await;
    link(&b2, &b1).await;
    befriend(&b2, &a1).await;
    b2.account.connect_account(a1.peer).await.unwrap();
    b2.account.connect_account(b2.peer).await.unwrap();
    ensure(&b2, a1.peer).await;
    settle!(
        [&a1, &b1, &b2],
        agreed(&a1, &b2, conv).await
            && agreed(&a1, &b1, conv).await
            && state(&a1, conv).await.members.len() == 3
            && state(&b2, conv).await.members.len() == 3
    );
    // B2 joined the existing group instead of making a second one.
    assert_eq!(
        state(&b2, conv).await.first_commit_hash,
        state(&a1, conv).await.first_commit_hash
    );
    assert!(state(&b2, conv).await.group_epoch.unwrap() >= 2);
    assert!(acked(&talk(&b2, &a1, conv, 3).await));
    assert!(acked(&talk(&a1, &b1, conv, 4).await));
}

#[tokio::test]
async fn reinstalled_device_queries_then_joins_existing_group() {
    // B9 is B's reinstalled phone: a new installation the record already lists.
    const B9: u8 = 19;
    let (ra, rb) = (record(A, &[A1], 0), record(B, &[B1, B9], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    b1.account.connect_account(a1.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;
    ensure(&a1, b1.peer).await;
    settle!([&a1, &b1], agreed(&a1, &b1, conv).await);
    assert!(acked(&talk(&a1, &b1, conv, 1).await));
    let first = state(&a1, conv).await.first_commit_hash;
    drop(b1);

    let b9 = device(B, B9, &rb, &net).await;
    link(&b9, &a1).await;
    befriend(&b9, &a1).await;
    b9.account.connect_account(a1.peer).await.unwrap();
    assert_ne!(ensure(&b9, a1.peer).await, GroupState::Ready);
    settle!([&a1, &b9], agreed(&a1, &b9, conv).await);
    assert_eq!(state(&b9, conv).await.first_commit_hash, first);
    assert!(
        state(&b9, conv)
            .await
            .members
            .contains(&installation(&b9).await)
    );
    // History is not copied (M6): the new installation starts without the earlier text.
    let page = b9
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .load_messages(conv, None, 50)
        .await
        .unwrap();
    assert!(page.items.is_empty());
    assert!(acked(&talk(&a1, &b9, conv, 2).await));
}

#[tokio::test]
async fn simultaneous_creation_converges_on_smaller_first_commit() {
    let (ra, rb) = (record(A, &[A1], 0), record(B, &[B1], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    a1.account.connect_account(b1.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;
    let (ma, mb) = (
        a1.account.storage_test_fixture().messages().await.unwrap(),
        b1.account.storage_test_fixture().messages().await.unwrap(),
    );
    // Both create before either hears of the other's group.
    let (ka, kb) = (
        ma.mls_key_package().await.unwrap(),
        mb.mls_key_package().await.unwrap(),
    );
    let ga = ma
        .mls_create_group(conv, b1.peer, vec![kb], NOW * 1000)
        .await
        .unwrap();
    let gb = mb
        .mls_create_group(conv, a1.peer, vec![ka], NOW * 1000)
        .await
        .unwrap();
    assert_ne!(ga.group_id, gb.group_id);
    let winner = ga.first_commit_hash.min(gb.first_commit_hash);

    settle!(
        [&a1, &b1],
        agreed(&a1, &b1, conv).await
            && state(&a1, conv).await.first_commit_hash == Some(winner)
            && state(&a1, conv).await.deliveries.is_empty()
            && state(&b1, conv).await.deliveries.is_empty()
    );
    assert_eq!(state(&b1, conv).await.first_commit_hash, Some(winner));
    assert!(acked(&talk(&a1, &b1, conv, 1).await));
    assert!(acked(&talk(&b1, &a1, conv, 2).await));
}

/// B1 makes the group with A1 and A2; A then revokes A2 (published by another of A's devices
/// while A1 is unreachable from B1).
async fn revoked_setup(hold: bool) -> (Fixture, Fixture, Fixture, Arc<Backend>, ConversationId) {
    let (ra, rb) = (record(A, &[A1, A2], 0), record(B, &[B1], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let a2 = device(A, A2, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    link(&a2, &b1).await;
    befriend(&a1, &b1).await;
    befriend(&a2, &b1).await;
    befriend(&b1, &a1).await;
    a1.account.connect_account(b1.peer).await.unwrap();
    a2.account.connect_account(b1.peer).await.unwrap();
    let conv = conversation(&b1, &a1).await;
    b1.account.mls_test_hold_delivery(hold).await.unwrap();
    ensure(&b1, a1.peer).await;
    if hold {
        settle!([&b1], state(&b1, conv).await.deliveries.len() == 2);
    } else {
        settle!(
            [&a1, &a2, &b1],
            agreed(&a1, &b1, conv).await
                && agreed(&a2, &b1, conv).await
                && state(&b1, conv).await.deliveries.is_empty()
        );
    }
    assert_eq!(state(&b1, conv).await.members.len(), 3);
    // A's devices now list only A1.
    let revoked = record(A, &[A1], 1);
    publish(&net, A, &revoked);
    assert!(b1.account.refresh_record(a1.peer).await.is_err());
    b1.account
        .confirm_remote_record(a1.peer, chat_core::account::record_hash(&revoked))
        .await
        .unwrap();
    (a1, a2, b1, net, conv)
}

#[tokio::test]
async fn peer_can_remove_my_revoked_device_while_i_am_offline() {
    let (a1, a2, b1, _net, conv) = revoked_setup(false).await;
    let (i1, i2, ib) = (
        installation(&a1).await,
        installation(&a2).await,
        installation(&b1).await,
    );
    // B1 removes A2 on its own; A1 is unreachable (the review closed B1's sessions with A).
    settle!([&b1], state(&b1, conv).await.group_epoch == Some(2));
    let s = state(&b1, conv).await;
    assert_eq!(sorted(s.members.clone()), sorted(vec![i1, ib]));
    assert!(s.deliveries.iter().all(|(to, _, _)| *to != i2));
    assert_eq!(s.deliveries, vec![(i1, 1, 2)], "the Commit waits for A1");
    // The removed device cannot read what B1 sends now.
    let reply = talk(&b1, &a2, conv, 7).await;
    assert!(
        matches!(reply, MlsReply::DecryptFailed { .. }),
        "{}",
        acked(&reply)
    );
}

#[tokio::test]
async fn removed_device_delivery_dropped() {
    let (a1, a2, b1, _net, conv) = revoked_setup(true).await;
    let (i1, i2) = (installation(&a1).await, installation(&a2).await);
    // The Remove of A2 is made while A2's Welcome is still queued: that delivery is dropped.
    settle!([&b1], state(&b1, conv).await.group_epoch == Some(2));
    let mut deliveries = state(&b1, conv).await.deliveries;
    deliveries.sort_by_key(|d| (d.2, d.1));
    assert_eq!(deliveries, vec![(i1, 2, 1), (i1, 1, 2)]);
    assert!(!state(&b1, conv).await.members.contains(&i2));
    b1.account.mls_test_hold_delivery(false).await.unwrap();
    assert_eq!(state(&a2, conv).await.group_epoch, None);
}

/// B1 and B2 share a group; A's record gains A2 while B1 still holds the older record. B2
/// reviews the new record and adds A2; its Commit reaches B1, which does not know A2 yet.
/// `revert`: the lookup B1 makes finds the old record again. `block_lookup`: B1's lookup of A
/// never returns.
async fn unknown_device_setup(
    revert: bool,
    block_lookup: bool,
) -> (
    Fixture,
    Fixture,
    Fixture,
    Fixture,
    Arc<Backend>,
    ConversationId,
    SignedPacket,
) {
    let (old, rb) = (record(A, &[A1], 0), record(B, &[B1, B2], 0));
    let net = backend(&[(A, old.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &old, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    let b2 = device(B, B2, &rb, &net).await;
    link(&b1, &b2).await;
    link(&b1, &a1).await;
    link(&b2, &a1).await;
    befriend(&b1, &a1).await;
    befriend(&b2, &a1).await;
    befriend(&a1, &b1).await;
    // Both B devices cache A's record as it is now (A1 only).
    b1.account.connect_account(a1.peer).await.unwrap();
    b2.account.connect_account(a1.peer).await.unwrap();
    b1.account.connect_account(b1.peer).await.unwrap();
    let conv = conversation(&b1, &a1).await;
    ensure(&b1, a1.peer).await;
    settle!(
        [&b1, &b2, &a1],
        agreed(&b1, &b2, conv).await && agreed(&b1, &a1, conv).await
    );

    let new = record(A, &[A1, A2], 1);
    publish(&net, A, &new);
    let a2 = device(A, A2, &new, &net).await;
    link(&a2, &b2).await;
    befriend(&a2, &b1).await;
    assert!(b2.account.refresh_record(a1.peer).await.is_err());
    b2.account
        .confirm_remote_record(a1.peer, chat_core::account::record_hash(&new))
        .await
        .unwrap();
    b2.account.mls_test_hold_delivery(true).await.unwrap();
    // A2 dials B2 (A1 still presents the old record, which B2 would take as a conflict).
    a2.account.connect_account(b2.peer).await.unwrap();
    settle!([&b2], state(&b2, conv).await.group_epoch == Some(2));
    if revert {
        publish(&net, A, &old);
    }
    if block_lookup {
        net.lookup_holds
            .lock()
            .unwrap()
            .insert(a1.peer, Arc::new(tokio::sync::Notify::new()));
    }
    b2.account.mls_test_hold_delivery(false).await.unwrap();
    (a1, a2, b1, b2, net, conv, new)
}

#[tokio::test]
async fn unknown_device_commit_is_held_then_applied_after_lookup() {
    let (_a1, a2, b1, b2, _net, conv, new) = unknown_device_setup(false, false).await;
    let ia2 = installation(&a2).await;
    // B1 holds the Commit and looks A up; the new list is a record change to review.
    settle!(
        [&b1, &b2, &a2],
        state(&b1, conv).await.holds == 1
            && b1
                .view()
                .await
                .remote_records
                .iter()
                .any(|r| r.conflict_hash.is_some())
            && state(&a2, conv).await.group_epoch == Some(2)
    );
    assert_eq!(state(&b1, conv).await.group_epoch, Some(1));
    b1.account
        .confirm_remote_record(a2.peer, chat_core::account::record_hash(&new))
        .await
        .unwrap();
    settle!([&b1, &b2], state(&b1, conv).await.group_epoch == Some(2));
    let s = state(&b1, conv).await;
    assert_eq!(s.holds, 0);
    assert!(s.members.contains(&ia2));
    assert!(agreed(&b1, &b2, conv).await);
    // B1 now shares the group state with the new device it was holding the Commit for.
    assert!(acked(&talk(&b1, &a2, conv, 5).await));
}

#[tokio::test]
async fn still_unknown_after_lookup_is_dropped() {
    let (_a1, a2, b1, b2, _net, conv, _new) = unknown_device_setup(true, false).await;
    let (ib1, ia2) = (installation(&b1).await, installation(&a2).await);
    // B1 got the Commit (B2 has nothing left for it), held it, looked A up again, still found
    // no A2, and dropped it: the Commit adding A2 is never applied on B1.
    settle!(
        [&b1, &b2],
        state(&b2, conv).await.deliveries.iter().all(|d| d.0 != ib1)
            && b1.account.mls_test_held_total().await.unwrap() >= 1
            && b1.account.mls_test_rejoin_total().await.unwrap() >= 1
    );
    assert!(!state(&b1, conv).await.members.contains(&ia2));
}

#[tokio::test]
async fn dropped_hold_leads_to_rejoin() {
    let (_a1, _a2, b1, b2, _net, conv, _new) = unknown_device_setup(true, false).await;
    // The dropped Commit leaves B1 unable to follow the group: it rejoins (spec 4.6).
    settle!(
        [&b1, &b2],
        b1.account.mls_test_held_total().await.unwrap() >= 1
            && b1.account.mls_test_rejoin_total().await.unwrap() >= 1
    );
    // The rejoin is a real one: B1 left its group state and asks to join again.
    let frames = b1.account.mls_test_sent_frames().await.unwrap();
    assert!(frames.iter().any(|f| f == "group_query"), "{frames:?}");
    let _ = conv;
}

#[tokio::test]
async fn held_lookup_does_not_block_inbound_requests() {
    let (_a1, a2, b1, b2, net, conv, _new) = unknown_device_setup(false, true).await;
    // B1 holds the Commit and its lookup of A hangs.
    settle!(
        [&b1, &b2],
        state(&b1, conv).await.holds == 1 && net.lookup_starts.lock().unwrap().contains(&a2.peer)
    );
    // B1 still answers a kind 17 request right away.
    let started = tokio::time::Instant::now();
    let answers = b2.account.mls_test_query(a2.peer).await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert!(
        answers.contains(&(installation(&b1).await, true)),
        "{answers:?}"
    );
}

#[tokio::test]
async fn unknown_device_join_request_is_looked_up() {
    let (a1, b1, net, conv) = two_party(&[A1], &[B1]).await;
    let mb = b1.account.storage_test_fixture().messages().await.unwrap();
    // A JoinRequest from a device A's record does not list (A9): held, not refused outright.
    let k9 = chat_core::storage_fixture::mls_detached_key_package(a1.peer, [29; 32]);
    let hold = Arc::new(tokio::sync::Notify::new());
    net.lookup_holds
        .lock()
        .unwrap()
        .insert(a1.peer, hold.clone());
    assert_eq!(
        mb.mls_join_request(conv, a1.peer, a1.peer, k9, NOW * 1000)
            .await
            .unwrap(),
        "held"
    );
    assert_eq!(state(&b1, conv).await.holds, 1);
    // B1 looks A up again (stopped here to see it start), finds no A9 and drops the request.
    settle!([&b1], net.lookup_starts.lock().unwrap().contains(&a1.peer));
    net.lookup_holds.lock().unwrap().remove(&a1.peer);
    hold.notify_one();
    settle!([&b1], state(&b1, conv).await.holds == 0);
    assert_eq!(state(&b1, conv).await.group_epoch, Some(1));
    // A refused JoinRequest is no reason for B1 to leave its group.
    assert_eq!(b1.account.mls_test_rejoin_total().await.unwrap(), 0);
}

/// A1 and B1 in a group at epoch 1, nothing left to deliver.
async fn two_party(
    a_devices: &[u8],
    b_devices: &[u8],
) -> (Fixture, Fixture, Arc<Backend>, ConversationId) {
    let (ra, rb) = (record(A, a_devices, 0), record(B, b_devices, 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    a1.account.connect_account(b1.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;
    ensure(&a1, b1.peer).await;
    settle!(
        [&a1, &b1],
        agreed(&a1, &b1, conv).await && state(&a1, conv).await.deliveries.is_empty()
    );
    (a1, b1, net, conv)
}

fn detached(peer: PeerId, seed: u8) -> Vec<u8> {
    chat_core::storage_fixture::mls_detached_key_package(peer, [seed; 32])
}

fn installation_of(seed: u8) -> chat_core::account::InstallationId {
    chat_core::account::derive_device(Secret32::from([seed; 32]))
        .unwrap()
        .installation_id()
}

/// The Commit `from` has queued for `to` at `epoch`.
async fn queued_commit(from: &Fixture, conv: ConversationId, to: &Fixture, epoch: u64) -> Vec<u8> {
    let wire = from
        .account
        .storage_test_fixture()
        .mls_delivery_frame(conv, installation(to).await, 1, epoch)
        .await
        .unwrap()
        .expect("queued Commit");
    let (id, claimed, commit) = chat_core::storage_fixture::mls_commit_from_wire(&wire).unwrap();
    assert_eq!((id, claimed), (conv, epoch));
    commit
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    Sha256::digest(bytes).into()
}

#[tokio::test]
async fn redelivered_commit_after_lost_ack_is_ignored() {
    let (a1, b1, _net, conv) = two_party(&[A1, A2], &[B1, B2]).await;
    let ib1 = installation(&b1).await;
    let (ma, mb) = (
        a1.account.storage_test_fixture().messages().await.unwrap(),
        b1.account.storage_test_fixture().messages().await.unwrap(),
    );
    a1.account.mls_test_hold_delivery(true).await.unwrap();
    b1.account.mls_test_hold_delivery(true).await.unwrap();
    let now = NOW * 1000;
    assert_eq!(
        ma.mls_commit(conv, b1.peer, vec![detached(a1.peer, A2)], vec![], now)
            .await
            .unwrap(),
        Some(2)
    );
    // B1 applies A1's epoch-2 Commit, but the Ack never reaches A1.
    let commit = queued_commit(&a1, conv, &b1, 2).await;
    assert_eq!(
        mb.mls_apply_commit(conv, a1.peer, a1.peer, 2, commit, now)
            .await
            .unwrap(),
        MlsCommitOutcome::Ack
    );
    // B1 moves on to epoch 3.
    assert_eq!(
        mb.mls_commit(conv, a1.peer, vec![detached(b1.peer, B2)], vec![], now)
            .await
            .unwrap(),
        Some(3)
    );
    let before = state(&b1, conv).await;
    // A1's retry of the epoch-2 Commit arrives: B1 is past it, says so, and stays put.
    a1.account.mls_test_hold_delivery(false).await.unwrap();
    settle!(
        [&a1, &b1],
        !state(&a1, conv)
            .await
            .deliveries
            .iter()
            .any(|d| d.0 == ib1 && d.2 == 2)
    );
    let after = state(&b1, conv).await;
    assert_eq!(after.epoch_authenticator, before.epoch_authenticator);
    assert_eq!(after.group_epoch, Some(3));
    assert_eq!(after.rejoin_count, Some(0));
    assert_eq!(b1.account.mls_test_rejoin_total().await.unwrap(), 0);
    // B1's own Commit then brings A1 along.
    b1.account.mls_test_hold_delivery(false).await.unwrap();
    settle!([&a1, &b1], agreed(&a1, &b1, conv).await);
}

#[tokio::test]
async fn garbage_commit_does_not_force_rejoin() {
    let (a1, b1, _net, conv) = two_party(&[A1, A2], &[B1]).await;
    let (ma, mb) = (
        a1.account.storage_test_fixture().messages().await.unwrap(),
        b1.account.storage_test_fixture().messages().await.unwrap(),
    );
    a1.account.mls_test_hold_delivery(true).await.unwrap();
    let now = NOW * 1000;
    ma.mls_commit(conv, b1.peer, vec![detached(a1.peer, A2)], vec![], now)
        .await
        .unwrap();
    let commit = queued_commit(&a1, conv, &b1, 2).await;
    mb.mls_apply_commit(conv, a1.peer, a1.peer, 2, commit.clone(), now)
        .await
        .unwrap();
    let before = state(&b1, conv).await;
    assert_eq!(before.group_epoch, Some(2));

    // Tampered copies of the real Commit, claiming an old, the current and the next epoch;
    // some of them hash below the Commit B1 is on, so they reach the fork comparison.
    let mut below = 0;
    for i in 1..=24 {
        let mut bad = commit.clone();
        let at = bad.len() - i;
        bad[at] ^= 0x5a;
        if digest(&bad) < digest(&commit) {
            below += 1;
        }
        for claim in [0, 2, 3] {
            let outcome = mb
                .mls_apply_commit(conv, a1.peer, a1.peer, claim, bad.clone(), now)
                .await
                .unwrap();
            assert!(
                matches!(outcome, MlsCommitOutcome::Failed { .. }),
                "claim {claim}: {outcome:?}"
            );
        }
    }
    assert!(below > 0, "no tampered Commit reached the fork comparison");
    for claim in [0, 2, 3] {
        let outcome = mb
            .mls_apply_commit(conv, a1.peer, a1.peer, claim, vec![7; 40], now)
            .await
            .unwrap();
        assert!(matches!(outcome, MlsCommitOutcome::Failed { .. }));
    }
    let after = state(&b1, conv).await;
    assert_eq!(after.epoch_authenticator, before.epoch_authenticator);
    assert_eq!(after.rejoin_count, Some(0));
    assert_eq!(after.holds, 0);
    // The real Commit, resent, is recognised as applied.
    a1.account.mls_test_hold_delivery(false).await.unwrap();
    settle!(
        [&a1, &b1],
        agreed(&a1, &b1, conv).await && state(&a1, conv).await.own_commit_unacked == Some(false)
    );
}

#[tokio::test]
async fn next_epoch_commit_on_winning_branch_is_retried_until_applied() {
    let (a1, b1, _net, conv) = two_party(&[A1, A2], &[B1, B2]).await;
    let (ma, mb) = (
        a1.account.storage_test_fixture().messages().await.unwrap(),
        b1.account.storage_test_fixture().messages().await.unwrap(),
    );
    a1.account.mls_test_hold_delivery(true).await.unwrap();
    b1.account.mls_test_hold_delivery(true).await.unwrap();
    let now = NOW * 1000;
    // Both commit from epoch 1.
    ma.mls_commit(conv, b1.peer, vec![detached(a1.peer, A2)], vec![], now)
        .await
        .unwrap();
    mb.mls_commit(conv, a1.peer, vec![detached(b1.peer, B2)], vec![], now)
        .await
        .unwrap();
    let (by_a, by_b) = (
        queued_commit(&a1, conv, &b1, 2).await,
        queued_commit(&b1, conv, &a1, 2).await,
    );
    let a_wins = digest(&by_a) < digest(&by_b);
    let (winner, loser, wm, lm, winning, added) = if a_wins {
        (&a1, &b1, &ma, &mb, by_a, A2)
    } else {
        (&b1, &a1, &mb, &ma, by_b, B2)
    };
    // The winner goes on to epoch 3 before the loser hears of anything.
    winner
        .account
        .storage_test_fixture()
        .mls_clear_commit_flag(conv)
        .await
        .unwrap();
    assert_eq!(
        wm.mls_commit(conv, loser.peer, vec![], vec![installation_of(added)], now)
            .await
            .unwrap(),
        Some(3)
    );
    let next = queued_commit(winner, conv, loser, 3).await;
    // Epoch 3 first: not applicable yet, so the sender keeps it (class 2) and nothing changes.
    let before = state(loser, conv).await;
    assert_eq!(
        lm.mls_apply_commit(conv, winner.peer, winner.peer, 3, next.clone(), now)
            .await
            .unwrap(),
        MlsCommitOutcome::Failed { class: 2, epoch: 2 }
    );
    assert_eq!(
        state(loser, conv).await.epoch_authenticator,
        before.epoch_authenticator
    );
    // The winning epoch-2 Commit is verified against the previous state and adopted...
    assert_eq!(
        lm.mls_apply_commit(conv, winner.peer, winner.peer, 2, winning, now)
            .await
            .unwrap(),
        MlsCommitOutcome::Adopted
    );
    // ...and the retried epoch 3 applies.
    assert_eq!(
        lm.mls_apply_commit(conv, winner.peer, winner.peer, 3, next, now)
            .await
            .unwrap(),
        MlsCommitOutcome::Ack
    );
    assert!(agreed(winner, loser, conv).await);
    // The real deliveries that follow (both branches' Commits) settle without moving anyone.
    a1.account.mls_test_hold_delivery(false).await.unwrap();
    b1.account.mls_test_hold_delivery(false).await.unwrap();
    let ia = installation(&a1).await;
    let ib = installation(&b1).await;
    settle!(
        [&a1, &b1],
        state(&a1, conv).await.deliveries.iter().all(|d| d.0 != ib)
            && state(&b1, conv).await.deliveries.iter().all(|d| d.0 != ia)
    );
    assert!(agreed(&a1, &b1, conv).await);
    assert_eq!(a1.account.mls_test_rejoin_total().await.unwrap(), 0);
    assert_eq!(b1.account.mls_test_rejoin_total().await.unwrap(), 0);
}

#[tokio::test]
async fn own_commit_flag_clears_when_a_later_commit_lands() {
    let (a1, b1, _net, conv) = two_party(&[A1, A2], &[B1, B2]).await;
    let (ma, mb) = (
        a1.account.storage_test_fixture().messages().await.unwrap(),
        b1.account.storage_test_fixture().messages().await.unwrap(),
    );
    // A1's Commit reaches B1 another way; its own delivery (and so any Ack) stays held.
    a1.account.mls_test_hold_delivery(true).await.unwrap();
    let now = NOW * 1000;
    ma.mls_commit(conv, b1.peer, vec![detached(a1.peer, A2)], vec![], now)
        .await
        .unwrap();
    assert_eq!(state(&a1, conv).await.own_commit_unacked, Some(true));
    assert_eq!(
        ma.mls_commit(conv, b1.peer, vec![], vec![installation_of(A2)], now)
            .await
            .unwrap(),
        None,
        "no second Commit before the first is confirmed"
    );
    let commit = queued_commit(&a1, conv, &b1, 2).await;
    mb.mls_apply_commit(conv, a1.peer, a1.peer, 2, commit, now)
        .await
        .unwrap();
    // B1 commits on top of it and that Commit is delivered to A1.
    assert_eq!(
        mb.mls_commit(conv, a1.peer, vec![detached(b1.peer, B2)], vec![], now)
            .await
            .unwrap(),
        Some(3)
    );
    settle!([&a1, &b1], state(&a1, conv).await.group_epoch == Some(3));
    let s = state(&a1, conv).await;
    assert_eq!(s.own_commit_unacked, Some(false));
    let ib1 = installation(&b1).await;
    assert!(
        s.deliveries.iter().any(|d| d.0 == ib1 && d.2 == 2),
        "A1's own delivery was never acknowledged"
    );
    assert_eq!(
        ma.mls_commit(conv, b1.peer, vec![], vec![installation_of(A2)], now)
            .await
            .unwrap(),
        Some(4)
    );
}

#[tokio::test]
async fn offline_recipients_do_not_starve_online_deliveries() {
    let (ra, rb) = (record(A, &[A1], 0), record(B, &[B1], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    a1.account.connect_account(b1.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;
    a1.account.mls_test_hold_delivery(true).await.unwrap();
    ensure(&a1, b1.peer).await;
    settle!([&a1], state(&a1, conv).await.deliveries.len() == 1);
    // Twenty recipients that never connect, all due before B1's Welcome.
    let fixture = a1.account.storage_test_fixture();
    for n in 0..20u8 {
        fixture
            .mls_set_delivery(
                ConversationId([100 + n; 32]),
                chat_core::account::InstallationId([200 + n; 32]),
                2,
                1,
                Some((vec![1; 8], 0)),
            )
            .await
            .unwrap();
    }
    a1.account.mls_test_hold_delivery(false).await.unwrap();
    settle!(
        [&a1, &b1],
        agreed(&a1, &b1, conv).await && state(&a1, conv).await.deliveries.is_empty()
    );
    // The unreachable ones are still queued as they were.
    for n in [0u8, 19] {
        assert_eq!(
            state(&a1, ConversationId([100 + n; 32])).await.deliveries,
            vec![(chat_core::account::InstallationId([200 + n; 32]), 2, 1)]
        );
    }
}

#[tokio::test]
async fn deep_fork_rejoins_and_resends_unacked() {
    let (a1, b1, _net, conv) = two_party(&[A1, A2], &[B1, B2]).await;
    let ib1 = installation(&b1).await;

    // B1 has a sealed message nobody acknowledged yet.
    b1.account.message_test_hold_scheduler(true).await.unwrap();
    let mb = b1.account.storage_test_fixture().messages().await.unwrap();
    let sent = mb
        .enqueue(
            a1.peer,
            MessageId([42; 16]),
            "before the fork".into(),
            NOW * 1000,
        )
        .await
        .unwrap();
    b1.account
        .storage_test_fixture()
        .mls_promote_outbox(sent.key)
        .await
        .unwrap();
    mb.mls_seal(sent.key, NOW * 1000).await.unwrap().unwrap();
    let before = b1
        .account
        .storage_test_fixture()
        .mls_outbox(sent.key)
        .await
        .unwrap()
        .wire;

    // A1 moves two epochs ahead, and its epoch-2 Commit to B1 is lost for good: B1's state can
    // no longer be brought forward, only replaced.
    a1.account.mls_test_hold_delivery(true).await.unwrap();
    let ma = a1.account.storage_test_fixture().messages().await.unwrap();
    let now = NOW * 1000;
    assert_eq!(
        ma.mls_commit(conv, b1.peer, vec![detached(a1.peer, A2)], vec![], now)
            .await
            .unwrap(),
        Some(2)
    );
    a1.account
        .storage_test_fixture()
        .mls_clear_commit_flag(conv)
        .await
        .unwrap();
    assert_eq!(
        ma.mls_commit(conv, b1.peer, vec![], vec![installation_of(A2)], now)
            .await
            .unwrap(),
        Some(3)
    );
    a1.account
        .storage_test_fixture()
        .mls_set_delivery(conv, ib1, 1, 2, None)
        .await
        .unwrap();
    a1.account.mls_test_hold_delivery(false).await.unwrap();

    // B1 keeps answering "a Commit is missing" (class 2) and, once that has gone on long
    // enough, drops the group, rejoins through A1 and seals its unacknowledged message again.
    settle!(
        [&a1, &b1],
        state(&b1, conv).await.rejoin_count == Some(1)
            && agreed(&a1, &b1, conv).await
            && b1
                .account
                .storage_test_fixture()
                .mls_outbox(sent.key)
                .await
                .unwrap()
                .codec
                == Some(2)
    );
    let after = b1
        .account
        .storage_test_fixture()
        .mls_outbox(sent.key)
        .await
        .unwrap()
        .wire;
    assert_ne!(after, before, "re-encrypted for the group B1 rejoined");
    let (_, ciphertext) = mls_app_from_wire(&after).unwrap();
    let reply = ma
        .mls_open_app(
            b1.peer,
            installation(&b1).await,
            conv,
            ciphertext,
            NOW * 1000,
        )
        .await
        .unwrap();
    assert!(acked(&reply));
    let page = ma.load_messages(conv, None, 50).await.unwrap();
    assert!(page.items.iter().any(|m| m.body == "before the fork"));
}

#[tokio::test]
async fn rejoin_is_rate_limited() {
    let (ra, rb) = (record(A, &[A1], 0), record(B, &[B1], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    a1.account.connect_account(b1.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;
    ensure(&a1, b1.peer).await;
    settle!([&a1, &b1], agreed(&a1, &b1, conv).await);

    for round in 1..=3u32 {
        assert!(
            b1.account.mls_test_rejoin(a1.peer).await.unwrap(),
            "rejoin {round}"
        );
        settle!(
            [&a1, &b1],
            state(&b1, conv).await.rejoin_count == Some(round) && agreed(&a1, &b1, conv).await
        );
        // A minute must pass between two rejoins.
        assert!(
            !b1.account.mls_test_rejoin(a1.peer).await.unwrap(),
            "too soon after {round}"
        );
        assert!(
            agreed(&a1, &b1, conv).await,
            "a refused rejoin keeps the group"
        );
        b1.clock.advance_seconds(61).unwrap();
    }
    // The third one used up the allowance: no fourth within the day.
    assert!(!b1.account.mls_test_rejoin(a1.peer).await.unwrap());
    assert_eq!(state(&b1, conv).await.rejoin_count, Some(3));
    assert!(acked(&talk(&b1, &a1, conv, 1).await));
}

#[tokio::test]
async fn no_proposals_are_sent() {
    let (ra, rb) = (record(A, &[A1], 0), record(B, &[B1, B2], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    let b2 = device(B, B2, &rb, &net).await;
    link(&a1, &b1).await;
    link(&a1, &b2).await;
    link(&b1, &b2).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    befriend(&b2, &a1).await;
    b1.account.connect_account(a1.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;
    ensure(&a1, b1.peer).await;
    settle!([&a1, &b1], agreed(&a1, &b1, conv).await);
    b2.account.connect_account(a1.peer).await.unwrap();
    b2.account.connect_account(b2.peer).await.unwrap();
    ensure(&b2, a1.peer).await;
    settle!(
        [&a1, &b1, &b2],
        agreed(&a1, &b2, conv).await
            && agreed(&a1, &b1, conv).await
            && state(&a1, conv).await.members.len() == 3
    );

    let mut frames = Vec::new();
    for device in [&a1, &b1, &b2] {
        frames.extend(device.account.mls_test_sent_frames().await.unwrap());
    }
    assert!(frames.iter().any(|f| f == "commit:commit"), "{frames:?}");
    assert!(frames.iter().any(|f| f == "welcome"), "{frames:?}");
    assert!(
        frames
            .iter()
            .all(|f| !f.contains("proposal") && !f.contains("unknown")),
        "{frames:?}"
    );
}

// Task 9 (spec 4.4a): a Welcome from this account's own device authorizes the contact.

/// B3 is a device of B that is never online.
const B3: u8 = 16;

/// A is `[A1, A2]` and B is `[B1]`. A1 and B1 are contacts of each other and share a session.
/// A2 is A's phone: it has no contact at all, and is linked to A1 and B1.
async fn phone_setup() -> (Fixture, Fixture, Fixture, Arc<Backend>, ConversationId) {
    let (ra, rb) = (record(A, &[A1, A2], 0), record(B, &[B1], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let a2 = device(A, A2, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    link(&a1, &a2).await;
    link(&a2, &b1).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    a1.account.connect_account(b1.peer).await.unwrap();
    a1.account.connect_account(a1.peer).await.unwrap();
    a2.account.connect_account(a2.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;
    conversation(&b1, &a1).await;
    assert!(a2.account.contacts().await.unwrap().is_empty());
    (a1, a2, b1, net, conv)
}

async fn contact_of(f: &Fixture, peer: PeerId) -> Option<chat_core::account::ContactView> {
    f.account
        .contacts()
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.peer_id == peer)
}

async fn sent_labels(f: &Fixture) -> Vec<String> {
    f.account.mls_test_sent_frames().await.unwrap()
}

/// Lets `devices` run for `rounds` more seconds of back-off time.
async fn idle(devices: &[&Fixture], rounds: usize) {
    for _ in 0..rounds {
        for d in devices {
            d.clock.advance_monotonic_ms(1_000);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn own_device_welcome_adds_contact_and_joins() {
    let (a1, a2, b1, _net, conv) = phone_setup().await;
    let (i1, i2, ib) = (
        installation(&a1).await,
        installation(&a2).await,
        installation(&b1).await,
    );
    ensure(&a1, b1.peer).await;
    settle!(
        [&a1, &a2, &b1],
        agreed(&a1, &a2, conv).await
            && agreed(&a1, &b1, conv).await
            && contact_of(&a2, b1.peer).await.is_some()
            && state(&a1, conv).await.deliveries.is_empty()
    );
    // The phone joined the conversation and got B as a contact without being asked.
    assert_eq!(
        sorted(state(&a2, conv).await.members),
        sorted(vec![i1, i2, ib])
    );
    let contacts = a2.account.contacts().await.unwrap();
    assert_eq!(contacts.len(), 1);
    assert_eq!(contacts[0].peer_id, b1.peer);
    assert!(acked(&talk(&a2, &b1, conv, 1).await));
    assert!(acked(&talk(&b1, &a2, conv, 2).await));
}

#[tokio::test]
async fn auto_contact_does_not_copy_safety_verification() {
    let (a1, a2, b1, _net, conv) = phone_setup().await;
    // A1 knows B by name and has checked B's safety number.
    a1.account
        .confirm_safety(b1.peer, chat_core::account::safety_digest(a1.peer, b1.peer))
        .await
        .unwrap();
    let before = contact_of(&a1, b1.peer).await.unwrap();
    assert!(before.verified && before.alias == "remote");
    ensure(&a1, b1.peer).await;
    settle!(
        [&a1, &a2, &b1],
        agreed(&a1, &a2, conv).await && contact_of(&a2, b1.peer).await.is_some()
    );
    let made = contact_of(&a2, b1.peer).await.unwrap();
    assert_eq!(made.alias, "");
    assert!(!made.verified, "safety verification is never copied");
    assert!(!made.new_device);
    // The other device's contact is untouched.
    let after = contact_of(&a1, b1.peer).await.unwrap();
    assert!(after.verified && after.alias == "remote");
}

#[tokio::test]
async fn peer_welcome_without_contact_is_held_as_contact_required() {
    // A1 and B1 share a group; A2 (no contact) is not reachable by A1.
    let (ra, rb) = (record(A, &[A1, A2], 0), record(B, &[B1], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let a2 = device(A, A2, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    link(&a2, &b1).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    a1.account.connect_account(b1.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;
    ensure(&a1, b1.peer).await;
    settle!(
        [&a1, &b1],
        agreed(&a1, &b1, conv).await && state(&a1, conv).await.deliveries.is_empty()
    );
    // B1 adds A2 to the group: the Welcome comes from B's device, not from one of mine.
    a2.account.connect_account(b1.peer).await.unwrap();
    b1.account.mls_test_hold_delivery(true).await.unwrap();
    let key_package = a2
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_key_package()
        .await
        .unwrap();
    let epoch = b1
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_commit(conv, a1.peer, vec![key_package], vec![], NOW * 1000)
        .await
        .unwrap()
        .expect("an own Commit");
    let i2 = installation(&a2).await;
    assert!(state(&b1, conv).await.deliveries.contains(&(i2, 2, epoch)));
    b1.account.mls_test_hold_delivery(false).await.unwrap();
    settle!(
        [&a1, &a2, &b1],
        sent_labels(&b1).await.iter().any(|f| f == "welcome")
    );
    idle(&[&a1, &a2, &b1], 30).await;
    // A2 answered ContactRequired: nothing kept, no contact, and B1 keeps the Welcome queued.
    assert_eq!(state(&a2, conv).await.group_epoch, None);
    assert!(a2.account.contacts().await.unwrap().is_empty());
    assert!(state(&b1, conv).await.deliveries.contains(&(i2, 2, epoch)));
}

#[tokio::test]
async fn own_device_welcome_with_unverified_peer_leaf_is_rejected() {
    let (a1, a2, b1, net, conv) = phone_setup().await;
    a1.account.mls_test_hold_delivery(true).await.unwrap();
    ensure(&a1, b1.peer).await;
    settle!(
        [&a1],
        state(&a1, conv).await.deliveries.len() == 2
            && state(&a1, conv).await.group_epoch.is_some()
    );
    // B's list no longer names B1 (published after A1 made the group, before A2 looks).
    publish(&net, B, &record(B, &[B3], 1));
    a1.account.mls_test_hold_delivery(false).await.unwrap();
    settle!(
        [&a1, &a2, &b1],
        sent_labels(&a2).await.iter().any(|f| f == "group_query")
            && a2
                .view()
                .await
                .remote_records
                .iter()
                .any(|r| r.peer == b1.peer)
    );
    idle(&[&a1, &a2, &b1], 30).await;
    // The leaf of B1 does not check out against B's record: nothing was kept.
    assert_eq!(state(&a2, conv).await.group_epoch, None);
    assert!(a2.account.contacts().await.unwrap().is_empty());
    let i2 = installation(&a2).await;
    assert!(state(&a1, conv).await.deliveries.iter().any(|d| d.0 == i2));
}

#[tokio::test]
async fn auto_contact_receives_the_peers_messages() {
    let (a1, a2, b1, _net, conv) = phone_setup().await;
    ensure(&a1, b1.peer).await;
    settle!(
        [&a1, &a2, &b1],
        agreed(&a1, &a2, conv).await
            && agreed(&a1, &b1, conv).await
            && contact_of(&a2, b1.peer).await.is_some()
            && state(&a1, conv).await.deliveries.is_empty()
    );
    // Only the phone is left for B to reach. B's text arrives through the contact rule of
    // the receive path, not through a store-level call.
    drop(a1);
    a2.account.connect_account(b1.peer).await.unwrap();
    let sent = b1
        .account
        .messages()
        .send_text(a2.peer, MessageId([5; 16]), "hello phone".into())
        .await
        .unwrap();
    settle!(
        [&a2, &b1],
        a2.account
            .messages()
            .load_messages(conv, None, 50)
            .await
            .unwrap()
            .items
            .iter()
            .any(|m| m.key == sent.key && m.body == "hello phone")
    );
}
