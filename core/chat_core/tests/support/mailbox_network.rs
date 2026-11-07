// Own-device sync between real cores (M6): the MLS messaging harness, with devices that go off
// and come back, and the notifications they publish.
include!("mls_messaging.rs");

use chat_core::NotifyItem;
use chat_core::storage_fixture::{MailboxFrame, MailboxReply, MailboxState, MlsFrame};

/// A third installation of account A.
const A3: u8 = 15;

/// Every `CoreEvent::Notify` item `f` published so far, in order.
fn notify_events(f: &Fixture) -> Vec<NotifyItem> {
    f.events
        .0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            CoreEvent::Notify { items } => Some(items.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// How many `CoreEvent::Notify` events `f` published so far.
fn notify_count(f: &Fixture) -> usize {
    f.events
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|event| matches!(event, CoreEvent::Notify { .. }))
        .count()
}

/// A's devices `seeds` and B1 (B's record lists `b_devices`; only B1 runs). Every device may
/// dial every other; A's devices and B1 are contacts both ways; B1 makes the group with all of
/// A's devices; then A's devices connect to each other (own-device sessions).
async fn accounts(
    seeds: &[u8],
    b_devices: &[u8],
) -> (Vec<Fixture>, Fixture, Arc<Backend>, ConversationId) {
    let (ra, rb) = (record(A, seeds, 0), record(B, b_devices, 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let mut a = Vec::new();
    for seed in seeds {
        a.push(device(A, *seed, &ra, &net).await);
    }
    let b1 = device(B, B1, &rb, &net).await;
    for (i, x) in a.iter().enumerate() {
        link(x, &b1).await;
        for y in &a[i + 1..] {
            link(x, y).await;
        }
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
    // One device at a time: two devices dialing each other at once end up without a session.
    for x in &a {
        x.account.mailbox_sync_now().await.unwrap();
        settle!(all.iter().copied(), {
            let mut ok = true;
            for y in &a {
                ok = ok && (std::ptr::eq(x, y) || own_session(x, y).await);
            }
            ok
        });
    }
    settle!(all.iter().copied(), {
        let mut ok = true;
        for x in &a {
            for y in &a {
                ok = ok
                    && (std::ptr::eq(x, y) || (own_session(x, y).await && caught_up(x, y).await));
            }
        }
        ok
    });
    (a, b1, net, conv)
}

/// Whether `x`'s latest word on a session with `y` is that it is ready.
async fn own_session(x: &Fixture, y: &Fixture) -> bool {
    let y = installation(y).await.to_string();
    x.events
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
            } if *installation_id == y => Some(*ready),
            _ => None,
        })
        .unwrap_or(false)
}

/// Starts a stopped device of A again, lets it reach `peers`, and syncs (the app came to the
/// foreground).
async fn on(dir: tempfile::TempDir, net: &Arc<Backend>, peers: &[&Fixture]) -> Fixture {
    let f = start(dir, net.clone()).await;
    for peer in peers {
        link(&f, peer).await;
    }
    f.account.mailbox_sync_now().await.unwrap();
    f
}

/// The texts `from` sent that `f` stores in the conversation, in the order it shows them.
async fn texts_from(f: &Fixture, conv: ConversationId, from: PeerId) -> Vec<MessageView> {
    let mut texts: Vec<MessageView> = page(f, conv)
        .await
        .into_iter()
        .filter(|m| m.key.sender == from)
        .collect();
    texts.sort_by_key(|m| (m.lamport, m.key.installation, m.key.id));
    texts
}

/// The keys `f` shows in the conversation, in its order.
async fn shown(f: &Fixture, conv: ConversationId) -> Vec<MessageKey> {
    let mut all = page(f, conv).await;
    all.sort_by_key(|m| (m.lamport, m.key.installation, m.key.id));
    all.into_iter().map(|m| m.key).collect()
}

async fn mailbox(f: &Fixture) -> MailboxState {
    f.account
        .storage_test_fixture()
        .mailbox_state()
        .await
        .unwrap()
}

/// Whether `puller` has processed everything `holder` logs now.
async fn caught_up(puller: &Fixture, holder: &Fixture) -> bool {
    let id = installation(holder).await;
    let since = mailbox(puller)
        .await
        .pulls
        .iter()
        .find(|(i, _)| *i == id)
        .map_or(0, |(_, since)| *since);
    mailbox(holder).await.entries.iter().all(|e| e.seq < since)
}

/// Lets the devices run (clocks moving) for a while, for anything still on its way.
async fn quiet(devices: &[&Fixture]) {
    for _ in 0..30 {
        for d in devices {
            d.clock.advance_monotonic_ms(1_000);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `from` sends a text to `to`'s account and it is delivered.
async fn delivered(from: &Fixture, to: &Fixture, others: &[&Fixture], n: u8) -> MessageKey {
    let key = send(from, to, n, &format!("text {n}")).await.key;
    let mut devices = vec![from, to];
    devices.extend_from_slice(others);
    settle!(
        devices.iter().copied(),
        delivery(from, key).await == DeliveryState::Delivered
    );
    key
}
