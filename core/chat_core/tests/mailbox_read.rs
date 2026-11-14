#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Read sync and receipt status across my devices (M6, spec 6.4, 4, 11): my Read receipt that
//! another of my devices sent means "read there" (read here, no notification, no receipt from
//! here); the peer's Delivered and Read reach every one of my devices, also for the copies of
//! what my other devices sent, and never go back.
include!("support/mailbox_network.rs");

use chat_core::storage_fixture::{EntryBody, LogEntry};

async fn unread(f: &Fixture, conv: ConversationId) -> u64 {
    f.account
        .messages()
        .list_conversations(None, 50)
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|c| c.id == conv)
        .map_or(0, |c| c.unread)
}

async fn seen(f: &Fixture, key: MessageKey) -> bool {
    f.account
        .storage_test_fixture()
        .message_facts(key)
        .await
        .unwrap()
        .seen_local
}

/// Whether `f` stores `key`.
async fn has(f: &Fixture, key: MessageKey) -> bool {
    message(f, key).await.is_some()
}

/// The Read receipts A2 seals are the only MLS application frames it sends here.
async fn app_frames(f: &Fixture) -> usize {
    app_ciphertexts(f).await.len()
}

/// Spec 11: read on A1, and A2 shows the message read, clears its notification (an item with
/// no new message and nothing unread), and sends no receipt of its own (none sent, none queued),
/// not even when the user then opens the conversation on A2 too.
#[tokio::test]
async fn read_on_a1_marks_a2_read_without_notification_or_receipt() {
    let (mut a, b1, _net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let key = delivered(&b1, &a1, &[&a2], 1).await;
    settle!([&a1, &a2, &b1], has(&a1, key).await && has(&a2, key).await);
    settle!([&a1, &a2, &b1], unread(&a2, conv).await == 1);
    assert!(!seen(&a2, key).await);
    let sent_before = app_frames(&a2).await;
    let notified_before = notify_events(&a2).len();

    a1.account
        .messages()
        .mark_read(conv, vec![key])
        .await
        .unwrap();
    settle!([&a1, &a2, &b1], unread(&a2, conv).await == 0);
    assert!(seen(&a2, key).await);
    settle!([&a1, &a2, &b1], notify_events(&a2).len() > notified_before);
    assert_eq!(
        notify_events(&a2)[notified_before..],
        [NotifyItem {
            conversation: conv,
            peer: b1.peer,
            new_messages: 0,
            unread: 0
        }]
    );
    // The user opens the conversation on A2 as well: nothing to send from here.
    a2.account
        .messages()
        .mark_read(conv, vec![key])
        .await
        .unwrap();
    quiet(&[&a1, &a2, &b1]).await;
    assert_eq!(receipt_jobs(&a2).await, 0);
    assert_eq!(app_frames(&a2).await, sent_before, "A2 sent no receipt");
    assert_eq!(delivery(&b1, key).await, DeliveryState::Read);
}

/// Final review I2: B1 sends and goes off; then A1 reads. A2 shows the text read (its
/// notification cleared, no alert, no receipt of its own) while B1 is still off: my read
/// reaches my devices without the contact. B1 comes back and gets one Read, from A1.
#[tokio::test]
async fn read_reaches_my_devices_while_contact_is_off() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let key = delivered(&b1, &a1, &[&a2], 1).await;
    settle!([&a1, &a2, &b1], has(&a2, key).await);
    settle!([&a1, &a2, &b1], unread(&a2, conv).await == 1);
    let b_peer = b1.peer;
    let b1_id = installation(&b1).await;
    let b_dir = stop(b1);
    // Their transports notice B1 is gone (no ready session with it).
    for f in [&a1, &a2] {
        f.account.mls_test_close_session(b1_id).await.unwrap();
    }
    let (a1_sent, a2_sent) = (app_ciphertexts(&a1).await.len(), app_frames(&a2).await);
    let notified_before = notify_events(&a2).len();

    a1.account
        .messages()
        .mark_read(conv, vec![key])
        .await
        .unwrap();
    settle!(
        [&a1, &a2],
        seen(&a2, key).await && unread(&a2, conv).await == 0
    );
    settle!([&a1, &a2], notify_events(&a2).len() > notified_before);
    assert_eq!(
        notify_events(&a2)[notified_before..],
        [NotifyItem {
            conversation: conv,
            peer: b_peer,
            new_messages: 0,
            unread: 0
        }]
    );

    let b1 = start_as(B, b_dir, net.clone()).await;
    link(&b1, &a1).await;
    link(&b1, &a2).await;
    settle!(
        [&a1, &a2, &b1],
        delivery(&b1, key).await == DeliveryState::Read && receipt_jobs(&a1).await == 0
    );
    quiet(&[&a1, &a2, &b1]).await;
    let receipts: std::collections::BTreeSet<Vec<u8>> = app_ciphertexts(&a1).await[a1_sent..]
        .iter()
        .cloned()
        .collect();
    assert_eq!(receipts.len(), 1, "one Read, sealed once");
    assert_eq!(app_frames(&a2).await, a2_sent, "A2 sent no receipt");
    assert_eq!(receipt_jobs(&a2).await, 0);
}

/// Spec 11 (both ways, the read part): A2 is off while B1 sends three texts and A1 reads them.
/// A2 comes back: all three read, nothing unread, and no notification of a new message.
#[tokio::test]
async fn pc_back_after_phone_read_shows_read_and_no_notification() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let dir = stop(a.pop().unwrap());
    let a1 = a.pop().unwrap();
    let mut keys = Vec::new();
    for n in 1..=3 {
        keys.push(delivered(&b1, &a1, &[], n).await);
    }
    a1.account
        .messages()
        .mark_read(conv, keys.clone())
        .await
        .unwrap();
    settle!([&a1, &b1], {
        let mut read = true;
        for key in &keys {
            read = read && delivery(&b1, *key).await == DeliveryState::Read;
        }
        read
    });
    let a2 = on(dir, &net, &[&a1, &b1]).await;
    settle!(
        [&a1, &a2, &b1],
        texts_from(&a2, conv, b1.peer).await.len() == 3 && caught_up(&a2, &a1).await
    );
    quiet(&[&a1, &a2, &b1]).await;
    for key in &keys {
        assert!(seen(&a2, *key).await, "{key:?} read on A2");
    }
    assert_eq!(unread(&a2, conv).await, 0);
    let new: u32 = notify_events(&a2).iter().map(|i| i.new_messages).sum();
    assert_eq!(new, 0, "no notification of a new message");
    assert_eq!(receipt_jobs(&a2).await, 0);
}

/// Review Focus 1: A3 gets my Read receipt (sent by A2) before it gets the message it is about
/// (from A1). The message is stored read: nothing unread, no notification.
#[tokio::test]
async fn own_read_before_message_still_suppresses_notification() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2, A3], &[B1]).await;
    let dir = stop(a.pop().unwrap());
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let key = delivered(&b1, &a1, &[&a2], 1).await;
    settle!([&a1, &a2, &b1], has(&a2, key).await);
    a2.account
        .messages()
        .mark_read(conv, vec![key])
        .await
        .unwrap();
    settle!(
        [&a1, &a2, &b1],
        delivery(&b1, key).await == DeliveryState::Read
    );
    // A2's log holds the message, then the receipt: A3 takes only the receipt from A2.
    let a2_id = installation(&a2).await;
    let receipt = mailbox(&a2)
        .await
        .entries
        .into_iter()
        .rev()
        .find(|e| e.sender_install == a2_id && e.kind == 1)
        .expect("A2 logged its receipt");
    assert!(receipt.msg_id.is_none());
    // A1 answers `Busy` until A3 has processed that receipt.
    a1.account.mailbox_test_hold(true).await.unwrap();
    let a3 = start(dir, net.clone()).await;
    a3.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mailbox_pulled(a2_id, receipt.seq)
        .await
        .unwrap();
    link(&a3, &a1).await;
    link(&a3, &a2).await;
    a3.account.mailbox_sync_now().await.unwrap();
    settle!(
        [&a1, &a2, &a3, &b1],
        caught_up(&a3, &a2).await && mailbox(&a3).await.status_ahead == 1
    );
    assert!(!has(&a3, key).await, "the message is not here yet");

    a1.account.mailbox_test_hold(false).await.unwrap();
    a3.account.mailbox_sync_now().await.unwrap();
    settle!(
        [&a1, &a2, &a3, &b1],
        has(&a3, key).await && caught_up(&a3, &a1).await
    );
    quiet(&[&a1, &a2, &a3, &b1]).await;
    assert!(seen(&a3, key).await, "stored read");
    assert_eq!(unread(&a3, conv).await, 0);
    assert_eq!(
        mailbox(&a3).await.status_ahead,
        0,
        "the early read was used up"
    );
    let new: u32 = notify_events(&a3).iter().map(|i| i.new_messages).sum();
    assert_eq!(new, 0, "no notification");
    assert_eq!(receipt_jobs(&a3).await, 0);
}

/// Spec 11: A1's message, A1 off, B1 reads it: the receipt goes to A2 (the copy there shows
/// Read). A1 comes back and has its message Read from A2's log.
#[tokio::test]
async fn peer_read_reaches_sender_while_sender_is_off() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let key = delivered(&a1, &b1, &[&a2], 1).await;
    settle!([&a1, &a2, &b1], has(&a2, key).await);
    let a1_id = installation(&a1).await;
    let dir = stop(a1);
    // B1's transport notices A1 is gone (no ready session with the message's sender).
    b1.account.mls_test_close_session(a1_id).await.unwrap();
    b1.account
        .messages()
        .mark_read(conv, vec![key])
        .await
        .unwrap();
    settle!(
        [&a2, &b1],
        delivery(&a2, key).await == DeliveryState::Read && receipt_jobs(&b1).await == 0
    );
    let a1 = on(dir, &net, &[&a2, &b1]).await;
    settle!(
        [&a1, &a2, &b1],
        delivery(&a1, key).await == DeliveryState::Read
    );
}

/// A2's copy of A1's message follows the peer: `Delivered` by A1's note, `Read` by B1's receipt,
/// and a late `Delivered` note leaves it `Read`.
#[tokio::test]
async fn copy_status_follows_peer_receipts_and_never_regresses() {
    let (mut a, b1, _net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    // A2 pulls nothing until A1's note is looked at: once A2 has pulled past it, A1 prunes it.
    a2.account.mailbox_test_hold(true).await.unwrap();
    let key = delivered(&a1, &b1, &[&a2], 1).await;
    settle!([&a1, &a2, &b1], has(&a2, key).await);
    // A1 logged the delivery as a note about its message.
    let note = mailbox(&a1)
        .await
        .entries
        .into_iter()
        .find(|e| e.kind == 2)
        .expect("a Delivered note");
    assert_eq!(
        (note.msg_id, note.sender_install),
        (Some(key.id), key.installation)
    );
    assert_ne!(delivery(&a2, key).await, DeliveryState::Delivered);
    a2.account.mailbox_test_hold(false).await.unwrap();
    a2.account.mailbox_sync_now().await.unwrap();
    settle!(
        [&a1, &a2, &b1],
        delivery(&a2, key).await == DeliveryState::Delivered
    );

    b1.account
        .messages()
        .mark_read(conv, vec![key])
        .await
        .unwrap();
    settle!(
        [&a1, &a2, &b1],
        delivery(&a1, key).await == DeliveryState::Read
            && delivery(&a2, key).await == DeliveryState::Read
    );
    let b1_id = installation(&b1).await;
    a2.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mailbox_apply_note(
            LogEntry {
                seq: 1_000,
                conversation: conv,
                sender_peer: key.sender,
                sender_install: key.installation,
                recorded_at_ms: NOW * 1000 + 60_000,
                body: EntryBody::Delivered {
                    msg_id: key.id,
                    receiver: b1_id,
                    at_ms: NOW * 1000 + 60_000,
                },
            },
            NOW * 1000 + 60_000,
        )
        .await
        .unwrap();
    assert_eq!(delivery(&a2, key).await, DeliveryState::Read);
    assert_eq!(delivery(&a1, key).await, DeliveryState::Read);
}

/// The M4 rule stays: marking a copy of my own sent message read is refused.
#[tokio::test]
async fn mark_read_still_rejects_own_copies() {
    let (mut a, b1, _net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let key = delivered(&a1, &b1, &[&a2], 1).await;
    settle!([&a1, &a2, &b1], has(&a2, key).await);
    let error = a2
        .account
        .messages()
        .mark_read(conv, vec![key])
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidMessage);
    assert_eq!(receipt_jobs(&a2).await, 0);
}
