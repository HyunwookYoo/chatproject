#![allow(dead_code, unused_imports)] // Shared real-network fixture has additional consumers.
#![cfg(all(feature = "test-hooks", debug_assertions))]
include!("support/message_network.rs");

#[tokio::test]
async fn queued_reply_precedes_network_but_follows_commit() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::Signature)
        .await
        .unwrap();
    let messages = a.account.messages();
    let m = messages
        .send_text(b.peer, MessageId([1; 16]), "display body".into())
        .await
        .unwrap();
    assert_eq!(m.state, DeliveryState::Queued);
    assert_eq!(m.sent_at_ms, NOW * 1000);
    let page = messages
        .load_messages(m.key.conversation, None, 50)
        .await
        .unwrap();
    assert_eq!(page.items, vec![m.clone()]);
    assert_eq!(
        a.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .jobs,
        1
    );
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    pause.release();
}

async fn service_state(a: &Fixture, key: chat_core::MessageKey, expected: DeliveryState) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let p = a
                .account
                .storage_test_fixture()
                .messages()
                .await
                .unwrap()
                .load_messages(key.conversation, None, 50)
                .await
                .unwrap();
            if p.items.iter().any(|v| v.key == key && v.state == expected) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn read_only_marks_visible_incoming_ids() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let mut keys = Vec::new();
    for n in 0..3 {
        let m = a
            .account
            .storage_test_fixture()
            .messages()
            .await
            .unwrap()
            .enqueue(b.peer, MessageId([n; 16]), "incoming".into(), NOW * 1000)
            .await
            .unwrap();
        service_state(&a, m.key, DeliveryState::Delivered).await;
        keys.push(m.key);
    }
    let service = b.account.messages();
    let visible = service
        .load_messages(keys[0].conversation, None, 1)
        .await
        .unwrap();
    assert_eq!(visible.items.len(), 1);
    let key = visible.items[0].key;
    let before = service.list_conversations(None, 50).await.unwrap();
    assert_eq!(before.items[0].unread, 3);
    let own = b
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .enqueue(a.peer, MessageId([20; 16]), "own".into(), NOW * 1000)
        .await
        .unwrap();
    let mut unknown = key;
    unknown.id = MessageId([99; 16]);
    let mut other = key;
    other.conversation = chat_core::ConversationId([99; 32]);
    for bad in [own.key, unknown, other] {
        assert!(
            service
                .mark_read(key.conversation, vec![key, bad])
                .await
                .is_err()
        );
        assert_eq!(
            service.list_conversations(None, 50).await.unwrap().items[0].unread,
            3
        );
    }
    assert_eq!(
        service
            .mark_read(key.conversation, vec![key; 51])
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidMessage
    );
    service
        .mark_read(key.conversation, vec![key, key])
        .await
        .unwrap();
    let after = service.list_conversations(None, 50).await.unwrap();
    assert_eq!(after.items[0].unread, 2);
    let revision = after.revision;
    service
        .mark_read(key.conversation, vec![key])
        .await
        .unwrap();
    assert_eq!(
        service.list_conversations(None, 50).await.unwrap().revision,
        revision
    );
    service_state(&a, key, DeliveryState::Read).await;
    for hidden in keys.into_iter().filter(|k| *k != key) {
        service_state(&a, hidden, DeliveryState::Delivered).await;
    }
}

#[tokio::test]
async fn event_gap_recovers_from_db() {
    let (a, b, _) = pair().await;
    let service = a.account.messages();
    let c = service.open_dm(b.peer).await.unwrap();
    let old = service.load_messages(c.id, None, 50).await.unwrap();
    let m = service
        .send_text(b.peer, MessageId([1; 16]), "retained".into())
        .await
        .unwrap();
    let sink = Arc::new(Events::default());
    a.bus.subscribe(sink.clone());
    assert!(
        !sink
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|e| serde_json::to_value(e).unwrap()["kind"] == "messages_changed")
    );
    let snapshot = service.load_messages(c.id, None, 50).await.unwrap();
    assert!(snapshot.revision > old.revision);
    assert_eq!(snapshot.items[0].key, m.key);
    let cancelled = service.cancel_send(m.key).await.unwrap();
    assert_eq!(cancelled.state, DeliveryState::Cancelled);
    let latest = service.load_messages(c.id, None, 50).await.unwrap();
    let events = sink.0.lock().unwrap();
    let changed: Vec<_> = events
        .iter()
        .map(|e| serde_json::to_value(e).unwrap())
        .filter(|e| e["kind"] == "messages_changed")
        .collect();
    assert!(changed.iter().any(|e| e["revision"] == latest.revision));
    assert!(
        events
            .iter()
            .all(|e| !serde_json::to_string(e).unwrap().contains("retained"))
    );
}

#[tokio::test]
async fn late_query_cannot_restore_locked_view() {
    let (a, b, _) = pair().await;
    let service = a.account.messages();
    let m = service
        .send_text(b.peer, MessageId([1; 16]), "retained".into())
        .await
        .unwrap();
    let fixture = a.account.storage_test_fixture();
    let pause = fixture.pause().await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    let cloned = service.clone();
    let query =
        tokio::spawn(async move { cloned.load_messages(m.key.conversation, None, 50).await });
    // Observe actual admission to the paused sole SQL worker before invalidation.
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.remaining_capacity() == 32 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    a.account.request_suspend();
    pause.release();
    assert_eq!(
        query.await.unwrap().unwrap_err().code,
        ErrorCode::StorageLocked
    );
    assert_eq!(
        service
            .load_messages(m.key.conversation, None, 50)
            .await
            .unwrap_err()
            .code,
        ErrorCode::StorageLocked
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while a.account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    a.account.request_resume();
    tokio::time::timeout(Duration::from_secs(5), async {
        while a.account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        service
            .load_messages(m.key.conversation, None, 50)
            .await
            .unwrap()
            .items[0]
            .key,
        m.key
    );
}

#[tokio::test]
async fn page_event_race_converges_without_duplicates() {
    let (a, b, _) = pair().await;
    let service = a.account.messages();
    let sink = Arc::new(Events::default());
    a.bus.subscribe(sink.clone());
    let conversation = service.open_dm(b.peer).await.unwrap().id;
    for n in 0..6 {
        service
            .send_text(b.peer, MessageId([n; 16]), "page".into())
            .await
            .unwrap();
    }
    let first = service.load_messages(conversation, None, 3).await.unwrap();
    let m = service
        .send_text(b.peer, MessageId([8; 16]), "new".into())
        .await
        .unwrap();
    let old = service
        .load_messages(conversation, first.next, 3)
        .await
        .unwrap();
    let fresh = service.load_messages(conversation, None, 50).await.unwrap();
    let mut merged = BTreeMap::new();
    for v in first
        .items
        .into_iter()
        .chain(old.items)
        .chain(fresh.items.clone())
    {
        merged.insert(v.key, v);
    }
    assert_eq!(merged.len(), 7);
    assert_eq!(merged.get(&m.key), Some(&m));
    assert_eq!(fresh.items.len(), 7);
    // Repeated/out-of-order notifications refresh from a versioned page and merge by original key.
    for _ in 0..2 {
        for v in fresh.items.clone() {
            merged.insert(v.key, v);
        }
    }
    assert_eq!(merged.len(), 7);
    assert!(sink.0.lock().unwrap().iter().any(|e| matches!(e, CoreEvent::MessagesChanged { revision, conversation: c } if *revision == fresh.revision && *c == conversation)));
}

#[path = "support/account_services.rs"]
mod offline_services;

#[tokio::test]
async fn owning_account_gates_candidate_and_allows_established_offline() {
    let dir = tempfile::tempdir().unwrap();
    let slot = CoreSlot::new();
    slot.start(
        CoreConfig {
            data_dir: dir.path().into(),
        },
        &EventBus::new(),
    )
    .unwrap();
    slot.network().unwrap().configure_local(None).await.unwrap();
    let account = slot
        .open_account_with_services(support::protector(), offline_services::offline())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let sql = sql_store::Store::from_fixture(account.storage_test_fixture());
    let service = account.messages();
    let peer = derive_account(&Secret32::from([1; 32])).unwrap().peer_id();
    assert_eq!(
        service
            .send_text(peer, MessageId([1; 16]), "pending".into())
            .await
            .unwrap_err()
            .code,
        ErrorCode::AccountRestricted
    );
    account.prepare_candidate().await.unwrap();
    assert_eq!(
        service.open_dm(peer).await.unwrap_err().code,
        ErrorCode::AccountRestricted
    );
    assert_eq!(
        service
            .send_text(peer, MessageId([1; 16]), "pending".into())
            .await
            .unwrap_err()
            .code,
        ErrorCode::AccountRestricted
    );
    account.discard_pending_import().await.unwrap();
    let display = account.begin_create().await.unwrap();
    let answers = display
        .positions
        .map(|p| display.words[usize::from(p - 1)].clone());
    account.confirm_create(display.flow, answers).await.unwrap();
    assert_eq!(
        account.snapshot().await.unwrap().phase,
        chat_core::account::AccountPhase::LocalReady
    );
    let c = service.open_dm(peer).await.unwrap();
    let m = service
        .send_text(peer, MessageId([1; 16]), "offline".into())
        .await
        .unwrap();
    assert_eq!(m.key.conversation, c.id);
    assert_eq!(
        m.key.sender,
        account.snapshot().await.unwrap().public_peer_id.unwrap()
    );
    assert_eq!(m.state, DeliveryState::Queued);
    assert_eq!(
        service
            .send_text(peer, MessageId([1; 16]), "different".into())
            .await
            .unwrap_err()
            .code,
        ErrorCode::MessageConflict
    );
    assert_eq!(account.message_test_write_starts().await.unwrap().0, 0);
    drop(sql);
}

#[tokio::test]
async fn committed_reply_is_not_success_or_event_after_suspend() {
    let (a, b, _) = pair().await;
    let sink = Arc::new(Events::default());
    a.bus.subscribe(sink.clone());
    let pause = a.account.storage_test_fixture().pause_message_reply().await;
    let service = a.account.messages();
    let cloned = service.clone();
    let send = tokio::spawn(async move {
        cloned
            .send_text(b.peer, MessageId([1; 16]), "committed".into())
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .unwrap();
    assert!(!send.is_finished());
    assert!(
        !sink
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, CoreEvent::MessagesChanged { .. }))
    );
    a.account.request_suspend();
    pause.release();
    assert_eq!(
        send.await.unwrap().unwrap_err().code,
        ErrorCode::StorageLocked
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while a.account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !sink
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, CoreEvent::MessagesChanged { .. }))
    );
    a.account.request_resume();
    tokio::time::timeout(Duration::from_secs(5), async {
        while a.account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let c = service.open_dm(b.peer).await.unwrap();
    assert_eq!(
        service
            .load_messages(c.id, None, 50)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
}

#[tokio::test]
async fn public_retry_preserves_payload_after_contact_block() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    let service = a.account.messages();
    let m = service
        .send_text(b.peer, MessageId([42; 16]), "retry".into())
        .await
        .unwrap();
    service_state(&a, m.key, DeliveryState::Blocked).await;
    let before = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    let view = service
        .load_messages(m.key.conversation, None, 50)
        .await
        .unwrap();
    assert_eq!(view.items[0].block, Some(BlockReason::ContactRequired));
    contacts(&b, &a).await;
    service.retry_send(m.key).await.unwrap();
    service_state(&a, m.key, DeliveryState::Delivered).await;
    let after = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(before.hash, after.hash);
    assert_eq!(before.prev_hash, after.prev_hash);
    assert_eq!(after.messages, 1);
}

#[tokio::test]
async fn read_transaction_rollback_and_uncertain_commit_recover_from_db() {
    use chat_core::storage_fixture::CommitFault;
    for committed in [false, true] {
        let (a, b, _) = pair().await;
        contacts(&a, &b).await;
        contacts(&b, &a).await;
        let m = a
            .account
            .messages()
            .send_text(b.peer, MessageId([50; 16]), "read fixture".into())
            .await
            .unwrap();
        service_state(&a, m.key, DeliveryState::Delivered).await;
        let sink = Arc::new(Events::default());
        b.bus.subscribe(sink.clone());
        let fixture = b.account.storage_test_fixture();
        fixture
            .message_fault(if committed {
                CommitFault::AfterCommit
            } else {
                CommitFault::BeforeCommit
            })
            .await;
        assert_eq!(
            b.account
                .messages()
                .mark_read(m.key.conversation, vec![m.key])
                .await
                .unwrap_err()
                .code,
            ErrorCode::StorageUncertain
        );
        assert!(
            !sink
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, CoreEvent::MessagesChanged { .. }))
        );
        b.account.retry_open().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while b.account.lifecycle_status() == 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(b.account.lifecycle_status(), 0);
        let c = b
            .account
            .messages()
            .list_conversations(None, 50)
            .await
            .unwrap();
        assert_eq!(c.items[0].unread, if committed { 0 } else { 1 });
        b.account
            .messages()
            .mark_read(m.key.conversation, vec![m.key])
            .await
            .unwrap();
        // Recovery restores storage/key authority; the existing M3 lifecycle deliberately leaves network stopped.
        b.slot
            .network()
            .unwrap()
            .execute(NetworkOp::Start)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let NetworkReply::Info(info) = b
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
        a.route(&b).await;
        b.route(&a).await;
        a.clock.advance_monotonic_ms(1000);
        b.clock.advance_monotonic_ms(1000);
        service_state(&a, m.key, DeliveryState::Read).await;
    }
}

#[tokio::test]
async fn reply_sent_before_storage_failure_cannot_restore_locked_body() {
    use chat_core::storage_fixture::CommitFault;
    let (a, b, _) = pair().await;
    let service = a.account.messages();
    let m = service
        .send_text(b.peer, MessageId([60; 16]), "retained page".into())
        .await
        .unwrap();
    let mut query = Box::pin(service.load_messages(m.key.conversation, None, 50));
    assert!(futures_util::poll!(&mut query).is_pending());
    // Actor FIFO: processing this snapshot proves the prior page command sent its reply.
    a.account.snapshot().await.unwrap();
    a.account
        .storage_test_fixture()
        .message_fault(CommitFault::BeforeCommit)
        .await;
    assert_eq!(
        service
            .send_text(b.peer, MessageId([61; 16]), "rollback".into())
            .await
            .unwrap_err()
            .code,
        ErrorCode::StorageUncertain
    );
    assert_eq!(
        a.account.snapshot().await.unwrap().phase,
        chat_core::account::AccountPhase::Locked
    );
    assert_eq!(query.await.unwrap_err().code, ErrorCode::StorageLocked);
}
