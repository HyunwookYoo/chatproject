#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Putting frames into other devices' server queues (M8 spec 8): after an attempt, every device
//! it did not reach that has a live queue (the peer's, and my other devices I have no session
//! with) gets the text, in one `SEND` for all of them, each entry the frame a direct attempt
//! writes (with the `Commit`s it lacks ahead of it). Receipts go to the message's sender the
//! same way; `Commit`s, `Welcome`s and `QueueRef`s with no text after them go by the standing
//! pass. The text stays sent until the receipt, goes in again once its hold ran out, and never
//! after the receipt. `gone` kills that queue; `full` and `limited` leave it to the outbox
//! retry. The mode "never", no address, or a queue on another server: no `SEND`.
include!("support/queue_network.rs");

use chat_core::push::{PushOutcome, ReceivedPush, Tag};
use queue_wire::{ENTRY_TTL_MS, ROTATE_MS};

async fn network(f: &Fixture, op: NetworkOp) {
    f.slot.network().unwrap().execute(op).await.unwrap();
}

/// Whether `f`'s latest word on a session with installation `id` is that it is ready.
fn ready_with(f: &Fixture, id: chat_core::account::InstallationId) -> bool {
    let id = id.to_string();
    f.events
        .0
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match event {
            CoreEvent::AccountAuthChanged {
                installation_id,
                ready,
                ..
            } if *installation_id == id => Some(*ready),
            _ => None,
        })
        .unwrap_or(false)
}

/// `f`'s network goes off, and `others` see it gone.
async fn off(f: &Fixture, others: &[&Fixture]) {
    let id = installation(f).await;
    network(f, NetworkOp::Stop).await;
    settle!(
        others.iter().copied(),
        others.iter().all(|o| !ready_with(o, id))
    );
}

/// Items of the conversation that alerted (`new_messages > 0`).
fn alerts(f: &Fixture, conv: ConversationId) -> Vec<NotifyItem> {
    notify_events(f)
        .into_iter()
        .filter(|i| i.conversation == conv && i.new_messages > 0)
        .collect()
}

/// The entries `queue` holds, decoded.
fn entries_in(server: &FakeQueueServer, queue: QueueId) -> Vec<QueueEntry> {
    server
        .stored(queue)
        .iter()
        .map(|ct| QueueEntry::decode(ct).unwrap())
        .collect()
}

/// The tag `f` holds for the conversation.
async fn own_tag(f: &Fixture, conv: ConversationId) -> Tag {
    push_state(f)
        .await
        .tags
        .into_iter()
        .find(|t| t.conversation == conv)
        .unwrap()
        .tag
}

/// `announced` on a queue server with `limits`.
async fn announced_limits(
    limits: queue_server::Limits,
) -> (
    Fixture,
    Fixture,
    Arc<Backend>,
    ConversationId,
    Arc<FakeQueueServer>,
) {
    let (mut a, b1, net, conv) = queue_accounts(&[A1]).await;
    let a1 = a.remove(0);
    let server = queue_server_with(&net, limits);
    configure(&a1, QUEUE_URL);
    configure(&b1, QUEUE_URL);
    settle!(
        [&a1, &b1],
        peer_queue(&b1, conv, &a1).await.flatten().is_some()
            && peer_queue(&a1, conv, &b1).await.flatten().is_some()
            && queue_ref_deliveries_to(&a1, &b1).await == 0
            && queue_ref_deliveries_to(&b1, &a1).await == 0
    );
    (a1, b1, net, conv, server)
}

/// A1 and A2 with queues B1 keeps, and B1.
async fn two_queued_devices() -> (
    Fixture,
    Fixture,
    Fixture,
    ConversationId,
    Arc<FakeQueueServer>,
) {
    let (mut a, b1, net, conv) = queue_accounts(&[A1, A2]).await;
    let (a1, a2) = (a.remove(0), a.remove(0));
    let server = queue_server(&net);
    for f in [&a1, &a2, &b1] {
        configure(f, QUEUE_URL);
    }
    settle!(
        [&a1, &a2, &b1],
        peer_queue(&b1, conv, &a1).await.flatten().is_some()
            && peer_queue(&b1, conv, &a2).await.flatten().is_some()
            && queue_ref_deliveries(&a1).await == 0
            && queue_ref_deliveries(&a2).await == 0
    );
    (a1, a2, b1, conv, server)
}

/// Spec 8.1: A1 is off, so B1's attempts do not reach it. The first one puts the text into
/// A1's queue, exactly the frame a direct attempt writes, naming B1; the retries that follow
/// put nothing more in while the server holds it.
#[tokio::test]
async fn unreached_device_with_queue_gets_one_send() {
    let (a1, b1, _net, conv, server) = announced().await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    let key = send(&b1, &a1, 1, "into the queue").await.key;
    settle!([&b1], server.held(queue.queue_id) == 1);
    settle!([&b1], outbox(&b1, key).await.attempts >= 4);
    quiet(&[&b1]).await;

    let sends = server.sends(&b1);
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].queues, [queue.queue_id]);
    let entries = entries_in(&server, queue.queue_id);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].frame, outbox(&b1, key).await.wire);
    assert_eq!(entries[0].sender_peer, b1.peer);
    assert_eq!(entries[0].sender_install, installation(&b1).await);
    assert_eq!(alerts(&a1, conv).len(), 0);

    // A1 comes on with B1 off: it stores the text from its queue.
    let b_peer = b1.peer;
    let _b1 = stop(b1);
    online(&a1).await;
    settle!([&a1], message(&a1, key).await.is_some());
    assert_eq!(texts_from(&a1, conv, b_peer).await.len(), 1);
}

/// Plan Task 10 performance: one text, two devices off: one `SEND` request carrying both.
#[tokio::test]
async fn one_send_covers_every_unreached_device() {
    let (a1, a2, b1, _conv, server) = two_queued_devices().await;
    let (q1, q2) = (
        active(&a1).await.remove(0).queue_id,
        active(&a2).await.remove(0).queue_id,
    );
    off(&a1, &[&a2, &b1]).await;
    off(&a2, &[&b1]).await;
    let key = send(&b1, &a1, 1, "to both").await.key;
    settle!([&b1], server.held(q1) == 1 && server.held(q2) == 1);
    settle!([&b1], outbox(&b1, key).await.attempts >= 4);
    quiet(&[&b1]).await;
    let sends = server.sends(&b1);
    assert_eq!(sends.len(), 1);
    let mut queues = sends[0].queues.clone();
    queues.sort();
    let mut both = vec![q1, q2];
    both.sort();
    assert_eq!(queues, both);
}

/// Spec 8.1: the device the attempt reached gets nothing in its queue; the one it did not
/// (off) does, though the text is delivered.
#[tokio::test]
async fn reached_devices_get_no_send() {
    let (a1, a2, b1, _conv, server) = two_queued_devices().await;
    let (q1, q2) = (
        active(&a1).await.remove(0).queue_id,
        active(&a2).await.remove(0).queue_id,
    );
    off(&a2, &[&a1, &b1]).await;
    let key = send(&b1, &a1, 1, "A1 is on").await.key;
    settle!(
        [&a1, &b1],
        delivery(&b1, key).await == DeliveryState::Delivered && server.held(q2) == 1
    );
    quiet(&[&a1, &b1]).await;
    let sends = server.sends(&b1);
    assert_eq!(sends.len(), 1);
    assert_eq!(sends[0].queues, [q2]);
    assert_eq!(server.held(q1), 0);
}

/// Spec 3 (user decision), 8.2: a text in a queue is not delivered. It stays as it was until
/// A1 takes it from the queue and its receipt comes, then it is delivered.
#[tokio::test]
async fn queued_text_stays_sent_until_receipt() {
    let (a1, b1, _net, conv, server) = announced().await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    let key = send(&b1, &a1, 1, "waits").await.key;
    let before = delivery(&b1, key).await;
    settle!([&b1], server.held(queue.queue_id) == 1);
    quiet(&[&b1]).await;
    assert_eq!(delivery(&b1, key).await, before);
    assert_ne!(before, DeliveryState::Delivered);

    online(&a1).await;
    link(&a1, &b1).await;
    settle!(
        [&a1, &b1],
        delivery(&b1, key).await == DeliveryState::Delivered
    );
    assert_eq!(texts_from(&a1, conv, b1.peer).await.len(), 1);
}

/// Spec 8.2 (Task 10 review C1): A1 is off for more than a day. B1's hold of its copy ends
/// after 24 h, and its next attempt (within minutes) puts the text in again, before the
/// server's hourly sweep dropped the old entry. The server holds it 24 h from that `SEND`, so
/// after the sweep that follows, A1 comes on (B1 off) about 25 h after the first `SEND` and
/// still stores it, once.
#[tokio::test]
async fn expired_queue_copy_is_sent_again_and_stored_once() {
    let (a1, b1, _net, conv, server) = announced().await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    let key = send(&b1, &a1, 1, "kept a day").await.key;
    settle!([&b1], server.held(queue.queue_id) == 1);
    quiet(&[&b1]).await;
    assert_eq!(server.sends(&b1).len(), 1);

    let day = ENTRY_TTL_MS / 1000 + 120;
    b1.clock.advance_seconds(day).unwrap();
    a1.clock.advance_seconds(day).unwrap();
    settle!([&b1], server.sends(&b1).len() == 2);
    quiet(&[&b1]).await;
    assert_eq!(server.sends(&b1).len(), 2);
    let entries = entries_in(&server, queue.queue_id);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].frame, outbox(&b1, key).await.wire);

    // The server's next hourly sweep, an hour after the first entry's 24 h ran out.
    b1.clock.advance_seconds(3_600).unwrap();
    a1.clock.advance_seconds(3_600).unwrap();
    server.sweep(a1.clock.unix_seconds() * 1000).await;
    let b_peer = b1.peer;
    let _b1 = stop(b1);
    online(&a1).await;
    settle!(
        [&a1],
        message(&a1, key).await.is_some() && server.held(queue.queue_id) == 0
    );
    assert_eq!(texts_from(&a1, conv, b_peer).await.len(), 1);
    assert_eq!(alerts(&a1, conv).len(), 1);
}

/// Spec 8.2: once the receipt came, nothing is put in again, even after the hold ran out.
#[tokio::test]
async fn receipt_clears_queue_delivery() {
    let (a1, b1, _net, _conv, server) = announced().await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    let key = send(&b1, &a1, 1, "receipted").await.key;
    settle!([&b1], server.held(queue.queue_id) == 1);
    online(&a1).await;
    link(&a1, &b1).await;
    settle!(
        [&a1, &b1],
        delivery(&b1, key).await == DeliveryState::Delivered
    );
    off(&a1, &[&b1]).await;
    let sends = server.sends(&b1).len();
    b1.clock
        .advance_seconds(ENTRY_TTL_MS / 1000 + 3_600)
        .unwrap();
    quiet(&[&b1]).await;
    assert_eq!(server.sends(&b1).len(), sends);
}

/// Spec 8.3: the server no longer knows A1's queue (`gone`): B1 marks it dead and puts
/// nothing in it again, though its attempts go on.
#[tokio::test]
async fn gone_marks_peer_dead_and_stops() {
    let (a1, b1, _net, conv, server) = announced().await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    assert_eq!(
        server
            .delete_as(queue.queue_id, &queue.recipient_secret)
            .await,
        ItemResult::Ok
    );
    let ia1 = installation(&a1).await;
    let key = send(&b1, &a1, 1, "to a lost queue").await.key;
    settle!(
        [&b1],
        peer_row(&b1, conv, ia1).await.is_some_and(|r| r.dead)
    );
    settle!([&b1], outbox(&b1, key).await.attempts >= 4);
    quiet(&[&b1]).await;
    assert_eq!(server.sends(&b1).len(), 1);
}

/// Spec 8.3: a full queue (256 unacknowledged) takes nothing: nothing is noted and the next
/// attempts try again, until there is room.
#[tokio::test]
async fn full_queue_falls_back_to_outbox_retry() {
    let (a1, b1, _net, _conv, server) = announced_limits(queue_server::Limits {
        send_per_minute: 1_000,
        send_burst: 1_000,
        ..Default::default()
    })
    .await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    for n in 0..queue_wire::MAX_UNACKED {
        let filler = format!("filler {n}");
        assert_eq!(
            server
                .send_as(queue.queue_id, &queue.sender_secret, filler.as_bytes())
                .await,
            ItemResult::Ok
        );
    }
    let key = send(&b1, &a1, 1, "no room").await.key;
    settle!([&b1], server.sends(&b1).len() >= 2);
    assert_eq!(server.held(queue.queue_id), queue_wire::MAX_UNACKED);

    // Room again (the fillers expired): the next attempt puts it in, and that is the last.
    server.sweep(NOW * 1000 + ENTRY_TTL_MS + 1).await;
    settle!([&b1], server.held(queue.queue_id) == 1);
    let sends = server.sends(&b1).len();
    quiet(&[&b1]).await;
    assert_eq!(server.sends(&b1).len(), sends);
    assert_eq!(
        entries_in(&server, queue.queue_id)[0].frame,
        outbox(&b1, key).await.wire
    );
}

/// Spec 8.3: over the queue's `SEND` limit (`limited`): nothing is noted and the next attempts
/// try again; once the limit refills, the text goes in and the `SEND`s stop.
#[tokio::test]
async fn limited_falls_back_to_outbox_retry() {
    let (a1, b1, _net, _conv, server) = announced_limits(queue_server::Limits {
        send_per_minute: 1,
        send_burst: 1,
        ..Default::default()
    })
    .await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    // The queue's one token.
    assert_eq!(
        server
            .send_as(queue.queue_id, &queue.sender_secret, b"filler")
            .await,
        ItemResult::Ok
    );
    send(&b1, &a1, 1, "over the limit").await;
    settle!([&b1], server.sends(&b1).len() >= 2);
    assert_eq!(server.held(queue.queue_id), 1);

    b1.clock.advance_seconds(60).unwrap();
    settle!([&b1], server.held(queue.queue_id) == 2);
    let sends = server.sends(&b1).len();
    quiet(&[&b1]).await;
    assert_eq!(server.sends(&b1).len(), sends);
}

/// Spec 8.2: A1 hands its text off to its mailbox A2 and goes off; B1 is off. A2's attempts
/// reach no device of B, so A2 puts the text into B1's queue, naming A1, which sealed it. B1
/// comes on and stores it once. (A1 has no queue address, so only A2 puts anything in.)
#[tokio::test]
async fn handed_off_entry_is_queued_by_mailbox() {
    let (mut a, b1, net, conv) = queue_accounts(&[A1, A2]).await;
    let (a1, a2) = (a.remove(0), a.remove(0));
    let server = queue_server(&net);
    configure(&a2, QUEUE_URL);
    configure(&b1, QUEUE_URL);
    settle!(
        [&a1, &a2, &b1],
        peer_queue(&a2, conv, &b1).await.flatten().is_some()
            && queue_ref_deliveries(&b1).await == 0
    );
    let queue = active(&b1).await.remove(0);
    a2.account.set_mailbox(true).await.unwrap();
    a1.account
        .storage_test_fixture()
        .mailbox_mark(installation(&a2).await, true, NOW * 1000)
        .await
        .unwrap();
    off(&b1, &[&a1, &a2]).await;
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    let key = send(&a1, &b1, 1, "handed over").await.key;
    assert_eq!(a1.account.mailbox_background().await.unwrap(), 1);
    settle!([&a1, &a2], server.held(queue.queue_id) == 1);
    let _a1 = stop(a1);
    let entries = entries_in(&server, queue.queue_id);
    assert_eq!(entries[0].sender_install, key.installation);
    assert!(server.sends(&a2).len() == 1);

    online(&b1).await;
    settle!(
        [&a2, &b1],
        message(&b1, key).await.is_some() && server.held(queue.queue_id) == 0
    );
    assert_eq!(texts_from(&b1, conv, key.sender).await.len(), 1);
}

/// Global Constraints: a sender in the mode "never" puts nothing into any queue, though it
/// keeps A1's.
#[tokio::test]
async fn never_mode_sender_sends_nothing() {
    let (a1, b1, _net, conv, server) = announced().await;
    b1.account.set_queue_mode(QueueMode::Never).await.unwrap();
    off(&a1, &[&b1]).await;
    let key = send(&b1, &a1, 1, "never").await.key;
    settle!([&b1], outbox(&b1, key).await.attempts >= 4);
    quiet(&[&b1]).await;
    assert!(peer_queue(&b1, conv, &a1).await.flatten().is_some());
    assert!(server.sends(&b1).is_empty());
}

/// Spec 7.2 (Task 8 review M6): B1 keeps A1's queue on its server; B1 is then pointed at
/// another server, and later at none. Neither puts anything into A1's queue (the test's
/// backend would take a request to any address, so a `SEND` would be counted).
#[tokio::test]
async fn queue_on_another_server_or_no_address_sends_nothing() {
    let (a1, b1, _net, conv, server) = announced().await;
    off(&a1, &[&b1]).await;
    configure(&b1, "https://other.test");
    let k1 = send(&b1, &a1, 1, "elsewhere").await.key;
    settle!([&b1], outbox(&b1, k1).await.attempts >= 2);
    quiet(&[&b1]).await;
    assert!(peer_queue(&b1, conv, &a1).await.flatten().is_some());
    assert!(server.sends(&b1).is_empty());

    b1.account.queue_configure(None).unwrap();
    let k2 = send(&b1, &a1, 2, "nowhere").await.key;
    settle!([&b1], outbox(&b1, k2).await.attempts >= 2);
    quiet(&[&b1]).await;
    assert!(server.sends(&b1).is_empty());
}

/// Spec 8.1: push and queue decide from the same list, each on its own: A1, off, gets both a
/// `cipher` push and the queue entry. The push stores it; the queue's copy and B1's direct
/// retry once A1 is on store nothing more, and it alerts once.
#[tokio::test]
async fn push_and_queue_both_for_same_unreached_device() {
    let (a1, b1, net, conv, server) = announced().await;
    let proxy = proxy(&net);
    android(&a1, "token-a1").await;
    let ia1 = installation(&a1).await;
    settle!(
        [&a1, &b1],
        peer_handle(&b1, conv, ia1).await.flatten().is_some()
            && push_state(&a1).await.deliveries.is_empty()
    );
    b1.account.push_configure(config(None)).unwrap();
    let tag = own_tag(&a1, conv).await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    let key = send(&b1, &a1, 1, "both ways").await.key;
    settle!(
        [&b1],
        server.held(queue.queue_id) == 1
            && proxy.push_requests().iter().any(|r| r
                .items
                .iter()
                .any(|i| matches!(i.kind, push_wire::PushItemKind::Cipher { .. })))
    );
    let (_, ciphertext) = mls_app_from_wire(&outbox(&b1, key).await.wire).unwrap();

    let push = ReceivedPush::Cipher { tag, ciphertext };
    assert_eq!(
        a1.account
            .push_received(push, Duration::from_secs(20))
            .await
            .unwrap(),
        PushOutcome::Notified
    );
    online(&a1).await;
    link(&a1, &b1).await;
    settle!(
        [&a1, &b1],
        server.held(queue.queue_id) == 0 && delivery(&b1, key).await == DeliveryState::Delivered
    );
    quiet(&[&a1, &b1]).await;
    assert_eq!(texts_from(&a1, conv, b1.peer).await.len(), 1);
    assert_eq!(alerts(&a1, conv).len(), 1);
}

/// B1's own Commit adding its device B2 (in B's record, never running): the new epoch.
async fn adds_b2(b1: &Fixture, a1: &Fixture, conv: ConversationId) -> u64 {
    b1.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_commit(
            conv,
            a1.peer,
            vec![mls_detached_key_package(b1.peer, [B2; 32])],
            vec![],
            NOW * 1000,
        )
        .await
        .unwrap()
        .expect("an own Commit")
}

/// Carry (Task 9 fix): a text sealed after a Commit A1 lacks goes into A1's queue behind that
/// Commit, in the same `SEND`, as a direct attempt carries it ahead. A1 comes on while B1 is
/// off, applies the Commit and opens the text in one fetch.
#[tokio::test]
async fn commit_goes_into_the_queue_ahead_of_the_text() {
    let (a1, b1, _net, conv, server) = announced_with(&[B1, B2]).await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    let epoch = adds_b2(&b1, &a1, conv).await;
    let commit = delivery_entry(&b1, conv, &a1, 1, epoch).await;
    let key = send(&b1, &a1, 1, "after the Commit").await.key;
    settle!([&b1], server.held(queue.queue_id) == 2);
    let stored = server.stored(queue.queue_id);
    assert_eq!(stored[0], commit);
    assert_eq!(
        QueueEntry::decode(&stored[1]).unwrap().frame,
        outbox(&b1, key).await.wire
    );
    assert_eq!(server.sends(&b1).len(), 1);
    // Review minor 4: the queue holds the Commit already, so the next text goes in alone.
    let second = send(&b1, &a1, 2, "and another").await.key;
    settle!([&b1], server.held(queue.queue_id) == 3);
    let sends = server.sends(&b1);
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[1].queues, [queue.queue_id]);
    let b_peer = b1.peer;
    let _b1 = stop(b1);

    online(&a1).await;
    settle!(
        [&a1],
        message(&a1, key).await.is_some()
            && message(&a1, second).await.is_some()
            && server.held(queue.queue_id) == 0
    );
    assert_eq!(state(&a1, conv).await.group_epoch, Some(epoch));
    assert_eq!(texts_from(&a1, conv, b_peer).await.len(), 2);
}

/// Review I1: a Commit with no text after it goes into the queue of the device it is queued
/// for by the standing pass. A1 comes on while B1 is off and takes the new epoch.
#[tokio::test]
async fn commit_with_no_text_after_it_goes_into_the_queue() {
    let (a1, b1, _net, conv, server) = announced_with(&[B1, B2]).await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    let epoch = adds_b2(&b1, &a1, conv).await;
    let commit = delivery_entry(&b1, conv, &a1, 1, epoch).await;
    // The standing pass's period.
    b1.clock.advance_monotonic_ms(300_000);
    settle!([&b1], server.held(queue.queue_id) == 1);
    assert_eq!(server.stored(queue.queue_id), [commit]);
    let _b1 = stop(b1);

    online(&a1).await;
    settle!(
        [&a1],
        state(&a1, conv).await.group_epoch == Some(epoch) && server.held(queue.queue_id) == 0
    );
}

/// Review I1: A1 takes B1's text from its queue while B1 is off and reads it. A1's `Read`
/// receipt (an MLS text's only receipt: its delivery is the direct `Ack`) goes into B1's queue,
/// and B1, coming on while A1 is off, sees the text read.
#[tokio::test]
async fn receipt_goes_into_the_senders_queue() {
    let (a1, b1, _net, conv, server) = announced().await;
    let (qa, qb) = (
        active(&a1).await.remove(0).queue_id,
        active(&b1).await.remove(0).queue_id,
    );
    off(&a1, &[&b1]).await;
    let key = send(&b1, &a1, 1, "read me").await.key;
    settle!([&b1], server.held(qa) == 1);
    off(&b1, &[]).await;

    online(&a1).await;
    settle!([&a1], message(&a1, key).await.is_some());
    a1.account
        .messages()
        .mark_read(conv, vec![key])
        .await
        .unwrap();
    settle!([&a1], server.held(qb) == 1);
    assert_eq!(
        entries_in(&server, qb)[0].sender_install,
        installation(&a1).await
    );
    off(&a1, &[]).await;

    online(&b1).await;
    settle!(
        [&b1],
        delivery(&b1, key).await == DeliveryState::Read && server.held(qb) == 0
    );
}

/// Review I1: A1 replaces its queue (7 days) while B1 is off. Its new `QueueRef` goes into B1's
/// queue; B1, coming on while A1 is off, takes it, and its next text goes into the new queue,
/// where A1 finds it.
#[tokio::test]
async fn rotated_queue_reaches_a_peer_that_is_off() {
    let (a1, b1, _net, conv, server) = announced().await;
    let (old, qb) = (
        active(&a1).await.remove(0),
        active(&b1).await.remove(0).queue_id,
    );
    off(&b1, &[&a1]).await;
    a1.clock
        .set_unix_seconds((old.created_at_ms + ROTATE_MS) / 1_000 + 1);
    settle!([&a1], {
        let now = active(&a1).await;
        now.len() == 1 && now[0].queue_id != old.queue_id && server.held(qb) == 1
    });
    let new = active(&a1).await.remove(0).queue_id;
    off(&a1, &[]).await;

    online(&b1).await;
    settle!(
        [&b1],
        peer_queue(&b1, conv, &a1).await.flatten().map(|q| q.1) == Some(new)
    );
    let key = send(&b1, &a1, 1, "to the new queue").await.key;
    settle!([&b1], server.held(new) == 1);
    let _b1 = stop(b1);

    online(&a1).await;
    settle!([&a1], message(&a1, key).await.is_some());
}

/// Review I2 (spec 7.1): my other devices send in the conversation too. A1 is off and A2 sends:
/// A1's queue gets A2's text, and A1 stores it while A2 and B1 are off.
#[tokio::test]
async fn own_device_off_gets_my_text_from_its_queue() {
    let (a1, a2, b1, conv, server) = two_queued_devices().await;
    settle!(
        [&a1, &a2, &b1],
        peer_queue(&a2, conv, &a1).await.flatten().is_some()
    );
    let q1 = active(&a1).await.remove(0).queue_id;
    off(&a1, &[&a2, &b1]).await;
    let key = send(&a2, &b1, 1, "from my other device").await.key;
    settle!(
        [&a2, &b1],
        delivery(&a2, key).await == DeliveryState::Delivered && server.held(q1) == 1
    );
    assert_eq!(entries_in(&server, q1)[0].sender_install, key.installation);
    let _a2 = stop(a2);
    let _b1 = stop(b1);

    online(&a1).await;
    settle!(
        [&a1],
        message(&a1, key).await.is_some() && server.held(q1) == 0
    );
}

/// Review minor 6: A1 puts its text into B1's queue, then hands it off to its mailbox A2,
/// which puts it in too. Both entries name A1, which sealed it, so they are the same entry and
/// the server keeps one.
#[tokio::test]
async fn origin_and_mailbox_put_one_entry() {
    let (mut a, b1, net, conv) = queue_accounts(&[A1, A2]).await;
    let (a1, a2) = (a.remove(0), a.remove(0));
    let server = queue_server(&net);
    for f in [&a1, &a2, &b1] {
        configure(f, QUEUE_URL);
    }
    settle!(
        [&a1, &a2, &b1],
        peer_queue(&a1, conv, &b1).await.flatten().is_some()
            && peer_queue(&a2, conv, &b1).await.flatten().is_some()
            && queue_ref_deliveries(&b1).await == 0
    );
    let queue = active(&b1).await.remove(0);
    a2.account.set_mailbox(true).await.unwrap();
    a1.account
        .storage_test_fixture()
        .mailbox_mark(installation(&a2).await, true, NOW * 1000)
        .await
        .unwrap();
    off(&b1, &[&a1, &a2]).await;
    let key = send(&a1, &b1, 1, "twice put").await.key;
    settle!([&a1], server.held(queue.queue_id) == 1);
    assert_eq!(a1.account.mailbox_background().await.unwrap(), 1);
    settle!([&a1, &a2], !server.sends(&a2).is_empty());
    quiet(&[&a1, &a2]).await;
    assert_eq!(server.held(queue.queue_id), 1);
    assert_eq!(
        entries_in(&server, queue.queue_id)[0].sender_install,
        key.installation
    );
}

/// Fix2 I3: the standing pass's request leaves a device out (two devices' `Commit`s together
/// pass `MAX_BODY`) and the server takes nothing (`limited`: every queue's burst is spent).
/// The pass does not come again at once: over 20 minutes B1 makes one standing `SEND` per
/// `STANDING_EVERY_MS` (5 min), not one a poll. Once the server takes them, it does.
#[tokio::test]
async fn standing_pass_that_stores_nothing_waits_its_period() {
    let (a1, a2, b1, conv, server) = two_queued_devices().await;
    let queues = [active(&a1).await.remove(0), active(&a2).await.remove(0)];
    off(&a1, &[&b1, &a2]).await;
    off(&a2, &[&b1]).await;
    for queue in &queues {
        for n in 0..20u32 {
            assert_eq!(
                server
                    .send_as(queue.queue_id, &queue.sender_secret, &n.to_be_bytes())
                    .await,
                ItemResult::Ok
            );
        }
    }
    // Eight Commits of 60 KiB for each device: one device's fit in a request, both do not.
    let fixture = b1.account.storage_test_fixture();
    for (d, f) in [&a1, &a2].into_iter().enumerate() {
        let to = installation(f).await;
        for epoch in 0..8u64 {
            let body = vec![(d * 8) as u8 + epoch as u8; 61_440];
            fixture
                .mls_set_delivery(conv, to, 1, 100 + epoch, Some((body, NOW * 1000 + DAY_MS)))
                .await
                .unwrap();
        }
    }
    let before = server.sends(&b1).len();
    b1.clock.advance_monotonic_ms(300_000);
    settle!([&b1], server.sends(&b1).len() > before);
    for _ in 0..60 {
        b1.clock.advance_monotonic_ms(20_000);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let sends = server.sends(&b1)[before..].to_vec();
    assert!(
        (4..=6).contains(&sends.len()),
        "{} standing SENDs in 20 min",
        sends.len()
    );
    // Each request carried one device's eight Commits.
    assert!(sends.iter().all(|s| s.queues.len() == 8));
    let held = || server.held(queues[0].queue_id) + server.held(queues[1].queue_id);
    assert_eq!(held(), 40);

    // The limit refills: the next pass stores one device's Commits and, having left the other
    // out, comes again at once (no scheduler time passes) for the other's.
    b1.clock.advance_seconds(60).unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    if held() == 40 {
        // The minute did not bring the pass round: one period does, once.
        b1.clock.advance_monotonic_ms(300_000);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while held() < 56 {
        assert!(tokio::time::Instant::now() < deadline, "held {}", held());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
