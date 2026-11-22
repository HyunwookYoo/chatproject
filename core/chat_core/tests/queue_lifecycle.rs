#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Queue reconciliation on the receiving side (M8 spec 6.2, 6.3, 7): a device without a
//! qualified mailbox makes one queue per conversation at the queue server and announces it
//! over MLS (`QueueRef`); it rotates it after 7 days, withdraws and later deletes it when the
//! verdict says no, and takes the queues its peers announce on its own server only.
include!("support/queue_network.rs");

use queue_wire::{RETIRE_MS, ROTATE_MS};

/// A1 and B1 with a conversation and a queue server, both configured with it; returns once
/// each keeps the other's announced queue and neither has anything left to deliver.
async fn announced() -> (
    Fixture,
    Fixture,
    Arc<Backend>,
    ConversationId,
    Arc<FakeQueueServer>,
) {
    let (mut a, b1, net, conv) = queue_accounts(&[A1]).await;
    let a1 = a.remove(0);
    let server = queue_server(&net);
    configure(&a1, QUEUE_URL);
    configure(&b1, QUEUE_URL);
    settle!(
        [&a1, &b1],
        peer_queue(&b1, conv, &a1).await.flatten().is_some()
            && peer_queue(&a1, conv, &b1).await.flatten().is_some()
            && queue_ref_deliveries(&a1).await == 0
            && queue_ref_deliveries(&b1).await == 0
    );
    (a1, b1, net, conv, server)
}

/// A2 becomes a qualified mailbox (uptime seeded over a week), and `a1` judges so.
async fn qualified_mailbox(a1: &Fixture, a2: &Fixture, others: &[&Fixture]) {
    a2.account.set_mailbox(true).await.unwrap();
    a2.account
        .mailbox_test_seed_uptime(950, 7 * DAY_MS)
        .await
        .unwrap();
    let a2_id = installation(a2).await;
    let mut all = vec![a1, a2];
    all.extend_from_slice(others);
    settle!(
        all.iter().copied(),
        verdict(a1).await
            == Verdict::HasMailbox {
                installation: a2_id
            }
    );
}

/// Spec 6.3: with no mailbox, a device makes one queue for its conversation (one `CREATE`, its
/// keys stored first), and the peer keeps its `QueueRef` with the server address, id and
/// sender key; the device says it uses a queue.
#[tokio::test]
async fn no_mailbox_creates_one_queue_per_conversation_and_announces() {
    let (a1, b1, _net, conv, server) = announced().await;
    assert_eq!(verdict(&a1).await, Verdict::NoMailbox);
    let mine = active(&a1).await;
    assert_eq!(mine.len(), 1);
    let queue = &mine[0];
    assert_eq!(queue.conversation, conv);
    assert!(queue.server_confirmed);
    assert_eq!(queue.last_seq, 0);
    assert!(retiring(&a1).await.is_empty());
    assert_eq!(
        peer_queue(&b1, conv, &a1).await,
        Some(Some((
            QUEUE_URL.to_owned(),
            queue.queue_id,
            queue.sender_secret
        )))
    );
    let b1_row = peer_row(&b1, conv, installation(&a1).await).await.unwrap();
    assert_eq!(b1_row.sent_at_ms, queue.created_at_ms);
    assert!(!b1_row.dead);
    let creates = server.sent(&a1, CommandName::Create);
    assert_eq!(creates.len(), 1);
    assert_eq!(creates[0].queues, [queue.queue_id]);
    assert!(server.queues().contains(&queue.queue_id));
    // Nothing but the one `CREATE`: no fetching or sending here yet.
    assert_eq!(server.calls_from(&a1).len(), 1);
    assert!(a1.account.queue_state().await.unwrap().using_queue);
    // And the other way: B1 has no mailbox either, and A1 keeps B1's queue.
    assert!(peer_queue(&a1, conv, &b1).await.flatten().is_some());
}

/// Spec 6.3: the account's mailbox becomes qualified: the queue is withdrawn at the peer and
/// goes `Retiring` (still at the server, fetched until then), and 48 hours later it is
/// deleted at the server and its row goes.
#[tokio::test]
async fn mailbox_recovery_withdraws_and_deletes_after_48h() {
    let (mut a, b1, net, conv) = queue_accounts(&[A1, A2]).await;
    let (a1, a2) = (a.remove(0), a.remove(0));
    let server = queue_server(&net);
    for f in [&a1, &a2, &b1] {
        configure(f, QUEUE_URL);
    }
    settle!(
        [&a1, &a2, &b1],
        peer_queue(&b1, conv, &a1).await.flatten().is_some()
            && queue_ref_deliveries(&a1).await == 0
    );
    let queue = active(&a1).await.remove(0);
    let retired_from = a1.clock.unix_seconds() * 1_000;

    qualified_mailbox(&a1, &a2, &[&b1]).await;
    settle!(
        [&a1, &a2, &b1],
        peer_queue(&b1, conv, &a1).await == Some(None) && active(&a1).await.is_empty()
    );
    let retired = retiring(&a1).await;
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].queue_id, queue.queue_id);
    let delete_at = retired[0].delete_at_ms.unwrap();
    assert!(delete_at >= retired_from + RETIRE_MS);
    assert!(!a1.account.queue_state().await.unwrap().using_queue);
    // Still at the server until then, and no `DELETE` yet.
    assert!(server.queues().contains(&queue.queue_id));
    assert!(server.sent(&a1, CommandName::Delete).is_empty());
    let b1_row = peer_row(&b1, conv, installation(&a1).await).await.unwrap();
    assert!(b1_row.sent_at_ms > queue.created_at_ms);

    a1.clock.set_unix_seconds(delete_at / 1_000 + 1);
    settle!(
        [&a1],
        tables(&a1).await.own.is_empty() && !server.queues().contains(&queue.queue_id)
    );
    let deletes = server.sent(&a1, CommandName::Delete);
    assert_eq!(deletes.len(), 1);
    assert_eq!(deletes[0].queues, [queue.queue_id]);
    // The tombstone at the peer stays.
    assert_eq!(peer_queue(&b1, conv, &a1).await, Some(None));
}

/// Spec 6.3 rotation: a queue older than 7 days is replaced by a new one, announced to the
/// peer; the old one stays `Retiring` (at the server too) for 48 hours.
#[tokio::test]
async fn rotation_after_7_days_keeps_old_until_retired() {
    let (a1, b1, _net, conv, server) = announced().await;
    let old = active(&a1).await.remove(0);
    a1.clock
        .set_unix_seconds((old.created_at_ms + ROTATE_MS) / 1_000 + 1);
    settle!([&a1, &b1], {
        let now = active(&a1).await;
        now.len() == 1
            && now[0].queue_id != old.queue_id
            && now[0].server_confirmed
            && peer_queue(&b1, conv, &a1).await.flatten().map(|q| q.1) == Some(now[0].queue_id)
            && queue_ref_deliveries(&a1).await == 0
    });
    let new = active(&a1).await.remove(0);
    let retired = retiring(&a1).await;
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].queue_id, old.queue_id);
    assert!(retired[0].delete_at_ms.unwrap() >= old.created_at_ms + ROTATE_MS + RETIRE_MS);
    assert!(new.created_at_ms >= old.created_at_ms + ROTATE_MS);
    let queues = server.queues();
    assert!(queues.contains(&old.queue_id) && queues.contains(&new.queue_id));
    assert!(server.sent(&a1, CommandName::Delete).is_empty());
    assert_eq!(server.sent(&a1, CommandName::Create).len(), 2);
    let b1_row = peer_row(&b1, conv, installation(&a1).await).await.unwrap();
    assert_eq!(b1_row.sent_at_ms, new.created_at_ms);
}

/// The job converges: run again (the mode set again, and after a restart) it adds no queue,
/// no request and no frame.
#[tokio::test]
async fn reconcile_twice_adds_nothing() {
    let (a1, b1, net, conv, server) = announced().await;
    let before = tables(&a1).await;
    let calls = server.calls_from(&a1).len();
    a1.account.set_queue_mode(QueueMode::Auto).await.unwrap();
    quiet(&[&a1, &b1]).await;
    assert_eq!(tables(&a1).await, before);
    assert_eq!(server.calls_from(&a1).len(), calls);
    assert_eq!(queue_ref_deliveries(&a1).await, 0);

    let dir = stop(a1);
    let a1 = on(dir, &net, &[&b1]).await;
    configure(&a1, QUEUE_URL);
    quiet(&[&a1, &b1]).await;
    assert_eq!(tables(&a1).await.own, before.own);
    assert!(server.calls_from(&a1).is_empty());
    assert_eq!(queue_ref_deliveries(&a1).await, 0);
    let _ = conv;
}

/// Review Focus 2: the device dies after the server stored its `CREATE` and before it heard
/// back. Started again it sends the same queue id (its keys were stored first), and ends with
/// one queue, confirmed and announced.
#[tokio::test]
async fn crash_mid_create_converges_to_one_queue() {
    let (mut a, b1, net, conv) = queue_accounts(&[A1]).await;
    let a1 = a.remove(0);
    let server = queue_server(&net);
    server.hold(true);
    configure(&a1, QUEUE_URL);
    settle!([&a1], !server.sent(&a1, CommandName::Create).is_empty());
    let first = server.sent(&a1, CommandName::Create)[0].queues.clone();
    assert_eq!(first.len(), 1);
    let id = first[0];
    // Stored first, and at the server already.
    let stored = active(&a1).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].queue_id, id);
    assert!(!stored[0].server_confirmed);
    assert!(server.queues().contains(&id));

    let dir = stop(a1);
    server.hold(false);
    let a1 = on(dir, &net, &[&b1]).await;
    configure(&a1, QUEUE_URL);
    configure(&b1, QUEUE_URL);
    settle!(
        [&a1, &b1],
        peer_queue(&b1, conv, &a1).await.flatten().map(|q| q.1) == Some(id)
    );
    let mine = active(&a1).await;
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].queue_id, id);
    assert!(mine[0].server_confirmed);
    assert_eq!(tables(&a1).await.own.len(), 1);
    // Every `CREATE` A1 ever sent (before and after the restart) named that one queue.
    let b1_clock = Some(Arc::as_ptr(&b1.clock) as usize);
    let named: Vec<QueueId> = server
        .calls()
        .into_iter()
        .filter(|c| c.command == CommandName::Create && c.from.is_some() && c.from != b1_clock)
        .flat_map(|c| c.queues)
        .collect();
    assert!(named.len() >= 2);
    assert!(named.iter().all(|q| *q == id));
}

/// Review Focus 1: the verdict goes "has a mailbox" and back within 48 hours. The released
/// queue stays `Retiring` with what a peer put in it meanwhile (fetched until it is deleted,
/// Task 9), and a new queue is made; queues are made only when the verdict changed.
#[tokio::test]
async fn release_then_recreate_keeps_entries() {
    let (mut a, b1, net, conv) = queue_accounts(&[A1, A2]).await;
    let (a1, a2) = (a.remove(0), a.remove(0));
    let server = queue_server(&net);
    for f in [&a1, &a2, &b1] {
        configure(f, QUEUE_URL);
    }
    settle!(
        [&a1, &a2, &b1],
        peer_queue(&b1, conv, &a1).await.flatten().is_some()
            && queue_ref_deliveries(&a1).await == 0
    );
    let first = active(&a1).await.remove(0);
    qualified_mailbox(&a1, &a2, &[&b1]).await;
    settle!([&a1, &a2, &b1], active(&a1).await.is_empty());
    // A peer that had not taken the withdrawal yet puts a ciphertext in the released queue.
    assert_eq!(
        server
            .send_as(first.queue_id, &first.sender_secret, b"sent while released")
            .await,
        ItemResult::Ok
    );

    a2.account.set_mailbox(false).await.unwrap();
    settle!([&a1, &a2, &b1], {
        let now = active(&a1).await;
        verdict(&a1).await == Verdict::NoMailbox
            && now.len() == 1
            && now[0].server_confirmed
            && peer_queue(&b1, conv, &a1).await.flatten().map(|q| q.1) == Some(now[0].queue_id)
    });
    let second = active(&a1).await.remove(0);
    assert_ne!(second.queue_id, first.queue_id);
    let retired = retiring(&a1).await;
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].queue_id, first.queue_id);
    assert_eq!(
        server
            .entries(first.queue_id, &first.recipient_secret)
            .await,
        Some(1)
    );
    assert!(server.sent(&a1, CommandName::Delete).is_empty());
    let created: Vec<QueueId> = server
        .sent(&a1, CommandName::Create)
        .into_iter()
        .flat_map(|c| c.queues)
        .collect();
    assert_eq!(created, [first.queue_id, second.queue_id]);
}

/// Global Constraints: the mode "never" sends the queue server nothing: no queue made, and one
/// made before is withdrawn and, 48 hours later, dropped here without a `DELETE`.
#[tokio::test]
async fn never_mode_sends_nothing() {
    let (mut a, b1, net, conv) = queue_accounts(&[A1]).await;
    let a1 = a.remove(0);
    let server = queue_server(&net);
    a1.account.set_queue_mode(QueueMode::Never).await.unwrap();
    configure(&a1, QUEUE_URL);
    configure(&b1, QUEUE_URL);
    // A1 runs its polls meanwhile: it takes B1's queue.
    settle!(
        [&a1, &b1],
        peer_queue(&a1, conv, &b1).await.flatten().is_some()
    );
    quiet(&[&a1, &b1]).await;
    assert!(server.calls_from(&a1).is_empty());
    assert!(tables(&a1).await.own.is_empty());
    assert!(!a1.account.queue_state().await.unwrap().using_queue);

    a1.account.set_queue_mode(QueueMode::Auto).await.unwrap();
    settle!(
        [&a1, &b1],
        peer_queue(&b1, conv, &a1).await.flatten().is_some()
            && queue_ref_deliveries(&a1).await == 0
    );
    let calls = server.calls_from(&a1).len();
    a1.account.set_queue_mode(QueueMode::Never).await.unwrap();
    settle!(
        [&a1, &b1],
        peer_queue(&b1, conv, &a1).await == Some(None) && active(&a1).await.is_empty()
    );
    let delete_at = retiring(&a1).await[0].delete_at_ms.unwrap();
    a1.clock.set_unix_seconds(delete_at / 1_000 + 1);
    settle!([&a1], tables(&a1).await.own.is_empty());
    quiet(&[&a1]).await;
    assert_eq!(server.calls_from(&a1).len(), calls);
}

/// Review Focus 5: a peer announces a queue on another server. The device takes the frame
/// (acknowledged, nothing left to send it) and keeps nothing of it, so nothing it ever sends
/// reaches that server.
#[tokio::test]
async fn foreign_queue_url_is_ignored() {
    let (mut a, b1, net, conv) = queue_accounts(&[A1]).await;
    let a1 = a.remove(0);
    let server = queue_server(&net);
    configure(&a1, "https://other.test");
    configure(&b1, QUEUE_URL);
    settle!([&a1, &b1], {
        let mine = active(&a1).await;
        mine.len() == 1 && mine[0].server_confirmed && queue_ref_deliveries(&a1).await == 0
    });
    // B1's own queue went the other way, on B1's server: A1 does not keep it either.
    settle!(
        [&a1, &b1],
        active(&b1)
            .await
            .first()
            .is_some_and(|q| q.server_confirmed)
            && queue_ref_deliveries(&b1).await == 0
    );
    assert_eq!(peer_queue(&b1, conv, &a1).await, None);
    assert_eq!(peer_queue(&a1, conv, &b1).await, None);
    let foreign = active(&a1).await[0].queue_id;
    assert!(
        server
            .calls_from(&b1)
            .iter()
            .all(|c| !c.queues.contains(&foreign))
    );
}

/// Ruling R10 for queues: a newer `QueueRef` replaces the stored one, a late older frame
/// (the device log, a hand-over) never brings back a replaced queue, and a withdrawal's
/// tombstone stays against an older frame too.
#[tokio::test]
async fn newer_queue_ref_frame_wins_and_tombstone_stays() {
    let (a1, b1, _net, conv, _server) = announced().await;
    let a1_id = installation(&a1).await;
    let first = active(&a1).await.remove(0);
    a1.clock
        .set_unix_seconds((first.created_at_ms + ROTATE_MS) / 1_000 + 1);
    settle!([&a1, &b1], {
        let now = active(&a1).await;
        now.len() == 1
            && now[0].queue_id != first.queue_id
            && peer_queue(&b1, conv, &a1).await.flatten().map(|q| q.1) == Some(now[0].queue_id)
    });
    let second = active(&a1).await.remove(0);
    let a_messages = a1.account.storage_test_fixture().messages().await.unwrap();
    let b_messages = b1.account.storage_test_fixture().messages().await.unwrap();
    let late = |queue: &QueueOwnRow| chat_core::storage_fixture::QueueRefBody {
        server_url: QUEUE_URL.into(),
        queue_id: queue.queue_id,
        sender_secret: queue.sender_secret,
    };
    // The first queue's frame again, late: the second stays.
    let ct = a_messages
        .mls_seal_queue_ref(conv, Some(late(&first)), first.created_at_ms)
        .await
        .unwrap();
    b_messages
        .mls_open_app_queue(a1.peer, a1_id, conv, ct, NOW * 1_000, QUEUE_URL)
        .await
        .unwrap();
    let row = peer_row(&b1, conv, a1_id).await.unwrap();
    assert_eq!(row.queue.map(|q| q.1), Some(second.queue_id));
    assert_eq!(row.sent_at_ms, second.created_at_ms);

    a1.account.set_queue_mode(QueueMode::Never).await.unwrap();
    settle!([&a1, &b1], peer_queue(&b1, conv, &a1).await == Some(None));
    let tombstone = peer_row(&b1, conv, a1_id).await.unwrap();
    assert!(tombstone.sent_at_ms > second.created_at_ms);
    // The second queue's frame again, late: the withdrawal stays.
    let ct = a_messages
        .mls_seal_queue_ref(conv, Some(late(&second)), second.created_at_ms)
        .await
        .unwrap();
    b_messages
        .mls_open_app_queue(a1.peer, a1_id, conv, ct, NOW * 1_000, QUEUE_URL)
        .await
        .unwrap();
    assert_eq!(peer_row(&b1, conv, a1_id).await.unwrap(), tombstone);
}

/// Plan "성능": a device turning on makes every conversation's queue with one `CREATE`
/// request (up to `MAX_ITEMS` items; 100 conversations is `create_batches_100_conversations_in_
/// one_request` in `manager/queue.rs`).
#[tokio::test]
async fn create_batches_every_conversation_in_one_request() {
    const C: u8 = 3;
    const D: u8 = 4;
    const C1: u8 = 16;
    const D1: u8 = 17;
    let ra = week_record(A, &[A1]);
    let others = [(B, B1), (C, C1), (D, D1)];
    let mut records = vec![(A, ra.clone())];
    records.extend(
        others
            .iter()
            .map(|(acct, seed)| (*acct, week_record(*acct, &[*seed]))),
    );
    let net = backend(&records);
    let a1 = device(A, A1, &ra, &net).await;
    let mut peers = Vec::new();
    for (acct, seed) in others {
        let x = device(acct, seed, &week_record(acct, &[seed]), &net).await;
        link(&a1, &x).await;
        befriend(&a1, &x).await;
        befriend(&x, &a1).await;
        a1.account.connect_account(x.peer).await.unwrap();
        let conv = conversation(&a1, &x).await;
        conversation(&x, &a1).await;
        grouped(&a1, &x, conv).await;
        configure(&x, QUEUE_URL);
        peers.push((x, conv));
    }
    let server = queue_server(&net);
    configure(&a1, QUEUE_URL);
    let mut all = vec![&a1];
    all.extend(peers.iter().map(|(x, _)| x));
    settle!(all.iter().copied(), {
        let mut ok = queue_ref_deliveries(&a1).await == 0;
        for (x, conv) in &peers {
            ok = ok && peer_queue(x, *conv, &a1).await.flatten().is_some();
        }
        ok
    });
    let creates = server.sent(&a1, CommandName::Create);
    assert_eq!(creates.len(), 1);
    assert_eq!(creates[0].queues.len(), 3);
    assert_eq!(active(&a1).await.len(), 3);
}

/// The push handle path is unchanged beside the queue: with push and the queue both on, the
/// peer keeps this device's handle and its queue, and both frames (kind 1 and kind 2) leave
/// the delivery table once taken.
#[tokio::test]
async fn push_handle_delivery_unchanged() {
    let (mut a, b1, net, conv) = queue_accounts(&[A1]).await;
    let a1 = a.remove(0);
    let a1_id = installation(&a1).await;
    let proxy = proxy(&net);
    let _server = queue_server(&net);
    configure(&a1, QUEUE_URL);
    configure(&b1, QUEUE_URL);
    android(&a1, "token-a1").await;
    settle!(
        [&a1, &b1],
        peer_handle(&b1, conv, a1_id).await.flatten().is_some()
            && peer_queue(&b1, conv, &a1).await.flatten().is_some()
            && push_state(&a1).await.deliveries.is_empty()
    );
    assert_eq!(
        peer_handle(&b1, conv, a1_id).await.flatten(),
        own_tag_handle(&a1, conv).await
    );
    assert_eq!(proxy.handle_requests().len(), 1);
    assert!(push_state(&a1).await.tags[0].announced);
}

/// Kind 2 frames join the mailbox hand-off (M6 spec 7) as kind 1 frames do: A1's `QueueRef`,
/// queued while B1 is off, reaches B1 through A2 while A1 is off too (when B1 most needs it),
/// and A1's row goes once A1 is back.
#[tokio::test]
async fn queue_ref_rides_the_mailbox_handoff() {
    let (mut a, b1, net, conv) = queue_accounts(&[A1, A2]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let (a1_id, a2_id) = (installation(&a1).await, installation(&a2).await);
    let _server = queue_server(&net);
    a2.account.set_mailbox(true).await.unwrap();
    settle!(
        [&a1, &a2],
        mailbox(&a1)
            .await
            .device_rows
            .iter()
            .any(|(i, m)| *i == a2_id && *m)
    );
    let b_dir = stop(b1);
    configure(&a1, QUEUE_URL);
    settle!(
        [&a1, &a2],
        queue_ref_deliveries(&a1).await == 1
            && active(&a1)
                .await
                .first()
                .is_some_and(|q| q.server_confirmed)
    );
    let queue = active(&a1).await.remove(0);
    assert!(a1.account.mailbox_background().await.unwrap() >= 1);
    settle!([&a1, &a2], !mailbox(&a2).await.handoff_rows.is_empty());
    let a1_dir = stop(a1);

    let b1 = start_as(B, b_dir, net.clone()).await;
    configure(&b1, QUEUE_URL);
    link(&b1, &a2).await;
    settle!(
        [&a2, &b1],
        peer_row(&b1, conv, a1_id)
            .await
            .and_then(|r| r.queue)
            .map(|q| q.1)
            == Some(queue.queue_id)
    );
    let a1 = on(a1_dir, &net, &[&a2]).await;
    configure(&a1, QUEUE_URL);
    settle!([&a1, &a2, &b1], queue_ref_deliveries(&a1).await == 0);
}
