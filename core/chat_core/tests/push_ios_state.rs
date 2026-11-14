#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! The iOS notification extension's state files (M7 spec 5.6, Task 11): an APNs device with a
//! snapshot folder keeps `<dir>/push/<hex(tag)>.state` for each conversation a push of it can
//! name, rewritten when the conversation's group state changes, removed when its handle is
//! withdrawn, and the folder is brought up to date at every start.
include!("support/push_network.rs");

use chat_core::message::preview_text;
use chat_core::push::ios_state::{PUSH_DIR, decode_state, state_file_name};
use chat_core::push::{PushItemKind, Tag};
use chat_core::storage_fixture::nse_preview;

/// `f` starts as an iOS device with push on, the notification extension's folder `dir`, and
/// its first APNs token.
async fn ios(f: &Fixture, dir: &std::path::Path, token: &str) {
    f.account
        .push_configure(PushConfig {
            proxy_url: Some(PROXY_URL.into()),
            platform: Some(Platform::Apns),
            snapshot_dir: Some(dir.into()),
        })
        .unwrap();
    f.account
        .push_set_token(Platform::Apns, token.into())
        .await
        .unwrap();
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

fn state_path(dir: &std::path::Path, tag: &Tag) -> std::path::PathBuf {
    dir.join(PUSH_DIR).join(state_file_name(tag))
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

/// The `cipher` ciphertexts (`e`, group id taken out) the proxy was asked to push, in order.
fn pushed_ciphertexts(proxy: &FakePushProxy) -> Vec<Vec<u8>> {
    proxy
        .push_requests()
        .into_iter()
        .flat_map(|r| r.items)
        .filter_map(|i| match i.kind {
            PushItemKind::Cipher { e } => Some(BASE64URL_NOPAD.decode(e.as_bytes()).unwrap()),
            _ => None,
        })
        .collect()
}

/// B1's network goes off; A1's texts to it are pushed from now on.
async fn offline(a1: &Fixture, b1: &Fixture) {
    let b1_id = installation(b1).await;
    b1.slot
        .network()
        .unwrap()
        .execute(NetworkOp::Stop)
        .await
        .unwrap();
    settle!([a1, b1], !ready_with(a1, b1_id));
}

/// A1 sends a text to offline B1: its key, and the ciphertext the proxy was asked to push.
async fn pushed_text(
    a1: &Fixture,
    b1: &Fixture,
    proxy: &FakePushProxy,
    n: u8,
    body: &str,
) -> (MessageKey, Vec<u8>) {
    let before = pushed_ciphertexts(proxy).len();
    let key = a1
        .account
        .messages()
        .send_text(b1.peer, MessageId([n; 16]), body.into())
        .await
        .unwrap()
        .key;
    settle!([a1], pushed_ciphertexts(proxy).len() > before);
    (key, pushed_ciphertexts(proxy).remove(before))
}

/// A1 and B1 in a group; B1 an iOS device whose handle A1 knows, A1 pushing to it.
async fn ios_receiver(
    dir: &std::path::Path,
) -> (Fixture, Fixture, ConversationId, Arc<Backend>, Arc<FakePushProxy>) {
    // B2 is listed and never online: the Commits add and remove it.
    let (a1, b1, net, conv) = setup(&[B1, B2]).await;
    grouped(&a1, &b1, conv).await;
    let proxy = proxy(&net);
    let b1_id = installation(&b1).await;
    ios(&b1, dir, "token-b1").await;
    settle!(
        [&a1, &b1],
        peer_handle(&a1, conv, b1_id).await.flatten().is_some()
            && push_state(&b1).await.deliveries.is_empty()
    );
    a1.account.push_configure(config(None)).unwrap();
    (a1, b1, conv, net, proxy)
}

#[tokio::test]
async fn state_file_follows_group_changes_and_disappears_on_withdraw() {
    let dir = tempfile::tempdir().unwrap();
    let (a1, b1, conv, _net, proxy) = ios_receiver(dir.path()).await;
    let tag = own_tag(&b1, conv).await;
    let path = state_path(dir.path(), &tag);
    settle!([&a1, &b1], path.exists());
    let first = std::fs::read(&path).unwrap();
    let state = decode_state(&first).unwrap();
    assert_eq!(state.alias, "remote", "the contact's alias is the title");
    assert!(!state.snapshot.is_empty());
    // Only B1 (APNs, with a folder) keeps files; nothing else is in the folder.
    let names: Vec<_> = std::fs::read_dir(dir.path().join(PUSH_DIR))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, vec![state_file_name(&tag)]);

    // A pushed text opens against the file as the extension opens it.
    offline(&a1, &b1).await;
    let body = "푸시로 온 첫 메시지, 잠긴 화면에 보여야 해요 (with a long tail of text)";
    let (key1, e1) = pushed_text(&a1, &b1, &proxy, 1, body).await;
    assert_eq!(
        nse_preview(&std::fs::read(&path).unwrap(), &e1).as_deref(),
        Some(preview_text(body, 0).as_str())
    );

    // B1 takes the text directly: its group state moved, and so did the file.
    online(&b1).await;
    link(&b1, &a1).await;
    settle!(
        [&a1, &b1],
        std::fs::read(&path).unwrap() != first
            && delivery(&a1, key1).await == DeliveryState::Delivered
    );
    // A Commit moves the epoch: the file follows it, and a text of the new epoch opens only
    // against the new file.
    let before_commit = std::fs::read(&path).unwrap();
    commits(&a1, &b1, conv, 1).await;
    settle!([&a1, &b1], {
        let now = std::fs::read(&path).unwrap();
        now != before_commit
            && decode_state(&now).unwrap().snapshot != decode_state(&before_commit).unwrap().snapshot
    });
    offline(&a1, &b1).await;
    let body2 = "에포크가 바뀐 뒤의 메시지";
    let (_, e2) = pushed_text(&a1, &b1, &proxy, 2, body2).await;
    assert_eq!(
        nse_preview(&std::fs::read(&path).unwrap(), &e2).as_deref(),
        Some(preview_text(body2, 0).as_str())
    );
    assert_eq!(nse_preview(&before_commit, &e2), None, "the old state is of the old epoch");

    // Push off withdraws the handle: the extension has nothing for the tag any more.
    b1.account.push_set_enabled(false).await.unwrap();
    settle!([&a1, &b1], !path.exists());
}

#[tokio::test]
async fn stale_state_files_removed_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let (a1, b1, conv, _net, _proxy) = ios_receiver(dir.path()).await;
    let tag = own_tag(&b1, conv).await;
    let path = state_path(dir.path(), &tag);
    settle!([&a1, &b1], path.exists());
    let push = dir.path().join(PUSH_DIR);
    // A file of a tag this device no longer has, a temporary file a crash left, the inbox of
    // the extension, and the live tag's file gone.
    let stale = state_path(dir.path(), &[7; 16]);
    let leftover = push.join(format!("{}.tmp", state_file_name(&tag)));
    let inbox = push.join("inbox.jsonl");
    for file in [&stale, &leftover, &inbox] {
        std::fs::write(file, b"x").unwrap();
    }
    std::fs::remove_file(&path).unwrap();

    // The next start configures again.
    b1.account
        .push_configure(PushConfig {
            proxy_url: Some(PROXY_URL.into()),
            platform: Some(Platform::Apns),
            snapshot_dir: Some(dir.path().into()),
        })
        .unwrap();
    settle!([&a1, &b1], !stale.exists() && !leftover.exists() && path.exists());
    assert!(decode_state(&std::fs::read(&path).unwrap()).is_some());
    assert_eq!(std::fs::read(&inbox).unwrap(), b"x", "the inbox is the app's to take");
}
