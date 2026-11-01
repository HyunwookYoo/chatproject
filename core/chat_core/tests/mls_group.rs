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
async fn unknown_device_setup(
    revert: bool,
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
    b2.account.mls_test_hold_delivery(false).await.unwrap();
    (a1, a2, b1, b2, net, conv, new)
}

#[tokio::test]
async fn unknown_device_commit_is_held_then_applied_after_lookup() {
    let (_a1, a2, b1, b2, _net, conv, new) = unknown_device_setup(false).await;
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
    let (_a1, _a2, b1, b2, _net, conv, _new) = unknown_device_setup(true).await;
    let ib1 = installation(&b1).await;
    // B1 got the Commit (B2 has nothing left for it), held it, looked A up again, still found
    // no A2, and dropped it.
    settle!(
        [&b1, &b2],
        state(&b2, conv).await.deliveries.iter().all(|d| d.0 != ib1)
            && state(&b1, conv).await.holds == 0
            && b1.account.mls_test_held_total().await.unwrap() >= 1
    );
    assert_eq!(state(&b1, conv).await.group_epoch, Some(1));
}

#[tokio::test]
async fn deep_fork_rejoins_and_resends_unacked() {
    let (ra, rb) = (record(A, &[A1, A2], 0), record(B, &[B1, B2], 0));
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

    // Each side commits without hearing the other: A1 once (epoch 2), B1 twice (epoch 3).
    a1.account.mls_test_hold_delivery(true).await.unwrap();
    b1.account.mls_test_hold_delivery(true).await.unwrap();
    let ma = a1.account.storage_test_fixture().messages().await.unwrap();
    let ka2 = chat_core::storage_fixture::mls_detached_key_package(a1.peer, [A2; 32]);
    let kb2 = chat_core::storage_fixture::mls_detached_key_package(b1.peer, [B2; 32]);
    let ib2 = chat_core::account::derive_device(Secret32::from([B2; 32]))
        .unwrap()
        .installation_id();
    assert_eq!(
        ma.mls_commit(conv, b1.peer, vec![ka2], vec![], NOW * 1000)
            .await
            .unwrap(),
        Some(2)
    );
    assert_eq!(
        mb.mls_commit(conv, a1.peer, vec![kb2], vec![], NOW * 1000)
            .await
            .unwrap(),
        Some(2)
    );
    b1.account
        .storage_test_fixture()
        .mls_clear_commit_flag(conv)
        .await
        .unwrap();
    assert_eq!(
        mb.mls_commit(conv, a1.peer, vec![], vec![ib2], NOW * 1000)
            .await
            .unwrap(),
        Some(3)
    );
    a1.account.mls_test_hold_delivery(false).await.unwrap();
    b1.account.mls_test_hold_delivery(false).await.unwrap();

    // B1 is two Commits away from A1's branch: it drops the group, rejoins through A1 and
    // seals its unacknowledged message again in the new group state.
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
