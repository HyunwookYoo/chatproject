#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Receiving pushes (M7 spec 5.3, 5.4, 5.6, 7): a `cipher` push is opened like a frame that came
//! directly and alerts once with the contact's alias and the start of the text; a copy that also
//! comes directly is taken once and alerts once; a text read already, or a tag that is not a
//! current handle, shows nothing; what cannot be opened alerts as "새 메시지". A `wake` alerts
//! first and fetches within the deadline, a read clear syncs with my devices, FCM's deleted
//! messages sync at the next network start, and the iOS inbox is taken without notifications.
include!("support/push_network.rs");

use chat_core::message::preview_text;
use chat_core::push::{PushItemKind, PushOutcome, ReceivedPush, Tag};
use chat_core::storage_fixture::strip_group_id;

const WAIT: Duration = Duration::from_secs(20);

async fn send_to(from: &Fixture, peer: PeerId, n: u8, body: &str) -> MessageKey {
    from.account
        .messages()
        .send_text(peer, MessageId([n; 16]), body.into())
        .await
        .unwrap()
        .key
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

async fn network(f: &Fixture, op: NetworkOp) {
    f.slot.network().unwrap().execute(op).await.unwrap();
}

async fn network_state(f: &Fixture) -> NetworkState {
    let NetworkReply::Info(info) = f
        .slot
        .network()
        .unwrap()
        .execute(NetworkOp::Info)
        .await
        .unwrap()
    else {
        panic!("no network info");
    };
    info.state
}

/// The conversation's unread count on `f`.
async fn unread(f: &Fixture, peer: PeerId) -> u64 {
    f.account.messages().open_dm(peer).await.unwrap().unread
}

/// The `cipher` and `wake` items the proxy was asked to push, with the tag each handle holds.
fn text_items(proxy: &FakePushProxy) -> Vec<(Tag, PushItemKind)> {
    proxy
        .push_requests()
        .into_iter()
        .flat_map(|r| r.items)
        .filter(|i| matches!(i.kind, PushItemKind::Cipher { .. } | PushItemKind::Wake))
        .map(|i| {
            let handle = BASE64URL_NOPAD.decode(i.handle.as_bytes()).unwrap();
            (proxy.open(&handle).tag, i.kind)
        })
        .collect()
}

fn cipher(kind: &PushItemKind) -> Vec<u8> {
    let PushItemKind::Cipher { e } = kind else {
        panic!("not a cipher item");
    };
    BASE64URL_NOPAD.decode(e.as_bytes()).unwrap()
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

/// The items `f` published since the first `from` of them.
fn notified_since(f: &Fixture, from: usize) -> Vec<NotifyItem> {
    notify_events(f)[from..].to_vec()
}

/// Items of the conversation that alerted (`new_messages > 0`).
fn alerts(f: &Fixture, conv: ConversationId) -> usize {
    notify_events(f)
        .iter()
        .filter(|i| i.conversation == conv && i.new_messages > 0)
        .count()
}

/// How many times `f` published that its text `key` was delivered.
fn delivered_events(f: &Fixture, key: MessageKey) -> usize {
    f.events
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|event| {
            matches!(event, CoreEvent::MessageStateChanged { key: k, state, .. }
                if *k == key && *state == DeliveryState::Delivered)
        })
        .count()
}

/// A1 (push on, no platform) and B1, an Android device that announced its handle to A1 and
/// whose network went off: A1's texts to it are pushed. B1's core keeps running.
async fn offline_receiver() -> (
    Fixture,
    Fixture,
    ConversationId,
    Arc<Backend>,
    Arc<FakePushProxy>,
) {
    let (a1, b1, net, conv) = setup(&[B1]).await;
    grouped(&a1, &b1, conv).await;
    let proxy = proxy(&net);
    let b1_id = installation(&b1).await;
    android(&b1, "token-b1").await;
    settle!(
        [&a1, &b1],
        peer_handle(&a1, conv, b1_id).await.flatten().is_some()
            && push_state(&b1).await.deliveries.is_empty()
    );
    a1.account.push_configure(config(None)).unwrap();
    network(&b1, NetworkOp::Stop).await;
    settle!([&a1, &b1], !ready_with(&a1, b1_id));
    (a1, b1, conv, net, proxy)
}

/// B1's network comes back and each side may dial the other again.
async fn back_online(b1: &Fixture, others: &[&Fixture]) {
    online(b1).await;
    for other in others {
        link(b1, other).await;
    }
}

/// `f` sends texts `n` with `bodies` and the proxy is asked to push each: their keys and items.
async fn pushed(
    from: &Fixture,
    to: PeerId,
    proxy: &FakePushProxy,
    bodies: &[&str],
) -> Vec<(MessageKey, Tag, PushItemKind)> {
    let mut out = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        let before = text_items(proxy).len();
        let key = send_to(from, to, 1 + i as u8, body).await;
        settle!([from], text_items(proxy).len() > before);
        let (tag, kind) = text_items(proxy).remove(before);
        out.push((key, tag, kind));
    }
    out
}

#[tokio::test]
async fn cipher_push_stores_and_notifies_with_alias_and_preview() {
    let (a1, b1, conv, _net, proxy) = offline_receiver().await;
    let first = "안녕하세요, 푸시로 온 첫 메시지입니다. 잠금 화면에서 보여야 해요.";
    let second = "두 번째 메시지";
    let sent = pushed(&a1, b1.peer, &proxy, &[first, second]).await;
    let alias = "remote".to_string();

    let before = notify_events(&b1).len();
    let (key, tag, kind) = &sent[0];
    let push = ReceivedPush::Cipher {
        tag: *tag,
        ciphertext: cipher(kind),
    };
    assert_eq!(
        b1.account.push_received(push, WAIT).await.unwrap(),
        PushOutcome::Notified
    );
    assert_eq!(message(&b1, *key).await.unwrap().body, first);
    let preview = preview_text(first, 0);
    assert!(preview.len() <= 48 && first.starts_with(&preview) && preview.len() < first.len());
    assert_eq!(
        notified_since(&b1, before),
        [NotifyItem {
            conversation: conv,
            peer: a1.peer,
            new_messages: 1,
            unread: 1,
            title: alias.clone(),
            preview: Some(preview),
        }]
    );

    // The second text: its start, and the other unread one counted.
    let (key, tag, kind) = &sent[1];
    let push = ReceivedPush::Cipher {
        tag: *tag,
        ciphertext: cipher(kind),
    };
    assert_eq!(
        b1.account.push_received(push, WAIT).await.unwrap(),
        PushOutcome::Notified
    );
    assert!(message(&b1, *key).await.is_some());
    let last = notify_events(&b1).pop().unwrap();
    assert_eq!(last.title, alias);
    assert_eq!((last.new_messages, last.unread), (1, 2));
    assert_eq!(last.preview.as_deref(), Some("두 번째 메시지 외 1개"));
    // No answer went back: the sender learns of the delivery from its next direct attempt.
    assert_ne!(delivery(&a1, *key).await, DeliveryState::Delivered);
}

/// Review Focus 2: the same ciphertext by push and directly, in either order: one row, one
/// alert, and the sender sees it delivered once.
#[tokio::test]
async fn push_and_direct_same_ciphertext_alert_once() {
    let (a1, b1, conv, _net, proxy) = offline_receiver().await;

    // The push first, then the sender's next attempt reaches B1 directly.
    let sent = pushed(&a1, b1.peer, &proxy, &["push first"]).await;
    let (key, tag, kind) = &sent[0];
    let push = ReceivedPush::Cipher {
        tag: *tag,
        ciphertext: cipher(kind),
    };
    assert_eq!(
        b1.account.push_received(push.clone(), WAIT).await.unwrap(),
        PushOutcome::Notified
    );
    back_online(&b1, &[&a1]).await;
    settle!(
        [&a1, &b1],
        delivery(&a1, *key).await == DeliveryState::Delivered
    );
    quiet(&[&a1, &b1]).await;
    let rows =
        |page: Vec<MessageView>, key: MessageKey| page.iter().filter(|m| m.key == key).count();
    assert_eq!(rows(page(&b1, conv).await, *key), 1);
    assert_eq!(alerts(&b1, conv), 1);
    assert_eq!(delivered_events(&a1, *key), 1);
    // The same push once more: already shown, never a second alert.
    assert_eq!(
        b1.account.push_received(push, WAIT).await.unwrap(),
        PushOutcome::Updated
    );
    assert_eq!(alerts(&b1, conv), 1);

    // Directly first, then the push of the same ciphertext.
    let direct = send_to(&a1, b1.peer, 2, "direct first").await;
    settle!(
        [&a1, &b1],
        delivery(&a1, direct).await == DeliveryState::Delivered
    );
    assert_eq!(alerts(&b1, conv), 2);
    let sealed = app_ciphertexts(&a1).await.pop().unwrap();
    let push = ReceivedPush::Cipher {
        tag: own_tag(&b1, conv).await,
        ciphertext: strip_group_id(&sealed).unwrap(),
    };
    let before = notify_events(&b1).len();
    assert_eq!(
        b1.account.push_received(push, WAIT).await.unwrap(),
        PushOutcome::Updated
    );
    quiet(&[&a1, &b1]).await;
    assert_eq!(rows(page(&b1, conv).await, direct), 1);
    assert_eq!(alerts(&b1, conv), 2);
    assert_eq!(delivered_events(&a1, direct), 1);
    let updated = notified_since(&b1, before);
    assert_eq!(updated.len(), 1);
    assert_eq!((updated[0].new_messages, updated[0].unread), (0, 2));
    assert_eq!(updated[0].preview.as_deref(), Some("direct first 외 1개"));
}

/// The text was read on A2 and A1 synced that before the push came: nothing is shown.
#[tokio::test]
async fn already_read_on_other_device_push_shows_nothing() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let proxy = proxy(&net);
    let a1_id = installation(&a1).await;
    android(&a1, "token-a1").await;
    settle!(
        [&a1, &a2, &b1],
        peer_handle(&b1, conv, a1_id).await.flatten().is_some()
    );
    b1.account.push_configure(config(None)).unwrap();
    network(&a1, NetworkOp::Stop).await;
    settle!(
        [&a1, &a2, &b1],
        !ready_with(&b1, a1_id) && !ready_with(&a2, a1_id)
    );

    let sent = pushed(&b1, a1.peer, &proxy, &["read on the pc"]).await;
    let (key, tag, kind) = &sent[0];
    settle!([&a2, &b1], message(&a2, *key).await.is_some());
    a2.account
        .messages()
        .mark_read(conv, vec![*key])
        .await
        .unwrap();
    // A1 comes back and syncs with A2: the text and its read.
    back_online(&a1, &[&a2, &b1]).await;
    a1.account.mailbox_sync_now().await.unwrap();
    settle!(
        [&a1, &a2, &b1],
        message(&a1, *key).await.is_some() && unread(&a1, b1.peer).await == 0
    );
    let before = notify_events(&a1).len();
    let push = ReceivedPush::Cipher {
        tag: *tag,
        ciphertext: cipher(kind),
    };
    assert_eq!(
        a1.account.push_received(push, WAIT).await.unwrap(),
        PushOutcome::Nothing
    );
    assert!(notified_since(&a1, before).is_empty());
}

/// Review Focus 1: a tag that is no current handle here (withdrawn, or never one) shows
/// nothing, stores nothing and does not fail.
#[tokio::test]
async fn push_for_unknown_tag_shows_nothing() {
    let (a1, b1, conv, _net, proxy) = offline_receiver().await;
    let sent = pushed(&a1, b1.peer, &proxy, &["withdrawn"]).await;
    let (key, tag, kind) = &sent[0];
    b1.account.push_set_enabled(false).await.unwrap();
    let before = notify_events(&b1).len();
    for push in [
        ReceivedPush::Cipher {
            tag: *tag,
            ciphertext: cipher(kind),
        },
        ReceivedPush::Wake { tag: *tag },
        ReceivedPush::Cipher {
            tag: [7; 16],
            ciphertext: cipher(kind),
        },
        ReceivedPush::Cipher {
            tag: [7; 16],
            ciphertext: Vec::new(),
        },
        ReceivedPush::Wake { tag: [7; 16] },
    ] {
        assert_eq!(
            b1.account.push_received(push, WAIT).await.unwrap(),
            PushOutcome::UnknownTag
        );
    }
    assert!(notified_since(&b1, before).is_empty());
    assert!(message(&b1, *key).await.is_none());
    assert_eq!(b1.account.push_conversation_of(*tag).await.unwrap(), None);
    // Nothing started the network for the wake.
    assert_eq!(network_state(&b1).await, NetworkState::Stopped);
}

#[tokio::test]
async fn undecryptable_push_notifies_generic() {
    let (a1, b1, conv, net, proxy) = offline_receiver().await;
    let sent = pushed(&a1, b1.peer, &proxy, &["cannot open"]).await;
    let (key, tag, kind) = &sent[0];
    assert_eq!(
        b1.account.push_conversation_of(*tag).await.unwrap(),
        Some(conv)
    );
    let mut broken = cipher(kind);
    let last = broken.len() - 1;
    broken[last] ^= 1;
    let generic = NotifyItem {
        conversation: conv,
        peer: a1.peer,
        new_messages: 1,
        unread: 1,
        title: "remote".into(),
        preview: None,
    };
    // Ruling R13: the first alerts; until the conversation is read up, the next ones only
    // update it.
    for (n, ciphertext) in [broken, vec![0xff; 40], Vec::new()].into_iter().enumerate() {
        let before = notify_events(&b1).len();
        let push = ReceivedPush::Cipher {
            tag: *tag,
            ciphertext,
        };
        let (outcome, new_messages) = if n == 0 {
            (PushOutcome::Notified, 1)
        } else {
            (PushOutcome::Updated, 0)
        };
        assert_eq!(b1.account.push_received(push, WAIT).await.unwrap(), outcome);
        assert_eq!(
            notified_since(&b1, before),
            [NotifyItem {
                new_messages,
                ..generic.clone()
            }]
        );
    }
    assert!(message(&b1, *key).await.is_none());

    // B1's process ends and starts again; the text then comes directly: it updates the generic
    // notification without alerting.
    let b1 = start_as(B, stop(b1), net.clone()).await;
    link(&b1, &a1).await;
    settle!([&a1, &b1], message(&b1, *key).await.is_some());
    quiet(&[&a1, &b1]).await;
    assert_eq!(
        notify_events(&b1),
        [NotifyItem {
            new_messages: 0,
            preview: Some("cannot open".into()),
            ..generic.clone()
        }]
    );
    // Read up: the next text alerts again.
    b1.account
        .messages()
        .mark_read(conv, vec![*key])
        .await
        .unwrap();
    let next = send_to(&a1, b1.peer, 2, "after reading").await;
    settle!([&a1, &b1], message(&b1, next).await.is_some());
    quiet(&[&a1, &b1]).await;
    assert_eq!(
        notify_events(&b1).last(),
        Some(&NotifyItem {
            preview: Some("after reading".into()),
            ..generic
        })
    );
    assert_eq!(alerts(&b1, conv), 1);
}

/// Ruling R13: the wake's fetch reached nobody before its deadline; the text comes directly
/// later and updates the generic notification without alerting a second time.
#[tokio::test]
async fn wake_then_late_direct_text_updates_without_second_alert() {
    let (a1, b1, conv, _net, proxy) = offline_receiver().await;
    let body = "늦게 온 ".repeat(1_200);
    let sent = pushed(&a1, b1.peer, &proxy, &[body.as_str()]).await;
    let (key, tag, kind) = &sent[0];
    assert!(*kind == PushItemKind::Wake);
    network(&a1, NetworkOp::Stop).await;
    let push = ReceivedPush::Wake { tag: *tag };
    assert_eq!(
        b1.account
            .push_received(push, Duration::from_secs(2))
            .await
            .unwrap(),
        PushOutcome::Notified
    );
    assert!(message(&b1, *key).await.is_none());
    assert_eq!(alerts(&b1, conv), 1);

    online(&a1).await;
    back_online(&b1, &[&a1]).await;
    settle!([&a1, &b1], message(&b1, *key).await.is_some());
    quiet(&[&a1, &b1]).await;
    assert_eq!(alerts(&b1, conv), 1);
    let last = notify_events(&b1).pop().unwrap();
    assert_eq!((last.new_messages, last.unread), (0, 1));
    assert_eq!(last.preview, Some(preview_text(&body, 0)));
}

/// A text too large to carry wakes B1: the generic notification first, then B1 starts its
/// network, dials A1 and gets the text within the deadline, the notification takes its start
/// without alerting again, and the network is off again.
#[tokio::test]
async fn wake_fetches_from_sender_within_deadline_and_updates_notification() {
    let (a1, b1, conv, _net, proxy) = offline_receiver().await;
    let body = "가".repeat(5_000);
    let sent = pushed(&a1, b1.peer, &proxy, &[body.as_str()]).await;
    let (key, tag, kind) = &sent[0];
    assert!(*kind == PushItemKind::Wake);
    let before = notify_events(&b1).len();
    let account = b1.account.clone();
    let tag = *tag;
    let task = tokio::spawn(async move {
        account
            .push_received(ReceivedPush::Wake { tag }, WAIT)
            .await
    });
    settle!([&a1, &b1], task.is_finished());
    assert_eq!(task.await.unwrap().unwrap(), PushOutcome::Notified);
    assert!(message(&b1, *key).await.is_some());
    let items = notified_since(&b1, before);
    let generic = NotifyItem {
        conversation: conv,
        peer: a1.peer,
        new_messages: 1,
        unread: 1,
        title: "remote".into(),
        preview: None,
    };
    assert_eq!(items[0], generic);
    assert_eq!(
        items.last().unwrap(),
        &NotifyItem {
            new_messages: 0,
            preview: Some(preview_text(&body, 0)),
            ..generic
        }
    );
    assert_eq!(items.iter().filter(|i| i.new_messages > 0).count(), 1);
    // B1 started the network for the fetch, and stopped it.
    assert_eq!(network_state(&b1).await, NetworkState::Stopped);
    settle!([&a1], delivery(&a1, *key).await == DeliveryState::Delivered);
}

/// Nobody can be reached: the generic notification stays, and the fetch ends at the deadline.
#[tokio::test]
async fn wake_without_reach_leaves_generic_notification() {
    let (a1, b1, conv, _net, _proxy) = offline_receiver().await;
    let a_peer = a1.peer;
    let _a = stop(a1);
    let before = notify_events(&b1).len();
    let started = tokio::time::Instant::now();
    let push = ReceivedPush::Wake {
        tag: own_tag(&b1, conv).await,
    };
    assert_eq!(
        b1.account
            .push_received(push, Duration::from_secs(2))
            .await
            .unwrap(),
        PushOutcome::Notified
    );
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(1_500) && took < Duration::from_secs(6));
    assert_eq!(
        notified_since(&b1, before),
        [NotifyItem {
            conversation: conv,
            peer: a_peer,
            new_messages: 1,
            unread: 1,
            title: "remote".into(),
            preview: None,
        }]
    );
    assert_eq!(network_state(&b1).await, NetworkState::Stopped);
}

/// Spec 7: A1 showed a text, A2 read it while A1 was off; A1's read clear push syncs with A2 and
/// clears the notification.
#[tokio::test]
async fn read_clear_syncs_and_cancels_notification() {
    let (mut a, b1, _net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let a1_id = installation(&a1).await;
    let key = delivered(&b1, &a1, &[&a2], 1).await;
    settle!(
        [&a1, &a2, &b1],
        message(&a1, key).await.is_some() && message(&a2, key).await.is_some()
    );
    assert_eq!(unread(&a1, b1.peer).await, 1);
    network(&a1, NetworkOp::Stop).await;
    settle!([&a1, &a2, &b1], !ready_with(&a2, a1_id));
    a2.account
        .messages()
        .mark_read(conv, vec![key])
        .await
        .unwrap();

    let before = notify_events(&a1).len();
    let account = a1.account.clone();
    let task =
        tokio::spawn(async move { account.push_received(ReceivedPush::ReadClear, WAIT).await });
    settle!([&a1, &a2, &b1], task.is_finished());
    assert_eq!(task.await.unwrap().unwrap(), PushOutcome::Updated);
    assert_eq!(unread(&a1, b1.peer).await, 0);
    let cleared = notified_since(&a1, before);
    assert!(cleared.iter().any(|i| i.conversation == conv
        && i.new_messages == 0
        && i.unread == 0
        && i.preview.is_none()));
    assert_eq!(network_state(&a1).await, NetworkState::Stopped);
}

/// FCM dropped pushes for A1 (`onDeletedMessages`): when its network starts next, it syncs with
/// A2 and dials B1 by itself, and gets the text it missed.
#[tokio::test]
async fn messages_deleted_triggers_full_sync_on_next_start() {
    let (mut a, b1, _net, _conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let a1_id = installation(&a1).await;
    network(&a1, NetworkOp::Stop).await;
    settle!(
        [&a1, &a2, &b1],
        !ready_with(&b1, a1_id) && !ready_with(&a2, a1_id)
    );
    let key = send_to(&b1, a1.peer, 1, "missed").await;
    settle!([&a2, &b1], message(&a2, key).await.is_some());

    // Without the note, a start alone brings nothing (the others cannot dial A1's new address).
    online(&a1).await;
    quiet(&[&a1, &a2, &b1]).await;
    assert!(message(&a1, key).await.is_none());
    network(&a1, NetworkOp::Stop).await;

    a1.account.push_messages_deleted().await.unwrap();
    quiet(&[&a1]).await;
    assert!(message(&a1, key).await.is_none());
    online(&a1).await;
    settle!([&a1, &a2, &b1], message(&a1, key).await.is_some());
    settle!([&a1, &a2, &b1], ready_with(&a1, installation(&b1).await));
}

/// iOS: the extension's inbox is taken in order without notifications; what was taken already
/// and unknown tags are skipped.
#[tokio::test]
async fn ingest_inbox_stores_without_events_and_dedupes() {
    let (a1, b1, conv, _net, proxy) = offline_receiver().await;
    let sent = pushed(&a1, b1.peer, &proxy, &["inbox one", "inbox two"]).await;
    let entry = |i: usize| (sent[i].1, cipher(&sent[i].2));
    let before = notify_events(&b1).len();
    let entries = vec![entry(0), entry(1), entry(0), ([9; 16], cipher(&sent[0].2))];
    assert_eq!(
        b1.account.push_ingest_inbox(entries.clone()).await.unwrap(),
        2
    );
    for (key, _, _) in &sent {
        assert!(message(&b1, *key).await.is_some());
    }
    assert_eq!(b1.account.push_ingest_inbox(entries).await.unwrap(), 0);
    assert!(notified_since(&b1, before).is_empty());
    assert_eq!(unread(&b1, a1.peer).await, 2);
    assert!(page(&b1, conv).await.len() == 2);
}

/// Review Focus 5: at most 48 bytes, cut between characters, " 외 N개" for the others.
#[test]
fn preview_cuts_on_char_boundary_within_48_bytes() {
    let hangul = "가나다라마바사아자차카타파하".repeat(3);
    let family = "👨‍👩‍👧".repeat(5);
    let combining = "e\u{301}".repeat(30);
    let mixed = format!("ok {family} 한글 {combining}");
    for body in [&hangul, &family, &combining, &mixed] {
        let start = preview_text(body, 0);
        assert!(start.len() <= 48, "{} bytes", start.len());
        assert!(!start.is_empty() && body.starts_with(&start));
        // Nothing more of the body fits: the next character would go over 48 bytes.
        let next = body[start.len()..].chars().next().unwrap();
        assert!(start.len() + next.len_utf8() > 48);
        assert!(std::str::from_utf8(start.as_bytes()).is_ok());
        let more = preview_text(body, 3);
        assert_eq!(more, format!("{start} 외 3개"));
    }
    // 16 Hangul syllables are exactly 48 bytes.
    assert_eq!(preview_text(&hangul, 0), "가나다라마바사아자차카타파하가나");
    assert_eq!(preview_text("짧은 글", 0), "짧은 글");
    assert_eq!(preview_text("짧은 글", 1), "짧은 글 외 1개");
}
