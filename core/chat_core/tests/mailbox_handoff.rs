#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Outbox handoff (M6 spec 7, 11): my device hands the sealed ciphertexts of its undelivered
//! messages to my mailbox device, which retries them as they are (back-off capped at 30 s) and
//! tells the origin how they ended: `Delivered`, or `HandoffStop` when the peer could not open
//! one, after which only the origin re-encrypts. Handoff goes to my own mailbox devices only.
include!("support/mailbox_network.rs");

use chat_core::storage_fixture::{HandoffEntry, HandoffRow};
use sha2::Digest;

/// `next_retry_at_ms` of a handed-off ciphertext the peer could not open.
const STOPPED: u64 = i64::MAX as u64;

/// Sets `mailbox` as a mailbox and records it as one on `f` at once (its `Announce` reaches `f`
/// a moment later).
async fn mark_mailbox(f: &Fixture, mailbox: &Fixture) {
    mailbox.account.set_mailbox(true).await.unwrap();
    let id = installation(mailbox).await;
    f.account
        .storage_test_fixture()
        .mailbox_mark(id, true, NOW * 1000)
        .await
        .unwrap();
}

async fn handoffs(f: &Fixture) -> Vec<HandoffRow> {
    mailbox(f).await.handoff_rows
}

/// The ciphertext of `key`'s sealed outbox row on `f`.
async fn sealed(f: &Fixture, key: MessageKey) -> Vec<u8> {
    let wire = outbox(f, key).await.wire;
    mls_app_from_wire(&wire).expect("sealed").1
}

fn hash(ciphertext: &[u8]) -> [u8; 32] {
    Sha256::digest(ciphertext).into()
}

/// The handed-off ciphertexts `f` (a mailbox) sent so far: scheduler time and hash.
async fn sends(f: &Fixture) -> Vec<(u64, [u8; 32])> {
    f.account.mailbox_test_handoff_sends().await.unwrap()
}

/// Waits (real time, no clock moving) until `$cond`.
macro_rules! until {
    ($cond:expr) => {{
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !$cond {
            assert!(
                tokio::time::Instant::now() < deadline,
                "never: {}",
                stringify!($cond)
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }};
}

/// Spec 11: A1 sends while B1 is off, goes to the background (handing the ciphertext to A2) and
/// off. B1 comes back, and A2 delivers it within its 30 s back-off cap. A1 comes back (reaching
/// only A2): the message shows Delivered and its outbox row is gone.
#[tokio::test]
async fn handoff_delivers_and_origin_shows_delivered() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    mark_mailbox(&a1, &a2).await;
    let b_peer = b1.peer;
    let b_dir = stop(b1);
    let key = a1
        .account
        .messages()
        .send_text(b_peer, MessageId([1; 16]), "handed over".into())
        .await
        .unwrap()
        .key;
    assert_eq!(a1.account.mailbox_background().await.unwrap(), 1);
    let a1_id = installation(&a1).await;
    settle!([&a1, &a2], handoffs(&a2).await.len() == 1);
    let row = handoffs(&a2).await.remove(0);
    assert_eq!(
        (row.origin, row.conversation, row.msg_id, row.peer),
        (a1_id, conv, key.id, b_peer)
    );
    assert_eq!(
        row.ciphertext,
        sealed(&a1, key).await,
        "the ciphertext as A1 sealed it"
    );
    let a1_dir = stop(a1);

    // A2 keeps trying while B1 is off; without the cap the 9th failure would back off 256 s.
    settle!([&a2], handoffs(&a2).await[0].attempts >= 9);
    let b1 = start_as(B, b_dir, net.clone()).await;
    link(&b1, &a2).await;
    let mut seconds = 0;
    while message(&b1, key).await.is_none() {
        assert!(
            seconds < 45,
            "not delivered within the 30 s cap (and the dial)"
        );
        for d in [&a2, &b1] {
            d.clock.advance_monotonic_ms(1_000);
        }
        seconds += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(message(&b1, key).await.unwrap().body, "handed over");
    settle!([&a2, &b1], handoffs(&a2).await.is_empty());

    let a1 = on(a1_dir, &net, &[&a2]).await;
    settle!(
        [&a1, &a2, &b1],
        delivery(&a1, key).await == DeliveryState::Delivered
    );
    assert_eq!(outbox(&a1, key).await.codec, None, "no outbox row");
    // B1 got it once, and only through A2: A1 never reached B1 after the handoff.
    assert_eq!(texts_from(&b1, conv, a1.peer).await.len(), 1, "stored once");
}

/// The mailbox retries the ciphertext it was handed as it is, 1, 2, 4, 8, 16, 30, 30 s apart
/// (B1 answers like a peer that has not updated, never with an `Ack`).
#[tokio::test]
async fn mailbox_retries_same_ciphertext_with_30s_cap() {
    let (mut a, b1, _net, _conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    mark_mailbox(&a1, &a2).await;
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    b1.account.message_test_m4_peer(true).await.unwrap();
    let key = send(&a1, &b1, 1, "retried as it is").await.key;
    assert_eq!(a1.account.mailbox_background().await.unwrap(), 1);
    let ciphertext = sealed(&a1, key).await;
    // Taken: due at once.
    settle!([&a1], !handoffs(&a2).await.is_empty());
    until!(sends(&a2).await.len() == 1);
    let mut delays = Vec::new();
    for k in 0..7 {
        let at = sends(&a2).await[k].0;
        // The attempt's result is in before the clock moves on.
        until!(handoffs(&a2).await[0].attempts == k as u64 + 1);
        let delay = handoffs(&a2).await[0].next_retry_at_ms - at;
        delays.push(delay);
        // Not a second early, then exactly on time.
        a2.clock.advance_monotonic_ms(delay - 1_000);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            sends(&a2).await.len(),
            k + 1,
            "attempt {} came early",
            k + 2
        );
        a2.clock.advance_monotonic_ms(1_000);
        until!(sends(&a2).await.len() == k + 2);
        assert_eq!(sends(&a2).await[k + 1].0, at + delay);
    }
    assert_eq!(delays, [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]);
    let all = sends(&a2).await;
    assert!(
        all.iter().all(|(_, h)| *h == hash(&ciphertext)),
        "never sealed again"
    );
    assert_eq!(app_ciphertexts(&a2).await, vec![ciphertext; all.len()]);
}

/// Spec 11: B1 is six epochs past the ciphertext A1 handed over. A2's attempt gets
/// `DecryptFailed` (class 1): A2 stops retrying it and notes it for A1. A1 comes back (reaching
/// only A2), re-encrypts once and hands the new ciphertext over; A2 delivers it. B1 stores the
/// message once.
#[tokio::test]
async fn handoff_reencrypt_goes_back_to_origin() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1, B2]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    let sent = send(&a1, &b1, 1, "sealed six epochs ago").await;
    a1.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_seal(sent.key, NOW * 1000)
        .await
        .unwrap()
        .unwrap();
    let old = sealed(&a1, sent.key).await;
    commits(&a1, &b1, conv, 6).await;
    // A2 becomes a mailbox only now: a handoff before the Commits (A1 hands over once a pull
    // with A2 is done) would reach B1 while the ciphertext still opens there.
    mark_mailbox(&a1, &a2).await;
    assert_eq!(a1.account.mailbox_background().await.unwrap(), 1);
    settle!([&a1, &a2, &b1], !handoffs(&a2).await.is_empty());
    let a1_dir = stop(a1);

    settle!(
        [&a2, &b1],
        handoffs(&a2).await[0].next_retry_at_ms == STOPPED
    );
    let stops = mailbox(&a2)
        .await
        .entries
        .into_iter()
        .filter(|e| e.kind == 3 && e.msg_id == Some(sent.key.id))
        .count();
    assert_eq!(stops, 1, "one HandoffStop note for A1");
    quiet(&[&a2, &b1]).await;
    assert_eq!(sends(&a2).await.len(), 1, "A2 does not retry it");
    assert!(message(&b1, sent.key).await.is_none());

    let a1 = on(a1_dir, &net, &[&a2]).await;
    settle!(
        [&a1, &a2, &b1],
        message(&b1, sent.key).await.is_some()
            && delivery(&a1, sent.key).await == DeliveryState::Delivered
    );
    // Re-encrypted once, by A1: A2 sent the old ciphertext once, then only the new one.
    let tried: Vec<[u8; 32]> = sends(&a2).await.into_iter().map(|(_, h)| h).collect();
    assert_eq!(tried[0], hash(&old));
    assert!(tried[1..].iter().all(|h| *h != hash(&old)));
    let reencrypted: std::collections::BTreeSet<[u8; 32]> = tried[1..].iter().copied().collect();
    assert_eq!(reencrypted.len(), 1, "one new ciphertext");
    let stored: Vec<_> = page(&b1, conv)
        .await
        .into_iter()
        .filter(|m| m.key == sent.key)
        .collect();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].body, "sealed six epochs ago");
    assert_eq!(
        app_ciphertexts(&a1).await,
        Vec::<Vec<u8>>::new(),
        "A1 sent nothing itself"
    );
}

/// Only messages still on their way are handed over: not a delivered one, a cancelled one, or
/// one waiting for the peer's update.
#[tokio::test]
async fn cancelled_or_delivered_rows_are_not_handed_off() {
    let (mut a, b1, _net, _conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let done = delivered(&a1, &b1, &[&a2], 1).await;
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    // B1 never acks from here on, so what A2 is handed stays there to be looked at.
    b1.account.message_test_m4_peer(true).await.unwrap();
    let cancelled = send(&a1, &b1, 2, "cancelled").await.key;
    a1.account.messages().cancel_send(cancelled).await.unwrap();
    let waiting = send(&a1, &b1, 3, "waiting for the update").await.key;
    let messages = a1.account.storage_test_fixture().messages().await.unwrap();
    messages
        .mls_seal(waiting, NOW * 1000)
        .await
        .unwrap()
        .unwrap();
    messages.mls_mark_waiting(waiting, true).await.unwrap();
    let pending = send(&a1, &b1, 4, "on its way").await.key;

    // A mailbox only now, so no handoff (A1 hands over once a pull with A2 is done) sees the
    // rows before they are set up.
    mark_mailbox(&a1, &a2).await;
    assert_eq!(a1.account.mailbox_background().await.unwrap(), 1);
    settle!([&a1, &a2, &b1], !handoffs(&a2).await.is_empty());
    quiet(&[&a1, &a2]).await;
    let ids: Vec<MessageId> = handoffs(&a2).await.iter().map(|r| r.msg_id).collect();
    assert_eq!(ids, [pending.id]);
    assert!(
        ![done.id, cancelled.id, waiting.id]
            .iter()
            .any(|id| ids.contains(id))
    );
}

/// Handoff goes to my own mailbox devices only: never to another account's device recorded as a
/// mailbox, never to a revoked one; and a mailbox takes a handoff only from my own devices.
#[tokio::test]
async fn handoff_to_foreign_or_revoked_device_never_happens() {
    let (mut a, b1, _net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    // B1 never acks, so what A2 is handed stays there to be looked at.
    b1.account.message_test_m4_peer(true).await.unwrap();
    let key = send(&a1, &b1, 1, "mine to hand over").await.key;

    // B1 (another account) recorded as a mailbox: nothing goes to it.
    mark_mailbox(&a1, &b1).await;
    assert_eq!(a1.account.mailbox_background().await.unwrap(), 0);
    // B1 handing a ciphertext to A2: refused, nothing kept.
    let a2_id = installation(&a2).await;
    let reply = b1
        .account
        .mailbox_test_exchange(
            a2_id,
            MailboxFrame::Handoff {
                entries: vec![HandoffEntry {
                    conversation: conv,
                    peer: b1.peer,
                    msg_id: key.id,
                    ciphertext: vec![1],
                }],
            },
        )
        .await
        .unwrap();
    assert!(reply == MailboxReply::Refused);

    // My own device recorded as a mailbox: it is handed over.
    mark_mailbox(&a1, &a2).await;
    assert_eq!(a1.account.mailbox_background().await.unwrap(), 1);
    settle!([&a1, &a2, &b1], handoffs(&a2).await.len() == 1);
    assert_eq!(handoffs(&a2).await[0].origin, installation(&a1).await);

    // Revoked: no longer.
    a1.account.revoke_device(a2_id).await.unwrap();
    assert_eq!(a1.account.mailbox_background().await.unwrap(), 0);
    assert!(handoffs(&b1).await.is_empty());
}

/// Ruling 26: A1 cancels a message it handed over. A2 pulls the `HandoffWithdraw` note, drops
/// its ciphertext and starts no attempt after that, however long it runs; it does not log the
/// note again.
#[tokio::test]
async fn cancel_withdraws_the_handed_off_ciphertext() {
    let (mut a, b1, _net, _conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    // B1 never acks: without the withdrawal A2 would go on trying.
    b1.account.message_test_m4_peer(true).await.unwrap();
    let key = send(&a1, &b1, 1, "cancelled after the handoff").await.key;
    mark_mailbox(&a1, &a2).await;
    assert_eq!(a1.account.mailbox_background().await.unwrap(), 1);
    settle!([&a1, &a2, &b1], !sends(&a2).await.is_empty());

    let cancelled = a1.account.messages().cancel_send(key).await.unwrap();
    assert!(cancelled.cancel_requested);
    settle!([&a1, &a2, &b1], handoffs(&a2).await.is_empty());
    let tried = sends(&a2).await.len();
    // Well past several 30 s caps.
    for _ in 0..150 {
        a2.clock.advance_monotonic_ms(1_000);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        sends(&a2).await.len(),
        tried,
        "no attempt after the withdrawal"
    );
    assert!(
        !mailbox(&a2).await.entries.iter().any(|e| e.kind == 4),
        "A2 does not log it again"
    );
}

/// A `Handoff` frame of `key`'s ciphertext from `from` to `to`, sent by hand (a late frame).
async fn late_handoff(from: &Fixture, to: &Fixture, peer: PeerId, key: MessageKey, ct: Vec<u8>) {
    let frame = MailboxFrame::Handoff {
        entries: vec![HandoffEntry {
            conversation: key.conversation,
            peer,
            msg_id: key.id,
            ciphertext: ct,
        }],
    };
    let reply = from
        .account
        .mailbox_test_exchange(installation(to).await, frame)
        .await
        .unwrap();
    assert!(
        reply == MailboxReply::Ok,
        "a settled entry is skipped, not refused"
    );
}

/// Ruling 27: A2 pulls A1's withdrawal before A1's `Handoff` frame of that message arrives
/// (the send was under way when the user cancelled). The late frame creates no row, and A2
/// never tries the message.
#[tokio::test]
async fn withdraw_pulled_before_the_handoff_frame_keeps_it_out() {
    let (mut a, b1, _net, _conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    b1.account.message_test_m4_peer(true).await.unwrap();
    let key = send(&a1, &b1, 1, "cancelled while handed over").await.key;
    a1.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_seal(key, NOW * 1000)
        .await
        .unwrap()
        .unwrap();
    let ct = sealed(&a1, key).await;
    a2.account.set_mailbox(true).await.unwrap();
    a1.account.messages().cancel_send(key).await.unwrap();
    // A2 has the withdrawal (kept, as nothing was handed over yet).
    settle!(
        [&a1, &a2, &b1],
        mailbox(&a2).await.status_ahead == 1 && caught_up(&a2, &a1).await
    );
    assert!(handoffs(&a2).await.is_empty());

    late_handoff(&a1, &a2, b1.peer, key, ct).await;
    quiet(&[&a1, &a2, &b1]).await;
    assert!(handoffs(&a2).await.is_empty(), "no row");
    assert!(sends(&a2).await.is_empty(), "no attempt");
}

/// Ruling 27: A2 already shows the message delivered (A1 delivered it and A2 pulled the note)
/// when a late `Handoff` frame of it arrives: no row, no attempt.
#[tokio::test]
async fn delivered_pulled_before_the_handoff_frame_keeps_it_out() {
    let (mut a, b1, _net, _conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    let key = send(&a1, &b1, 1, "delivered meanwhile").await.key;
    a1.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_seal(key, NOW * 1000)
        .await
        .unwrap()
        .unwrap();
    let ct = sealed(&a1, key).await;
    a1.account.message_test_hold_scheduler(false).await.unwrap();
    settle!(
        [&a1, &a2, &b1],
        delivery(&a1, key).await == DeliveryState::Delivered
            && message(&a2, key).await.map(|m| m.state) == Some(DeliveryState::Delivered)
    );
    // From here B1 would never ack: a row, if made, would stay and be tried.
    b1.account.message_test_m4_peer(true).await.unwrap();
    a2.account.set_mailbox(true).await.unwrap();

    late_handoff(&a1, &a2, b1.peer, key, ct).await;
    quiet(&[&a1, &a2, &b1]).await;
    assert!(handoffs(&a2).await.is_empty(), "no row");
    assert!(sends(&a2).await.is_empty(), "no attempt");
}
