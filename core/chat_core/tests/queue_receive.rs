#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Fetching from this device's server queues (M8 spec 6.4): at a network start, a push's
//! fetch, a full sync and every 5 minutes while online, one `RECV` asks every queue; entries
//! are taken through the receive path and acknowledged only after they are stored, so a lost
//! `ACK` brings them again and they are taken once. Pages go on until a queue answers none,
//! a queue the answer had no room for is asked first next, a queue the server lost is
//! replaced, and the mode "never" asks nothing.
include!("support/queue_network.rs");

use chat_core::push::{PushOutcome, ReceivedPush, Tag};
use queue_wire::ROTATE_MS;

/// The periodic fetch while online (spec 6.4).
const FETCH_EVERY_MS: u64 = 300_000;
const WAIT: Duration = Duration::from_secs(20);

/// Polls `$cond` for up to `$secs` real seconds without moving any clock: what happens must
/// come from what the test did, not from a timer.
macro_rules! within {
    ($secs:expr, $cond:expr) => {{
        let deadline = tokio::time::Instant::now() + Duration::from_secs($secs);
        loop {
            if $cond {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "not within {}s: {}",
                $secs,
                stringify!($cond)
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }};
}

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

/// Items of the conversation that alerted (`new_messages > 0`).
fn alerts(f: &Fixture, conv: ConversationId) -> Vec<NotifyItem> {
    notify_events(f)
        .into_iter()
        .filter(|i| i.conversation == conv && i.new_messages > 0)
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

/// `f`'s network goes off, and `others` see it gone.
async fn off(f: &Fixture, others: &[&Fixture]) {
    let id = installation(f).await;
    network(f, NetworkOp::Stop).await;
    settle!(
        others.iter().copied(),
        others.iter().all(|o| !ready_with(o, id))
    );
}

/// How many entries `server` holds in `queue` now.
async fn held(server: &FakeQueueServer, queue: &QueueOwnRow) -> usize {
    server.held(queue.queue_id)
}

/// The test puts `ct` in `queue`, as a peer with its sender key would.
async fn put(server: &FakeQueueServer, queue: &QueueOwnRow, ct: &[u8]) {
    assert_eq!(
        server
            .send_as(queue.queue_id, &queue.sender_secret, ct)
            .await,
        ItemResult::Ok
    );
}

/// `from` sends text `n` to `to`'s account while it cannot reach it, and the test puts the
/// frame it sealed in `queue` (Task 10 will do that in the core).
async fn queued_text(
    from: &Fixture,
    to: &Fixture,
    server: &FakeQueueServer,
    queue: &QueueOwnRow,
    n: u8,
) -> MessageKey {
    let key = send(from, to, n, &format!("queued {n}")).await.key;
    put(server, queue, &sealed_frame(from, key).await).await;
    key
}

/// A1's fetches since the first `from` of them, once the last one was answered.
async fn fetched_since(server: &FakeQueueServer, f: &Fixture, from: usize) -> Vec<Call> {
    server.fetches(f).split_off(from)
}

/// Lets `f`'s periodic fetch come once (its clock moves past it), and waits until its answers
/// are in and taken.
async fn periodic(server: &FakeQueueServer, f: &Fixture) {
    let before = server.fetches(f).len();
    f.clock.advance_monotonic_ms(FETCH_EVERY_MS + 1_000);
    within!(10, {
        let after = fetched_since(server, f, before).await;
        after.last().is_some_and(|c| !c.got.is_empty())
    });
    // The run ends (an `ACK`, its next page) without the clock moving.
    tokio::time::sleep(Duration::from_millis(500)).await;
}

/// Spec 6.4 / 2 goal 5: texts that came while A1 and the sender were both off arrive from the
/// queue once A1's network is on, once each and in order, with one alert; they are
/// acknowledged once and the queue is empty after.
#[tokio::test]
async fn offline_texts_arrive_once_in_order_after_start() {
    let (a1, b1, _net, conv, server) = announced().await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    let k1 = queued_text(&b1, &a1, &server, &queue, 1).await;
    let k2 = queued_text(&b1, &a1, &server, &queue, 2).await;
    let b_peer = b1.peer;
    // The sender is off too: only the queue has them.
    let _b1 = stop(b1);
    let before = notify_events(&a1).len();

    online(&a1).await;
    settle!(
        [&a1],
        texts_from(&a1, conv, b_peer).await.len() == 2 && held(&server, &queue).await == 0
    );
    let keys: Vec<MessageKey> = texts_from(&a1, conv, b_peer)
        .await
        .into_iter()
        .map(|m| m.key)
        .collect();
    assert_eq!(keys, [k1, k2]);
    let alerted: Vec<NotifyItem> = notify_events(&a1)
        .split_off(before)
        .into_iter()
        .filter(|i| i.conversation == conv && i.new_messages > 0)
        .collect();
    assert_eq!(alerted.len(), 1);
    assert_eq!(alerted[0].new_messages, 2);
    assert_eq!(server.sent(&a1, CommandName::Ack).len(), 1);
    assert!(active(&a1).await[0].last_seq > 0);
}

/// Spec 6.4: A1 stored what it fetched, and died before any `ACK` reached the server. Started
/// again it is given the entry again (its cursor never moved), takes it once (no second row,
/// no alert) and acknowledges it.
#[tokio::test]
async fn lost_ack_is_resent_and_deduped() {
    let (a1, b1, net, conv, server) = announced().await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    let key = queued_text(&b1, &a1, &server, &queue, 1).await;
    let b_peer = b1.peer;
    let _b1 = stop(b1);
    server.lose_acks(true);

    online(&a1).await;
    settle!(
        [&a1],
        message(&a1, key).await.is_some() && !server.sent(&a1, CommandName::Ack).is_empty()
    );
    assert_eq!(alerts(&a1, conv).len(), 1);
    assert_eq!(active(&a1).await[0].last_seq, 0);
    let dir = stop(a1);
    assert_eq!(held(&server, &queue).await, 1);

    server.lose_acks(false);
    let a1 = start(dir, net.clone()).await;
    configure(&a1, QUEUE_URL);
    settle!([&a1], held(&server, &queue).await == 0);
    let again = server.fetches(&a1);
    assert!(again.iter().any(|c| {
        c.got
            .iter()
            .any(|g| matches!(g, Got::Entries(e) if e.len() == 1))
    }));
    assert_eq!(texts_from(&a1, conv, b_peer).await.len(), 1);
    assert!(alerts(&a1, conv).is_empty());
    assert!(active(&a1).await[0].last_seq > 0);
}

/// Review Focus 4: the same ciphertext comes as a `cipher` push and from the queue. One row,
/// one alert, and the queue's copy is acknowledged at the next fetch.
#[tokio::test]
async fn push_and_queue_same_ciphertext_alert_once() {
    let (a1, b1, net, conv, server) = announced().await;
    let _proxy = proxy(&net);
    android(&a1, "token-a1").await;
    settle!(
        [&a1, &b1],
        push_state(&a1)
            .await
            .tags
            .iter()
            .any(|t| t.conversation == conv)
    );
    let tag = own_tag(&a1, conv).await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    let key = queued_text(&b1, &a1, &server, &queue, 1).await;
    let (_, ciphertext) = mls_app_from_wire(&outbox(&b1, key).await.wire).unwrap();
    let b_peer = b1.peer;
    let _b1 = stop(b1);

    let push = ReceivedPush::Cipher { tag, ciphertext };
    assert_eq!(
        a1.account.push_received(push, WAIT).await.unwrap(),
        PushOutcome::Notified
    );
    assert!(message(&a1, key).await.is_some());
    online(&a1).await;
    settle!([&a1], held(&server, &queue).await == 0);
    assert_eq!(texts_from(&a1, conv, b_peer).await.len(), 1);
    assert_eq!(alerts(&a1, conv).len(), 1);
    assert!(!server.sent(&a1, CommandName::Ack).is_empty());
}

/// The same ciphertext reaches A1 from A2's log (M6 sync) and from A1's queue: stored once,
/// alerted once, and the queue emptied.
#[tokio::test]
async fn mailbox_sync_and_queue_same_ciphertext_store_once() {
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
    off(&a1, &[&a2, &b1]).await;
    // Sealed before any attempt: once A2 has it, B1 keeps no copy.
    b1.account.message_test_hold_scheduler(true).await.unwrap();
    let key = send(&b1, &a1, 1, "both ways").await.key;
    put(&server, &queue, &sealed_frame(&b1, key).await).await;
    b1.account.message_test_hold_scheduler(false).await.unwrap();
    settle!([&a2, &b1], message(&a2, key).await.is_some());
    let b_peer = b1.peer;
    let _b1 = stop(b1);

    online(&a1).await;
    link(&a1, &a2).await;
    a1.account.mailbox_sync_now().await.unwrap();
    settle!(
        [&a1, &a2],
        message(&a1, key).await.is_some()
            && held(&server, &queue).await == 0
            && caught_up(&a1, &a2).await
    );
    assert_eq!(texts_from(&a1, conv, b_peer).await.len(), 1);
    assert_eq!(alerts(&a1, conv).len(), 1);
}

/// M7 `wake`: its fetch asks the queue too, so the text that only the queue holds arrives
/// within the push's deadline, with no clock moving.
#[tokio::test]
async fn push_wake_fetches_queue_too() {
    let (a1, b1, net, conv, server) = announced().await;
    let _proxy = proxy(&net);
    android(&a1, "token-a1").await;
    settle!(
        [&a1, &b1],
        push_state(&a1)
            .await
            .tags
            .iter()
            .any(|t| t.conversation == conv)
    );
    let tag = own_tag(&a1, conv).await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    let key = send(&b1, &a1, 1, "woken").await.key;
    let frame = sealed_frame(&b1, key).await;
    let _b1 = stop(b1);
    // Back on: the fetch of the network start finds nothing yet.
    let before = server.fetches(&a1).len();
    online(&a1).await;
    within!(10, {
        let after = fetched_since(&server, &a1, before).await;
        after.last().is_some_and(|c| !c.got.is_empty())
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    put(&server, &queue, &frame).await;

    let push = ReceivedPush::Wake { tag };
    assert_eq!(
        a1.account
            .push_received(push, Duration::from_secs(5))
            .await
            .unwrap(),
        PushOutcome::Notified
    );
    assert!(message(&a1, key).await.is_some());
    assert_eq!(a1.account.push_test_fetches().last(), Some(&(true, true)));
}

/// Spec 11 (roadmap M7): after FCM dropped pushes (`onDeletedMessages`) the full sync fetches
/// the queue too, with no clock moving.
#[tokio::test]
async fn on_deleted_messages_fetches_queue() {
    let (a1, b1, _net, _conv, server) = announced().await;
    let queue = active(&a1).await.remove(0);
    off(&a1, &[&b1]).await;
    let key = send(&b1, &a1, 1, "dropped push").await.key;
    let frame = sealed_frame(&b1, key).await;
    let _b1 = stop(b1);
    let before = server.fetches(&a1).len();
    online(&a1).await;
    within!(10, {
        let after = fetched_since(&server, &a1, before).await;
        after.last().is_some_and(|c| !c.got.is_empty())
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    put(&server, &queue, &frame).await;

    a1.account.push_messages_deleted().await.unwrap();
    within!(10, message(&a1, key).await.is_some());
}

/// Spec 6.4: every 5 minutes while the network is on, and not while it is off.
#[tokio::test]
async fn five_minute_poll_while_online() {
    let (a1, b1, _net, _conv, server) = announced().await;
    let queue = active(&a1).await.remove(0);
    periodic(&server, &a1).await;
    let n = server.fetches(&a1).len();
    a1.clock.advance_monotonic_ms(FETCH_EVERY_MS - 2_000);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(server.fetches(&a1).len(), n);
    put(&server, &queue, b"not a frame").await;
    a1.clock.advance_monotonic_ms(3_000);
    within!(10, held(&server, &queue).await == 0);
    assert_eq!(server.fetches(&a1)[n].queues, [queue.queue_id]);

    // Off: the time passes and nothing is asked.
    off(&a1, &[&b1]).await;
    let n = server.fetches(&a1).len();
    a1.clock.advance_monotonic_ms(FETCH_EVERY_MS + 1_000);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(server.fetches(&a1).len(), n);
}

/// Global Constraints: with the mode "never" no fetch goes out, though a `Retiring` queue is
/// still there with something in it: not at a network start, a full sync or the 5 minutes.
#[tokio::test]
async fn never_mode_fetches_nothing() {
    let (a1, b1, _net, conv, server) = announced().await;
    let queue = active(&a1).await.remove(0);
    a1.account.set_queue_mode(QueueMode::Never).await.unwrap();
    settle!(
        [&a1, &b1],
        peer_queue(&b1, conv, &a1).await == Some(None) && active(&a1).await.is_empty()
    );
    quiet(&[&a1]).await;
    assert_eq!(retiring(&a1).await[0].queue_id, queue.queue_id);
    put(&server, &queue, b"not a frame").await;
    let calls = server.calls_from(&a1).len();

    network(&a1, NetworkOp::Stop).await;
    online(&a1).await;
    a1.account.push_messages_deleted().await.unwrap();
    a1.clock.advance_monotonic_ms(FETCH_EVERY_MS + 1_000);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(server.calls_from(&a1).len(), calls);
    assert_eq!(held(&server, &queue).await, 1);
}

/// A1 rotated: its new `Active` queue and the old `Retiring` one. Returns both.
async fn rotated(a1: &Fixture, b1: &Fixture, conv: ConversationId) -> (QueueOwnRow, QueueOwnRow) {
    let old = active(a1).await.remove(0);
    a1.clock
        .set_unix_seconds((old.created_at_ms + ROTATE_MS) / 1_000 + 1);
    settle!([a1, b1], {
        let now = active(a1).await;
        now.len() == 1
            && now[0].queue_id != old.queue_id
            && now[0].server_confirmed
            && peer_queue(b1, conv, a1).await.flatten().map(|q| q.1) == Some(now[0].queue_id)
    });
    let old = retiring(a1).await.remove(0);
    (old, active(a1).await.remove(0))
}

/// Spec 6.3, 6.4: a `Retiring` queue is still fetched, and one `RECV` asks every queue.
#[tokio::test]
async fn retiring_queue_still_fetched_until_delete() {
    let (a1, b1, _net, conv, server) = announced().await;
    let (old, new) = rotated(&a1, &b1, conv).await;
    put(&server, &old, b"in the old queue").await;
    put(&server, &new, b"in the new queue").await;
    let before = server.fetches(&a1).len();
    periodic(&server, &a1).await;
    within!(10, {
        held(&server, &old).await == 0 && held(&server, &new).await == 0
    });
    let first = &fetched_since(&server, &a1, before).await[0];
    assert_eq!(first.queues, [old.queue_id, new.queue_id]);
}

/// One `RECV` answer holds about 1 MiB: what does not fit is fetched with the next page,
/// and a queue the answer had no room for at all (`limited`) is asked first in the next one.
/// Entries that are no frame are taken (acknowledged) and hold nothing up.
#[tokio::test]
async fn pages_past_one_answer_and_limited_queue_asked_first() {
    let (a1, b1, _net, conv, server) = announced().await;
    let (old, new) = rotated(&a1, &b1, conv).await;
    // 14 entries of 60,000 bytes: 13 fill one answer.
    for n in 0..14u8 {
        put(&server, &old, &vec![n; 60_000]).await;
    }
    // 2,000 bytes: more than the room the 13 leave.
    put(&server, &new, &vec![0xAB; 2_000]).await;
    let before = server.fetches(&a1).len();
    periodic(&server, &a1).await;
    within!(10, {
        held(&server, &old).await == 0 && held(&server, &new).await == 0
    });
    let asked = fetched_since(&server, &a1, before).await;
    assert_eq!(asked[0].queues, [old.queue_id, new.queue_id]);
    assert!(matches!(&asked[0].got[0], Got::Entries(e) if e.len() == 13));
    assert_eq!(asked[0].got[1], Got::Limited);
    assert_eq!(asked[1].queues[0], new.queue_id);
    let Got::Entries(last) = &asked[1].got[asked[1]
        .queues
        .iter()
        .position(|q| *q == old.queue_id)
        .unwrap()]
    else {
        panic!("{:?}", asked[1].got);
    };
    let old_now = tables(&a1)
        .await
        .own
        .into_iter()
        .find(|q| q.queue_id == old.queue_id)
        .unwrap();
    assert_eq!(Some(&old_now.last_seq), last.last());
}

/// The server lost a queue (`gone`): A1 makes a fresh one (a new id, never the old one again)
/// and B1 takes its `QueueRef` as newer.
#[tokio::test]
async fn gone_queue_is_replaced_and_peer_takes_the_new_one() {
    let (a1, b1, _net, conv, server) = announced().await;
    let old = active(&a1).await.remove(0);
    assert_eq!(
        server.delete_as(old.queue_id, &old.recipient_secret).await,
        ItemResult::Ok
    );
    periodic(&server, &a1).await;
    settle!([&a1, &b1], {
        let now = active(&a1).await;
        now.len() == 1
            && now[0].queue_id != old.queue_id
            && now[0].server_confirmed
            && peer_queue(&b1, conv, &a1).await.flatten().map(|q| q.1) == Some(now[0].queue_id)
    });
    let row = peer_row(&b1, conv, installation(&a1).await).await.unwrap();
    assert!(!row.dead);
    assert!(row.sent_at_ms > old.created_at_ms);
    let made_old = server
        .sent(&a1, CommandName::Create)
        .into_iter()
        .flat_map(|c| c.queues)
        .filter(|q| *q == old.queue_id)
        .count();
    assert_eq!(made_old, 1);
    // The next fetch finds it gone again and drops it.
    periodic(&server, &a1).await;
    within!(
        10,
        tables(&a1)
            .await
            .own
            .iter()
            .all(|q| q.queue_id != old.queue_id)
    );
}
