#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Sending pushes (M7 spec 6, 7): the device that sealed a text pushes once, when its first
//! attempt has a result, to each current device of the contact that announced a handle and was
//! neither reached directly nor sent a copy; retries, mailbox handoffs, receipts, Commits and
//! PushHandles never push; a proxy's `gone` withdraws the handle; reading on one device clears
//! the notifications of my Android devices that have no session, once per second at most.
include!("support/push_network.rs");

use chat_core::push::{PushItemKind, PushResult};

/// The `/push` requests that carry a text (`cipher` or `wake` items).
fn text_pushes(proxy: &FakePushProxy) -> Vec<PushRequest> {
    proxy
        .push_requests()
        .into_iter()
        .filter(|r| {
            r.items
                .iter()
                .any(|i| matches!(i.kind, PushItemKind::Cipher { .. } | PushItemKind::Wake))
        })
        .collect()
}

/// The `/push` requests that clear reads.
fn read_pushes(proxy: &FakePushProxy) -> Vec<PushRequest> {
    proxy
        .push_requests()
        .into_iter()
        .filter(|r| r.items.iter().any(|i| i.kind == PushItemKind::Read))
        .collect()
}

fn b64(bytes: &[u8]) -> String {
    BASE64URL_NOPAD.encode(bytes)
}

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

/// A1 sends with push on and no platform (Windows); B1, an Android device, announced its handle
/// to A1 and went off. Returns A1, B1's data directory, B1's installation and handle, the
/// conversation, the backend and the proxy.
async fn contact_offline(
    b_devices: &[u8],
) -> (
    Fixture,
    tempfile::TempDir,
    chat_core::account::InstallationId,
    Vec<u8>,
    ConversationId,
    Arc<Backend>,
    Arc<FakePushProxy>,
) {
    let (a1, b1, net, conv) = setup(b_devices).await;
    grouped(&a1, &b1, conv).await;
    let proxy = proxy(&net);
    let b1_id = installation(&b1).await;
    android(&b1, "token-b1").await;
    settle!(
        [&a1, &b1],
        peer_handle(&a1, conv, b1_id).await.flatten().is_some()
            && push_state(&b1).await.deliveries.is_empty()
    );
    let handle = own_tag_handle(&b1, conv).await.unwrap();
    a1.account.push_configure(config(None)).unwrap();
    let dir = stop(b1);
    (a1, dir, b1_id, handle, conv, net, proxy)
}

#[tokio::test]
async fn offline_contact_device_gets_one_cipher_push() {
    let (a1, _b, _b1_id, handle, conv, _net, proxy) = contact_offline(&[B1]).await;
    let key = send_to(&a1, peer_of(B), 1, "pushed once").await;
    settle!([&a1], !text_pushes(&proxy).is_empty());
    // The retries that follow push nothing more.
    settle!([&a1], outbox(&a1, key).await.attempts >= 3);
    quiet(&[&a1]).await;
    let pushes = text_pushes(&proxy);
    assert_eq!(pushes.len(), 1);
    assert_eq!(pushes[0].items.len(), 1);
    let item = &pushes[0].items[0];
    assert_eq!(item.handle, b64(&handle));
    let PushItemKind::Cipher { e } = &item.kind else {
        panic!("not a cipher item");
    };
    let e = BASE64URL_NOPAD.decode(e.as_bytes()).unwrap();
    assert!(e.len() <= push_wire::PUSH_CIPHER_MAX);
    // The payload names no conversation: the group id (which starts with it) is stripped.
    let group_id = state(&a1, conv).await.group_id.unwrap();
    assert!(!contains(&e, &group_id));
    assert!(!contains(&e, &conv.0));
    assert!(!contains(&e, SENTINEL.as_bytes()));
    // The ciphertext is the one sealed for the text, less its group id.
    let row = outbox(&a1, key).await;
    let sealed = mls_app_from_wire(&row.wire).unwrap().1;
    assert!(e.len() < sealed.len() && sealed.len() - e.len() >= group_id.len());
    assert!(row.pushed);
}

/// The text reaches A1 or A2 directly and the other by copy (spec 5.3): only A3, which is off,
/// gets a push.
#[tokio::test]
async fn directly_reached_or_copied_devices_get_no_push() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2, A3], &[B1]).await;
    let a3 = a.pop().unwrap();
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let proxy = proxy(&net);
    let ids = [
        installation(&a1).await,
        installation(&a2).await,
        installation(&a3).await,
    ];
    for (f, token) in [(&a1, "token-a1"), (&a2, "token-a2"), (&a3, "token-a3")] {
        android(f, token).await;
    }
    settle!([&a1, &a2, &a3, &b1], {
        let mut ok = true;
        for id in ids {
            ok = ok && peer_handle(&b1, conv, id).await.flatten().is_some();
        }
        ok
    });
    let a3_handle = own_tag_handle(&a3, conv).await.unwrap();
    b1.account.push_configure(config(None)).unwrap();
    let _a3 = stop(a3);
    settle!([&a1, &a2, &b1], !ready_with(&b1, ids[2]));

    let key = send_to(&b1, a1.peer, 1, "to all of A").await;
    settle!(
        [&a1, &a2, &b1],
        delivery(&b1, key).await == DeliveryState::Delivered && !text_pushes(&proxy).is_empty()
    );
    quiet(&[&a1, &a2, &b1]).await;
    let pushes = text_pushes(&proxy);
    assert_eq!(pushes.len(), 1);
    let handles: Vec<&str> = pushes[0].items.iter().map(|i| i.handle.as_str()).collect();
    assert_eq!(handles, [b64(&a3_handle)]);
    assert!(message(&a1, key).await.is_some() && message(&a2, key).await.is_some());
}

/// A1 tries three more times while B1 is off, then hands the text to its mailbox A2, which keeps
/// trying it: one push in all, from A1's first attempt.
#[tokio::test]
async fn retries_and_mailbox_handoff_do_not_push_again() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let proxy = proxy(&net);
    let b1_id = installation(&b1).await;
    android(&b1, "token-b1").await;
    settle!(
        [&a1, &a2, &b1],
        peer_handle(&a1, conv, b1_id).await.flatten().is_some()
            && peer_handle(&a2, conv, b1_id).await.flatten().is_some()
    );
    for f in [&a1, &a2] {
        f.account.push_configure(config(None)).unwrap();
    }
    a2.account.set_mailbox(true).await.unwrap();
    let a2_id = installation(&a2).await;
    a1.account
        .storage_test_fixture()
        .mailbox_mark(a2_id, true, NOW * 1000)
        .await
        .unwrap();
    let b_peer = b1.peer;
    let _b = stop(b1);

    let key = send_to(&a1, b_peer, 1, "tried and handed over").await;
    settle!([&a1, &a2], outbox(&a1, key).await.attempts >= 4);
    assert_eq!(text_pushes(&proxy).len(), 1);

    assert!(a1.account.mailbox_background().await.unwrap() >= 1);
    settle!([&a1, &a2], !mailbox(&a2).await.handoff_rows.is_empty());
    settle!(
        [&a2],
        mailbox(&a2).await.handoff_rows[0].attempts >= 3
    );
    quiet(&[&a1, &a2]).await;
    assert_eq!(text_pushes(&proxy).len(), 1);
}

/// A1 goes to the background (handing the text to its mailbox A2) before any attempt of the
/// text has a result, and may be suspended right after: the handoff itself pushes, once. A2
/// never pushes, and A1's later attempts do not push again.
#[tokio::test]
async fn handoff_before_first_outcome_pushes_once() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let proxy = proxy(&net);
    let b1_id = installation(&b1).await;
    android(&b1, "token-b1").await;
    settle!(
        [&a1, &a2, &b1],
        peer_handle(&a1, conv, b1_id).await.flatten().is_some()
            && peer_handle(&a2, conv, b1_id).await.flatten().is_some()
    );
    let handle = own_tag_handle(&b1, conv).await.unwrap();
    for f in [&a1, &a2] {
        f.account.push_configure(config(None)).unwrap();
    }
    a2.account.set_mailbox(true).await.unwrap();
    let a2_id = installation(&a2).await;
    a1.account
        .storage_test_fixture()
        .mailbox_mark(a2_id, true, NOW * 1000)
        .await
        .unwrap();
    let b_peer = b1.peer;
    let _b = stop(b1);
    // No attempt starts: the text has no outcome when it is handed over.
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    // The configuration is taken at the next poll.
    quiet(&[&a1, &a2]).await;

    let key = send_to(&a1, b_peer, 1, "handed over at once").await;
    assert_eq!(outbox(&a1, key).await.attempts, 0);
    assert_eq!(a1.account.mailbox_background().await.unwrap(), 1);
    assert!(outbox(&a1, key).await.pushed);
    settle!([&a1, &a2], !text_pushes(&proxy).is_empty());
    let pushes = text_pushes(&proxy);
    assert_eq!(pushes.len(), 1);
    assert_eq!(pushes[0].items.len(), 1);
    assert_eq!(pushes[0].items[0].handle, b64(&handle));
    assert!(matches!(pushes[0].items[0].kind, PushItemKind::Cipher { .. }));

    // A1's attempts and A2's handed-over retries push nothing more; a second handoff neither.
    a1.account.message_test_hold_scheduler(false).await.unwrap();
    settle!([&a1, &a2], !mailbox(&a2).await.handoff_rows.is_empty());
    settle!(
        [&a1, &a2],
        outbox(&a1, key).await.attempts >= 3
            && mailbox(&a2).await.handoff_rows[0].attempts >= 3
    );
    assert!(a1.account.mailbox_background().await.unwrap() >= 1);
    quiet(&[&a1, &a2]).await;
    assert_eq!(text_pushes(&proxy).len(), 1);
}

/// A text whose stripped ciphertext is over `PUSH_CIPHER_MAX` (the 16 KiB bucket) only wakes.
#[tokio::test]
async fn large_text_sends_wake() {
    let (a1, _b, _b1_id, handle, _conv, _net, proxy) = contact_offline(&[B1]).await;
    let body = "x".repeat(16_000);
    let key = send_to(&a1, peer_of(B), 1, &body).await;
    settle!([&a1], !text_pushes(&proxy).is_empty());
    let pushes = text_pushes(&proxy);
    assert_eq!(pushes.len(), 1);
    assert_eq!(pushes[0].items.len(), 1);
    assert_eq!(pushes[0].items[0].handle, b64(&handle));
    assert!(pushes[0].items[0].kind == PushItemKind::Wake);
    assert!(outbox(&a1, key).await.wire.len() > push_wire::PUSH_CIPHER_MAX);
}

/// B1 (off, with a handle) is owed a `Read` receipt, a PushHandle and a Commit by A1, whose push
/// is on: none of them is pushed.
#[tokio::test]
async fn receipts_commits_and_push_handles_never_push() {
    let (a1, b1, net, conv) = setup(&[B1, B2]).await;
    grouped(&a1, &b1, conv).await;
    let proxy = proxy(&net);
    let (a1_id, b1_id) = (installation(&a1).await, installation(&b1).await);
    android(&a1, "token-a1").await;
    android(&b1, "token-b1").await;
    // B2 never runs: what A1 queued for it stays.
    let to_b1 = |state: PushState| state.deliveries.iter().any(|d| d.recipient == b1_id);
    settle!(
        [&a1, &b1],
        peer_handle(&a1, conv, b1_id).await.flatten().is_some()
            && peer_handle(&b1, conv, a1_id).await.flatten().is_some()
            && !to_b1(push_state(&a1).await)
    );
    let key = send_to(&b1, a1.peer, 1, "from b").await;
    settle!([&a1, &b1], delivery(&b1, key).await == DeliveryState::Delivered);
    let b_peer = b1.peer;
    let _b = stop(b1);

    // A Read receipt, a renewed handle and a Commit, each due to B1.
    a1.account.messages().mark_read(conv, vec![key]).await.unwrap();
    let issued = push_state(&a1).await.tags[0].issued_at_ms.unwrap();
    let summary = a1
        .account
        .push_renew_due(issued + RENEW_AFTER_MS)
        .await
        .unwrap();
    assert_eq!(summary.announced, 1);
    let messages = a1.account.storage_test_fixture().messages().await.unwrap();
    messages
        .mls_commit(
            conv,
            b_peer,
            vec![mls_detached_key_package(b_peer, [B2; 32])],
            vec![],
            NOW * 1000,
        )
        .await
        .unwrap()
        .expect("an own Commit");
    quiet(&[&a1]).await;
    assert!(receipt_jobs(&a1).await > 0);
    assert!(to_b1(push_state(&a1).await));
    assert!(!state(&a1, conv).await.deliveries.is_empty());
    assert!(proxy.push_requests().is_empty());
}

/// The proxy answers `gone` (B1's handle does not open at another proxy): A1 withdraws it,
/// keeping the time it was sent at (Ruling R10), and the next text pushes nothing.
#[tokio::test]
async fn gone_drops_peer_handle() {
    let (a1, _b, b1_id, _handle, conv, net, _old) = contact_offline(&[B1]).await;
    let before = peer_rows(&a1)
        .await
        .into_iter()
        .find(|r| r.installation == b1_id)
        .unwrap();
    let fresh = proxy(&net);
    send_to(&a1, peer_of(B), 1, "gone").await;
    settle!(
        [&a1],
        peer_handle(&a1, conv, b1_id).await == Some(None)
    );
    let pushes = fresh.push_requests();
    assert_eq!(pushes.len(), 1);
    let after = peer_rows(&a1)
        .await
        .into_iter()
        .find(|r| r.installation == b1_id)
        .unwrap();
    assert_eq!(after.sent_at_ms, before.sent_at_ms);

    send_to(&a1, peer_of(B), 2, "no handle now").await;
    quiet(&[&a1]).await;
    assert_eq!(fresh.push_requests().len(), 1);
}

/// Spec 7: A1 (Android) announced its own-device handle to A2 (Windows). Reads on A2 while A1
/// has a session push nothing; with A1 off, three reads within a second push one `read`.
#[tokio::test]
async fn read_on_pc_sends_one_read_clear_to_offline_android() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let proxy = proxy(&net);
    let a1_id = installation(&a1).await;
    android(&a1, "token-a1").await;
    settle!([&a1, &a2, &b1], {
        let own = push_state(&a1).await.own_handle;
        own.is_some() && push_state(&a2).await.own == vec![(a1_id, own.unwrap())]
    });
    let a1_handle = push_state(&a1).await.own_handle.unwrap();
    a2.account.push_configure(config(None)).unwrap();

    // A1 has a session: nothing to clear there.
    let first = delivered(&b1, &a2, &[&a1], 1).await;
    settle!([&a1, &a2, &b1], message(&a2, first).await.is_some());
    a2.account
        .messages()
        .mark_read(conv, vec![first])
        .await
        .unwrap();
    quiet(&[&a1, &a2, &b1]).await;
    assert!(read_pushes(&proxy).is_empty());

    let _a1 = stop(a1);
    settle!([&a2, &b1], !ready_with(&a2, a1_id));
    let mut keys = Vec::new();
    for n in 2..5 {
        keys.push(send_to(&b1, a2.peer, n, &format!("unread {n}")).await);
    }
    settle!([&a2, &b1], {
        let mut ok = true;
        for key in &keys {
            ok = ok && message(&a2, *key).await.is_some();
        }
        ok
    });
    // Three reads at the same moment (the clock does not move).
    for key in &keys {
        a2.account
            .messages()
            .mark_read(conv, vec![*key])
            .await
            .unwrap();
    }
    settle!([&a2, &b1], !read_pushes(&proxy).is_empty());
    quiet(&[&a2, &b1]).await;
    let reads = read_pushes(&proxy);
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].items.len(), 1);
    assert_eq!(reads[0].items[0].handle, b64(&a1_handle));
    assert!(text_pushes(&proxy).is_empty());
}

#[tokio::test]
async fn push_disabled_sender_sends_nothing() {
    let (a1, _b, _b1_id, _handle, _conv, _net, proxy) = contact_offline(&[B1]).await;
    a1.account.push_set_enabled(false).await.unwrap();
    let key = send_to(&a1, peer_of(B), 1, "push is off").await;
    settle!([&a1], outbox(&a1, key).await.attempts >= 2);
    quiet(&[&a1]).await;
    assert!(proxy.push_requests().is_empty());
    assert!(!outbox(&a1, key).await.pushed);
}
