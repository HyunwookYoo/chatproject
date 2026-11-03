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
