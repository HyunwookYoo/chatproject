#![cfg(all(feature = "test-hooks", debug_assertions))]
include!("support/message_network.rs");

#[tokio::test]
async fn receipt_retries_until_fact_commit_ack() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    wait_state(&a, m.key, DeliveryState::Delivered).await;
    b.clock.advance_monotonic_ms(1_000);
    let applied = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if b.account
                .storage_test_fixture()
                .message_facts(m.key)
                .await
                .unwrap()
                .receipt_jobs
                == 0
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        applied.is_ok(),
        "committed receipt obligation must retry until matching applied ACK"
    );
}

#[tokio::test]
async fn backoff_caps_without_exhaustion() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    wait_state(&a, m.key, DeliveryState::Delivered).await;
    let initial = b
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(initial.receipt_retry, Some((1, NOW * 1000 + 1000)));
    let mut delays = vec![1];
    for attempt in 2..=11 {
        assert!(
            b.account
                .message_test_attempt(m.key, true, MessageMutation::ReceiptHash)
                .await
                .is_err()
        );
        let facts = b
            .account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap();
        let (count, due) = facts.receipt_retry.unwrap();
        assert_eq!(count, attempt);
        delays.push(due.saturating_sub(NOW * 1000) / 1000);
        assert_eq!(facts.receipt_jobs, 1);
    }
    assert_eq!(delays, [1, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
    b.account
        .storage_test_fixture()
        .saturate_message_attempts(m.key, true)
        .await
        .unwrap();
    assert!(
        b.account
            .message_test_attempt(m.key, true, MessageMutation::ReceiptHash)
            .await
            .is_err()
    );
    let facts = b
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(
        facts.receipt_retry,
        Some((u32::MAX as u64, NOW * 1000 + 300000))
    );
    assert_eq!(facts.receipt_jobs, 1);
}

#[tokio::test]
async fn cancel_then_late_receipt_keeps_both_facts() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::FirstByte)
        .await
        .unwrap();
    // Install the checkpoint before eligibility; the scheduler is the sole owner.
    let m = enqueue(&a, &b).await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .expect("the intended attempt must enter the checkpoint");
    let cancelled = a
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .cancel_send(m.key)
        .await
        .unwrap();
    assert!(cancelled.cancel_requested);
    assert_eq!(cancelled.state, DeliveryState::Cancelled);
    assert_eq!(
        a.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .jobs,
        0
    );
    pause.release();
    wait_state(&a, m.key, DeliveryState::Delivered).await;
    let page = a
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .load_messages(m.key.conversation, None, 50)
        .await
        .unwrap();
    assert!(page.items[0].cancel_requested);
    assert_eq!(page.items[0].state, DeliveryState::Delivered);
    assert_eq!(
        a.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .jobs,
        0
    );
    assert!(
        a.account
            .message_test_attempt(m.key, false, MessageMutation::None)
            .await
            .is_err()
    );
}

async fn wait_state(
    f: &Fixture,
    key: chat_core::storage_fixture::MessageKey,
    state: DeliveryState,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let page = f
                .account
                .storage_test_fixture()
                .messages()
                .await
                .unwrap()
                .load_messages(key.conversation, None, 50)
                .await
                .unwrap();
            if page.items.iter().any(|m| m.key == key && m.state == state) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn automatic_due_job_acquires_normal_proof_without_manual_connect() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    let m = enqueue(&a, &b).await;
    wait_state(&a, m.key, DeliveryState::Delivered).await;
    assert!(a.account.message_test_write_starts().await.unwrap().0 >= 1);
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .messages,
        1
    );
}
#[tokio::test]
async fn offline_peer_does_not_starve_other_peer() {
    let (a, b, backend) = pair().await;
    let offline_packet = packet(3, &[13], 3, NOW, NOW + 600, NOW * 1_000_000);
    let offline = Fixture::new(3, 13, &offline_packet, backend.clone()).await;
    backend
        .packets
        .lock()
        .unwrap()
        .insert(offline.peer, offline_packet);
    contacts(&a, &offline).await;
    contacts(&offline, &a).await;
    a.route(&offline).await;
    offline
        .slot
        .network()
        .unwrap()
        .execute(NetworkOp::Stop)
        .await
        .unwrap();
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    let messages = a.account.storage_test_fixture().messages().await.unwrap();
    for id in 0..40 {
        messages
            .enqueue(
                offline.peer,
                MessageId([id; 16]),
                "queued".into(),
                NOW * 1000,
            )
            .await
            .unwrap();
    }
    let online = enqueue(&a, &b).await;
    wait_state(&a, online.key, DeliveryState::Delivered).await;
    let page = messages.list_conversations(None, 50).await.unwrap();
    let blocked = page.items.iter().find(|c| c.peer == offline.peer).unwrap();
    let rows = messages.load_messages(blocked.id, None, 50).await.unwrap();
    assert_eq!(rows.items.len(), 40);
    assert!(
        rows.items
            .iter()
            .all(|m| m.state != DeliveryState::Delivered)
    );
}
#[tokio::test]
async fn ninety_days_preserves_jobs_but_not_auth() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    a.clock.set_unix_seconds(NOW + 90 * 86400);
    b.clock.set_unix_seconds(NOW + 90 * 86400);
    a.clock.advance_monotonic_ms(90 * 86400 * 1000);
    b.clock.advance_monotonic_ms(90 * 86400 * 1000);
    let m = enqueue(&a, &b).await;
    wait_state(&a, m.key, DeliveryState::Blocked).await;
    assert_eq!(
        a.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .jobs,
        1
    );
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    // Normal owners publish new records; old clock/grant state alone cannot resume.
    a.account.refresh_record(a.peer).await.unwrap();
    a.account.refresh_record(a.peer).await.unwrap();
    b.account.refresh_record(b.peer).await.unwrap();
    b.account.refresh_record(b.peer).await.unwrap();
    a.account.connect_account(b.peer).await.unwrap();
    wait_state(&a, m.key, DeliveryState::Delivered).await;
}

async fn lifecycle(f: &Fixture, suspend: bool) {
    if suspend {
        f.account.request_suspend();
    } else {
        f.account.request_resume();
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while f.account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.account.lifecycle_status(), 0);
}
async fn network_start(f: &Fixture) {
    f.slot
        .network()
        .unwrap()
        .execute(NetworkOp::Start)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let NetworkReply::Info(info) = f
                .slot
                .network()
                .unwrap()
                .execute(NetworkOp::Info)
                .await
                .unwrap()
            else {
                panic!()
            };
            if info.state == NetworkState::Online {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn reopen(f: Fixture, backend: Arc<Backend>, now: u64) -> Fixture {
    let Fixture {
        _sql,
        _dir,
        slot,
        account,
        peer,
        ..
    } = f;
    let observer = account.storage_test_fixture();
    drop(slot);
    drop(account);
    drop(_sql);
    assert_eq!(observer.live_connections(), 0);
    let slot = CoreSlot::new();
    let bus = EventBus::new();
    slot.start(
        CoreConfig {
            data_dir: _dir.path().into(),
        },
        &bus,
    )
    .unwrap();
    slot.network().unwrap().configure_local(None).await.unwrap();
    let clock = Arc::new(FakeClock::new(now, 0));
    let account = slot
        .open_account_with_services(
            support::protector(),
            AccountServices::new(backend, clock.clone()),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(account.lifecycle_status(), 0);
    account.snapshot().await.unwrap();
    let f = Fixture {
        _sql: sql_store::Store::from_fixture(account.storage_test_fixture()),
        _dir,
        slot,
        bus,
        account,
        peer,
        clock,
    };
    network_start(&f).await;
    f
}
#[tokio::test]
async fn same_wire_after_lost_ack_and_crash() {
    let (a, b, backend) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let mut pause = b
        .account
        .message_test_pause(chat_core::account::MessagePoint::Response)
        .await
        .unwrap();
    let m = enqueue(&a, &b).await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    let before = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .messages,
        1
    );
    assert!(
        tokio::time::timeout(
            Duration::from_secs(5),
            b.account.message_test_attempt(m.key, true, MessageMutation::ReceiptHash),
        ).await.unwrap().is_err()
    );
    let progress = b
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap()
        .receipt_retry;
    assert_eq!(progress, Some((2, NOW * 1000 + 2000)));
    let a = tokio::time::timeout(Duration::from_secs(5), reopen(a, backend, NOW))
        .await
        .unwrap();
    let recovered = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(recovered.dispatch_state, Some(0));
    assert!(before.wire == recovered.wire);
    assert!(before.hash == recovered.hash && before.prev_hash == recovered.prev_hash);
    a.route(&b).await;
    b.route(&a).await;
    a.clock.advance_monotonic_ms(1000);
    wait_state(&a, m.key, DeliveryState::Delivered).await;
    pause.release();
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .receipt_retry,
        progress,
        "duplicate text must preserve the existing obligation's retry progress"
    );
    let rows = b
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .load_messages(m.key.conversation, None, 50)
        .await
        .unwrap();
    assert_eq!(rows.items.len(), 1);
    assert_eq!(rows.items[0].key, m.key);
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .processed,
        1
    );
}
#[tokio::test]
async fn wall_clock_backward_restart_is_bounded() {
    let (a, b, backend) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    // Automatic delivery could send the message before the pause is armed.
    a.account.message_test_hold_scheduler(true).await.unwrap();
    let m = enqueue(&a, &b).await;
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::FirstByte)
        .await
        .unwrap();
    let account = a.account.clone();
    let attempt = tokio::spawn(async move {
        account
            .message_test_attempt(m.key, false, MessageMutation::None)
            .await
    });
    pause.wait_entered().await;
    let wire = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap()
        .wire;
    let now = NOW - 86400;
    let a = reopen(a, backend, now).await;
    pause.release();
    assert!(attempt.await.unwrap().is_err());
    let recovered = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    let (_, delay, due) = recovered.retry.unwrap();
    assert!(due.saturating_sub(now * 1000) <= delay && delay <= 300000);
    assert!(wire == recovered.wire);
    assert_eq!(recovered.jobs, 1);
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
}
#[tokio::test]
async fn live_attempt_survives_lifecycle_until_actual_completion_without_duplicate() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    // Automatic delivery could send the message before the pause is armed;
    // nothing would then reach the pause and wait_entered would never return.
    a.account.message_test_hold_scheduler(true).await.unwrap();
    let m = enqueue(&a, &b).await;
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::FirstByte)
        .await
        .unwrap();
    let account = a.account.clone();
    let attempt = tokio::spawn(async move {
        account
            .message_test_attempt(m.key, false, MessageMutation::None)
            .await
    });
    pause.wait_entered().await;
    lifecycle(&a, true).await;
    lifecycle(&a, false).await;
    network_start(&a).await;
    a.route(&b).await;
    b.route(&a).await;
    a.clock.advance_monotonic_ms(300000);
    // The actual task is still at its pause. Restore cannot claim completion.
    let facts = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(facts.dispatch_state, Some(1));
    assert_eq!(facts.retry.unwrap().0, 1);
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    // The retry after the failed attempt is automatic.
    a.account.message_test_hold_scheduler(false).await.unwrap();
    pause.release();
    assert!(attempt.await.unwrap().is_err());
    wait_state(&a, m.key, DeliveryState::Delivered).await;
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 1);
}

#[tokio::test]
async fn read_before_delivered_never_regresses() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let mut pause = b
        .account
        .message_test_pause(chat_core::account::MessagePoint::Response)
        .await
        .unwrap();
    let m = enqueue(&a, &b).await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    // A real persisted receipt travels on its authenticated RPC with the Read kind.
    // Its ACK does not match the original Delivered obligation, which must remain.
    assert!(
        tokio::time::timeout(
            Duration::from_secs(5),
            b.account.message_test_attempt(m.key, true, MessageMutation::ReadReceipt),
        ).await.unwrap().is_err()
    );
    wait_state(&a, m.key, DeliveryState::Read).await;
    tokio::time::timeout(
        Duration::from_secs(5),
        b.account.message_test_attempt(m.key, true, MessageMutation::None),
    ).await.unwrap().unwrap();
    wait_state(&a, m.key, DeliveryState::Read).await;
    pause.release();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            // Read has already removed the outbox, so SQL alone cannot tell
            // whether the paused network owner finished. Busy must clear first.
            let code = a.account.message_test_attempt(m.key, false, MessageMutation::None).await.unwrap_err().code;
            if code != ErrorCode::Busy {
                assert_eq!(code, ErrorCode::MessageNotFound);
                break;
            }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    wait_state(&a, m.key, DeliveryState::Read).await;
}
#[tokio::test]
async fn retry_only_releases_current_resolvable_blocked_and_never_recreates_terminal() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    a.account.connect_account(b.peer).await.unwrap();
    // Automatic delivery could take the message before the manual attempt.
    a.account.message_test_hold_scheduler(true).await.unwrap();
    let m = enqueue(&a, &b).await;
    assert_eq!(
        a.account
            .message_test_attempt(m.key, false, MessageMutation::None)
            .await
            .unwrap_err()
            .code,
        ErrorCode::ContactRequired
    );
    let repo = a.account.storage_test_fixture().messages().await.unwrap();
    assert_eq!(
        repo.load_messages(m.key.conversation, None, 50)
            .await
            .unwrap()
            .items[0]
            .block,
        Some(BlockReason::ContactRequired)
    );
    assert_eq!(
        repo.retry_send(m.key, (NOW + 601) * 1000)
            .await
            .unwrap_err()
            .code,
        ErrorCode::AccountRestricted
    );
    assert_eq!(
        repo.load_messages(m.key.conversation, None, 50)
            .await
            .unwrap()
            .items[0]
            .state,
        DeliveryState::Blocked
    );
    contacts(&b, &a).await;
    // The released retry is delivered automatically.
    a.account.message_test_hold_scheduler(false).await.unwrap();
    repo.retry_send(m.key, NOW * 1000).await.unwrap();
    wait_state(&a, m.key, DeliveryState::Delivered).await;
    assert_eq!(
        repo.retry_send(m.key, NOW * 1000).await.unwrap().state,
        DeliveryState::Delivered
    );
    // Hold again so the next message cannot be sent before it is cancelled.
    a.account.message_test_hold_scheduler(true).await.unwrap();
    assert_eq!(
        a.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .jobs,
        0
    );
    let cancelled = repo
        .enqueue(b.peer, MessageId([88; 16]), "cancelled".into(), NOW * 1000)
        .await
        .unwrap();
    repo.cancel_send(cancelled.key).await.unwrap();
    assert_eq!(
        repo.retry_send(cancelled.key, NOW * 1000)
            .await
            .unwrap()
            .state,
        DeliveryState::Cancelled
    );
    assert_eq!(
        a.account
            .storage_test_fixture()
            .message_facts(cancelled.key)
            .await
            .unwrap()
            .jobs,
        0
    );
}

#[tokio::test]
async fn locked_completion_retains_exact_owner_across_failed_settlement() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    // Automatic delivery could send the message before the pause is armed.
    a.account.message_test_hold_scheduler(true).await.unwrap();
    let m = enqueue(&a, &b).await;
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::FirstByte)
        .await
        .unwrap();
    let account = a.account.clone();
    let attempt = tokio::spawn(async move {
        account
            .message_test_attempt(m.key, false, MessageMutation::None)
            .await
    });
    pause.wait_entered().await;
    lifecycle(&a, true).await;
    pause.release();
    assert!(attempt.await.unwrap().is_err());
    a.account.snapshot().await.unwrap();
    for _ in 0..2 {
        a.account
            .storage_test_fixture()
            .message_fault(chat_core::storage_fixture::CommitFault::BeforeCommit)
            .await;
        a.account.request_resume();
        tokio::time::timeout(Duration::from_secs(5), async {
            while a.account.lifecycle_status() == 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            a.account.lifecycle_status(),
            2,
            "failed settlement must retain completion owner and fail readiness"
        );
        assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
        lifecycle(&a, true).await;
    }
    lifecycle(&a, false).await;
    let facts = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(facts.dispatch_state, Some(0));
    assert_eq!(facts.retry.unwrap().0, 1);
    // Final delivery is automatic.
    a.account.message_test_hold_scheduler(false).await.unwrap();
    network_start(&a).await;
    a.route(&b).await;
    b.route(&a).await;
    a.clock.advance_monotonic_ms(1000);
    wait_state(&a, m.key, DeliveryState::Delivered).await;
}

#[tokio::test]
async fn delayed_connection_acquisition_lifecycle_sends_zero_payload() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::Proof)
        .await
        .unwrap();
    let m = enqueue(&a, &b).await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    lifecycle(&a, true).await;
    pause.release();
    a.account.snapshot().await.unwrap();
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_counts()
            .await
            .unwrap()
            .0,
        0
    );
    lifecycle(&a, false).await;
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    network_start(&a).await;
    a.route(&b).await;
    b.route(&a).await;
    a.clock.advance_monotonic_ms(1000);
    wait_state(&a, m.key, DeliveryState::Delivered).await;
}

#[tokio::test]
async fn offline_after_write_remains_retryable_without_new_user_connect() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    // Automatic delivery could send the message before the manual attempt.
    a.account.message_test_hold_scheduler(true).await.unwrap();
    let m = enqueue(&a, &b).await;
    let mut pause = b
        .account
        .message_test_pause(chat_core::account::MessagePoint::Response)
        .await
        .unwrap();
    let account = a.account.clone();
    let attempt = tokio::spawn(async move {
        account
            .message_test_attempt(m.key, false, MessageMutation::None)
            .await
    });
    pause.wait_entered().await;
    b.slot
        .network()
        .unwrap()
        .execute(NetworkOp::Stop)
        .await
        .unwrap();
    pause.release();
    assert!(attempt.await.unwrap().is_err());
    let rows = a
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .load_messages(m.key.conversation, None, 50)
        .await
        .unwrap();
    assert_eq!(
        rows.items[0].state,
        DeliveryState::SentDirect,
        "transport closure with current records is transient, not an authorization block"
    );
    // The retry is automatic, without a new user connect.
    a.account.message_test_hold_scheduler(false).await.unwrap();
    network_start(&b).await;
    a.route(&b).await;
    b.route(&a).await;
    a.clock.advance_monotonic_ms(1000);
    wait_state(&a, m.key, DeliveryState::Delivered).await;
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .messages,
        1
    );
}

#[tokio::test]
async fn wall_jumps_do_not_change_live_receipt_deadlines_or_security_clock() {
    for now in [NOW + 300, NOW - 60] {
        let (a, b, _) = pair().await;
        contacts(&a, &b).await;
        contacts(&b, &a).await;
        a.account.connect_account(b.peer).await.unwrap();
        a.clock.set_unix_seconds(now);
        b.clock.set_unix_seconds(now);
        let m = enqueue(&a, &b).await;
        wait_state(&a, m.key, DeliveryState::Delivered).await;
        let before = b
            .account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap();
        assert_eq!(before.receipt_retry.unwrap().1, NOW * 1000 + 1000);
        b.clock.advance_monotonic_ms(999);
        assert_eq!(
            b.account
                .storage_test_fixture()
                .message_facts(m.key)
                .await
                .unwrap()
                .receipt_jobs,
            1
        );
        b.clock.advance_monotonic_ms(1);
        tokio::time::timeout(Duration::from_secs(3), async {
            while b
                .account
                .storage_test_fixture()
                .message_facts(m.key)
                .await
                .unwrap()
                .receipt_jobs
                != 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            chat_core::account::clock::Clock::unix_seconds(&*b.clock),
            now
        );
        wait_state(&a, m.key, DeliveryState::Delivered).await;
    }
}
#[tokio::test]
async fn receipts_yield_to_eligible_text_after_eight_dispatches() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let repo = b.account.storage_test_fixture().messages().await.unwrap();
    let mut first = None;
    for n in 20..30 {
        let m = repo
            .enqueue(
                a.peer,
                MessageId([n; 16]),
                "receipt backlog".into(),
                NOW * 1000,
            )
            .await
            .unwrap();
        wait_state(&b, m.key, DeliveryState::Delivered).await;
        first.get_or_insert(m.key);
    }
    assert_eq!(
        a.account
            .storage_test_fixture()
            .message_facts(first.unwrap())
            .await
            .unwrap()
            .receipt_jobs,
        10
    );
    a.clock.advance_monotonic_ms(1000);
    tokio::time::timeout(Duration::from_secs(3), async {
        while a.account.message_test_write_starts().await.unwrap().0 == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let m = enqueue(&a, &b).await;
    wait_state(&a, m.key, DeliveryState::Delivered).await;
    let remaining = a
        .account
        .storage_test_fixture()
        .message_facts(first.unwrap())
        .await
        .unwrap()
        .receipt_jobs;
    assert!(
        remaining >= 2,
        "eligible text must run before the ninth consecutive receipt"
    );
}

#[tokio::test]
async fn connection_acquisition_is_bounded_four_global_one_per_peer() {
    let (a, _b, backend) = pair().await;
    let mut peers = Vec::new();
    // Create valid ordinary contacts sequentially, without retaining five extra runtimes.
    for seed in 3..8 {
        let signed = packet(seed, &[seed + 10], seed, NOW, NOW + 600, NOW * 1_000_000);
        let remote = Fixture::new(seed, seed + 10, &signed, backend.clone()).await;
        backend.packets.lock().unwrap().insert(remote.peer, signed);
        contacts(&a, &remote).await;
        peers.push(remote.peer);
    }
    for peer in &peers {
        backend
            .lookup_holds
            .lock()
            .unwrap()
            .insert(*peer, Arc::new(tokio::sync::Notify::new()));
    }
    let repo = a.account.storage_test_fixture().messages().await.unwrap();
    for peer in peers {
        for n in 50..53 {
            repo.enqueue(
                peer,
                MessageId([n; 16]),
                "bounded lookup".into(),
                NOW * 1000,
            )
            .await
            .unwrap();
        }
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        while backend.lookup_starts.lock().unwrap().len() < 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    a.account.snapshot().await.unwrap();
    // Allow another due turn while all four real lookup futures are held.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let starts = backend.lookup_starts.lock().unwrap().clone();
    assert_eq!(starts.len(), 4);
    assert_eq!(
        starts
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4
    );
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
}

#[tokio::test]
async fn changed_remote_record_cancels_before_signature_write() {
    let (a, b, backend) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::Signature)
        .await
        .unwrap();
    let m = enqueue(&a, &b).await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    backend.packets.lock().unwrap().insert(
        b.peer,
        packet(2, &[12], 9, NOW, NOW + 600, NOW * 1_000_000 + 1),
    );
    assert_eq!(
        a.account.refresh_record(b.peer).await.unwrap_err().code,
        ErrorCode::RecordConflict
    );
    pause.release();
    wait_state(&a, m.key, DeliveryState::Blocked).await;
    assert_eq!(
        a.account.message_test_proof_write_starts().await.unwrap(),
        0,
        "observed conflict must invalidate the outstanding proof before signature write entry"
    );
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
}

#[tokio::test]
async fn expired_acquisition_cannot_enter_signature_write() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::Signature)
        .await
        .unwrap();
    let m = enqueue(&a, &b).await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    a.clock.set_unix_seconds(NOW + 601);
    pause.release();
    wait_state(&a, m.key, DeliveryState::Blocked).await;
    assert_eq!(
        a.account.message_test_proof_write_starts().await.unwrap(),
        0
    );
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
}
#[tokio::test]
async fn lifecycle_cancels_before_signature_and_current_proof_can_resume() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::Signature)
        .await
        .unwrap();
    let m = enqueue(&a, &b).await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    lifecycle(&a, true).await;
    pause.release();
    lifecycle(&a, false).await;
    assert_eq!(
        a.account.message_test_proof_write_starts().await.unwrap(),
        0
    );
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    network_start(&a).await;
    a.route(&b).await;
    b.route(&a).await;
    a.clock.advance_monotonic_ms(1000);
    wait_state(&a, m.key, DeliveryState::Delivered).await;
    assert_eq!(
        a.account.message_test_proof_write_starts().await.unwrap(),
        1
    );
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 1);
}

#[tokio::test]
async fn completed_acquisition_survives_failed_defer_settlement() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::Signature)
        .await
        .unwrap();
    let m = enqueue(&a, &b).await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    lifecycle(&a, true).await;
    pause.release();
    a.account.snapshot().await.unwrap();
    for _ in 0..2 {
        a.account
            .storage_test_fixture()
            .message_fault(chat_core::storage_fixture::CommitFault::BeforeCommit)
            .await;
        a.account.request_resume();
        tokio::time::timeout(Duration::from_secs(5), async {
            while a.account.lifecycle_status() == 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            a.account.lifecycle_status(),
            2,
            "failed Defer must retain the completed acquisition owner"
        );
        lifecycle(&a, true).await;
    }
    lifecycle(&a, false).await;
    assert_eq!(
        a.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .retry
            .unwrap()
            .0,
        1
    );
    network_start(&a).await;
    a.route(&b).await;
    b.route(&a).await;
    a.clock.advance_monotonic_ms(1000);
    wait_state(&a, m.key, DeliveryState::Delivered).await;
}
#[tokio::test]
async fn changed_own_record_cancels_before_signature_write() {
    let (a, b, backend) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::Signature)
        .await
        .unwrap();
    let m = enqueue(&a, &b).await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    backend.packets.lock().unwrap().insert(
        a.peer,
        packet(1, &[11], 9, NOW, NOW + 600, NOW * 1_000_000 + 1),
    );
    assert_eq!(
        a.account.refresh_record(a.peer).await.unwrap_err().code,
        ErrorCode::RecordConflict
    );
    pause.release();
    wait_state(&a, m.key, DeliveryState::Blocked).await;
    assert_eq!(
        a.account.message_test_proof_write_starts().await.unwrap(),
        0
    );
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
}

#[tokio::test]
async fn auth_transport_close_retries_automatically_after_peer_returns() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::Signature)
        .await
        .unwrap();
    let m = enqueue(&a, &b).await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    b.slot
        .network()
        .unwrap()
        .execute(NetworkOp::Stop)
        .await
        .unwrap();
    pause.release();
    let facts = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let facts = a
                .account
                .storage_test_fixture()
                .message_facts(m.key)
                .await
                .unwrap();
            if facts.dispatch_state == Some(2) || facts.retry.is_some_and(|r| r.0 > 0) {
                break facts;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        facts.dispatch_state,
        Some(0),
        "auth transport interruption must remain due, not require external Retry"
    );
    assert_eq!(facts.retry.unwrap().0, 1);
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    network_start(&b).await;
    a.route(&b).await;
    b.route(&a).await;
    a.clock.advance_monotonic_ms(1000);
    wait_state(&a, m.key, DeliveryState::Delivered).await;
}

fn proof_control(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(3).unwrap().u8(1).unwrap().u8(kind).unwrap();
    if kind == 0 {
        let mut bytes = e.into_writer();
        bytes.extend(payload);
        bytes
    } else {
        e.bytes(payload).unwrap();
        e.into_writer()
    }
}
async fn proof_write(send: &mut iroh::endpoint::SendStream, body: &[u8]) {
    send.write_all(&(body.len() as u32).to_be_bytes())
        .await
        .unwrap();
    send.write_all(body).await.unwrap();
}
async fn proof_read(recv: &mut iroh::endpoint::RecvStream) -> Vec<u8> {
    let mut len = [0; 4];
    recv.read_exact(&mut len).await.unwrap();
    let len = u32::from_be_bytes(len) as usize;
    assert!(len <= 65536);
    let mut bytes = vec![0; len];
    recv.read_exact(&mut bytes).await.unwrap();
    bytes
}
#[tokio::test]
async fn invalid_remote_signature_blocks_automatic_acquisition() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    b.slot
        .network()
        .unwrap()
        .execute(NetworkOp::Stop)
        .await
        .unwrap();
    let mut seed = [0; 32];
    Hkdf::<Sha256>::new(Some(b"ChatProject/device-keys/v1"), &[12; 32])
        .expand(b"transport-ed25519", &mut seed)
        .unwrap();
    let raw = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&seed))
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![b"chatproject/account/1".to_vec()])
        .bind()
        .await
        .unwrap();
    let addresses = raw.bound_sockets().into_iter().map(|mut socket| {
        if socket.ip().is_unspecified() {
            socket.set_ip(if socket.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        iroh::TransportAddr::Ip(socket)
    });
    let ticket = iroh_tickets::endpoint::EndpointTicket::new(iroh::EndpointAddr::from_parts(
        raw.id(),
        addresses,
    ))
    .to_string();
    a.slot
        .network()
        .unwrap()
        .account_target_for_test(ticket)
        .await
        .unwrap();
    let endpoint = raw.clone();
    let server = tokio::spawn(async move {
        let conn = endpoint.accept().await.unwrap().await.unwrap();
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        proof_read(&mut recv).await;
        let keys = derive_account(&Secret32::from([2; 32])).unwrap();
        let device = derive_device(Secret32::from([12; 32])).unwrap();
        let cert = sign_cert(&keys, &device, DeviceKind::Desktop, NOW).unwrap();
        let mut e = minicbor::Encoder::new(Vec::new());
        e.array(5)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&keys.peer_id().0)
            .unwrap()
            .bytes(&[2; 16])
            .unwrap();
        let mut hello = e.into_writer();
        hello.extend(cert.to_bytes());
        minicbor::Encoder::new(&mut hello).bytes(&[44; 32]).unwrap();
        proof_write(&mut send, &proof_control(0, &hello)).await;
        proof_read(&mut recv).await;
        proof_write(
            &mut send,
            &proof_control(
                2,
                packet(2, &[12], 2, NOW, NOW + 600, NOW * 1_000_000).as_bytes(),
            ),
        )
        .await;
        proof_read(&mut recv).await;
        proof_write(&mut send, &proof_control(1, &[0; 64])).await;
        tokio::time::timeout(Duration::from_secs(5), conn.closed())
            .await
            .unwrap();
    });
    let m = enqueue(&a, &b).await;
    wait_state(&a, m.key, DeliveryState::Blocked).await;
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    let rows = a
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .load_messages(m.key.conversation, None, 50)
        .await
        .unwrap();
    assert_eq!(rows.items[0].block, Some(BlockReason::Account));
    assert_eq!(
        a.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .jobs,
        1
    );
    server.await.unwrap();
    raw.close().await;
}

#[tokio::test]
async fn completed_proof_queued_behind_new_local_record_closes_before_payload() {
    let (a, b, backend) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    // Stage a normal renewal after completed absence, but keep the accepted
    // packet visible until the completed old proof has reached its handoff.
    let old = backend.packets.lock().unwrap().remove(&a.peer).unwrap();
    a.account.refresh_record(a.peer).await.unwrap();
    let renewed = backend.packets.lock().unwrap().insert(a.peer, old).unwrap();
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::ProofResult)
        .await
        .unwrap();
    let m = enqueue(&a, &b).await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    assert_eq!(
        a.account.message_test_proof_write_starts().await.unwrap(),
        1
    );
    let hold = Arc::new(tokio::sync::Notify::new());
    backend
        .lookup_holds
        .lock()
        .unwrap()
        .insert(a.peer, hold.clone());
    backend.packets.lock().unwrap().insert(a.peer, renewed);
    backend
        .lookup_starts
        .lock()
        .unwrap()
        .retain(|p| *p != a.peer);
    let account = a.account.clone();
    let peer = a.peer;
    let refresh = tokio::spawn(async move { account.refresh_record(peer).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !backend.lookup_starts.lock().unwrap().contains(&a.peer) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), pause.release_and_wait_queued())
        .await
        .unwrap();
    backend.lookup_holds.lock().unwrap().remove(&a.peer);
    hold.notify_one();
    tokio::time::timeout(Duration::from_secs(5), refresh)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), pause.wait_connection_closed())
        .await
        .expect("completed old proof must close after exact snapshot cancellation");
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_counts()
            .await
            .unwrap()
            .0,
        0
    );
    a.clock.advance_monotonic_ms(1000);
    wait_state(&a, m.key, DeliveryState::Delivered).await;
    assert_eq!(
        a.account.message_test_proof_write_starts().await.unwrap(),
        2
    );
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 1);
}
