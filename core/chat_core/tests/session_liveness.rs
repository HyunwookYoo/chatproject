#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Session liveness (M6 Rulings 21 and 23): two devices that dial each other at once keep one
//! session, the same connection on both sides; a receipt whose preferred device stopped
//! answering goes to another device of that account instead of waiting for the dead session.
include!("support/mailbox_network.rs");

use chat_core::account::InstallationId;

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

/// How many connections `f` finished with so far, and for each live ready session it holds
/// with `with`: whether `f` dialed it.
async fn sessions(f: &Fixture, with: InstallationId) -> (u64, Vec<bool>) {
    let (settled, sessions) = f.account.mls_test_sessions().await.unwrap();
    (
        settled,
        sessions
            .into_iter()
            .filter(|(i, _)| *i == with)
            .map(|(_, dialed)| dialed)
            .collect(),
    )
}

/// `rounds` times: `x` and `y` drop their session, then dial each other at once (both dials
/// spawned before either connects). Each time both end with one session, the same connection
/// (dialed by one, accepted by the other), and it carries a request both ways.
async fn race(x: &Fixture, y: &Fixture, conv: ConversationId, rounds: usize) {
    let (x_id, y_id) = (installation(x).await, installation(y).await);
    for round in 0..rounds {
        x.account.mls_test_close_session(y_id).await.unwrap();
        until!(sessions(x, y_id).await.1.is_empty() && sessions(y, x_id).await.1.is_empty());
        let (x_before, y_before) = (sessions(x, y_id).await.0, sessions(y, x_id).await.0);
        x.account.mls_test_hold_dials(true).await.unwrap();
        y.account.mls_test_hold_dials(true).await.unwrap();
        assert!(x.account.mls_test_dial(y.peer, y_id).await.unwrap());
        assert!(y.account.mls_test_dial(x.peer, x_id).await.unwrap());
        x.account.mls_test_hold_dials(false).await.unwrap();
        y.account.mls_test_hold_dials(false).await.unwrap();
        // Each side is done with both connections: its own dial and the one it accepted.
        until!(
            sessions(x, y_id).await.0 >= x_before + 2 && sessions(y, x_id).await.0 >= y_before + 2
        );
        // The losing connection's close may still be on its way; no clock moves, nothing
        // dials again.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let (xs, ys) = (sessions(x, y_id).await.1, sessions(y, x_id).await.1);
            if xs.len() == 1 && ys.len() == 1 && xs[0] != ys[0] {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "round {round}: one shared session, got {xs:?} and {ys:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let query = MlsFrame::GroupQuery { conversation: conv };
        x.account
            .mls_test_exchange(y_id, query.clone())
            .await
            .unwrap();
        y.account.mls_test_exchange(x_id, query).await.unwrap();
    }
}

/// Ruling 21: my two devices dial each other at the same moment (both apps came to the
/// foreground): they keep one shared session, with no further dial.
#[tokio::test]
async fn simultaneous_dials_keep_one_shared_session() {
    let (mut a, _b1, _net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    race(&a1, &a2, conv, 20).await;
}

/// Ruling 21, between accounts: A1 and B1 dial each other at once (a member dial landing in the
/// other's handshake) and keep one shared session.
#[tokio::test]
async fn simultaneous_peer_dials_keep_one_shared_session() {
    let (mut a, b1, _net, conv) = accounts(&[A1], &[B1]).await;
    let a1 = a.pop().unwrap();
    race(&a1, &b1, conv, 20).await;
}

/// Ruling 21 without breaking a restart: the device with the smaller installation id dialed its
/// session with my other device, which then stops (that session stays ready here, dead) and
/// comes back, dialing it. Its dial is not refused for the dead session: both ends talk at once.
#[tokio::test]
async fn restarted_device_dials_past_a_dead_session() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let y = a.pop().unwrap();
    let x = a.pop().unwrap();
    let (v, r) = if installation(&x).await < installation(&y).await {
        (x, y)
    } else {
        (y, x)
    };
    let (v_id, r_id) = (installation(&v).await, installation(&r).await);
    if sessions(&v, r_id).await.1 != [true] {
        v.account.mls_test_close_session(r_id).await.unwrap();
        until!(sessions(&v, r_id).await.1.is_empty() && sessions(&r, v_id).await.1.is_empty());
        assert!(v.account.mls_test_dial(r.peer, r_id).await.unwrap());
        until!(sessions(&v, r_id).await.1 == [true] && sessions(&r, v_id).await.1 == [false]);
    }
    let dir = stop(r);
    assert_eq!(
        sessions(&v, r_id).await.1,
        [true],
        "the dead session looks ready"
    );
    let r = on(dir, &net, &[&v, &b1]).await;
    until!(sessions(&r, v_id).await.1 == [true]);
    let query = MlsFrame::GroupQuery { conversation: conv };
    v.account
        .mls_test_exchange(r_id, query.clone())
        .await
        .unwrap();
    r.account.mls_test_exchange(v_id, query).await.unwrap();
}

/// Ruling 23: like `peer_read_reaches_sender_while_sender_is_off`, but nobody closes B1's
/// session with A1: A1's core stops and B1 still holds that session as ready. B1 reads A1's
/// message; after one unanswered receipt attempt to A1 the receipt goes to A2, which shows Read
/// long before the dead session would close.
#[tokio::test]
async fn peer_read_reaches_other_device_when_original_session_is_stale() {
    let (mut a, b1, _net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let key = delivered(&a1, &b1, &[&a2], 1).await;
    settle!([&a1, &a2, &b1], message(&a2, key).await.is_some());
    let a1_id = installation(&a1).await;
    let _dir = stop(a1);
    assert_eq!(
        sessions(&b1, a1_id).await.1.len(),
        1,
        "B1 still holds its session with A1"
    );
    b1.account
        .messages()
        .mark_read(conv, vec![key])
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !(delivery(&a2, key).await == DeliveryState::Read && receipt_jobs(&b1).await == 0) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "A2 did not show Read within 20 s"
        );
        for d in [&a2, &b1] {
            d.clock.advance_monotonic_ms(1_000);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
