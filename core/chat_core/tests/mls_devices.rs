#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared M4 harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Application messages between accounts with more than one device, through the actors: a
//! device that fell behind one sender recovers by itself (C1).
include!("support/mls_messaging.rs");

/// A = [A1, A2], B = [B1]. A1 and A2 each have B as a contact and dial B1 (B1 dials nobody, so
/// no `connect_account` of B's takes A's sessions away). B1 makes the group with both.
async fn three() -> (Fixture, Fixture, Fixture, Arc<Backend>, ConversationId) {
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
    conversation(&a1, &b1).await;
    conversation(&a2, &b1).await;
    ensure(&b1, a1.peer).await;
    settle!(
        [&a1, &a2, &b1],
        agreed(&a1, &b1, conv).await
            && agreed(&a2, &b1, conv).await
            && state(&b1, conv).await.deliveries.is_empty()
    );
    (a1, a2, b1, net, conv)
}

/// `n` messages from `from` that only `to` receives, sealed and opened through the two stores
/// (the message path would take 100 ms per message): they move `from`'s sender ratchet `n`
/// generations on, exactly like `n` messages the scheduler sends to `to` alone.
async fn sent_only_to(from: &Fixture, to: &Fixture, conv: ConversationId, n: u32) {
    let fixture = from.account.storage_test_fixture();
    let messages = fixture.messages().await.unwrap();
    let opener = to.account.storage_test_fixture().messages().await.unwrap();
    let sender = installation(from).await;
    for i in 0..n {
        let mut id = [0xC1; 16];
        id[..4].copy_from_slice(&i.to_be_bytes());
        let sent = messages
            .enqueue(to.peer, MessageId(id), format!("burst {i}"), NOW * 1000)
            .await
            .unwrap();
        messages
            .mls_seal(sent.key, NOW * 1000)
            .await
            .unwrap()
            .unwrap();
        let wire = fixture.mls_outbox(sent.key).await.unwrap().wire;
        let (_, ct) = mls_app_from_wire(&wire).unwrap();
        let reply = opener
            .mls_open_app(from.peer, sender, conv, ct, NOW * 1000)
            .await
            .unwrap();
        assert!(acked(&reply), "message {i}");
        fixture.deliver_message(sent.key).await.unwrap();
    }
}

fn no_failure(events: &Events) {
    for event in events.0.lock().unwrap().iter() {
        match event {
            CoreEvent::MessageStateChanged { state, .. } => {
                assert_ne!(*state, DeliveryState::Failed, "{event:?}")
            }
            CoreEvent::Error { .. } => panic!("an error event: {event:?}"),
            _ => {}
        }
    }
}

/// C1: B1 sent more than `maximum_forward_distance` (1000) messages in one epoch, all to A1.
/// A2 saw none of them, so B1's next ciphertext is too far ahead of A2's ratchet for B1 to
/// ever fix by sealing again in that epoch. A2 moves the group to a new epoch itself (an empty
/// Commit); B1 waits for it, seals the message again there without spending its re-encryption
/// budget, and A2 stores it. B1 never shows the message as failed; A2 shows nothing of it.
#[tokio::test]
async fn lagging_device_recovers_when_a_sender_ran_far_ahead() {
    let (a1, a2, b1, _net, conv) = three().await;
    b1.account.message_test_hold_scheduler(true).await.unwrap();
    sent_only_to(&b1, &a1, conv, 1_001).await;
    let epoch = state(&b1, conv).await.group_epoch.unwrap();
    // A1 goes away: the next message can only go to A2.
    drop(a1);
    let (events_b, events_a) = (Arc::new(Events::default()), Arc::new(Events::default()));
    b1.bus.subscribe(events_b.clone());
    a2.bus.subscribe(events_a.clone());
    b1.account.message_test_hold_scheduler(false).await.unwrap();
    let sent = send(&b1, &a2, 7, "after the burst").await;
    settle!(
        [&a2, &b1],
        matches!(
            delivery(&b1, sent.key).await,
            DeliveryState::Delivered | DeliveryState::Failed
        )
    );
    assert_eq!(delivery(&b1, sent.key).await, DeliveryState::Delivered);
    no_failure(&events_b);
    no_failure(&events_a);
    // A2 moved the group on by itself, once, and B1 followed it.
    assert_eq!(a2.account.mls_test_recovery_total().await.unwrap(), 1);
    assert_eq!(state(&a2, conv).await.group_epoch, Some(epoch + 1));
    assert_eq!(state(&b1, conv).await.group_epoch, Some(epoch + 1));
    // The first ciphertext (resent as is while B1 waited for the Commit), then one sealed in
    // the new epoch.
    let mut cts = app_ciphertexts(&b1).await;
    let last = cts.last().cloned().unwrap();
    cts.dedup();
    assert_eq!(cts.len(), 2, "two ciphertexts");
    assert!(cts[1] == last);
    let stored: Vec<_> = page(&a2, conv)
        .await
        .into_iter()
        .filter(|m| m.key == sent.key)
        .collect();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].body, "after the burst");
}

/// A = [A1]; B = [B1, B2] where B2 still runs the released M4 app (the M4-peer fixture). B2
/// dials A1 first, so its session is the first of B's in A1's registry. B1 makes the group.
async fn mixed() -> (Fixture, Fixture, Fixture, Arc<Backend>, ConversationId) {
    let (ra, rb) = (record(A, &[A1], 0), record(B, &[B1, B2], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    let b2 = device(B, B2, &rb, &net).await;
    b2.account.message_test_m4_peer(true).await.unwrap();
    link(&a1, &b1).await;
    link(&a1, &b2).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    befriend(&b2, &a1).await;
    b2.account.connect_account(a1.peer).await.unwrap();
    b1.account.connect_account(a1.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;
    conversation(&b1, &a1).await;
    conversation(&b2, &a1).await;
    ensure(&b1, a1.peer).await;
    settle!(
        [&a1, &b1, &b2],
        agreed(&a1, &b1, conv).await && state(&b1, conv).await.deliveries.is_empty()
    );
    (a1, b1, b2, net, conv)
}

/// I2: one account, one device on M5 and one on M4. A's messages go to the M5 device that is a
/// group member, not to the M4 device that refuses kind 17 (which must not make the whole
/// account wait for an update). After B1 spoke MLS, B2's plaintext texts are still taken: the
/// downgrade guard refuses plaintext only from an installation that itself spoke MLS.
#[tokio::test]
async fn m5_and_m4_devices_of_one_peer_account() {
    let (a1, b1, b2, _net, conv) = mixed().await;
    let ib2 = installation(&b2).await;
    // A1's first session to B is B2's, the M4 app.
    let sent = send(&a1, &b1, 1, "to the device that can read it").await;
    settle!(
        [&a1, &b1, &b2],
        delivery(&a1, sent.key).await == DeliveryState::Delivered
            || outbox(&a1, sent.key).await.waiting_peer_update
    );
    assert_eq!(delivery(&a1, sent.key).await, DeliveryState::Delivered);
    assert!(message(&b1, sent.key).await.is_some());
    assert!(!state(&a1, conv).await.members.contains(&ib2));
    // B1 speaks MLS to A1.
    let from_b1 = send(&b1, &a1, 2, "over mls").await;
    settle!(
        [&a1, &b1, &b2],
        delivery(&b1, from_b1.key).await == DeliveryState::Delivered
    );
    assert_eq!(state(&a1, conv).await.peer_spoke, true);
    // B2, which never spoke MLS, still gets its M4 text through.
    let from_b2 = send(&b2, &a1, 3, "from the M4 app").await;
    settle!(
        [&a1, &b1, &b2],
        matches!(
            delivery(&b2, from_b2.key).await,
            DeliveryState::Delivered | DeliveryState::Failed
        )
    );
    assert_eq!(delivery(&b2, from_b2.key).await, DeliveryState::Delivered);
    assert_eq!(
        message(&a1, from_b2.key).await.unwrap().body,
        "from the M4 app"
    );
    // And A's next message still goes to B1 without waiting for B's "update".
    let again = send(&a1, &b1, 4, "still to B1").await;
    settle!(
        [&a1, &b1, &b2],
        delivery(&a1, again.key).await == DeliveryState::Delivered
            || outbox(&a1, again.key).await.waiting_peer_update
    );
    assert_eq!(delivery(&a1, again.key).await, DeliveryState::Delivered);
    // B2 holds no leaf and refused kind 17: no copy went to it.
    assert_eq!(app_ciphertexts(&a1).await.len(), 2);
}

/// The two seeds ordered by installation id: a dial to the account reaches the first.
fn ordered(x: u8, y: u8) -> (u8, u8) {
    let id = |seed: u8| {
        derive_device(Secret32::from([seed; 32]))
            .unwrap()
            .installation_id()
    };
    if id(x) < id(y) { (x, y) } else { (y, x) }
}

/// A = [A1, A2], B = [B1, B2], returned as each account's first and second device by
/// installation id (`af`, `as`, `bf`, `bs`): a dial to an account reaches its first device.
/// The sessions come from dials that end there (`bf` and `bs` dial A, `as` dials A and B, `bs`
/// dials B); `af` dials nobody, so none of its own dials takes them away. `af` and `bf` reach
/// all three other devices; `as` and `bs` reach the two first devices.
async fn four() -> (
    Fixture,
    Fixture,
    Fixture,
    Fixture,
    Arc<Backend>,
    ConversationId,
) {
    let (ra, rb) = (record(A, &[A1, A2], 0), record(B, &[B1, B2], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let ((a_first, a_second), (b_first, b_second)) = (ordered(A1, A2), ordered(B1, B2));
    let af = device(A, a_first, &ra, &net).await;
    let a_s = device(A, a_second, &ra, &net).await;
    let bf = device(B, b_first, &rb, &net).await;
    let bs = device(B, b_second, &rb, &net).await;
    for (x, y) in [(&af, &bf), (&af, &bs), (&af, &a_s), (&a_s, &bf), (&bf, &bs)] {
        link(x, y).await;
    }
    befriend(&af, &bf).await;
    befriend(&a_s, &bf).await;
    befriend(&bf, &af).await;
    befriend(&bs, &af).await;
    bf.account.connect_account(af.peer).await.unwrap();
    bs.account.connect_account(af.peer).await.unwrap();
    a_s.account.connect_account(af.peer).await.unwrap();
    bs.account.connect_account(bf.peer).await.unwrap();
    a_s.account.connect_account(bf.peer).await.unwrap();
    let conv = conversation(&af, &bf).await;
    for f in [&a_s, &bf, &bs] {
        conversation(f, if f.peer == af.peer { &bf } else { &af }).await;
    }
    ensure(&af, bf.peer).await;
    settle!(
        [&af, &a_s, &bf, &bs],
        agreed(&af, &a_s, conv).await
            && agreed(&af, &bf, conv).await
            && agreed(&af, &bs, conv).await
            && state(&af, conv).await.deliveries.is_empty()
    );
    (af, a_s, bf, bs, net, conv)
}

async fn unread(f: &Fixture, conv: ConversationId) -> u64 {
    f.account
        .messages()
        .list_conversations(None, 50)
        .await
        .unwrap()
        .items
        .iter()
        .find(|c| c.id == conv)
        .map_or(0, |c| c.unread)
}

/// I3 (spec 5.3): a text goes to one device of the peer, which makes it Delivered, and the same
/// ciphertext goes best effort to the peer's other device and to the sender's own other device.
/// The own copy is the sender account's outgoing message there: shown sent, not unread, never
/// answered with a receipt. Both directions, two devices per account.
#[tokio::test]
async fn texts_reach_the_other_devices_of_both_accounts() {
    let (af, a_s, bf, bs, _net, conv) = four().await;
    let all = [&af, &a_s, &bf, &bs];
    let sent = send(&af, &bf, 1, "to every device").await;
    settle!(
        all,
        delivery(&af, sent.key).await == DeliveryState::Delivered
            && message(&bf, sent.key).await.is_some()
            && message(&bs, sent.key).await.is_some()
            && message(&a_s, sent.key).await.is_some()
    );
    for b in [&bf, &bs] {
        let got = message(b, sent.key).await.unwrap();
        assert!(!got.outgoing);
        assert_eq!(
            (got.body.as_str(), got.lamport),
            ("to every device", sent.lamport)
        );
        assert_eq!(page(b, conv).await.len(), 1);
    }
    let copy = message(&a_s, sent.key).await.unwrap();
    assert!(copy.outgoing);
    assert_eq!(copy.state, DeliveryState::SentDirect);
    assert_eq!(
        (copy.body.as_str(), copy.lamport),
        ("to every device", sent.lamport)
    );
    assert_eq!(unread(&a_s, conv).await, 0);
    assert_eq!(receipt_jobs(&a_s).await, 0);
    assert!(
        a_s.account
            .messages()
            .mark_read(conv, vec![sent.key])
            .await
            .is_err(),
        "an own message is never read with a receipt"
    );

    let back = send(&bf, &af, 2, "and back").await;
    settle!(
        all,
        delivery(&bf, back.key).await == DeliveryState::Delivered
            && message(&af, back.key).await.is_some()
            && message(&a_s, back.key).await.is_some()
            && message(&bs, back.key).await.is_some()
    );
    for a in [&af, &a_s] {
        assert!(!message(a, back.key).await.unwrap().outgoing);
    }
    let copy = message(&bs, back.key).await.unwrap();
    assert!(copy.outgoing);
    assert_eq!(copy.state, DeliveryState::SentDirect);
    // Copies changed no status: each sender's message is Delivered, A2's copy still sent.
    idle(&all, 5).await;
    assert_eq!(delivery(&af, sent.key).await, DeliveryState::Delivered);
    assert_eq!(delivery(&a_s, sent.key).await, DeliveryState::SentDirect);
    assert_eq!(page(&a_s, conv).await.len(), 2);
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

async fn key_packages_kept(f: &Fixture) -> usize {
    f.account
        .storage_test_fixture()
        .mls_global_rows(None)
        .await
        .unwrap()
        .iter()
        .filter(|label| *label == "KeyPackage")
        .count()
}

/// I1: a KeyPackageRequest from one installation is answered at most once a second, and only
/// its newest 4 KeyPackages are kept (each holds private keys until it is used).
#[tokio::test]
async fn key_package_requests_are_rate_limited_and_capped() {
    let (a1, b1, _net, _conv) = setup(&[B1]).await;
    let ask = || async { a1.account.mls_test_ask_key_package(b1.peer).await.unwrap() };
    let ib = installation(&b1).await;
    assert_eq!(ask().await, vec![(ib, true)]);
    assert_eq!(ask().await, vec![(ib, false)], "too soon: Busy");
    for _ in 0..10 {
        b1.clock.advance_monotonic_ms(1_100);
        assert_eq!(ask().await, vec![(ib, true)]);
    }
    assert_eq!(key_packages_kept(&b1).await, 4);
}

/// I1: a device whose `JoinRequest` keeps being refused asks again and again; each time it
/// deletes the KeyPackage of the request before, so it never keeps more than one.
#[tokio::test]
async fn a_stuck_join_keeps_one_key_package() {
    let (ra, rb) = (record(A, &[A1, A2], 0), record(B, &[B1, B2], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let a2 = device(A, A2, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    link(&a1, &a2).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    befriend(&a2, &b1).await;
    a1.account.connect_account(b1.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;
    conversation(&b1, &a1).await;
    conversation(&a2, &b1).await;
    grouped(&a1, &b1, conv).await;
    // A1's own Commit stays unacknowledged: it answers every JoinRequest Busy.
    a1.account.mls_test_hold_delivery(true).await.unwrap();
    commits_held(&a1, &b1, conv).await;
    a2.account.connect_account(a2.peer).await.unwrap();
    ensure(&a2, b1.peer).await;
    settle!(
        [&a1, &a2, &b1],
        a2.account
            .mls_test_sent_frames()
            .await
            .unwrap()
            .iter()
            .filter(|f| *f == "join_request")
            .count()
            >= 4
    );
    assert_eq!(state(&a2, conv).await.group_epoch, None);
    assert!(key_packages_kept(&a2).await <= 1);
}

/// One own Commit on `a` (adding B's never-online B2) whose delivery is held.
async fn commits_held(a: &Fixture, b: &Fixture, conv: ConversationId) {
    let messages = a.account.storage_test_fixture().messages().await.unwrap();
    messages
        .mls_commit(
            conv,
            b.peer,
            vec![mls_detached_key_package(b.peer, [B2; 32])],
            vec![],
            NOW * 1000,
        )
        .await
        .unwrap()
        .expect("an own Commit");
    assert_eq!(state(a, conv).await.own_commit_unacked, Some(true));
}
