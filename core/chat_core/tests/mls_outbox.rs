#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Task 8: the send-side guarantees the M4 suites (`message_auth.rs`, `message_outbox.rs`) prove
//! for kind 16, proved again between two M5 cores, where texts travel sealed (kind 17) and the
//! recipient's `Ack` takes the place of the M4 `Receipt`.
include!("support/mls_messaging.rs");

use chat_core::account::MessagePoint;

/// A third account, offline in one test.
const C: u8 = 3;
const C1: u8 = 15;

/// Two M5 cores sharing a group; attempts are driven by hand.
async fn manual() -> (Fixture, Fixture, Arc<Backend>, ConversationId) {
    let (a, b, net, conv) = setup(&[B1]).await;
    grouped(&a, &b, conv).await;
    for f in [&a, &b] {
        f.account.message_test_hold_scheduler(true).await.unwrap();
    }
    (a, b, net, conv)
}
async fn facts(f: &Fixture, key: MessageKey) -> chat_core::storage_fixture::MessageFacts {
    f.account
        .storage_test_fixture()
        .message_facts(key)
        .await
        .unwrap()
}
/// The delivery state as stored (readable also while the account's record has expired).
async fn stored_state(f: &Fixture, key: MessageKey) -> Option<MessageView> {
    f.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .load_messages(key.conversation, None, 50)
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|m| m.key == key)
}
async fn attempt(f: &Fixture, key: MessageKey) -> Result<(), CoreError> {
    f.account
        .message_test_attempt(key, false, MessageMutation::None)
        .await
}
async fn lifecycle(f: &Fixture, suspend: bool) {
    if suspend {
        f.account.request_suspend();
    } else {
        f.account.request_resume();
    }
    lifecycle_idle_handle(&f.account).await;
}

#[tokio::test]
async fn lost_text_ack_retries_same_payload_without_duplicate_storage() {
    let (a, b, _net, conv) = manual().await;
    let m = send(&a, &b, 9, "private text").await;
    let mut pause = b
        .account
        .message_test_pause(MessagePoint::Response)
        .await
        .unwrap();
    let account = a.account.clone();
    let task = tokio::spawn(async move { attempt_on(&account, m.key).await });
    pause.wait_entered().await;
    // B stored the text; its record expires before the Ack is written.
    let stored = state(&b, conv).await.processed_mls;
    b.clock.set_unix_seconds(NOW + 600);
    pause.release();
    assert!(task.await.unwrap().is_err());
    b.clock.set_unix_seconds(NOW);
    assert_ne!(
        stored_state(&a, m.key).await.unwrap().state,
        DeliveryState::Delivered
    );
    let sealed = outbox(&a, m.key).await.wire;
    a.account.connect_account(b.peer).await.unwrap();
    attempt(&a, m.key).await.unwrap();
    // The same ciphertext went again; B recognised it and stored nothing new.
    let cts = app_ciphertexts(&a).await;
    assert_eq!(cts.len(), 2);
    assert_eq!(cts[0], cts[1]);
    assert_eq!(cts[0], mls_app_from_wire(&sealed).unwrap().1);
    assert_eq!(state(&b, conv).await.processed_mls, stored);
    assert_eq!(facts(&b, m.key).await.messages, 1);
    assert_eq!(delivery(&a, m.key).await, DeliveryState::Delivered);
    assert_eq!(facts(&a, m.key).await.jobs, 0);
}
async fn attempt_on(account: &AccountHandle, key: MessageKey) -> Result<(), CoreError> {
    account
        .message_test_attempt(key, false, MessageMutation::None)
        .await
}

#[tokio::test]
async fn same_wire_after_lost_ack_and_crash() {
    let (a, b, net, conv) = setup(&[B1]).await;
    grouped(&a, &b, conv).await;
    let mut pause = b
        .account
        .message_test_pause(MessagePoint::Response)
        .await
        .unwrap();
    // Automatic delivery sends it; B stores it, and A's app dies before the Ack arrives.
    let m = send(&a, &b, 9, "private text").await;
    tokio::time::timeout(Duration::from_secs(10), pause.wait_entered())
        .await
        .unwrap();
    let before = outbox(&a, m.key).await;
    assert_eq!(before.codec, Some(2));
    assert_eq!(facts(&b, m.key).await.messages, 1);
    let processed = state(&b, conv).await.processed_mls;
    let a = start(stop(a), net).await;
    link(&a, &b).await;
    let recovered = outbox(&a, m.key).await;
    assert_eq!(recovered.dispatch_state, 0);
    assert_eq!(recovered.wire, before.wire, "the same ciphertext is kept");
    settle!(
        [&a, &b],
        delivery(&a, m.key).await == DeliveryState::Delivered
    );
    pause.release();
    // B has the text once and opened no second ciphertext.
    assert_eq!(
        page(&b, conv)
            .await
            .iter()
            .filter(|x| x.key == m.key)
            .count(),
        1
    );
    assert_eq!(state(&b, conv).await.processed_mls, processed);
    for ct in app_ciphertexts(&a).await {
        assert_eq!(ct, mls_app_from_wire(&before.wire).unwrap().1);
    }
}

#[tokio::test]
async fn expiry_after_db_commit_sends_no_ack() {
    let (a, b, _net, _conv) = manual().await;
    let m = send(&a, &b, 9, "private text").await;
    // B's record expires right after B committed the opened text.
    b.account
        .storage_test_fixture()
        .message_clock_after_operation(b.clock.clone(), NOW + 600)
        .await;
    assert!(attempt(&a, m.key).await.is_err());
    let remote = facts(&b, m.key).await;
    assert_eq!(
        (remote.messages, remote.processed, remote.receipt_jobs),
        (1, 1, 0)
    );
    assert_eq!(facts(&a, m.key).await.jobs, 1);
    assert_ne!(
        stored_state(&a, m.key).await.unwrap().state,
        DeliveryState::Delivered
    );
}

#[tokio::test]
async fn expiry_after_response_handoff_sends_no_ack() {
    let (a, b, _net, _conv) = manual().await;
    let m = send(&a, &b, 9, "private text").await;
    // The checkpoint is keyed: it holds exactly the Ack that stores this text.
    let replies = b.account.message_test_write_starts().await.unwrap().1;
    let (mut response, _first_byte) = b.account.message_ack_pause(m.key).await.unwrap();
    let account = a.account.clone();
    let task = tokio::spawn(async move { attempt_on(&account, m.key).await });
    response.wait_entered().await;
    assert_eq!(facts(&b, m.key).await.messages, 1);
    b.clock.set_unix_seconds(NOW + 600);
    response.release();
    assert!(task.await.unwrap().is_err());
    assert_eq!(
        b.account.message_test_write_starts().await.unwrap().1,
        replies,
        "no Ack was written"
    );
    assert_eq!(facts(&a, m.key).await.jobs, 1);
}

#[tokio::test]
async fn expiry_after_stream_open_sends_no_message_bytes() {
    let (a, b, _net, _conv) = manual().await;
    let m = send(&a, &b, 9, "private text").await;
    let counts = b
        .account
        .storage_test_fixture()
        .message_counts()
        .await
        .unwrap();
    let mut pause = a
        .account
        .message_test_pause(MessagePoint::FirstByte)
        .await
        .unwrap();
    let account = a.account.clone();
    let task = tokio::spawn(async move { attempt_on(&account, m.key).await });
    pause.wait_entered().await;
    let local = facts(&a, m.key).await;
    assert_eq!(local.attempted_receivers, vec![installation(&b).await]);
    assert_eq!(local.dispatch_state, Some(1));
    a.clock.set_unix_seconds(NOW + 600);
    pause.release();
    assert!(task.await.unwrap().is_err());
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    assert_eq!(app_ciphertexts(&a).await.len(), 0);
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_counts()
            .await
            .unwrap(),
        counts
    );
}

#[tokio::test]
async fn delayed_connection_acquisition_lifecycle_sends_zero_payload() {
    let (ra, rb) = (record(A, &[A1], 0), record(B, &[B1], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a = device(A, A1, &ra, &net).await;
    let b = device(B, B1, &rb, &net).await;
    link(&a, &b).await;
    befriend(&a, &b).await;
    befriend(&b, &a).await;
    // No session yet: the message path acquires one (and is stopped on the way).
    let mut pause = a
        .account
        .message_test_pause(MessagePoint::Proof)
        .await
        .unwrap();
    let m = send(&a, &b, 9, "private text").await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    lifecycle(&a, true).await;
    pause.release();
    a.account.snapshot().await.unwrap();
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_counts()
            .await
            .unwrap()
            .0,
        0
    );
    lifecycle(&a, false).await;
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    assert!(frames(&a).await.is_empty(), "nothing at all was sent");
    online(&a).await;
    link(&a, &b).await;
    settle!(
        [&a, &b],
        delivery(&a, m.key).await == DeliveryState::Delivered
    );
    assert!(frames(&a).await.iter().all(|f| kind(f) == 17));
}

#[tokio::test]
async fn live_attempt_survives_lifecycle_until_actual_completion_without_duplicate() {
    let (a, b, _net, conv) = setup(&[B1]).await;
    grouped(&a, &b, conv).await;
    a.account.message_test_hold_scheduler(true).await.unwrap();
    let m = send(&a, &b, 9, "private text").await;
    let mut pause = a
        .account
        .message_test_pause(MessagePoint::FirstByte)
        .await
        .unwrap();
    let account = a.account.clone();
    let task = tokio::spawn(async move { attempt_on(&account, m.key).await });
    pause.wait_entered().await;
    lifecycle(&a, true).await;
    lifecycle(&a, false).await;
    online(&a).await;
    link(&a, &b).await;
    a.clock.advance_monotonic_ms(300_000);
    // The actual task is still at its pause: restore cannot claim it completed.
    let local = facts(&a, m.key).await;
    assert_eq!(local.dispatch_state, Some(1));
    assert_eq!(local.retry.unwrap().0, 1);
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    a.account.message_test_hold_scheduler(false).await.unwrap();
    pause.release();
    assert!(task.await.unwrap().is_err());
    settle!(
        [&a, &b],
        delivery(&a, m.key).await == DeliveryState::Delivered
    );
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 1);
    assert_eq!(app_ciphertexts(&a).await.len(), 1);
}

#[tokio::test]
async fn cancel_then_late_receipt_keeps_both_facts() {
    let (a, b, _net, conv) = setup(&[B1]).await;
    grouped(&a, &b, conv).await;
    let mut pause = a
        .account
        .message_test_pause(MessagePoint::FirstByte)
        .await
        .unwrap();
    let m = send(&a, &b, 9, "private text").await;
    tokio::time::timeout(Duration::from_secs(10), pause.wait_entered())
        .await
        .expect("the automatic attempt must enter the checkpoint");
    let cancelled = a.account.messages().cancel_send(m.key).await.unwrap();
    assert!(cancelled.cancel_requested);
    assert_eq!(cancelled.state, DeliveryState::Cancelled);
    assert_eq!(facts(&a, m.key).await.jobs, 0);
    // The App frame already on its way is stored and Acked: delivered, and still cancelled.
    pause.release();
    settle!(
        [&a, &b],
        delivery(&a, m.key).await == DeliveryState::Delivered
    );
    let view = message(&a, m.key).await.unwrap();
    assert!(view.cancel_requested);
    assert_eq!(facts(&a, m.key).await.jobs, 0);
    assert!(message(&b, m.key).await.is_some());
    assert!(attempt(&a, m.key).await.is_err());
}

#[tokio::test]
async fn backoff_caps_without_exhaustion() {
    let (a, b, _net, conv) = manual().await;
    let m = send(&a, &b, 9, "never opened").await;
    // B lost its group: every attempt is refused (class 4) and backs off, nothing is spent.
    b.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_drop_group(conv)
        .await
        .unwrap();
    let mut delays = Vec::new();
    for count in 1..=11 {
        assert!(attempt(&a, m.key).await.is_err());
        let local = facts(&a, m.key).await;
        let (attempts, delay, _) = local.retry.unwrap();
        assert_eq!(attempts, count);
        assert_eq!(local.jobs, 1);
        delays.push(delay / 1000);
    }
    assert_eq!(delays, [1, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
    assert_eq!(outbox(&a, m.key).await.reencrypts, 0);
    a.account
        .storage_test_fixture()
        .saturate_message_attempts(m.key, false)
        .await
        .unwrap();
    assert!(attempt(&a, m.key).await.is_err());
    let local = facts(&a, m.key).await;
    let (attempts, delay, _) = local.retry.unwrap();
    assert_eq!((attempts, delay), (u32::MAX as u64, 300_000));
    assert_eq!(local.jobs, 1);
    assert_eq!(delivery(&a, m.key).await, DeliveryState::Queued);
}

#[tokio::test]
async fn offline_peer_does_not_starve_other_peer() {
    let (a, b, net, _conv) = setup(&[B1]).await;
    let rc = record(C, &[C1], 0);
    publish(&net, C, &rc);
    let c = device(C, C1, &rc, &net).await;
    befriend(&a, &c).await;
    befriend(&c, &a).await;
    a.route(&c).await;
    c.slot
        .network()
        .unwrap()
        .execute(NetworkOp::Stop)
        .await
        .unwrap();
    let messages = a.account.storage_test_fixture().messages().await.unwrap();
    for id in 0..40 {
        messages
            .enqueue(c.peer, MessageId([id; 16]), "queued".into(), NOW * 1000)
            .await
            .unwrap();
    }
    let online_text = send(&a, &b, 99, "to the online peer").await;
    settle!(
        [&a, &b],
        delivery(&a, online_text.key).await == DeliveryState::Delivered
    );
    let page = messages.list_conversations(None, 50).await.unwrap();
    let blocked = page.items.iter().find(|x| x.peer == c.peer).unwrap();
    let rows = messages.load_messages(blocked.id, None, 50).await.unwrap();
    assert_eq!(rows.items.len(), 40);
    assert!(
        rows.items
            .iter()
            .all(|x| x.state != DeliveryState::Delivered)
    );
}

#[tokio::test]
async fn lost_ack_after_restart_is_not_taken_for_an_m4_refusal() {
    let (a, b, net, conv) = setup(&[B1]).await;
    grouped(&a, &b, conv).await;
    // A restarts: what it saw this run is gone, but B's device still holds a leaf in the group.
    let a = start(stop(a), net).await;
    link(&a, &b).await;
    for f in [&a, &b] {
        f.account.message_test_hold_scheduler(true).await.unwrap();
    }
    a.account.connect_account(b.peer).await.unwrap();
    let m = send(&a, &b, 9, "private text").await;
    let mut pause = b
        .account
        .message_test_pause(MessagePoint::Response)
        .await
        .unwrap();
    let account = a.account.clone();
    let task = tokio::spawn(async move { attempt_on(&account, m.key).await });
    pause.wait_entered().await;
    // B closes the connection without an answer (its record expired before the Ack).
    b.clock.set_unix_seconds(NOW + 600);
    pause.release();
    assert!(task.await.unwrap().is_err());
    b.clock.set_unix_seconds(NOW);
    // An M5 peer's lost answer, not an M4 refusal: written, and not waiting for an update.
    let row = outbox(&a, m.key).await;
    assert!(!row.waiting_peer_update);
    assert_eq!(
        stored_state(&a, m.key).await.unwrap().state,
        DeliveryState::SentDirect
    );
}
