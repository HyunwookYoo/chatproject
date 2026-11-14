#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! The mailbox role (M6 spec 8.4, 7): a device set as a mailbox tells my other devices (never a
//! contact) with `Announce`, on the change and on every own-device session that becomes ready;
//! they record it, hand off to it and dial it first. A mailbox caps its own outbox back-off at
//! 30 s, records its uptime over a 7-day window (no judgement in M6), and a device with a ready
//! session to one of my mailboxes reports `mailbox_ok`.
include!("support/mailbox_network.rs");

use chat_core::account::{InstallationId, clock::Clock};
use chat_core::storage_fixture::HandoffEntry;

const DAY: u64 = 86_400;

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

/// What `f` recorded from `of`'s announcements: `None` until `of` announced.
async fn announced(f: &Fixture, of: &Fixture) -> Option<bool> {
    let id = installation(of).await;
    mailbox(f)
        .await
        .device_rows
        .into_iter()
        .find(|(i, _)| *i == id)
        .map(|(_, mailbox)| mailbox)
}

/// Every `Announce` `f` sent: to which installation, and what it said.
async fn announces(f: &Fixture) -> Vec<(InstallationId, bool)> {
    f.account.mailbox_test_announces().await.unwrap()
}

/// `mailbox_ok` of the latest connection state `f` published.
fn mailbox_ok(f: &Fixture) -> bool {
    f.events
        .0
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match event {
            CoreEvent::ConnectionState(snapshot) => Some(snapshot.mailbox_ok),
            _ => None,
        })
        .unwrap_or(false)
}

fn installation_of(seed: u8) -> InstallationId {
    derive_device(Secret32::from([seed; 32]))
        .unwrap()
        .installation_id()
}

/// Account A's record lists `listed`; of them `running` run, each pair of those with a ready
/// own-device session. No other account.
async fn own_devices(listed: &[u8], running: &[u8]) -> (Vec<Fixture>, Arc<Backend>) {
    let ra = record(A, listed, 0);
    let net = backend(&[(A, ra.clone())]);
    let mut a = Vec::new();
    for seed in running {
        a.push(device(A, *seed, &ra, &net).await);
    }
    for (i, x) in a.iter().enumerate() {
        for y in &a[i + 1..] {
            link(x, y).await;
        }
    }
    // One device at a time: two devices dialing each other at once end up without a session.
    for x in &a {
        x.account.mailbox_sync_now().await.unwrap();
        settle!(a.iter(), {
            let mut ok = true;
            for y in &a {
                ok = ok && (std::ptr::eq(x, y) || own_session(x, y).await);
            }
            ok
        });
    }
    (a, net)
}

/// Spec 8.4: A2 announces its mailbox role to A1 only (B1, a contact with a session, gets no
/// `Announce` and records nothing); a contact's `Announce` is refused. A1 hands off to A2 while
/// it is a mailbox and stops once A2 turns the role off; A2 keeps trying what it was handed, and
/// takes no new handoff.
#[tokio::test]
async fn announce_reaches_own_devices_only() {
    let (mut a, b1, _net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let (a1_id, b1_id) = (installation(&a1).await, installation(&b1).await);
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    // B1 never acks, so what A2 is handed stays there.
    b1.account.message_test_m4_peer(true).await.unwrap();
    let key = send(&a1, &b1, 1, "handed to the mailbox").await.key;

    a2.account.set_mailbox(true).await.unwrap();
    assert!(a2.account.mailbox_state().await.unwrap().enabled);
    settle!([&a1, &a2, &b1], announced(&a1, &a2).await == Some(true));
    assert!(announces(&a2).await.contains(&(a1_id, true)));
    for f in [&a1, &a2] {
        assert!(
            announces(f).await.iter().all(|(to, _)| *to != b1_id),
            "no Announce to a contact"
        );
    }
    assert!(mailbox(&b1).await.device_rows.is_empty());
    // A contact announcing itself is refused, and nothing is recorded.
    let reply = b1
        .account
        .mailbox_test_exchange(a1_id, MailboxFrame::Announce { mailbox: true })
        .await
        .unwrap();
    assert!(reply == MailboxReply::Refused);
    assert_eq!(announced(&a1, &b1).await, None);

    assert_eq!(a1.account.mailbox_background().await.unwrap(), 1);
    settle!([&a1, &a2, &b1], mailbox(&a2).await.handoff_rows.len() == 1);

    a2.account.set_mailbox(false).await.unwrap();
    settle!([&a1, &a2, &b1], announced(&a1, &a2).await == Some(false));
    assert_eq!(
        a1.account.mailbox_background().await.unwrap(),
        0,
        "no handoff to a device that is no longer a mailbox"
    );
    // What A2 already took it keeps trying.
    let tried = a2.account.mailbox_test_handoff_sends().await.unwrap().len();
    settle!(
        [&a2],
        a2.account.mailbox_test_handoff_sends().await.unwrap().len() > tried
    );
    assert_eq!(mailbox(&a2).await.handoff_rows.len(), 1);
    // A new handoff is not taken: `Busy` (the device is still mine, so not `Refused`).
    let reply = a1
        .account
        .mailbox_test_exchange(
            installation(&a2).await,
            MailboxFrame::Handoff {
                entries: vec![HandoffEntry {
                    conversation: conv,
                    peer: b1.peer,
                    msg_id: MessageId([2; 16]),
                    ciphertext: vec![1],
                }],
            },
        )
        .await
        .unwrap();
    assert!(reply == MailboxReply::Busy);
    assert_eq!(mailbox(&a2).await.handoff_rows.len(), 1);
    assert_eq!(mailbox(&a2).await.handoff_rows[0].msg_id, key.id);
}

/// A2 becomes a mailbox while A1 is off. A1 comes back: once their session is ready A2 tells
/// it, and A1 records A2 as a mailbox.
#[tokio::test]
async fn new_device_learns_mailbox_on_session_ready() {
    let (mut a, net) = own_devices(&[A1, A2], &[A1, A2]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    // Each side told the other its state once the session was ready.
    settle!(
        [&a1, &a2],
        announced(&a1, &a2).await == Some(false) && announced(&a2, &a1).await == Some(false)
    );
    let a1_id = installation(&a1).await;
    let a1_dir = stop(a1);
    a2.account.set_mailbox(true).await.unwrap();
    let a1 = on(a1_dir, &net, &[&a2]).await;
    settle!([&a1, &a2], announced(&a1, &a2).await == Some(true));
    assert!(announces(&a2).await.contains(&(a1_id, true)));
}

/// Ruling 7: a mailbox backs its own outbox off 1, 2, 4, 8, 16, 30, 30 s for a peer that is off;
/// once the role is off the 300 s cap is back.
#[tokio::test]
async fn mailbox_enabled_caps_outbox_backoff_at_30s() {
    let (mut a, b1, net, _conv) = accounts(&[A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    a2.account.set_mailbox(true).await.unwrap();
    let b_peer = b1.peer;
    let _b_dir = stop(b1);
    // Restarted, A2 no longer knows where B1 was: each attempt fails at once.
    let a2 = start(stop(a2), net.clone()).await;
    let key = a2
        .account
        .messages()
        .send_text(b_peer, MessageId([1; 16]), "to a peer that is off".into())
        .await
        .unwrap()
        .key;
    let retry = |f: &Fixture| {
        let observer = f.account.storage_test_fixture();
        async move { observer.message_facts(key).await.unwrap().retry.unwrap() }
    };
    let mut delays = Vec::new();
    for attempt in 1..=7 {
        until!(retry(&a2).await.0 == attempt);
        let (_, delay, _) = retry(&a2).await;
        delays.push(delay / 1000);
        if attempt < 7 {
            a2.clock.advance_monotonic_ms(delay);
        }
    }
    assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30]);

    a2.account.set_mailbox(false).await.unwrap();
    a2.clock.advance_monotonic_ms(30_000);
    until!(retry(&a2).await.0 == 8);
    assert_eq!(retry(&a2).await.1, 128_000, "the 30 s cap is gone");
    a2.account
        .storage_test_fixture()
        .saturate_message_attempts(key, false)
        .await
        .unwrap();
    let (attempts, _, due) = retry(&a2).await;
    a2.clock.advance_monotonic_ms(128_000);
    until!(retry(&a2).await.2 != due);
    assert_eq!(
        (retry(&a2).await.0, retry(&a2).await.1),
        (attempts, 300_000),
        "the 300 s cap"
    );
}

/// Spec 8.4: a mailbox records its uptime (no judgement in M6). Over 8 days A1 is up for an
/// hour, sleeps, then is up for the last 3.5 days: the 7-day window holds 3.5 days (500 per
/// mille), and the hour from 8 days ago is gone. Nothing is recorded while the role is off.
#[tokio::test]
async fn uptime_window_is_seven_days() {
    let (mut a, _net) = own_devices(&[A1], &[A1]).await;
    let a1 = a.pop().unwrap();
    let state = a1.account.mailbox_state().await.unwrap();
    assert_eq!(
        (
            state.enabled,
            state.first_link_shown,
            state.uptime_7d_permille
        ),
        (false, false, 0)
    );
    a1.account.mark_first_link_shown().await.unwrap();
    assert!(a1.account.mailbox_state().await.unwrap().first_link_shown);
    a1.account.set_mailbox(true).await.unwrap();
    up(&a1, 3_600).await;
    assert_eq!(mailbox(&a1).await.uptime.len(), 1);
    // Asleep: the first tick on waking, 4.5 days after the start, opens a new period.
    a1.clock.advance_seconds(DAY * 9 / 2 - 3_600).unwrap();
    let woke = a1.clock.wall_ms();
    until!(mailbox(&a1).await.uptime.last().unwrap().0 == woke);
    up(&a1, DAY * 7 / 2).await;
    let end = a1.clock.wall_ms();
    assert_eq!(end - woke, DAY * 7 / 2 * 1000);
    assert_eq!(
        mailbox(&a1).await.uptime,
        vec![(woke, end)],
        "one period, and the one from 8 days ago is gone"
    );
    let permille = a1.account.mailbox_state().await.unwrap().uptime_7d_permille;
    assert!((499..=501).contains(&permille), "{permille}");

    a1.account.set_mailbox(false).await.unwrap();
    for _ in 0..5 {
        a1.clock.advance_seconds(60).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert_eq!(mailbox(&a1).await.uptime, vec![(woke, end)], "off: not up");
}

/// Keeps `f` up for `seconds` in 120 s steps (the longest gap one period spans), each step
/// waiting for the tick at the new time.
async fn up(f: &Fixture, seconds: u64) {
    for _ in 0..seconds / 120 {
        f.clock.advance_seconds(120).unwrap();
        let now = f.clock.wall_ms();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while mailbox(f).await.uptime.last().is_none_or(|p| p.1 != now) {
            assert!(tokio::time::Instant::now() < deadline, "no tick at {now}");
            tokio::task::yield_now().await;
        }
    }
}

/// `mailbox_ok`: A1 has a ready session with a device announced as my mailbox. It follows A2
/// turning the role on, going off, coming back, and turning the role off; A2 itself (my only
/// mailbox) never reports it.
#[tokio::test]
async fn mailbox_ok_follows_a_ready_mailbox_session() {
    let (mut a, net) = own_devices(&[A1, A2], &[A1, A2]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    settle!([&a1, &a2], announced(&a1, &a2).await == Some(false));
    assert!(!mailbox_ok(&a1));
    a2.account.set_mailbox(true).await.unwrap();
    settle!([&a1, &a2], mailbox_ok(&a1));
    let a2_dir = stop(a2);
    settle!([&a1], !mailbox_ok(&a1));
    let a2 = on(a2_dir, &net, &[&a1]).await;
    settle!([&a1, &a2], mailbox_ok(&a1));
    assert!(!mailbox_ok(&a2));
    a2.account.set_mailbox(false).await.unwrap();
    settle!([&a1, &a2], !mailbox_ok(&a1));
}

/// Spec 8.4: the app comes to the foreground and dials my devices without a session, the
/// device announced as a mailbox first (the record lists it last).
#[tokio::test]
async fn sync_now_dials_announced_mailbox_first() {
    let (first, last) = if installation_of(A2) < installation_of(A3) {
        (A2, A3)
    } else {
        (A3, A2)
    };
    let (mut a, _net) = own_devices(&[A1, A2, A3], &[A1, last]).await;
    let mailbox_device = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    mailbox_device.account.set_mailbox(true).await.unwrap();
    settle!(
        [&a1, &mailbox_device],
        announced(&a1, &mailbox_device).await == Some(true) && mailbox_ok(&a1)
    );
    let _dir = stop(mailbox_device);
    settle!([&a1], !mailbox_ok(&a1));
    a1.account.mailbox_sync_now().await.unwrap();
    assert_eq!(
        a1.account.mailbox_test_sync_dials().await.unwrap(),
        [installation_of(last), installation_of(first)]
    );
}
