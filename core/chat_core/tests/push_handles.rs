#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! PushHandle lifecycle (M7 spec 5.1, 5.2, 8): a device issues a handle per conversation at the
//! proxy, announces it to the peer's devices over MLS, renews it after 7 days and on a new
//! token, withdraws it when push goes off, hands queued frames to its mailbox, carries its
//! own-device handle in the kind 18 `Announce`, and drops what revoked devices announced.
include!("support/push_network.rs");

/// More accounts and their one installation, for a device with several conversations.
const C: u8 = 3;
const D: u8 = 4;
const C1: u8 = 16;
const D1: u8 = 17;

#[tokio::test]
async fn first_token_issues_and_announces_handle_per_conversation() {
    let (a1, b1, net, conv) = setup(&[B1]).await;
    grouped(&a1, &b1, conv).await;
    let proxy = proxy(&net);
    let a1_id = installation(&a1).await;
    android(&a1, "token-a1").await;
    settle!(
        [&a1, &b1],
        peer_handle(&b1, conv, a1_id).await.flatten().is_some()
            && push_state(&a1).await.deliveries.is_empty()
    );
    let mine = push_state(&a1).await;
    assert_eq!(mine.tags.len(), 1);
    let tag = &mine.tags[0];
    assert!(tag.announced);
    assert_eq!(peer_handle(&b1, conv, a1_id).await.flatten(), tag.handle);
    // One conversation, and no other own device to need an own-device handle.
    assert_eq!(proxy.handle_requests().len(), 1);
    assert_eq!(asked_for(&proxy, &tag.tag), 1);
    let plain = proxy.open(tag.handle.as_ref().unwrap());
    assert_eq!(plain.token, "token-a1");
    assert_eq!(plain.tag, tag.tag);
    assert_eq!(plain.platform, Platform::Fcm);
    assert_eq!(peer_rows(&b1).await.len(), 1);
    let status = a1.account.push_status().await.unwrap();
    assert!(status.enabled);
    assert_eq!(status.platform, Some(Platform::Fcm));
    assert_eq!(status.handles, 1);
    assert_eq!(status.oldest_issued_at_ms, tag.issued_at_ms);
    assert!(proxy.push_requests().is_empty());
}

#[tokio::test]
async fn renewal_after_seven_days_only() {
    let (a1, b1, net, conv) = setup(&[B1]).await;
    grouped(&a1, &b1, conv).await;
    let proxy = proxy(&net);
    let a1_id = installation(&a1).await;
    android(&a1, "token-a1").await;
    settle!(
        [&a1, &b1],
        peer_handle(&b1, conv, a1_id).await.flatten().is_some()
    );
    let first = own_tag_handle(&a1, conv).await.unwrap();
    let issued = push_state(&a1).await.tags[0].issued_at_ms.unwrap();

    // 6 days 23 hours later nothing is due.
    let summary = a1
        .account
        .push_renew_due(issued + RENEW_AFTER_MS - 3_600_000)
        .await
        .unwrap();
    assert_eq!(summary, RenewSummary::default());
    assert_eq!(proxy.handle_requests().len(), 1);

    // 7 days: one new handle, announced.
    let summary = a1
        .account
        .push_renew_due(issued + RENEW_AFTER_MS)
        .await
        .unwrap();
    assert_eq!(
        summary,
        RenewSummary {
            issued: 1,
            announced: 1,
            withdrawn: 0
        }
    );
    assert_eq!(proxy.handle_requests().len(), 2);
    let renewed = own_tag_handle(&a1, conv).await.unwrap();
    assert_ne!(renewed, first);
    assert_eq!(
        push_state(&a1).await.tags[0].issued_at_ms,
        Some(issued + RENEW_AFTER_MS)
    );
    settle!(
        [&a1, &b1],
        peer_handle(&b1, conv, a1_id).await == Some(Some(renewed.clone()))
    );
    // Asked again at the same time: nothing more.
    let summary = a1
        .account
        .push_renew_due(issued + RENEW_AFTER_MS)
        .await
        .unwrap();
    assert_eq!(summary, RenewSummary::default());
    assert_eq!(proxy.handle_requests().len(), 2);
}

/// Review Focus 3: a new token issues every conversation's handle again, and each peer gets
/// the new one.
#[tokio::test]
async fn token_change_reissues_every_conversation_handle() {
    let ra = record(A, &[A1], 0);
    let others = [(B, B1), (C, C1), (D, D1)];
    let mut records = vec![(A, ra.clone())];
    records.extend(others.iter().map(|(acct, seed)| (*acct, record(*acct, &[*seed], 0))));
    let net = backend(&records);
    let a1 = device(A, A1, &ra, &net).await;
    let mut peers = Vec::new();
    for (acct, seed) in others {
        let x = device(acct, seed, &record(acct, &[seed], 0), &net).await;
        link(&a1, &x).await;
        befriend(&a1, &x).await;
        befriend(&x, &a1).await;
        a1.account.connect_account(x.peer).await.unwrap();
        let conv = conversation(&a1, &x).await;
        conversation(&x, &a1).await;
        grouped(&a1, &x, conv).await;
        peers.push((x, conv));
    }
    let proxy = proxy(&net);
    let a1_id = installation(&a1).await;
    let mut all = vec![&a1];
    all.extend(peers.iter().map(|(x, _)| x));
    let token_at = |token: &'static str| {
        let proxy = proxy.clone();
        move |handle: Option<Option<Vec<u8>>>| {
            handle
                .flatten()
                .is_some_and(|h| proxy.open(&h).token == token)
        }
    };

    android(&a1, "token-1").await;
    let first = token_at("token-1");
    settle!(all.iter().copied(), {
        let mut ok = true;
        for (x, conv) in &peers {
            ok = ok && first(peer_handle(x, *conv, a1_id).await);
        }
        ok
    });
    assert_eq!(proxy.handle_requests().len(), 3);

    a1.account
        .push_set_token(Platform::Fcm, "token-2".into())
        .await
        .unwrap();
    let second = token_at("token-2");
    settle!(all.iter().copied(), {
        let mut ok = true;
        for (x, conv) in &peers {
            ok = ok && second(peer_handle(x, *conv, a1_id).await);
        }
        ok
    });
    assert_eq!(proxy.handle_requests().len(), 6);
    for (x, conv) in &peers {
        assert_eq!(
            peer_handle(x, *conv, a1_id).await.flatten(),
            own_tag_handle(&a1, *conv).await
        );
    }
    // The same token again changes nothing.
    a1.account
        .push_set_token(Platform::Fcm, "token-2".into())
        .await
        .unwrap();
    quiet(&all).await;
    assert_eq!(proxy.handle_requests().len(), 6);
}

/// Review Focus 4: push goes off while a renewal's `/handle` reply is held. The reply, released
/// afterwards, stores nothing; B1 keeps a withdrawal; the proxy is asked nothing after that.
#[tokio::test]
async fn turning_push_off_mid_renewal_sends_nothing_after() {
    let (a1, b1, net, conv) = setup(&[B1]).await;
    grouped(&a1, &b1, conv).await;
    let proxy = proxy(&net);
    let a1_id = installation(&a1).await;
    android(&a1, "token-a1").await;
    settle!(
        [&a1, &b1],
        peer_handle(&b1, conv, a1_id).await.flatten().is_some()
            && push_state(&a1).await.deliveries.is_empty()
    );
    let issued = push_state(&a1).await.tags[0].issued_at_ms.unwrap();

    proxy.hold(true);
    let account = a1.account.clone();
    let renewal =
        tokio::spawn(async move { account.push_renew_due(issued + RENEW_AFTER_MS).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while proxy.handle_requests().len() < 2 {
        assert!(tokio::time::Instant::now() < deadline, "renewal never asked");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    a1.account.push_set_enabled(false).await.unwrap();
    settle!(
        [&a1, &b1],
        peer_handle(&b1, conv, a1_id).await == Some(None)
    );
    proxy.hold(false);
    let summary = renewal.await.unwrap().unwrap();
    assert_eq!(summary.issued, 0);

    let mine = push_state(&a1).await;
    assert!(!mine.enabled);
    assert_eq!(mine.token, None);
    assert_eq!(mine.own_handle, None);
    assert!(mine.tags.iter().all(|t| t.handle.is_none() && t.announced));
    // The withdrawal was sent later than the handle it withdraws.
    assert!(mine.tags[0].issued_at_ms.unwrap() > issued);
    assert!(!a1.account.push_status().await.unwrap().enabled);

    // Nothing reaches the proxy while push is off: no renewal, and a token is not taken.
    quiet(&[&a1, &b1]).await;
    let summary = a1
        .account
        .push_renew_due(issued + 3 * RENEW_AFTER_MS)
        .await
        .unwrap();
    assert_eq!(summary.issued, 0);
    a1.account
        .push_set_token(Platform::Fcm, "token-later".into())
        .await
        .unwrap();
    quiet(&[&a1, &b1]).await;
    assert_eq!(proxy.handle_requests().len(), 2);
    assert!(proxy.push_requests().is_empty());
    assert_eq!(peer_handle(&b1, conv, a1_id).await, Some(None));
    assert_eq!(push_state(&a1).await.token, None);
}

/// M6 spec 7: B1 is off when A1 renews; A1 hands the queued frame to its mailbox A2 and goes
/// off itself; B1 comes back and gets A1's new handle from A2.
#[tokio::test]
async fn renewal_reaches_offline_contact_through_my_mailbox() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let proxy = proxy(&net);
    let (a1_id, a2_id, b1_id) = (
        installation(&a1).await,
        installation(&a2).await,
        installation(&b1).await,
    );
    a2.account.set_mailbox(true).await.unwrap();
    a1.account
        .storage_test_fixture()
        .mailbox_mark(a2_id, true, NOW * 1000)
        .await
        .unwrap();
    let b_dir = stop(b1);

    android(&a1, "token-a1").await;
    settle!(
        [&a1, &a2],
        push_state(&a1)
            .await
            .deliveries
            .iter()
            .any(|d| d.conversation == conv && d.recipient == b1_id)
    );
    let handle = own_tag_handle(&a1, conv).await.unwrap();
    assert!(a1.account.mailbox_background().await.unwrap() >= 1);
    assert!(push_state(&a1).await.deliveries.iter().all(|d| d.handed_off));
    settle!([&a1, &a2], !mailbox(&a2).await.handoff_rows.is_empty());
    let _a1_dir = stop(a1);

    let b1 = start_as(B, b_dir, net.clone()).await;
    link(&b1, &a2).await;
    settle!(
        [&a2, &b1],
        peer_handle(&b1, conv, a1_id).await == Some(Some(handle.clone()))
    );
    assert!(!proxy.handle_requests().is_empty());
}

/// Spec 5.2: an Android device's own-device handle rides its `Announce`, and my other device
/// keeps it; an M6 `Announce` (three fields) still decodes.
#[tokio::test]
async fn own_handle_rides_announce_and_m6_announce_still_decodes() {
    let (mut a, b1, net, _conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let proxy = proxy(&net);
    let a1_id = installation(&a1).await;
    android(&a1, "token-a1").await;
    settle!([&a1, &a2, &b1], {
        let own = push_state(&a1).await.own_handle;
        own.is_some() && push_state(&a2).await.own == vec![(a1_id, own.unwrap())]
    });
    let mine = push_state(&a1).await;
    // One conversation and the own-device handle.
    assert_eq!(proxy.handle_requests().len(), 2);
    assert_eq!(asked_for(&proxy, &mine.own_tag), 1);
    let plain = proxy.open(mine.own_handle.as_ref().unwrap());
    assert_eq!(plain.tag, mine.own_tag);
    assert_ne!(mine.own_tag, mine.tags[0].tag);
    // A device without a handle announces none: A1 keeps nothing for A2.
    assert!(mine.own.is_empty());

    // M6 sends `[1, 4, mailbox]`; M7 `[1, 4, mailbox, handle or null]`.
    let m6 = [0x83, 0x01, 0x04, 0xf5];
    assert!(
        MailboxFrame::decode(&m6).unwrap()
            == MailboxFrame::Announce {
                mailbox: true,
                handle: None
            }
    );
    let with = MailboxFrame::Announce {
        mailbox: false,
        handle: Some(vec![7; 40]),
    };
    assert!(MailboxFrame::decode(&with.inner()).unwrap() == with);
    let without = MailboxFrame::Announce {
        mailbox: true,
        handle: None,
    };
    assert_eq!(*without.inner(), [0x84, 0x01, 0x04, 0xf5, 0xf6]);
    assert!(MailboxFrame::decode(&without.inner()).unwrap() == without);
    for bad in [
        &[0x84, 0x01, 0x04, 0xf5, 0x40][..],
        &[0x85, 0x01, 0x04, 0xf5, 0xf6, 0xf6][..],
        &[0x82, 0x01, 0x04][..],
    ] {
        assert!(MailboxFrame::decode(bad).is_err());
    }

    // Push off: my other device forgets the own-device handle.
    a1.account.push_set_enabled(false).await.unwrap();
    settle!(
        [&a1, &a2, &b1],
        push_state(&a2).await.own.is_empty()
    );
}

/// Spec 5.2: what a device announced goes once it is revoked (M3) or no longer listed.
#[tokio::test]
async fn revoked_device_handles_are_dropped() {
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1, B2]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let _proxy = proxy(&net);
    let (a1_id, b1_id) = (installation(&a1).await, installation(&b1).await);
    android(&a1, "token-a1").await;
    android(&b1, "token-b1").await;
    settle!(
        [&a1, &a2, &b1],
        push_state(&a2).await.own.iter().any(|(i, _)| *i == a1_id)
            && peer_handle(&a1, conv, b1_id).await.flatten().is_some()
            && peer_handle(&a2, conv, b1_id).await.flatten().is_some()
    );

    // B's record no longer lists B1, and A1 takes it.
    let newer = record(B, &[B2], 1);
    publish(&net, B, &newer);
    if a1.account.refresh_record(b1.peer).await.is_err() {
        a1.account
            .confirm_remote_record(b1.peer, chat_core::account::record_hash(&newer))
            .await
            .unwrap();
    }
    settle!(
        [&a1, &a2],
        peer_handle(&a1, conv, b1_id).await.is_none()
    );

    // A2 revokes A1: the own-device handle A1 announced goes.
    a2.account.revoke_device(a1_id).await.unwrap();
    settle!([&a2], push_state(&a2).await.own.is_empty());
}

/// Spec 3: a background renewal starts the core, configures push and asks for what is due at
/// once; the configuration counts although the actor has not polled since.
#[tokio::test]
async fn renew_due_right_after_configure_issues() {
    let (a1, b1, net, conv) = setup(&[B1]).await;
    grouped(&a1, &b1, conv).await;
    let proxy = proxy(&net);
    let a1_id = installation(&a1).await;
    // Without an address the token is stored but nothing is asked.
    a1.account
        .push_set_token(Platform::Fcm, "token-a1".into())
        .await
        .unwrap();
    a1.account.push_configure(config(Some(Platform::Fcm))).unwrap();
    let summary = a1.account.push_renew_due(NOW * 1000).await.unwrap();
    assert_eq!(
        summary,
        RenewSummary {
            issued: 1,
            announced: 1,
            withdrawn: 0
        }
    );
    assert_eq!(proxy.handle_requests().len(), 1);
    settle!(
        [&a1, &b1],
        peer_handle(&b1, conv, a1_id).await.flatten().is_some()
    );
}

#[tokio::test]
async fn windows_and_disabled_devices_never_call_proxy() {
    let (a1, b1, net, conv) = setup(&[B1]).await;
    grouped(&a1, &b1, conv).await;
    let proxy = proxy(&net);
    // A1 runs on Windows: no platform.
    a1.account.push_configure(config(None)).unwrap();
    // B1 is Android with push turned off before its token comes.
    b1.account
        .push_configure(config(Some(Platform::Fcm)))
        .unwrap();
    b1.account.push_set_enabled(false).await.unwrap();
    b1.account
        .push_set_token(Platform::Fcm, "token-b1".into())
        .await
        .unwrap();
    quiet(&[&a1, &b1]).await;
    let later = NOW * 1000 + 5 * RENEW_AFTER_MS;
    assert_eq!(
        a1.account.push_renew_due(later).await.unwrap(),
        RenewSummary::default()
    );
    assert_eq!(
        b1.account.push_renew_due(later).await.unwrap(),
        RenewSummary::default()
    );
    assert!(proxy.handle_requests().is_empty());
    assert!(proxy.push_requests().is_empty());
    let (a, b) = (
        a1.account.push_status().await.unwrap(),
        b1.account.push_status().await.unwrap(),
    );
    assert_eq!((a.platform, a.handles), (None, 0));
    assert_eq!((b.enabled, b.handles), (false, 0));
    assert_eq!(push_state(&b1).await.token, None);
    assert!(peer_rows(&a1).await.is_empty() && peer_rows(&b1).await.is_empty());
}

#[cfg(feature = "push-http")]
#[test]
fn release_proxy_url_must_be_https() {
    use chat_core::push::{HttpPushProxy, Url, check_proxy_url};
    let new = |url: &str| HttpPushProxy::new(Url::parse(url).unwrap()).map(|_| ());
    assert!(new("https://push.example").is_ok());
    assert!(new("https://push.example/base").is_ok());
    assert_eq!(
        new("ftp://push.example").unwrap_err().code,
        ErrorCode::InvalidConfig
    );
    // Only a debug build may use a local plain-HTTP proxy.
    assert_eq!(new("http://127.0.0.1:1").is_ok(), cfg!(debug_assertions));
    assert_eq!(
        check_proxy_url("http://127.0.0.1:1").is_ok(),
        cfg!(debug_assertions)
    );
    assert!(check_proxy_url("https://push.example").is_ok());
    assert_eq!(
        check_proxy_url("push.example").unwrap_err().code,
        ErrorCode::InvalidConfig
    );
}
