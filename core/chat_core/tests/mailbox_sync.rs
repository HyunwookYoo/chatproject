#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Own-device sync through the actors (M6, spec 6.1–6.2, 11): a device that was off gets, from
//! another device of its account, what that device received and sent meanwhile, in order and
//! once, with one notification per sync.
include!("support/mailbox_network.rs");

/// Spec 11 (actor path): A = [A1 phone, A2 PC], B = [B1]. A1 is off while B1 sends five texts,
/// which A2 takes. A1 comes back and gets them from A2: in order, once each, and one
/// notification for the conversation.
#[tokio::test]
async fn phone_gets_what_mailbox_kept_in_order_once() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let dir = stop(a.pop().unwrap());
    let mut keys = Vec::new();
    for n in 1..=5 {
        keys.push(delivered(&b1, &a2, &[], n).await);
    }
    let a1 = on(dir, &net, &[&a2, &b1]).await;
    settle!(
        [&a1, &a2, &b1],
        texts_from(&a1, conv, b1.peer).await.len() == 5 && caught_up(&a1, &a2).await
    );
    quiet(&[&a1, &a2, &b1]).await;
    let got: Vec<MessageKey> = texts_from(&a1, conv, b1.peer)
        .await
        .iter()
        .map(|m| m.key)
        .collect();
    assert_eq!(got, shown(&b1, conv).await, "B1's order, each once");
    let bodies: Vec<String> = texts_from(&a1, conv, b1.peer)
        .await
        .into_iter()
        .map(|m| m.body)
        .collect();
    assert_eq!(
        bodies,
        (1..=5).map(|n| format!("text {n}")).collect::<Vec<_>>()
    );
    assert_eq!(notify_count(&a1), 1, "one notification");
    assert_eq!(
        notify_events(&a1),
        vec![NotifyItem {
            conversation: conv,
            peer: b1.peer,
            new_messages: 5,
            unread: 5
        }]
    );
}

/// A = [A1, A2, A3]: A2 and A3 both log the same ciphertext (one got it from B1, the other
/// pulled it). A1 pulls from both and stores it once.
#[tokio::test]
async fn two_mailboxes_same_ciphertext_store_once() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2, A3], &[B1]).await;
    let a3 = a.pop().unwrap();
    let a2 = a.pop().unwrap();
    let dir = stop(a.pop().unwrap());
    let key = delivered(&b1, &a2, &[&a3], 1).await;
    // The device that did not get it from B1 pulls it (after the other's `Notify`).
    settle!([&a2, &a3, &b1], {
        let (x, y) = (mailbox(&a2).await, mailbox(&a3).await);
        x.entries.iter().any(|e| e.msg_id == Some(key.id))
            && y.entries.iter().any(|e| e.msg_id == Some(key.id))
    });
    let a1 = on(dir, &net, &[&a2, &a3, &b1]).await;
    settle!(
        [&a1, &a2, &a3, &b1],
        !texts_from(&a1, conv, b1.peer).await.is_empty()
            && caught_up(&a1, &a2).await
            && caught_up(&a1, &a3).await
    );
    quiet(&[&a1, &a2, &a3, &b1]).await;
    let got = texts_from(&a1, conv, b1.peer).await;
    assert_eq!(got.len(), 1, "one row");
    assert_eq!(got[0].key, key);
    let new: u32 = notify_events(&a1).iter().map(|i| i.new_messages).sum();
    assert_eq!(new, 1, "notified once");
}

/// While A1 is off, B adds a device (a Commit) between B1's texts. A1 opens the texts of the
/// old epoch, applies the Commit, then opens those of the new one: nothing it could not open.
#[tokio::test]
async fn entries_are_processed_in_seq_order_across_a_commit() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1, B2]).await;
    let a2 = a.pop().unwrap();
    let dir = stop(a.pop().unwrap());
    let mut keys = vec![
        delivered(&b1, &a2, &[], 1).await,
        delivered(&b1, &a2, &[], 2).await,
    ];
    let before = state(&a2, conv).await.group_epoch.unwrap();
    let epoch = b1
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_commit(
            conv,
            a2.peer,
            vec![mls_detached_key_package(b1.peer, [B2; 32])],
            vec![],
            NOW * 1000,
        )
        .await
        .unwrap()
        .expect("an own Commit");
    assert_eq!(epoch, before + 1);
    settle!(
        [&a2, &b1],
        state(&a2, conv).await.group_epoch == Some(epoch)
    );
    // A1 gets the Commit only from A2's log.
    b1.account.mls_test_hold_delivery(true).await.unwrap();
    keys.push(delivered(&b1, &a2, &[], 3).await);
    keys.push(delivered(&b1, &a2, &[], 4).await);
    let a1 = on(dir, &net, &[&a2, &b1]).await;
    settle!(
        [&a1, &a2, &b1],
        texts_from(&a1, conv, b1.peer).await.len() == 4 && caught_up(&a1, &a2).await
    );
    assert_eq!(state(&a1, conv).await.group_epoch, Some(epoch));
    let got: Vec<MessageKey> = texts_from(&a1, conv, b1.peer)
        .await
        .iter()
        .map(|m| m.key)
        .collect();
    assert_eq!(got, keys);
    // Every page was processed whole (an entry it could not open would hold the cursor).
    let new: u32 = notify_events(&a1).iter().map(|i| i.new_messages).sum();
    assert_eq!(new, 4);
}

/// Review Focus 2: A1 dies right after storing the third entry of a page. Restarted, it pulls
/// the page again: every text once, and one notification in all.
#[tokio::test]
async fn restart_mid_page_resumes_without_duplicates_or_second_notification() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let dir = stop(a.pop().unwrap());
    for n in 1..=5 {
        delivered(&b1, &a2, &[], n).await;
    }
    let a1 = start(dir, net.clone()).await;
    a1.account.mailbox_test_halt_after(3).await.unwrap();
    link(&a1, &a2).await;
    link(&a1, &b1).await;
    a1.account.mailbox_sync_now().await.unwrap();
    settle!(
        [&a1, &a2, &b1],
        texts_from(&a1, conv, b1.peer).await.len() == 3
    );
    quiet(&[&a1, &a2, &b1]).await;
    assert_eq!(
        texts_from(&a1, conv, b1.peer).await.len(),
        3,
        "stopped there"
    );
    let first_run = notify_count(&a1);
    assert_eq!(first_run, 0, "a pull that did not end notifies nothing");
    let a1 = on(stop(a1), &net, &[&a2, &b1]).await;
    settle!(
        [&a1, &a2, &b1],
        texts_from(&a1, conv, b1.peer).await.len() == 5 && caught_up(&a1, &a2).await
    );
    quiet(&[&a1, &a2, &b1]).await;
    let got: Vec<MessageKey> = texts_from(&a1, conv, b1.peer)
        .await
        .iter()
        .map(|m| m.key)
        .collect();
    assert_eq!(got, shown(&b1, conv).await, "each once, in order");
    assert_eq!(first_run + notify_count(&a1), 1, "one notification in all");
}

/// Spec 11 (both ways): A2 is off while A1 and B1 talk. A2 comes back and has the six texts in
/// their order; those A1 sent are A2's own sent messages.
#[tokio::test]
async fn pc_gets_what_phone_exchanged_while_pc_was_off() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let dir = stop(a.pop().unwrap());
    let a1 = a.pop().unwrap();
    for n in 1..=3 {
        delivered(&a1, &b1, &[], 2 * n).await;
        delivered(&b1, &a1, &[], 2 * n + 1).await;
    }
    let a2 = on(dir, &net, &[&a1, &b1]).await;
    settle!(
        [&a1, &a2, &b1],
        page(&a2, conv).await.len() == 6 && caught_up(&a2, &a1).await
    );
    assert_eq!(shown(&a2, conv).await, shown(&a1, conv).await, "same order");
    for m in page(&a2, conv).await {
        assert_eq!(m.outgoing, m.key.sender == a1.peer, "{:?}", m.key);
    }
}

/// Spec 11 (third device): A = [A1, A2, A3]. While A2 and A3 are off, A1 and B1 talk. A2 comes
/// back and pulls from A1; A1 goes off; A3 comes back and, from A2 alone, has everything.
#[tokio::test]
async fn third_device_gets_conversation_from_any_device() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2, A3], &[B1]).await;
    let dir3 = stop(a.pop().unwrap());
    let dir2 = stop(a.pop().unwrap());
    let a1 = a.pop().unwrap();
    for n in 1..=2 {
        delivered(&a1, &b1, &[], 2 * n).await;
        delivered(&b1, &a1, &[], 2 * n + 1).await;
    }
    let a2 = on(dir2, &net, &[&a1, &b1]).await;
    settle!(
        [&a1, &a2, &b1],
        page(&a2, conv).await.len() == 4 && caught_up(&a2, &a1).await
    );
    let expected = shown(&a1, conv).await;
    drop(stop(a1));
    let a3 = on(dir3, &net, &[&a2, &b1]).await;
    settle!(
        [&a2, &a3, &b1],
        page(&a3, conv).await.len() == 4 && caught_up(&a3, &a2).await
    );
    assert_eq!(shown(&a3, conv).await, expected);
}

/// Another account's device asking for the log is refused, and nothing is read or written.
#[tokio::test]
async fn foreign_account_sync_request_is_refused() {
    let (mut a, b1, _net, _conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a2_id = installation(&a2).await;
    let before = mailbox(&a2).await;
    for frame in [
        MailboxFrame::SyncReq { since_seq: 0 },
        MailboxFrame::Notify { head_seq: 1 },
    ] {
        let reply = b1
            .account
            .mailbox_test_exchange(a2_id, frame)
            .await
            .unwrap();
        assert!(reply == MailboxReply::Refused);
    }
    let after = mailbox(&a2).await;
    assert_eq!(after.cursors, before.cursors);
    assert_eq!(after.pulls, before.pulls);
    let b1_id = installation(&b1).await;
    assert!(!after.cursors.iter().any(|(i, _)| *i == b1_id));
}

/// A logged entry this device cannot open is never answered to its sender: B1 sends nothing
/// again and A1 sends B1 nothing. It goes to the plaintext hand-over instead, where the holder
/// has no record of it (it names no message): it is skipped and counted, and the cursor passes.
#[tokio::test]
async fn mailbox_entry_failure_sends_no_decrypt_failed_to_sender() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let dir = stop(a.pop().unwrap());
    let key = delivered(&b1, &a2, &[], 1).await;
    // A2's log gets B1's ciphertext with one byte changed after it: nothing can open it.
    let mut broken = app_ciphertexts(&b1).await.pop().unwrap();
    let last = broken.len() - 1;
    broken[last] ^= 0x55;
    let b1_id = installation(&b1).await;
    let body = MlsFrame::App {
        conversation: conv,
        ciphertext: broken,
    }
    .inner()
    .to_vec();
    let seq = a2
        .account
        .storage_test_fixture()
        .mailbox_record_as(conv, b1.peer, b1_id, body, NOW * 1000)
        .await
        .unwrap()
        .unwrap();
    let sent_before = b1.account.mls_test_sent_frames().await.unwrap();
    let a1 = on(dir, &net, &[&a2, &b1]).await;
    settle!(
        [&a1, &a2, &b1],
        texts_from(&a1, conv, b1.peer).await.len() == 1 && caught_up(&a1, &a2).await
    );
    quiet(&[&a1, &a2, &b1]).await;
    assert_eq!(texts_from(&a1, conv, b1.peer).await[0].key, key);
    assert_eq!(
        b1.account.mls_test_sent_frames().await.unwrap(),
        sent_before
    );
    assert_eq!(delivery(&b1, key).await, DeliveryState::Delivered);
    assert!(
        a1.account
            .mls_test_sent_frames_to(b1_id)
            .await
            .unwrap()
            .is_empty()
    );
    let a2_id = installation(&a2).await;
    let since = mailbox(&a1)
        .await
        .pulls
        .iter()
        .find(|(i, _)| *i == a2_id)
        .map_or(0, |(_, since)| *since);
    assert!(since > seq, "the unopened entry is passed");
    assert_eq!(a1.account.mailbox_test_counters().await.unwrap().missing, 1);
}
