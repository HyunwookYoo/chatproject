#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared M4 harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Task 8: messages and read receipts travel as MLS application messages (kind 17), with
//! recovery from decrypt failures and the transition from M4 peers.
include!("support/mls_messaging.rs");

#[tokio::test]
async fn send_reaches_peer_encrypted_and_shows_delivered() {
    let (a1, b1, _net, _conv) = setup(&[B1]).await;
    // No group yet: sending asks for one, seals into it and delivers.
    let sent = send(&a1, &b1, 1, SENTINEL).await;
    assert_eq!(sent.state, DeliveryState::Queued);
    settle!(
        [&a1, &b1],
        delivery(&a1, sent.key).await == DeliveryState::Delivered
    );
    let got = message(&b1, sent.key).await.expect("stored by B");
    assert_eq!((got.body.as_str(), got.lamport), (SENTINEL, sent.lamport));
    assert!(!got.outgoing);
    // Delivered means the outbox row is gone (it never held a plaintext frame).
    assert_eq!(outbox(&a1, sent.key).await.codec, None);
    // Nothing that crossed the network carried the text, and no frame was an M4 plaintext.
    let all: Vec<Vec<u8>> = [frames(&a1).await, frames(&b1).await].concat();
    assert!(!all.is_empty());
    for frame in &all {
        assert!(
            !contains(frame, SENTINEL.as_bytes()),
            "plaintext on the wire"
        );
        assert_eq!(kind(frame), 17, "only kind 17 between two M5 devices");
    }
    assert_eq!(app_ciphertexts(&a1).await.len(), 1);
    // The Ack went out through the response gate like an M4 Receipt does.
    assert!(b1.account.message_test_write_starts().await.unwrap().1 >= 1);
}

#[tokio::test]
async fn read_receipt_travels_inside_mls() {
    let (a1, b1, _net, conv) = setup(&[B1]).await;
    let sent = send(&a1, &b1, 1, "read me").await;
    settle!(
        [&a1, &b1],
        delivery(&a1, sent.key).await == DeliveryState::Delivered
    );
    // A message received over MLS has no M4 delivery receipt job.
    assert_eq!(receipt_jobs(&b1).await, 0);
    let before = frames(&b1).await.len();
    b1.account
        .messages()
        .mark_read(conv, vec![sent.key])
        .await
        .unwrap();
    assert_eq!(receipt_jobs(&b1).await, 1);
    settle!(
        [&a1, &b1],
        delivery(&a1, sent.key).await == DeliveryState::Read && receipt_jobs(&b1).await == 0
    );
    // B sent the Read receipt as an MLS application message, never as an M4 Receipt.
    let after = frames(&b1).await;
    let receipts: Vec<_> = after[before..]
        .iter()
        .filter(|f| mls_app_from_wire(f).is_some())
        .collect();
    assert_eq!(receipts.len(), 1);
    assert!(after.iter().all(|f| kind(f) == 17));
    // Read never regresses, and a late repeat changes nothing.
    b1.account
        .messages()
        .mark_read(conv, vec![sent.key])
        .await
        .unwrap();
    assert_eq!(receipt_jobs(&b1).await, 0);
    assert_eq!(delivery(&a1, sent.key).await, DeliveryState::Read);
}

#[tokio::test]
async fn stale_epoch_retry_reencrypts_once() {
    let (a1, b1, _net, conv) = setup(&[B1, B2]).await;
    grouped(&a1, &b1, conv).await;
    // The message is sealed, then waits while the group moves on six epochs.
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    let sent = send(&a1, &b1, 1, "waited a long time").await;
    a1.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_seal(sent.key, NOW * 1000)
        .await
        .unwrap()
        .unwrap();
    let old = outbox(&a1, sent.key).await.wire;
    commits(&a1, &b1, conv, 6).await;
    // The same ciphertext goes out first; B cannot open it (epoch too old) and A seals it again
    // exactly once, then it is delivered.
    a1.account.message_test_hold_scheduler(false).await.unwrap();
    settle!(
        [&a1, &b1],
        delivery(&a1, sent.key).await == DeliveryState::Delivered
    );
    let cts = app_ciphertexts(&a1).await;
    assert_eq!(
        cts.len(),
        2,
        "one resend of the old ciphertext, one re-encryption"
    );
    assert_eq!(cts[0], mls_app_from_wire(&old).unwrap().1);
    assert_ne!(cts[0], cts[1]);
    let stored: Vec<_> = page(&b1, conv)
        .await
        .into_iter()
        .filter(|m| m.key == sent.key)
        .collect();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].body, "waited a long time");
}

#[tokio::test]
async fn missing_commit_is_redelivered_before_message() {
    let (a1, b1, _net, conv) = setup(&[B1, B2]).await;
    grouped(&a1, &b1, conv).await;
    // A Commit B never got (deliveries held), and a message sealed after it.
    a1.account.mls_test_hold_delivery(true).await.unwrap();
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    let messages = a1.account.storage_test_fixture().messages().await.unwrap();
    let epoch = messages
        .mls_commit(
            conv,
            b1.peer,
            vec![mls_detached_key_package(b1.peer, [B2; 32])],
            vec![],
            NOW * 1000,
        )
        .await
        .unwrap()
        .unwrap();
    let sent = send(&a1, &b1, 1, "after the commit").await;
    // B cannot open it: the Commit before it is missing (class 2). A keeps the ciphertext and
    // its re-encryption budget (M2): once the Commit is there the same ciphertext opens.
    let error = a1
        .account
        .message_test_attempt(sent.key, false, MessageMutation::None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotAvailable);
    assert_eq!(outbox(&a1, sent.key).await.reencrypts, 0);
    assert_eq!(state(&b1, conv).await.group_epoch, Some(epoch - 1));
    assert!(message(&b1, sent.key).await.is_none());

    // The next attempt sends the Commit ahead of the message over the same session.
    a1.account.mls_test_hold_delivery(false).await.unwrap();
    a1.account.message_test_hold_scheduler(false).await.unwrap();
    settle!(
        [&a1, &b1],
        delivery(&a1, sent.key).await == DeliveryState::Delivered
    );
    assert_eq!(state(&b1, conv).await.group_epoch, Some(epoch));
    let sent_frames = frames(&a1).await;
    let commit = sent_frames
        .iter()
        .position(|f| mls_commit_from_wire(f).is_some_and(|(_, e, _)| e == epoch))
        .expect("the Commit was sent again");
    let apps: Vec<usize> = sent_frames
        .iter()
        .enumerate()
        .filter(|(_, f)| mls_app_from_wire(f).is_some())
        .map(|(i, _)| i)
        .collect();
    assert_eq!(apps.len(), 2);
    assert!(commit < apps[1], "the Commit goes before the message");
    let cts = app_ciphertexts(&a1).await;
    assert!(cts[0] == cts[1], "the same ciphertext both times");
    assert_eq!(
        page(&b1, conv)
            .await
            .iter()
            .filter(|m| m.key == sent.key)
            .count(),
        1
    );
}

#[tokio::test]
async fn recipient_that_never_gets_the_missing_commit_fails_the_message() {
    let (a1, b1, _net, conv) = setup(&[B1, B2]).await;
    grouped(&a1, &b1, conv).await;
    // A Commit B never gets: A's deliveries stay held, so no attempt carries it ahead either.
    a1.account.mls_test_hold_delivery(true).await.unwrap();
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    let messages = a1.account.storage_test_fixture().messages().await.unwrap();
    let epoch = messages
        .mls_commit(
            conv,
            b1.peer,
            vec![mls_detached_key_package(b1.peer, [B2; 32])],
            vec![],
            NOW * 1000,
        )
        .await
        .unwrap()
        .unwrap();
    let sent = send(&a1, &b1, 1, "behind for good").await;
    // B answers class 2 from the epoch before, every time. The first answer is free (M2); a
    // repeat at the same epoch spends the budget, so the message ends Failed, not Queued.
    a1.account.message_test_hold_scheduler(false).await.unwrap();
    settle!(
        [&a1],
        delivery(&a1, sent.key).await == DeliveryState::Failed
    );
    assert_eq!(outbox(&a1, sent.key).await.reencrypts, 3);
    assert_eq!(state(&b1, conv).await.group_epoch, Some(epoch - 1));
    assert!(message(&b1, sent.key).await.is_none());
    // The user's Retry still works: once the Commit can travel, the message is delivered.
    let view = a1.account.messages().retry_send(sent.key).await.unwrap();
    assert_eq!(view.state, DeliveryState::Queued);
    a1.account.mls_test_hold_delivery(false).await.unwrap();
    settle!(
        [&a1, &b1],
        delivery(&a1, sent.key).await == DeliveryState::Delivered
    );
    assert_eq!(
        message(&b1, sent.key).await.unwrap().body,
        "behind for good"
    );
}

#[tokio::test]
async fn m4_peer_keeps_message_waiting_not_failed() {
    let (a1, b1, _net, _conv) = setup(&[B1]).await;
    b1.account.message_test_m4_peer(true).await.unwrap();
    let sent = send(&a1, &b1, 1, SENTINEL).await;
    settle!([&a1, &b1], outbox(&a1, sent.key).await.waiting_peer_update);
    // It keeps waiting (retrying with back-off), never failing, and is never sent in plaintext.
    for _ in 0..20 {
        a1.clock.advance_monotonic_ms(1_000);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(delivery(&a1, sent.key).await, DeliveryState::Queued);
    let row = outbox(&a1, sent.key).await;
    assert!(row.waiting_peer_update);
    assert_eq!(
        row.codec,
        Some(3),
        "never sealed: there is no group with an M4 peer"
    );
    assert!(message(&b1, sent.key).await.is_none());
    for frame in frames(&a1).await {
        assert!(!is_plaintext_text(&frame));
        assert!(!contains(&frame, SENTINEL.as_bytes()));
    }
}

#[tokio::test]
async fn m4_peer_text_is_accepted_during_transition() {
    let (a1, b1, _net, conv) = setup(&[B1]).await;
    b1.account.message_test_m4_peer(true).await.unwrap();
    b1.account.connect_account(a1.peer).await.unwrap();
    let sent = send(&b1, &a1, 1, "from an M4 app").await;
    settle!(
        [&a1, &b1],
        delivery(&b1, sent.key).await == DeliveryState::Delivered && receipt_jobs(&a1).await == 0
    );
    let got = message(&a1, sent.key).await.expect("A stored the M4 text");
    assert_eq!(got.body, "from an M4 app");
    // B (M4) sent its text in plaintext; A answered it, and sent no plaintext text itself.
    assert!(frames(&b1).await.iter().any(|f| is_plaintext_text(f)));
    assert!(!frames(&a1).await.iter().any(|f| is_plaintext_text(f)));
    assert_eq!(state(&a1, conv).await.group_epoch, None);
}

#[tokio::test]
async fn pre_update_m4_receipts_still_apply() {
    let (a1, b1, _net, conv) = setup(&[B1]).await;
    // Before the update both apps are M4 and exchange plaintext texts.
    a1.account.message_test_m4_peer(true).await.unwrap();
    b1.account.message_test_m4_peer(true).await.unwrap();
    b1.account.connect_account(a1.peer).await.unwrap();
    let from_a = send(&a1, &b1, 1, "before the update, from A").await;
    let from_b = send(&b1, &a1, 2, "before the update, from B").await;
    settle!(
        [&a1, &b1],
        delivery(&a1, from_a.key).await == DeliveryState::Delivered
            && delivery(&b1, from_b.key).await == DeliveryState::Delivered
            && receipt_jobs(&a1).await == 0
            && receipt_jobs(&b1).await == 0
    );
    // A updates; B does not yet.
    a1.account.message_test_m4_peer(false).await.unwrap();
    let before = frames(&a1).await.len();
    // B's M4 Read receipt for A's pre-update message still applies on A.
    b1.account
        .messages()
        .mark_read(conv, vec![from_a.key])
        .await
        .unwrap();
    // A's Read receipt for B's pre-update (plaintext) message goes back as an M4 receipt.
    a1.account
        .messages()
        .mark_read(conv, vec![from_b.key])
        .await
        .unwrap();
    settle!(
        [&a1, &b1],
        delivery(&a1, from_a.key).await == DeliveryState::Read
            && delivery(&b1, from_b.key).await == DeliveryState::Read
            && receipt_jobs(&a1).await == 0
    );
    let after = &frames(&a1).await[before..];
    assert!(after.iter().all(|f| kind(f) == 16 && !is_plaintext_text(f)));
}

#[tokio::test]
async fn m4_text_rejected_after_peer_sent_mls() {
    let (a1, b1, _net, conv) = setup(&[B1]).await;
    b1.account.connect_account(a1.peer).await.unwrap();
    let first = send(&b1, &a1, 1, "over mls").await;
    settle!(
        [&a1, &b1],
        delivery(&b1, first.key).await == DeliveryState::Delivered
    );
    assert_eq!(state(&a1, conv).await.peer_mls, Some(true));
    // B now sends a plaintext text (a downgrade): A refuses it and stores nothing.
    b1.account.message_test_m4_peer(true).await.unwrap();
    b1.account.message_test_hold_scheduler(true).await.unwrap();
    let second = send(&b1, &a1, 2, "plaintext after mls").await;
    let error = b1
        .account
        .message_test_attempt(second.key, false, MessageMutation::None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedMessageVersion);
    assert!(frames(&b1).await.iter().any(|f| is_plaintext_text(f)));
    assert!(message(&a1, second.key).await.is_none());
    assert_eq!(page(&a1, conv).await.len(), 1);
}

#[tokio::test]
async fn upgraded_outbox_delivers_with_same_msg_id() {
    let (a1, b1, net, _conv) = setup(&[B1]).await;
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    let queued = send(&a1, &b1, 1, "queued by the M4 app").await;
    // A's app stops; its store is rewritten into the M4 (v1) shape: a plaintext codec 1 job.
    let dir = stop(a1);
    SqlFixture::downgrade_to_v1(dir.path(), support::protector()).unwrap();
    let probe = SqlFixture::probe_database(dir.path(), support::protector()).unwrap();
    assert_eq!(probe.user_version, 1);
    assert_eq!(probe.outbox.len(), 1);
    assert_eq!(probe.outbox[0].codec, 1);
    // The updated app opens it: the store is upgraded and the job awaits encryption.
    let a1 = start(dir, net).await;
    link(&a1, &b1).await;
    let row = outbox(&a1, queued.key).await;
    assert_eq!((row.codec, row.wire.len()), (Some(3), 0));
    a1.account.connect_account(b1.peer).await.unwrap();
    settle!(
        [&a1, &b1],
        delivery(&a1, queued.key).await == DeliveryState::Delivered
    );
    let got = message(&b1, queued.key).await.expect("same message id");
    assert_eq!(
        (got.body.as_str(), got.lamport, got.sent_at_ms),
        ("queued by the M4 app", queued.lamport, queued.sent_at_ms)
    );
    assert!(!frames(&a1).await.iter().any(|f| is_plaintext_text(f)));
}

#[tokio::test]
async fn no_codec1_frame_is_ever_sent() {
    let (a1, b1, _net, conv) = setup(&[B1, B2]).await;
    b1.account.connect_account(a1.peer).await.unwrap();
    // Texts both ways, a read receipt each way, and a re-encryption after a stale epoch.
    let one = send(&a1, &b1, 1, SENTINEL).await;
    let two = send(&b1, &a1, 2, SENTINEL).await;
    settle!(
        [&a1, &b1],
        delivery(&a1, one.key).await == DeliveryState::Delivered
            && delivery(&b1, two.key).await == DeliveryState::Delivered
    );
    a1.account
        .messages()
        .mark_read(conv, vec![two.key])
        .await
        .unwrap();
    b1.account
        .messages()
        .mark_read(conv, vec![one.key])
        .await
        .unwrap();
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    let three = send(&a1, &b1, 3, SENTINEL).await;
    a1.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_seal(three.key, NOW * 1000)
        .await
        .unwrap()
        .unwrap();
    commits(&a1, &b1, conv, 6).await;
    a1.account.message_test_hold_scheduler(false).await.unwrap();
    settle!(
        [&a1, &b1],
        delivery(&a1, one.key).await == DeliveryState::Read
            && delivery(&b1, two.key).await == DeliveryState::Read
            && delivery(&a1, three.key).await == DeliveryState::Delivered
    );
    let all: Vec<Vec<u8>> = [frames(&a1).await, frames(&b1).await].concat();
    assert!(all.len() > 10);
    assert_eq!(all.iter().filter(|f| is_plaintext_text(f)).count(), 0);
    assert!(all.iter().all(|f| !contains(f, SENTINEL.as_bytes())));
}

#[tokio::test]
async fn receiver_never_surfaces_decrypt_failure() {
    let (a1, b1, _net, conv) = setup(&[B1, B2]).await;
    grouped(&a1, &b1, conv).await;
    let events = Arc::new(Events::default());
    b1.bus.subscribe(events.clone());
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    let sent = send(&a1, &b1, 1, "opened on the second try").await;
    a1.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_seal(sent.key, NOW * 1000)
        .await
        .unwrap()
        .unwrap();
    commits(&a1, &b1, conv, 6).await;
    // The first ciphertext fails on B; B shows nothing of it.
    let error = a1
        .account
        .message_test_attempt(sent.key, false, MessageMutation::None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotAvailable);
    assert!(page(&b1, conv).await.is_empty());
    assert_eq!(
        b1.account
            .messages()
            .list_conversations(None, 50)
            .await
            .unwrap()
            .items[0]
            .unread,
        0
    );
    a1.account.message_test_hold_scheduler(false).await.unwrap();
    settle!(
        [&a1, &b1],
        delivery(&a1, sent.key).await == DeliveryState::Delivered
    );
    // Afterwards B has exactly the message, and no event or row ever told of a failure.
    let items = page(&b1, conv).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].body, "opened on the second try");
    for event in events.0.lock().unwrap().iter() {
        match event {
            CoreEvent::MessageStateChanged { state, .. } => {
                assert_ne!(*state, DeliveryState::Failed)
            }
            CoreEvent::Error { .. } => panic!("an error event on the receiver: {event:?}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn retry_after_reencrypt_cap_starts_over() {
    let (a1, b1, _net, conv) = setup(&[B1]).await;
    grouped(&a1, &b1, conv).await;
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    let sent = send(&a1, &b1, 1, "given up, then retried").await;
    let messages = a1.account.storage_test_fixture().messages().await.unwrap();
    messages
        .mls_seal(sent.key, NOW * 1000)
        .await
        .unwrap()
        .unwrap();
    for _ in 0..3 {
        messages.mls_reencrypt(sent.key, NOW * 1000).await.unwrap();
    }
    assert_eq!(delivery(&a1, sent.key).await, DeliveryState::Failed);
    // A failed message is not due any more.
    a1.account.message_test_hold_scheduler(false).await.unwrap();
    for _ in 0..5 {
        a1.clock.advance_monotonic_ms(1_000);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(delivery(&a1, sent.key).await, DeliveryState::Failed);
    // The user's Retry starts the re-encryption count over and makes it due again.
    let view = a1.account.messages().retry_send(sent.key).await.unwrap();
    assert_eq!(view.state, DeliveryState::Queued);
    assert_eq!(outbox(&a1, sent.key).await.reencrypts, 0);
    settle!(
        [&a1, &b1],
        delivery(&a1, sent.key).await == DeliveryState::Delivered
    );
    assert_eq!(
        message(&b1, sent.key).await.unwrap().body,
        "given up, then retried"
    );
}

#[tokio::test]
async fn text_m4_would_refuse_fails_for_good_and_moves_the_ratchet() {
    let (a1, b1, _net, conv) = setup(&[B1]).await;
    grouped(&a1, &b1, conv).await;
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    // A lamport far beyond what B has seen: M4 refuses such a text, so B does too.
    a1.account
        .storage_test_fixture()
        .set_message_lamport(conv, 5_000_000)
        .await
        .unwrap();
    let sent = send(&a1, &b1, 1, "too far ahead").await;
    let before = state(&b1, conv).await;
    let error = a1
        .account
        .message_test_attempt(sent.key, false, MessageMutation::None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidMessage);
    assert_eq!(delivery(&a1, sent.key).await, DeliveryState::Failed);
    assert_eq!(outbox(&a1, sent.key).await.codec, None);
    // Nothing stored, but the ratchet moved and the ciphertext is remembered.
    assert!(message(&b1, sent.key).await.is_none());
    let after = state(&b1, conv).await;
    assert_ne!(after.kv_digest, before.kv_digest);
    assert_eq!(after.processed_mls, before.processed_mls + 1);
}

#[tokio::test]
async fn closing_m4_peer_marks_waiting_without_churn() {
    let (a1, b1, _net, _conv) = setup(&[B1]).await;
    // A released M4 app closes the connection on any kind 17 frame.
    b1.account.message_test_m4_peer_closing(true).await.unwrap();
    let sent = send(&a1, &b1, 1, SENTINEL).await;
    settle!([&a1, &b1], outbox(&a1, sent.key).await.waiting_peer_update);
    let kind17 = |frames: Vec<Vec<u8>>| frames.iter().filter(|f| kind(f) == 17).count();
    let asked = kind17(frames(&a1).await);
    assert!(asked >= 1);
    // A simulated minute: the M4 app's own texts keep arriving, and A does not ask it again.
    let texts = [
        send(&b1, &a1, 2, "from the M4 app").await,
        send(&b1, &a1, 3, "again from the M4 app").await,
    ];
    for _ in 0..60 {
        for f in [&a1, &b1] {
            f.clock.advance_monotonic_ms(1_000);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(kind17(frames(&a1).await), asked, "no kind 17 churn");
    for text in &texts {
        assert_eq!(
            message(&a1, text.key).await.map(|m| m.body),
            Some(text.body.clone()),
            "the M4 app's text arrived"
        );
    }
    let row = outbox(&a1, sent.key).await;
    assert!(row.waiting_peer_update);
    assert_eq!(row.codec, Some(3));
    assert_eq!(delivery(&a1, sent.key).await, DeliveryState::Queued);
    assert!(message(&b1, sent.key).await.is_none());
    for frame in frames(&a1).await {
        assert!(!is_plaintext_text(&frame));
        assert!(!contains(&frame, SENTINEL.as_bytes()));
    }
}

#[tokio::test]
async fn rejected_text_replay_is_not_delivered() {
    let (a1, b1, _net, conv) = setup(&[B1]).await;
    grouped(&a1, &b1, conv).await;
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    // A text B refuses (lamport far ahead), whose answer is lost after it was written.
    a1.account
        .storage_test_fixture()
        .set_message_lamport(conv, 5_000_000)
        .await
        .unwrap();
    let sent = send(&a1, &b1, 1, "refused").await;
    let mut pause = b1
        .account
        .message_test_pause(chat_core::account::MessagePoint::Response)
        .await
        .unwrap();
    let account = a1.account.clone();
    let key = sent.key;
    let task = tokio::spawn(async move {
        account
            .message_test_attempt(key, false, MessageMutation::None)
            .await
    });
    pause.wait_entered().await;
    let refused = state(&b1, conv).await.processed_mls;
    // A gives up waiting (the answer is lost); the text was sent but is not delivered.
    assert!(task.await.unwrap().is_err());
    pause.release();
    assert_ne!(delivery(&a1, sent.key).await, DeliveryState::Delivered);
    assert_ne!(delivery(&a1, sent.key).await, DeliveryState::Failed);
    // The same ciphertext again: B answers it as before (refused), never as stored.
    let _ = a1.account.connect_account(b1.peer).await;
    let error = a1
        .account
        .message_test_attempt(sent.key, false, MessageMutation::None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidMessage);
    assert_eq!(delivery(&a1, sent.key).await, DeliveryState::Failed);
    let cts = app_ciphertexts(&a1).await;
    assert_eq!(cts.len(), 2);
    assert_eq!(cts[0], cts[1], "a retry resends the same ciphertext");
    assert!(message(&b1, sent.key).await.is_none());
    assert_eq!(state(&b1, conv).await.processed_mls, refused);
}

#[tokio::test]
async fn downgrade_guard_survives_rejoin() {
    let (a1, b1, _net, conv) = setup(&[B1]).await;
    b1.account.connect_account(a1.peer).await.unwrap();
    let first = send(&b1, &a1, 1, "over mls").await;
    settle!(
        [&a1, &b1],
        delivery(&b1, first.key).await == DeliveryState::Delivered
    );
    assert!(state(&a1, conv).await.peer_spoke);
    // A leaves its group to rejoin: the group row (and its peer_mls) is gone or new.
    assert!(a1.account.mls_test_rejoin(b1.peer).await.unwrap());
    assert_ne!(state(&a1, conv).await.peer_mls, Some(true));
    assert!(state(&a1, conv).await.peer_spoke, "kept across the rejoin");
    // A plaintext text from B is still refused.
    b1.account.message_test_m4_peer(true).await.unwrap();
    b1.account.message_test_hold_scheduler(true).await.unwrap();
    let second = send(&b1, &a1, 2, "plaintext after the rejoin").await;
    let _ = b1.account.connect_account(a1.peer).await;
    let error = b1
        .account
        .message_test_attempt(second.key, false, MessageMutation::None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedMessageVersion);
    assert!(message(&a1, second.key).await.is_none());
}

#[tokio::test]
async fn closing_m4_peer_record_change_requeries_at_most_once() {
    let (a1, b1, net, _conv) = setup(&[B1]).await;
    b1.account.message_test_m4_peer_closing(true).await.unwrap();
    let sent = send(&a1, &b1, 1, SENTINEL).await;
    settle!(
        [&a1, &b1],
        outbox(&a1, sent.key).await.waiting_peer_update
    );
    let kind17 = |frames: Vec<Vec<u8>>| frames.iter().filter(|f| kind(f) == 17).count();
    let asked = kind17(frames(&a1).await);
    // B's record changes (a newer one listing the same device, adopted and republished by B,
    // still the M4 app), and A takes it.
    let newer = record(B, &[B1], 1);
    publish(&net, B, &newer);
    if b1.account.refresh_record(b1.peer).await.is_err() {
        b1.account
            .confirm_record_conflict(chat_core::account::record_hash(&newer))
            .await
            .unwrap();
    }
    // B publishes the record it settled on.
    settle!(
        [&a1, &b1],
        b1.view().await.publication.pending_count == 0 || {
            let _ = b1.account.refresh_record(b1.peer).await;
            false
        }
    );
    let renewed = net.packets.lock().unwrap()[&b1.peer].clone();
    if a1.account.refresh_record(b1.peer).await.is_err() {
        a1.account
            .confirm_remote_record(b1.peer, chat_core::account::record_hash(&renewed))
            .await
            .unwrap();
    }
    // Taken: asking again finds nothing to review.
    a1.account.refresh_record(b1.peer).await.unwrap();
    // A simulated minute: the newer record buys at most one more request, and B's texts arrive.
    let texts = [
        send(&b1, &a1, 2, "from the M4 app").await,
        send(&b1, &a1, 3, "again from the M4 app").await,
    ];
    for _ in 0..60 {
        for f in [&a1, &b1] {
            f.clock.advance_monotonic_ms(1_000);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let extra = kind17(frames(&a1).await) - asked;
    assert!(extra <= 1, "{extra} kind 17 requests after the record change");
    for text in &texts {
        assert_eq!(
            message(&a1, text.key).await.map(|m| m.body),
            Some(text.body.clone()),
            "the M4 app's text arrived"
        );
    }
    assert!(outbox(&a1, sent.key).await.waiting_peer_update);
    assert_eq!(delivery(&a1, sent.key).await, DeliveryState::Queued);
}

// ---- M6 Task 8: a ciphertext handed over by another device of its sender's account ----------

use chat_core::storage_fixture::MlsFrame;

/// A third installation of account A.
const A3: u8 = 15;

/// A's devices `seeds` and B1. Each of A's devices is a contact of B1's with a session to it
/// (none with each other); B1 makes the group with all of them.
async fn relay(seeds: &[u8]) -> (Vec<Fixture>, Fixture, Arc<Backend>, ConversationId) {
    let (ra, rb) = (record(A, seeds, 0), record(B, &[B1], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let mut a = Vec::new();
    for seed in seeds {
        a.push(device(A, *seed, &ra, &net).await);
    }
    let b1 = device(B, B1, &rb, &net).await;
    for x in &a {
        link(x, &b1).await;
        befriend(x, &b1).await;
    }
    befriend(&b1, &a[0]).await;
    for x in &a {
        x.account.connect_account(b1.peer).await.unwrap();
    }
    let conv = conversation(&b1, &a[0]).await;
    for x in &a {
        conversation(x, &b1).await;
    }
    ensure(&b1, a[0].peer).await;
    let mut all: Vec<&Fixture> = a.iter().collect();
    all.push(&b1);
    settle!(all.iter().copied(), {
        let mut ok = state(&b1, conv).await.deliveries.is_empty();
        for x in &a {
            ok = ok && agreed(x, &b1, conv).await;
        }
        ok
    });
    (a, b1, net, conv)
}

/// `from` seals text `n` for `to`, its own delivery held: the message and its MLS ciphertext.
async fn sealed(from: &Fixture, to: &Fixture, n: u8, body: &str) -> (MessageKey, Vec<u8>) {
    from.account
        .message_test_hold_scheduler(true)
        .await
        .unwrap();
    let sent = send(from, to, n, body).await;
    from.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_seal(sent.key, NOW * 1000)
        .await
        .unwrap()
        .unwrap();
    let wire = outbox(from, sent.key).await.wire;
    (sent.key, mls_app_from_wire(&wire).unwrap().1)
}

/// `via` hands `ciphertext` over to `to` as is, in a kind 17 `App` frame.
async fn hand_over(
    via: &Fixture,
    to: &Fixture,
    conversation: ConversationId,
    ciphertext: &[u8],
) -> Result<MlsReply, chat_core::CoreError> {
    via.account
        .mls_test_exchange(
            installation(to).await,
            MlsFrame::App {
                conversation,
                ciphertext: ciphertext.to_vec(),
            },
        )
        .await
}

/// Spec 7 and 11: B1 takes A1's ciphertext from A2's session (a mailbox relaying it) and
/// records A1, the device that sealed it, as its sender.
#[tokio::test]
async fn handed_off_ciphertext_is_accepted_and_recorded_as_origin() {
    let (a, b1, _net, conv) = relay(&[A1, A2]).await;
    let (a1, a2) = (&a[0], &a[1]);
    let (key, ct) = sealed(a1, &b1, 1, "handed over as is").await;
    let reply = hand_over(a2, &b1, conv, &ct).await.unwrap();
    assert!(acked(&reply), "B1 took it from A2");
    let got = message(&b1, key).await.expect("stored under A1's key");
    assert_eq!(got.key.installation, installation(a1).await);
    assert_eq!(got.body, "handed over as is");
    assert!(!got.outgoing);
    assert_eq!(page(&b1, conv).await.len(), 1);
}

/// Spec 11: a ciphertext sealed by a device A revoked, or by a device of another account than
/// the one whose session carries it, is refused (the M4 `Reject(InvalidMessage)`) and leaves
/// nothing stored.
#[tokio::test]
async fn revoked_or_foreign_leaf_via_mailbox_is_rejected() {
    let (a, b1, net, conv) = relay(&[A1, A2, A3]).await;
    let (a1, a2, a3) = (&a[0], &a[1], &a[2]);
    let all = [a1, a2, a3, &b1];
    let kept = |s: &MlsState| (s.kv_digest, s.processed_mls);

    // B1 carries what A2 sealed to A1 as if from B's account.
    let (foreign, ct) = sealed(a2, &b1, 1, "a leaf of A's, carried by B1").await;
    let before = state(a1, conv).await;
    let error = hand_over(&b1, a1, conv, &ct).await.err().unwrap();
    assert_eq!(error.code, ErrorCode::InvalidMessage);
    assert!(message(a1, foreign).await.is_none());
    assert!(page(a1, conv).await.is_empty());
    // Opened, then refused for its leaf: nothing of it kept.
    let after = state(a1, conv).await;
    assert_eq!(kept(&after), kept(&before));
    assert_eq!(after.open_calls, before.open_calls + 1);

    // A3 seals a message, then A1 revokes A3. B1 takes A's new record (the user confirms the
    // changed device list) and the Commit that removes A3.
    let ia3 = installation(a3).await;
    let (revoked, ct) = sealed(a3, &b1, 2, "sealed before the revocation").await;
    a1.account.revoke_device(ia3).await.unwrap();
    // Looking at its own record moves A1's publication on.
    settle!(
        all,
        a1.view().await.publication.pending_count == 0 || {
            let _ = a1.account.refresh_record(a1.peer).await;
            false
        }
    );
    // Then A1 has a new session with B1 (B1 dropped A's sessions when it took the record).
    settle!(
        all,
        (!state(&b1, conv).await.members.contains(&ia3)
            && a1.account.connect_account(b1.peer).await.is_ok())
            || {
                let renewed = net.packets.lock().unwrap()[&a1.peer].clone();
                if b1.account.refresh_record(a1.peer).await.is_err() {
                    let _ = b1
                        .account
                        .confirm_remote_record(a1.peer, chat_core::account::record_hash(&renewed))
                        .await;
                }
                false
            }
    );
    // A1 hands A3's ciphertext over. It is from the epoch before the removal, which B1 still
    // opens.
    let before = state(&b1, conv).await;
    let error = hand_over(a1, &b1, conv, &ct).await.err().unwrap();
    assert_eq!(error.code, ErrorCode::InvalidMessage);
    assert!(message(&b1, revoked).await.is_none());
    // Opened, then refused for its leaf (B1's group state itself may move on meanwhile with
    // the Commits after the removal, so only what this ciphertext would add is compared).
    let after = state(&b1, conv).await;
    assert_eq!(after.processed_mls, before.processed_mls);
    assert_eq!(after.open_calls, before.open_calls + 1);
}

/// Review Focus 4: the mailbox A2 and the origin A1 both send B1 the same ciphertext. B1
/// stores it once and answers both `Ack`, the second without opening it; A1 never seals it
/// again and shows it delivered once.
#[tokio::test]
async fn same_ciphertext_via_origin_and_mailbox_acks_twice_stores_once() {
    let (a, b1, _net, conv) = relay(&[A1, A2]).await;
    let (a1, a2) = (&a[0], &a[1]);
    let (key, ct) = sealed(a1, &b1, 1, "one message, two ways").await;
    let opened = state(&b1, conv).await.open_calls;
    assert!(acked(&hand_over(a2, &b1, conv, &ct).await.unwrap()));
    assert_eq!(state(&b1, conv).await.open_calls, opened + 1);

    // A1 sends the same ciphertext itself: answered from `processed_ct`.
    a1.account.message_test_hold_scheduler(false).await.unwrap();
    settle!(
        [a1, a2, &b1],
        delivery(a1, key).await == DeliveryState::Delivered
    );
    // And the mailbox once more.
    assert!(acked(&hand_over(a2, &b1, conv, &ct).await.unwrap()));
    assert_eq!(state(&b1, conv).await.open_calls, opened + 1);

    assert_eq!(page(&b1, conv).await.len(), 1);
    assert_eq!(
        message(&b1, key).await.unwrap().body,
        "one message, two ways"
    );
    let cts = app_ciphertexts(a1).await;
    assert!(!cts.is_empty());
    assert!(cts.iter().all(|c| *c == ct), "A1 sealed it again");
    let delivered = a1
        .events
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|e| match e {
            CoreEvent::MessageStateChanged { key: k, state, .. } => {
                *k == key && *state == DeliveryState::Delivered
            }
            _ => false,
        })
        .count();
    assert_eq!(delivered, 1);
}

/// Waits in real time, without moving any clock, until the text shows `expected`.
async fn shows(f: &Fixture, key: MessageKey, expected: DeliveryState) -> MessageView {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(view) = message(f, key).await
                && view.state == expected
            {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("never {expected:?}"))
}

/// M4's contact hold is kept (M5 spec: states as in M4; a peer that has not allowed you is
/// held `ContactRequired`): a text to an account that has not added this one ends `Blocked`,
/// not waiting for a group forever, and after it adds this one the user's retry delivers it.
/// No clock moves, as in the e2e m4 scenario. With `a2` A's second device is asked too and
/// hands a KeyPackage; an own device's answer does not lift the hold.
async fn held_until_retry(a2: bool) {
    let own: &[u8] = if a2 { &[A1, A2] } else { &[A1] };
    let (ra, rb) = (record(A, own, 0), record(B, &[B1], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    let second = if a2 {
        let a2 = device(A, A2, &ra, &net).await;
        link(&a1, &a2).await;
        a1.account.connect_account(a1.peer).await.unwrap();
        Some(a2)
    } else {
        None
    };
    // A has B as a contact; B does not have A.
    befriend(&a1, &b1).await;
    a1.account.connect_account(b1.peer).await.unwrap();
    let service = a1.account.messages();
    let sent = service
        .send_text(b1.peer, MessageId([7; 16]), "private text".into())
        .await
        .unwrap();
    let held = shows(&a1, sent.key, DeliveryState::Blocked).await;
    assert_eq!(held.block, Some(BlockReason::ContactRequired));
    if let Some(a2) = &second {
        let asked = a1
            .account
            .mls_test_sent_frames_to(installation(a2).await)
            .await
            .unwrap();
        assert!(asked.iter().any(|f| f == "key_package_request"));
    }
    befriend(&b1, &a1).await;
    service.retry_send(sent.key).await.unwrap();
    shows(&a1, sent.key, DeliveryState::Delivered).await;
    let got = message(&b1, sent.key).await.expect("stored by B");
    assert_eq!(got.body, "private text");
}

#[tokio::test]
async fn text_to_account_without_us_is_held_until_retry() {
    held_until_retry(false).await;
}

#[tokio::test]
async fn own_device_key_package_does_not_lift_the_contact_hold() {
    held_until_retry(true).await;
}

/// The user's retry restarts a group setup that is backing off: without it a stopped clock
/// would never end the back-off, and the retried text would wait forever.
#[tokio::test]
async fn retry_restarts_a_group_setup_that_backs_off() {
    let (ra, rb) = (record(A, &[A1], 0), record(B, &[B1], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    befriend(&a1, &b1).await;
    a1.account.connect_account(b1.peer).await.unwrap();
    let service = a1.account.messages();
    let sent = service
        .send_text(b1.peer, MessageId([8; 16]), "private text".into())
        .await
        .unwrap();
    shows(&a1, sent.key, DeliveryState::Blocked).await;
    befriend(&b1, &a1).await;
    // With no session to B, asking for the group backs off (until a time that never comes).
    a1.account
        .mls_test_close_session(installation(&b1).await)
        .await
        .unwrap();
    assert_eq!(ensure(&a1, b1.peer).await, GroupState::WaitingForPeer);
    a1.account.connect_account(b1.peer).await.unwrap();
    service.retry_send(sent.key).await.unwrap();
    shows(&a1, sent.key, DeliveryState::Delivered).await;
}
