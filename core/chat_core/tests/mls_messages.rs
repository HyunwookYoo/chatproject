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
    // B cannot open it: the Commit before it is missing (class 2). A seals it again.
    let error = a1
        .account
        .message_test_attempt(sent.key, false, MessageMutation::None)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotAvailable);
    assert_eq!(outbox(&a1, sent.key).await.reencrypts, 1);
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
    assert!(
        commit < apps[1],
        "the Commit goes before the re-encrypted message"
    );
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
