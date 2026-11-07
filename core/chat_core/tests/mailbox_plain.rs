#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Plaintext hand-over (M6, spec 6.3): an entry of another own device's log that this device
//! cannot open is asked for as plaintext (`PlainReq`) and stored under its original logical key;
//! the record goes only to a current device of the account, over its authenticated session.
include!("support/mailbox_network.rs");

use chat_core::storage_fixture::{HandoffEntry, PlainRecord};
use sha2::Digest;

async fn unread(f: &Fixture, conv: ConversationId) -> u64 {
    f.account
        .messages()
        .list_conversations(None, 50)
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|c| c.id == conv)
        .map_or(0, |c| c.unread)
}

/// The seq under which `f` logged the text `key`.
async fn logged(f: &Fixture, key: MessageKey) -> u64 {
    mailbox(f)
        .await
        .entries
        .iter()
        .find(|e| e.msg_id == Some(key.id))
        .expect("the text is logged")
        .seq
}

/// Starts a stopped device of A again with the conversation's group state gone (it can open
/// nothing of it), then lets it reach `peers` and sync.
async fn back_without_group(
    dir: tempfile::TempDir,
    net: &Arc<Backend>,
    conv: ConversationId,
    peers: &[&Fixture],
) -> Fixture {
    let f = start(dir, net.clone()).await;
    f.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_drop_group(conv)
        .await
        .unwrap();
    for peer in peers {
        link(&f, peer).await;
    }
    f.account.mailbox_sync_now().await.unwrap();
    f
}

/// MLS application frames (kind 17 `App`) `f` sent: A1 sends no text in these tests, so these
/// are its receipts.
async fn receipts_sent(f: &Fixture) -> usize {
    app_ciphertexts(f).await.len()
}

/// Spec 6.3: A1 lost the conversation's group while it was off, so the text B1 sent to A2
/// meanwhile cannot be opened from A2's log. A1 asks A2 for its plaintext and stores it under
/// the same key, body, lamport and chain position (and the same payload hash, the ciphertext's,
/// as a text opened from it would have): unread, notified once, no receipt sent or queued, and
/// not in A1's own log. Its ciphertext counts as processed (Ruling 24): when it comes again it
/// is answered as already taken and nothing changes.
#[tokio::test]
async fn unopenable_entry_is_stored_from_plaintext() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let processed = state(&a1, conv).await.processed_mls;
    let dir = stop(a1);
    let key = delivered(&b1, &a2, &[], 1).await;
    let a1 = back_without_group(dir, &net, conv, &[&a2, &b1]).await;
    settle!(
        [&a1, &a2, &b1],
        message(&a1, key).await.is_some() && caught_up(&a1, &a2).await
    );
    quiet(&[&a1, &a2, &b1]).await;
    assert_eq!(state(&a1, conv).await.group_epoch, None, "still no group");
    let (got, kept) = (
        message(&a1, key).await.unwrap(),
        message(&a2, key).await.unwrap(),
    );
    assert_eq!(got.body, "text 1");
    assert_eq!((got.lamport, got.outgoing), (kept.lamport, false));
    let fixture = |f: &Fixture| f.account.storage_test_fixture();
    let (facts, held) = (
        fixture(&a1).message_facts(key).await.unwrap(),
        fixture(&a2).message_facts(key).await.unwrap(),
    );
    assert_eq!(facts.hash, held.hash, "payload hash of the ciphertext");
    assert_eq!(facts.prev_hash, held.prev_hash);
    assert!(!facts.seen_local);
    assert_eq!(unread(&a1, conv).await, 1);
    assert_eq!(
        notify_events(&a1),
        vec![NotifyItem {
            conversation: conv,
            peer: b1.peer,
            new_messages: 1,
            unread: 1
        }]
    );
    assert_eq!(notify_count(&a1), 1, "one notification");
    assert_eq!(receipt_jobs(&a1).await, 0, "no receipt queued");
    assert_eq!(receipts_sent(&a1).await, 0, "no receipt sent");
    assert_eq!(
        state(&a1, conv).await.processed_mls,
        processed + 1,
        "its ciphertext's processed_ct row"
    );
    assert!(
        !mailbox(&a1)
            .await
            .entries
            .iter()
            .any(|e| e.msg_id == Some(key.id)),
        "not in A1's log"
    );
    assert_eq!(a1.account.mailbox_test_counters().await.unwrap().missing, 0);
    // The ciphertext itself arrives after all: already taken, though A1 has no group to open it.
    let ciphertext = app_ciphertexts(&b1).await.pop().unwrap();
    let ct_hash: [u8; 32] = sha2::Sha256::digest(&ciphertext).into();
    assert_eq!(facts.hash, ct_hash);
    let b1_id = installation(&b1).await;
    let reply = fixture(&a1)
        .messages()
        .await
        .unwrap()
        .mls_open_app(b1.peer, b1_id, conv, ciphertext, NOW * 1000)
        .await
        .unwrap();
    assert!(reply == MlsReply::Ack { ct_hash }, "already taken");
    assert_eq!(texts_from(&a1, conv, b1.peer).await.len(), 1);
    assert_eq!(unread(&a1, conv).await, 1);
}

/// Ruling 24: a text stored from plaintext is one received over MLS. Read later, its receipt
/// never leaves as an M4 (kind 16) frame: it is sealed in the group, which A1 gets again.
#[tokio::test]
async fn plaintext_stored_text_is_read_over_mls() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let dir = stop(a.pop().unwrap());
    let key = delivered(&b1, &a2, &[], 1).await;
    let a1 = back_without_group(dir, &net, conv, &[&a2, &b1]).await;
    settle!(
        [&a1, &a2, &b1],
        message(&a1, key).await.is_some() && caught_up(&a1, &a2).await
    );
    a1.account
        .messages()
        .mark_read(conv, vec![key])
        .await
        .unwrap();
    settle!(
        [&a1, &a2, &b1],
        delivery(&b1, key).await == DeliveryState::Read
    );
    assert!(receipts_sent(&a1).await >= 1, "a sealed (kind 17) receipt");
    assert!(
        !frames(&a1).await.iter().any(|frame| kind(frame) == 16),
        "no M4 receipt"
    );
}

/// Reviewer check: a text the holder sent itself is handed over too. The copy is stored as A1's
/// own sent message, with the payload hash of the logged ciphertext (as an opened copy has).
#[tokio::test]
async fn own_sent_text_is_handed_over() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let dir = stop(a.pop().unwrap());
    let key = delivered(&a2, &b1, &[], 1).await;
    let a1 = back_without_group(dir, &net, conv, &[&a2, &b1]).await;
    settle!(
        [&a1, &a2, &b1],
        message(&a1, key).await.is_some() && caught_up(&a1, &a2).await
    );
    let (got, sent) = (
        message(&a1, key).await.unwrap(),
        message(&a2, key).await.unwrap(),
    );
    assert!(got.outgoing);
    assert_eq!((got.body, got.lamport), (sent.body, sent.lamport));
    let ciphertext = app_ciphertexts(&a2).await.pop().unwrap();
    let ct_hash: [u8; 32] = sha2::Sha256::digest(&ciphertext).into();
    let fixture = |f: &Fixture| f.account.storage_test_fixture();
    let (facts, held) = (
        fixture(&a1).message_facts(key).await.unwrap(),
        fixture(&a2).message_facts(key).await.unwrap(),
    );
    assert_eq!(facts.hash, ct_hash);
    assert_eq!(facts.prev_hash, held.prev_hash);
    // The sender's own row keeps the canonical M4 text hash (store.rs `Enqueue`), not the
    // ciphertext's: the holder finds the record by the entry's logical key, never by hash.
    assert_ne!(held.hash, ct_hash);
    assert_eq!(notify_count(&a1), 0, "my own text is not notified");
}

/// A plaintext record of a message this device already has changes nothing: neither the same
/// record again nor one with another body.
#[tokio::test]
async fn plaintext_for_existing_message_is_ignored() {
    let (mut a, b1, _net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let key = delivered(&b1, &a2, &[&a1], 1).await;
    settle!(
        [&a1, &a2, &b1],
        message(&a1, key).await.is_some() && message(&a2, key).await.is_some()
    );
    let seq = logged(&a2, key).await;
    let holder = a2.account.storage_test_fixture().messages().await.unwrap();
    let record = holder.mailbox_plain(seq).await.unwrap().expect("a record");
    assert!(record.body == "text 1");
    let facts = a1
        .account
        .storage_test_fixture()
        .message_facts(key)
        .await
        .unwrap();
    let before = (message(&a1, key).await.unwrap(), unread(&a1, conv).await);
    let puller = a1.account.storage_test_fixture().messages().await.unwrap();
    let mut other = record.clone();
    other.body = "another body".into();
    for record in [record, other] {
        let stored = puller
            .mailbox_store_plain(record, b1.peer, facts.hash, NOW * 1000)
            .await
            .unwrap();
        assert!(!stored);
    }
    let after = (message(&a1, key).await.unwrap(), unread(&a1, conv).await);
    assert_eq!(after.0.body, before.0.body);
    assert_eq!(after.0.lamport, before.0.lamport);
    assert_eq!(after.1, before.1);
}

/// Another account's device asking for a plaintext record gets `Refused`, and no byte of the
/// body; an own device asking for the same entry gets the record.
#[tokio::test]
async fn foreign_account_plain_request_is_refused() {
    let (mut a, b1, _net, _conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let key = send(&b1, &a2, 1, SENTINEL).await.key;
    settle!(
        [&a1, &a2, &b1],
        delivery(&b1, key).await == DeliveryState::Delivered
            && mailbox(&a2)
                .await
                .entries
                .iter()
                .any(|e| e.msg_id == Some(key.id))
    );
    let seq = logged(&a2, key).await;
    let a2_id = installation(&a2).await;
    let own = a1
        .account
        .mailbox_test_exchange(a2_id, MailboxFrame::PlainReq { seq })
        .await
        .unwrap();
    assert!(matches!(&own, MailboxReply::Plain(record) if record.body == SENTINEL));
    let reply = b1
        .account
        .mailbox_test_exchange(a2_id, MailboxFrame::PlainReq { seq })
        .await
        .unwrap();
    assert!(reply == MailboxReply::Refused);
    assert!(!contains(&reply.encode(), SENTINEL.as_bytes()));
}

/// Review Focus 3: from the moment A2 revokes A3, A3's session that is still open gets nothing
/// for `SyncReq`, `PlainReq` or `Handoff` (it is refused, or the connection is gone), no byte of
/// a body, and A2 keeps nothing for it.
#[tokio::test]
async fn revoked_device_open_session_gets_nothing() {
    let (mut a, b1, _net, _conv) = accounts(&[A2, A3], &[B1]).await;
    let a3 = a.pop().unwrap();
    let a2 = a.pop().unwrap();
    let key = send(&b1, &a2, 1, SENTINEL).await.key;
    settle!(
        [&a2, &a3, &b1],
        delivery(&b1, key).await == DeliveryState::Delivered
            && mailbox(&a2)
                .await
                .entries
                .iter()
                .any(|e| e.msg_id == Some(key.id))
    );
    let seq = logged(&a2, key).await;
    let (a2_id, a3_id) = (installation(&a2).await, installation(&a3).await);
    // Before the revoke the entry is A3's to have.
    let before = a3
        .account
        .mailbox_test_exchange(a2_id, MailboxFrame::PlainReq { seq })
        .await
        .unwrap();
    assert!(matches!(&before, MailboxReply::Plain(record) if record.body == SENTINEL));
    let frames = || {
        [
            MailboxFrame::SyncReq { since_seq: 0 },
            MailboxFrame::PlainReq { seq },
            MailboxFrame::Handoff {
                entries: vec![HandoffEntry {
                    conversation: key.conversation,
                    peer: b1.peer,
                    msg_id: key.id,
                    ciphertext: vec![1],
                }],
            },
        ]
    };
    // An error is a connection that is gone (A2 closes it when the revoke is applied).
    let refused = |reply: Result<MailboxReply, chat_core::CoreError>| {
        if let Ok(reply) = reply {
            assert!(
                reply == MailboxReply::Refused,
                "a revoked device got an answer"
            );
            assert!(!contains(&reply.encode(), SENTINEL.as_bytes()));
        }
    };
    a2.account.revoke_device(a3_id).await.unwrap();
    for frame in frames() {
        refused(a3.account.mailbox_test_exchange(a2_id, frame).await);
    }
    // Once A2 dropped what it kept for A3, asking again writes nothing back.
    settle!(
        [&a2, &a3, &b1],
        !mailbox(&a2).await.cursors.iter().any(|(i, _)| *i == a3_id)
    );
    for frame in frames() {
        refused(a3.account.mailbox_test_exchange(a2_id, frame).await);
    }
    quiet(&[&a2, &a3, &b1]).await;
    let kept = mailbox(&a2).await;
    assert!(!kept.cursors.iter().any(|(i, _)| *i == a3_id));
    assert!(!kept.pulls.iter().any(|(i, _)| *i == a3_id));
    assert_eq!(kept.handoffs, 0);
}

/// The holder no longer has the text (its conversation was deleted there): that entry is
/// skipped and counted, the next one is stored, and the cursor moves past both.
#[tokio::test]
async fn missing_plaintext_is_counted_not_fatal() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let dir = stop(a.pop().unwrap());
    let gone = delivered(&b1, &a2, &[], 1).await;
    let kept = delivered(&b1, &a2, &[], 2).await;
    a2.account
        .storage_test_fixture()
        .drop_message(gone)
        .await
        .unwrap();
    let a1 = back_without_group(dir, &net, conv, &[&a2, &b1]).await;
    settle!(
        [&a1, &a2, &b1],
        message(&a1, kept).await.is_some() && caught_up(&a1, &a2).await
    );
    quiet(&[&a1, &a2, &b1]).await;
    assert!(message(&a1, gone).await.is_none());
    assert_eq!(message(&a1, kept).await.unwrap().body, "text 2");
    assert_eq!(a1.account.mailbox_test_counters().await.unwrap().missing, 1);
}

/// Final review I3: while A1 is off, A3 sends a text and A2 takes its copy; then A3 is revoked
/// (a lost device), and A1 learns it before it pulls. A1 can still open the logged ciphertext,
/// but not take it: A3 is not a current device. It is not skipped either: A2 took it while A3
/// was current and hands it over as plaintext (not logged on A1, nothing counted missing).
#[tokio::test]
async fn text_sealed_by_since_revoked_device_comes_as_plaintext() {
    let (mut a, b1, net, _conv) = accounts(&[A1, A2, A3], &[B1]).await;
    let a3 = a.pop().unwrap();
    let a2 = a.pop().unwrap();
    let dir = stop(a.pop().unwrap());
    let key = delivered(&a3, &b1, &[&a2], 1).await;
    settle!([&a2, &a3, &b1], message(&a2, key).await.is_some());
    let a3_id = installation(&a3).await;
    drop(stop(a3));
    let a1 = start(dir, net.clone()).await;
    a1.account.revoke_device(a3_id).await.unwrap();
    link(&a1, &a2).await;
    link(&a1, &b1).await;
    a1.account.mailbox_sync_now().await.unwrap();
    settle!(
        [&a1, &a2, &b1],
        message(&a1, key).await.is_some() && caught_up(&a1, &a2).await
    );
    let got = message(&a1, key).await.unwrap();
    assert_eq!(got.body, "text 1");
    assert!(got.outgoing, "my own text");
    assert!(
        !mailbox(&a1)
            .await
            .entries
            .iter()
            .any(|e| e.kind == 1 && e.msg_id == Some(key.id)),
        "stored from plaintext, not opened"
    );
    assert_eq!(a1.account.mailbox_test_counters().await.unwrap().missing, 0);
}

/// Task 5 carry-over: the conversation was made while A1 was off, so A1 has no group for it
/// (a logged Welcome is not joined from a log). The texts in A2's log reach the plaintext
/// hand-over instead of being dropped, and A1 ends with them.
#[tokio::test]
async fn conversation_created_while_device_was_off_ends_with_text() {
    created_while_off(true).await;
}

/// Ruling 25: as above, but A1 never fetched B's record. A2's own Commit adding A1, in A2's
/// log, cannot be vetted on A1: it is held for a record lookup and the log goes on behind it.
#[tokio::test]
async fn logged_commit_without_peer_record_does_not_hold_up_the_log() {
    created_while_off(false).await;
}

/// B1 and A2 make the conversation while A1 is off; A1 comes back and pulls A2's log only once
/// A2's own Commit adding A1 is in it (A2 delivers nothing to A1 directly). `record_known`: A1 had
/// B's record before it went off (as a device with B as a contact does).
async fn created_while_off(record_known: bool) {
    let (ra, rb) = (record(A, &[A1, A2], 0), record(B, &[B1], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let a2 = device(A, A2, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &a2).await;
    for x in [&a1, &a2] {
        link(x, &b1).await;
        befriend(x, &b1).await;
    }
    befriend(&b1, &a2).await;
    if record_known {
        a1.account.connect_account(b1.peer).await.unwrap();
    }
    // A2 keeps A1's place in its log from before anything is logged.
    let (a1_id, a2_id) = (installation(&a1).await, installation(&a2).await);
    settle!(
        [&a1, &a2, &b1],
        mailbox(&a2).await.cursors.iter().any(|(i, _)| *i == a1_id)
    );
    let dir = stop(a1);
    a2.account.connect_account(b1.peer).await.unwrap();
    let conv = conversation(&b1, &a2).await;
    conversation(&a2, &b1).await;
    ensure(&b1, a2.peer).await;
    settle!([&a2, &b1], agreed(&a2, &b1, conv).await);
    let key = delivered(&b1, &a2, &[], 1).await;
    a2.account.mls_test_hold_delivery(true).await.unwrap();
    let a1 = start(dir, net.clone()).await;
    a1.account.mailbox_test_hold(true).await.unwrap();
    link(&a1, &a2).await;
    link(&a1, &b1).await;
    a1.account.mailbox_sync_now().await.unwrap();
    settle!(
        [&a1, &a2, &b1],
        mailbox(&a2)
            .await
            .entries
            .iter()
            .any(|e| e.sender_install == a2_id)
    );
    a1.account.mailbox_test_hold(false).await.unwrap();
    a1.account.mailbox_sync_now().await.unwrap();
    settle!(
        [&a1, &a2, &b1],
        message(&a1, key).await.is_some() && caught_up(&a1, &a2).await
    );
    assert_eq!(message(&a1, key).await.unwrap().body, "text 1");
}
