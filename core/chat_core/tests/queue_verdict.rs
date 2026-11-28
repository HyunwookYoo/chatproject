#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! The mailbox verdict (M8 spec 5) between two real cores of one account: a mailbox works out
//! hourly whether it qualifies and announces only a change; every device works out whether the
//! account has a qualified mailbox it saw in the last 24 hours, and whether it wants a queue.
include!("support/mailbox_network.rs");

use chat_core::account::{InstallationId, QueueMode, QueueStateView, Verdict, clock::Clock};

const HOUR_MS: u64 = 3_600_000;
const WEEK_MS: u64 = 7 * 24 * HOUR_MS;

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

/// A1 and A2 of account A, with a ready own-device session. The record lasts 3 days, so the
/// devices stay authorized over a 25-hour wall clock step.
async fn pair_of_devices() -> (Fixture, Fixture, Arc<Backend>) {
    let ra = packet(A, &[A1, A2], A, NOW, NOW + 3 * 86_400, NOW * 1_000_000);
    let net = backend(&[(A, ra.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let a2 = device(A, A2, &ra, &net).await;
    link(&a1, &a2).await;
    a1.account.mailbox_sync_now().await.unwrap();
    settle!(
        [&a1, &a2],
        own_session(&a1, &a2).await && own_session(&a2, &a1).await
    );
    (a1, a2, net)
}

async fn queue(f: &Fixture) -> QueueStateView {
    f.account.queue_state().await.unwrap()
}

/// Whether the table of spec 5.3 wants a queue for `view`'s mode and verdict. No queue server
/// is configured here, so none is made: `using_queue` (an `Active` queue exists) stays false;
/// `queue_lifecycle.rs` makes them.
fn wants(view: &QueueStateView) -> bool {
    assert!(!view.using_queue);
    match view.mode {
        QueueMode::Always => true,
        QueueMode::Never => false,
        QueueMode::Auto => view.verdict == Verdict::NoMailbox,
    }
}

/// What `f` recorded of `of`: (mailbox, qualified, last seen).
async fn known(f: &Fixture, of: &Fixture) -> Option<(bool, bool, Option<u64>)> {
    let id = installation(of).await;
    let state = mailbox(f).await;
    let mailbox = state.device_rows.iter().find(|(i, _)| *i == id)?.1;
    let (_, qualified, seen) = *state.seen_rows.iter().find(|(i, ..)| *i == id)?;
    Some((mailbox, qualified, seen))
}

/// `Announce`s `from` sent to `to`, as (mailbox, qualified).
async fn told(from: &Fixture, to: &Fixture) -> Vec<(bool, bool)> {
    let to = installation(to).await;
    from.account
        .mailbox_test_announces()
        .await
        .unwrap()
        .into_iter()
        .filter(|(i, ..)| *i == to)
        .map(|(_, mailbox, qualified)| (mailbox, qualified))
        .collect()
}

async fn verdicts(f: &Fixture) -> u64 {
    f.account.mailbox_test_verdicts().await.unwrap()
}

/// A2 becomes a qualified mailbox (uptime seeded: 950 per mille over a week), and A1 knows.
async fn qualified_mailbox(a1: &Fixture, a2: &Fixture) {
    a2.account.set_mailbox(true).await.unwrap();
    settle!([a1, a2], known(a1, a2).await.is_some_and(|k| k.0));
    a2.account
        .mailbox_test_seed_uptime(950, WEEK_MS)
        .await
        .unwrap();
    let a2_id = installation(a2).await;
    settle!(
        [a1, a2],
        queue(a1).await.verdict
            == Verdict::HasMailbox {
                installation: a2_id
            }
    );
}

/// Spec 5.2: A2 announces `qualified` once when it becomes qualified (worked out at the next
/// poll after the seed), not again while it stays so over the hourly runs, and once more when
/// it drops. While not qualified every `Announce` says so (sent in M7's four-field form,
/// `four_field_announce_while_not_qualified`).
#[tokio::test]
async fn qualified_announced_only_on_change() {
    let (a1, a2, _net) = pair_of_devices().await;
    let a2_id = installation(&a2).await;
    a2.account.set_mailbox(true).await.unwrap();
    settle!([&a1, &a2], known(&a1, &a2).await.is_some_and(|k| k.0));
    assert!(told(&a2, &a1).await.iter().all(|(_, qualified)| !qualified));
    let fresh = queue(&a2).await;
    assert_eq!(
        fresh.verdict,
        Verdict::NoMailbox,
        "a mailbox just set does not qualify"
    );
    assert!(wants(&fresh));

    a2.account
        .mailbox_test_seed_uptime(950, WEEK_MS)
        .await
        .unwrap();
    settle!([&a1, &a2], known(&a1, &a2).await.is_some_and(|k| k.1));
    assert_eq!(told(&a2, &a1).await.last(), Some(&(true, true)));
    let mine = queue(&a2).await;
    assert_eq!(
        (mine.verdict, wants(&mine), mine.uptime_7d_permille),
        (
            Verdict::HasMailbox {
                installation: a2_id
            },
            false,
            950
        ),
        "the qualified mailbox itself wants no queue"
    );
    let theirs = queue(&a1).await;
    assert_eq!(
        (theirs.verdict, wants(&theirs)),
        (
            Verdict::HasMailbox {
                installation: a2_id
            },
            false
        )
    );
    let qualified_at = mailbox(&a2).await.queue_local.2;
    assert!(qualified_at.is_some());

    // Three hourly runs, still qualified: nothing announced, nothing written.
    let sent = told(&a2, &a1).await.len();
    for _ in 0..3 {
        let before = verdicts(&a2).await;
        a2.clock.advance_monotonic_ms(HOUR_MS);
        until!(verdicts(&a2).await > before);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(told(&a2, &a1).await.len(), sent);
    assert!(mailbox(&a2).await.queue_local.1);
    assert_eq!(mailbox(&a2).await.queue_local.2, qualified_at);

    // Down to 500 per mille: worked out at the next poll after the seed, announced once.
    a2.account
        .mailbox_test_seed_uptime(500, WEEK_MS)
        .await
        .unwrap();
    settle!([&a1, &a2], known(&a1, &a2).await.is_some_and(|k| !k.1));
    assert_eq!(told(&a2, &a1).await.len(), sent + 1);
    assert_eq!(told(&a2, &a1).await.last(), Some(&(true, false)));
    assert_eq!(queue(&a1).await.verdict, Verdict::NoMailbox);
    assert!(wants(&queue(&a1).await));
    assert!(
        wants(&queue(&a2).await),
        "an unqualified mailbox uses a queue"
    );
}

/// Review Focus 1: the verdict is worked out hourly and when what it is made of changes, not
/// for every `Announce` that says the same again.
#[tokio::test]
async fn verdict_recomputed_hourly_not_per_announce() {
    let (a1, a2, _net) = pair_of_devices().await;
    qualified_mailbox(&a1, &a2).await;
    let a1_id = installation(&a1).await;
    let say = |qualified| MailboxFrame::Announce {
        mailbox: true,
        handle: None,
        qualified,
    };
    // Whatever is still on its way settles first.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let start = verdicts(&a1).await;
    for _ in 0..20 {
        let reply = a2
            .account
            .mailbox_test_exchange(a1_id, say(true))
            .await
            .unwrap();
        assert!(reply == MailboxReply::Ok);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(verdicts(&a1).await, start, "the same Announce 20 times");

    a1.clock.advance_monotonic_ms(HOUR_MS);
    until!(verdicts(&a1).await > start);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(verdicts(&a1).await, start + 1, "one hourly run");

    // A change is worked out at once, and only once however often it is said.
    for _ in 0..5 {
        a2.account
            .mailbox_test_exchange(a1_id, say(false))
            .await
            .unwrap();
    }
    until!(queue(&a1).await.verdict == Verdict::NoMailbox);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(verdicts(&a1).await, start + 2);
}

/// Spec 5.1 rule 3: A1 sees A2 through its `Announce` and through an authenticated session;
/// unseen for more than 24 hours it no longer counts.
#[tokio::test]
async fn seen_on_auth_and_announce() {
    let (a1, a2, net) = pair_of_devices().await;
    qualified_mailbox(&a1, &a2).await;
    let a2_id = installation(&a2).await;
    let seen = known(&a1, &a2).await.unwrap().2;
    assert_eq!(
        seen,
        Some(a1.clock.wall_ms()),
        "seen when its Announce came"
    );

    // A2 off, A1 25 hours later: unseen for more than 24 hours. A1's end of the session is
    // closed as QUIC's idle timeout would (about 30 s of real time), or the hourly run would
    // count the session as a sighting.
    let a2_dir = stop(a2);
    a1.account.mls_test_close_session(a2_id).await.unwrap();
    until!(
        a1.account
            .mailbox_test_own_sessions()
            .await
            .unwrap()
            .is_empty()
    );
    a1.clock.advance_seconds(25 * 3_600).unwrap();
    until!(queue(&a1).await.verdict == Verdict::NoMailbox);
    assert!(wants(&queue(&a1).await));

    // A2 back but silent (it sends no `Announce` while held): the session alone is a sighting.
    let a2 = start(a2_dir, net.clone()).await;
    a2.account.mailbox_test_hold(true).await.unwrap();
    a2.clock.advance_seconds(25 * 3_600).unwrap();
    // A2 dials.
    link(&a1, &a2).await;
    a2.account.mailbox_sync_now().await.unwrap();
    let at_auth = a1.clock.wall_ms();
    settle!(
        [&a1, &a2],
        known(&a1, &a2).await.unwrap().2 == Some(at_auth)
    );
    until!(
        queue(&a1).await.verdict
            == Verdict::HasMailbox {
                installation: a2_id
            }
    );

    // Its `Announce` once it speaks again is one too.
    a1.clock.advance_seconds(60).unwrap();
    a2.account.mailbox_test_hold(false).await.unwrap();
    let at_announce = a1.clock.wall_ms();
    settle!(
        [&a1, &a2],
        known(&a1, &a2).await.unwrap().2 == Some(at_announce)
    );
}

/// Spec 5.1 rule 3: a session that stays open is a sighting. A2 says nothing new while A1's
/// wall clock steps 25 hours (its scheduler clock one); A1's hourly run sees A2 over the open
/// session, so the qualified mailbox still counts.
#[tokio::test]
async fn open_session_keeps_the_mailbox_seen() {
    let (a1, a2, _net) = pair_of_devices().await;
    qualified_mailbox(&a1, &a2).await;
    let a2_id = installation(&a2).await;
    // Whatever is still on its way settles first.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let sent = told(&a2, &a1).await.len();
    let start = verdicts(&a1).await;

    a1.clock
        .set_unix_seconds(a1.clock.unix_seconds() + 25 * 3_600);
    let stepped = a1.clock.wall_ms();
    a1.clock.advance_monotonic_ms(HOUR_MS);
    until!(verdicts(&a1).await > start);
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(own_session(&a1, &a2).await, "the same session, still open");
    assert_eq!(told(&a2, &a1).await.len(), sent, "no new Announce");
    assert_eq!(
        queue(&a1).await.verdict,
        Verdict::HasMailbox {
            installation: a2_id
        }
    );
    assert_eq!(known(&a1, &a2).await.unwrap().2, Some(stepped));
}

/// The setting, the qualification and the verdict survive a restart: worked out again from what
/// is stored, the same (`qualified_at_ms` does not move), before A2 is reached again.
#[tokio::test]
async fn queue_state_survives_restart() {
    let (a1, a2, net) = pair_of_devices().await;
    qualified_mailbox(&a1, &a2).await;
    let a2_id = installation(&a2).await;
    let has = Verdict::HasMailbox {
        installation: a2_id,
    };
    settle!([&a1, &a2], known(&a1, &a2).await.is_some_and(|k| k.1));
    a1.account.set_queue_mode(QueueMode::Always).await.unwrap();
    a2.account.set_queue_mode(QueueMode::Never).await.unwrap();
    let a1_before = queue(&a1).await;
    assert_eq!(
        (a1_before.mode, a1_before.verdict, wants(&a1_before)),
        (QueueMode::Always, has, true)
    );
    let a2_local = mailbox(&a2).await.queue_local;

    let (a1_dir, a2_dir) = (stop(a1), stop(a2));
    let a1 = start(a1_dir, net.clone()).await;
    let a2 = start(a2_dir, net.clone()).await;
    assert_eq!(queue(&a1).await, a1_before);
    let mine = queue(&a2).await;
    assert_eq!(
        (mine.mode, mine.verdict, wants(&mine)),
        (QueueMode::Never, has, false)
    );
    let after = mailbox(&a2).await.queue_local;
    assert_eq!(
        (after.0, after.1, after.2),
        (a2_local.0, a2_local.1, a2_local.2),
        "qualified, and since the same time"
    );
    a1.account.set_queue_mode(QueueMode::Auto).await.unwrap();
    assert!(!wants(&queue(&a1).await));
}
