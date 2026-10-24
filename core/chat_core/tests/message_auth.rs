#![cfg(all(feature = "test-hooks", debug_assertions))]
include!("support/message_network.rs");
#[tokio::test]
async fn before_auth_no_message_bytes() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    let mut pause=a.account.message_test_pause(chat_core::account::MessagePoint::Signature).await.unwrap();
    let m = enqueue(&a, &b).await;
    tokio::time::timeout(Duration::from_secs(5),pause.wait_entered()).await.unwrap();
    assert_eq!(a.account.message_test_proof_write_starts().await.unwrap(),0);
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    let result = a
        .account
        .message_test_attempt(m.key, false, MessageMutation::None)
        .await;
    assert!(matches!(result,Err(e) if e.code==ErrorCode::Busy));
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    assert_eq!(a.account.message_test_proof_write_starts().await.unwrap(),0);
    assert!(
        b.account
            .storage_test_fixture()
            .messages()
            .await
            .unwrap()
            .list_conversations(None, 50)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    pause.release();
}
#[tokio::test]
async fn unknown_contact_gets_no_stored_text() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    let result = a
        .account
        .message_test_attempt(m.key, false, MessageMutation::None)
        .await;
    assert!(matches!(result,Err(e) if e.code==ErrorCode::ContactRequired));
    let rows = a
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .load_messages(m.key.conversation, None, 50)
        .await
        .unwrap();
    assert_eq!(rows.items[0].state, DeliveryState::Blocked);
    assert_eq!(rows.items[0].block, Some(BlockReason::ContactRequired));
    assert!(
        b.account
            .storage_test_fixture()
            .messages()
            .await
            .unwrap()
            .list_conversations(None, 50)
            .await
            .unwrap()
            .items
            .is_empty()
    );
}
#[tokio::test]
async fn sender_and_receipt_are_session_bound() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    let result = a
        .account
        .message_test_attempt(m.key, false, MessageMutation::Sender)
        .await;
    assert!(matches!(result,Err(e) if e.code==ErrorCode::InvalidMessage));
    assert!(
        b.account
            .storage_test_fixture()
            .messages()
            .await
            .unwrap()
            .list_conversations(None, 50)
            .await
            .unwrap()
            .items
            .is_empty()
    );
}
#[tokio::test]
async fn actual_text_and_durable_receipt_roundtrip() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    a.account
        .message_test_attempt(m.key, false, MessageMutation::None)
        .await
        .unwrap();
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 1);
    assert_eq!(b.account.message_test_write_starts().await.unwrap().1, 1);
    let rows = a
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .load_messages(m.key.conversation, None, 50)
        .await
        .unwrap();
    assert_eq!(rows.items[0].state, DeliveryState::Delivered);
    b.account
        .message_test_attempt(m.key, true, MessageMutation::None)
        .await
        .unwrap();
    let received = b
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .load_messages(m.key.conversation, None, 50)
        .await
        .unwrap();
    assert_eq!(received.items.len(), 1);
    assert!(received.items[0].body == "private text");
}

#[tokio::test]
async fn expiry_after_db_commit_sends_no_ack() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    b.account
        .storage_test_fixture()
        .message_clock_after_operation(b.clock.clone(), NOW + 600)
        .await;
    assert!(
        a.account
            .message_test_attempt(m.key, false, MessageMutation::None)
            .await
            .is_err()
    );
    let remote = b
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(
        (remote.messages, remote.processed, remote.receipt_jobs),
        (1, 1, 1)
    );
    let local = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(local.jobs, 1);
    let rows = a
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .load_messages(m.key.conversation, None, 50)
        .await
        .unwrap();
    assert_ne!(rows.items[0].state, DeliveryState::Delivered);
}
#[tokio::test]
async fn invalid_frames_mutate_no_history_dedupe_or_receipts() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    for (i, mutation, code) in [
        (21, MessageMutation::Sender, ErrorCode::InvalidMessage),
        (22, MessageMutation::Conversation, ErrorCode::InvalidMessage),
        (23, MessageMutation::Trailing, ErrorCode::InvalidMessage),
        (
            24,
            MessageMutation::UnsupportedVersion,
            ErrorCode::UnsupportedMessageVersion,
        ),
    ] {
        let m = a
            .account
            .storage_test_fixture()
            .messages()
            .await
            .unwrap()
            .enqueue(
                b.peer,
                MessageId([i; 16]),
                "private text".into(),
                NOW * 1000,
            )
            .await
            .unwrap();
        let before = b
            .account
            .storage_test_fixture()
            .message_counts()
            .await
            .unwrap();
        assert!(
            matches!(a.account.message_test_attempt(m.key,false,mutation).await,Err(e) if e.code==code)
        );
        assert_eq!(
            b.account
                .storage_test_fixture()
                .message_counts()
                .await
                .unwrap(),
            before
        );
    }
}
#[tokio::test]
async fn receipt_jobs_require_matching_committed_application_ack() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    a.account
        .message_test_attempt(m.key, false, MessageMutation::None)
        .await
        .unwrap();
    for mutation in [
        MessageMutation::ReceiptRecipient,
        MessageMutation::ReceiptHash,
        MessageMutation::ReceiptAsApplied,
    ] {
        assert!(
            matches!(b.account.message_test_attempt(m.key,true,mutation).await,Err(e) if e.code==ErrorCode::InvalidMessage)
        );
        assert_eq!(
            b.account
                .storage_test_fixture()
                .message_facts(m.key)
                .await
                .unwrap()
                .receipt_jobs,
            1
        );
    }
    b.account
        .message_test_attempt(m.key, true, MessageMutation::None)
        .await
        .unwrap();
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .receipt_jobs,
        0
    );
}
#[tokio::test]
async fn out_of_order_chain_gap_preserves_exact_predecessor_hash() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let first = enqueue(&a, &b).await;
    let second = a
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .enqueue(b.peer, MessageId([10; 16]), "second".into(), NOW * 1000)
        .await
        .unwrap();
    let hash = a
        .account
        .storage_test_fixture()
        .message_facts(first.key)
        .await
        .unwrap()
        .hash;
    a.account
        .message_test_attempt(second.key, false, MessageMutation::None)
        .await
        .unwrap();
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_facts(second.key)
            .await
            .unwrap()
            .prev_hash,
        hash
    );
    a.account
        .message_test_attempt(first.key, false, MessageMutation::None)
        .await
        .unwrap();
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_counts()
            .await
            .unwrap()
            .0,
        2
    );
}

#[tokio::test]
async fn expiry_after_response_handoff_sends_no_ack() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    let mut pause = b
        .account
        .message_test_pause(chat_core::account::MessagePoint::Response)
        .await
        .unwrap();
    let account = a.account.clone();
    let task = tokio::spawn(async move {
        account
            .message_test_attempt(m.key, false, MessageMutation::None)
            .await
    });
    pause.wait_entered().await;
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .receipt_jobs,
        1
    );
    b.clock.set_unix_seconds(NOW + 600);
    pause.release();
    assert!(task.await.unwrap().is_err());
    assert_eq!(b.account.message_test_write_starts().await.unwrap().1, 0);
    assert_eq!(
        a.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .jobs,
        1
    );
}
#[tokio::test]
async fn expiry_after_stream_open_sends_no_message_bytes() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::FirstByte)
        .await
        .unwrap();
    let account = a.account.clone();
    let task = tokio::spawn(async move {
        account
            .message_test_attempt(m.key, false, MessageMutation::None)
            .await
    });
    pause.wait_entered().await;
    let facts = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(
        facts.attempted_receivers,
        vec![b.view().await.installation.unwrap()]
    );
    assert_eq!(facts.dispatch_state, Some(1));
    a.clock.set_unix_seconds(NOW + 600);
    pause.release();
    assert!(task.await.unwrap().is_err());
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_counts()
            .await
            .unwrap(),
        (0, 0, 0, 0)
    );
}

#[tokio::test]
async fn lost_text_ack_retries_same_payload_without_duplicate_storage() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    let original = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    let mut pause = b
        .account
        .message_test_pause(chat_core::account::MessagePoint::Response)
        .await
        .unwrap();
    let account = a.account.clone();
    let task = tokio::spawn(async move {
        account
            .message_test_attempt(m.key, false, MessageMutation::None)
            .await
    });
    pause.wait_entered().await;
    b.clock.set_unix_seconds(NOW + 600);
    pause.release();
    assert!(task.await.unwrap().is_err());
    b.clock.set_unix_seconds(NOW);
    a.account.connect_account(b.peer).await.unwrap();
    a.account
        .message_test_attempt(m.key, false, MessageMutation::None)
        .await
        .unwrap();
    let remote = b
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(
        (remote.messages, remote.processed, remote.receipt_jobs),
        (1, 1, 1)
    );
    assert_eq!(remote.hash, original.hash);
    assert_eq!(
        a.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .jobs,
        0
    );
}

#[tokio::test]
async fn conflicting_duplicate_preserves_original_and_sender_wire() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    let original = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    let mut pause = b
        .account
        .message_test_pause(chat_core::account::MessagePoint::Response)
        .await
        .unwrap();
    let account = a.account.clone();
    let task = tokio::spawn(async move {
        account
            .message_test_attempt(m.key, false, MessageMutation::None)
            .await
    });
    pause.wait_entered().await;
    b.clock.set_unix_seconds(NOW + 600);
    pause.release();
    assert!(task.await.unwrap().is_err());
    b.clock.set_unix_seconds(NOW);
    a.account.connect_account(b.peer).await.unwrap();
    let before = b
        .account
        .storage_test_fixture()
        .message_counts()
        .await
        .unwrap();
    assert!(
        matches!(a.account.message_test_attempt(m.key,false,MessageMutation::ConflictingText).await,Err(e) if e.code==ErrorCode::InvalidMessage)
    );
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_counts()
            .await
            .unwrap(),
        before
    );
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .hash,
        original.hash
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
    assert!(rows.items[0].body == m.body);
    let local = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(local.hash, original.hash);
}
#[tokio::test]
async fn uncertain_receiver_commit_sends_no_success_and_reconciles_original_key() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    b.account
        .storage_test_fixture()
        .message_fault(chat_core::storage_fixture::CommitFault::AfterCommit)
        .await;
    assert!(
        a.account
            .message_test_attempt(m.key, false, MessageMutation::None)
            .await
            .is_err()
    );
    assert_eq!(
        a.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .jobs,
        1
    );
    assert_eq!(
        b.view().await.phase,
        chat_core::account::AccountPhase::Locked
    );
    // Existing explicit lifecycle restore reloads actual SQL; no automatic authority resume.
    for suspend in [true, false] {
        if suspend {
            b.account.request_suspend();
        } else {
            b.account.request_resume();
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            while b.account.lifecycle_status() == 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(b.account.lifecycle_status(), 0);
    }
    b.slot
        .network()
        .unwrap()
        .execute(NetworkOp::Start)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
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
    a.account.connect_account(b.peer).await.unwrap();
    a.account
        .message_test_attempt(m.key, false, MessageMutation::None)
        .await
        .unwrap();
    let facts = b
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(
        (facts.messages, facts.processed, facts.receipt_jobs),
        (1, 1, 1)
    );
}

async fn restart(f: &Fixture) {
    for suspend in [true, false] {
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
    f.slot
        .network()
        .unwrap()
        .execute(NetworkOp::Start)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
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
#[tokio::test]
async fn uncertain_prebyte_attempt_reconciles_before_retry_without_sending() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    a.account
        .storage_test_fixture()
        .message_fault(chat_core::storage_fixture::CommitFault::AfterCommit)
        .await;
    assert!(
        matches!(a.account.message_test_attempt(m.key,false,MessageMutation::None).await,Err(e) if e.code==ErrorCode::StorageUncertain)
    );
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_counts()
            .await
            .unwrap(),
        (0, 0, 0, 0)
    );
    restart(&a).await;
    a.route(&b).await;
    b.route(&a).await;
    a.account.connect_account(b.peer).await.unwrap();
    a.account
        .message_test_attempt(m.key, false, MessageMutation::None)
        .await
        .unwrap();
    assert_eq!(
        a.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .jobs,
        0
    );
}

#[tokio::test]
async fn fenced_successful_prepare_releases_only_never_launched_job_on_restore() {
    for before_reply in [true, false] {
        let (a, b, _) = pair().await;
        contacts(&a, &b).await;
        contacts(&b, &a).await;
        a.account.connect_account(b.peer).await.unwrap();
        let m = enqueue(&a, &b).await;
        let original = a
            .account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap();
        let mut actor_pause = if before_reply {
            None
        } else {
            Some(
                a.account
                    .message_test_pause(chat_core::account::MessagePoint::Prelaunch)
                    .await
                    .unwrap(),
            )
        };
        let worker_pause = if before_reply {
            Some(a.account.storage_test_fixture().pause_message_reply().await)
        } else {
            None
        };
        let account = a.account.clone();
        let task = tokio::spawn(async move {
            account
                .message_test_attempt(m.key, false, MessageMutation::None)
                .await
        });
        if let Some(pause) = &worker_pause {
            pause.wait_entered().await;
        }
        if let Some(pause) = &mut actor_pause {
            pause.wait_entered().await;
        }
        a.account.request_suspend();
        if let Some(pause) = worker_pause {
            pause.release();
        }
        if let Some(pause) = actor_pause {
            pause.release();
        }
        assert!(task.await.unwrap().is_err());
        tokio::time::timeout(Duration::from_secs(10), async {
            while a.account.lifecycle_status() == 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            b.account
                .storage_test_fixture()
                .message_counts()
                .await
                .unwrap(),
            (0, 0, 0, 0)
        );
        restart(&a).await;
        assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);

        a.route(&b).await;
        b.route(&a).await;
        let persisted = a
            .account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap();
        assert_eq!(persisted.dispatch_state, Some(0));
        assert!(persisted.wire == original.wire);
        assert_eq!(persisted.attempted_receivers.len(), 1);
        a.account.connect_account(b.peer).await.unwrap();
        a.account
            .message_test_attempt(m.key, false, MessageMutation::None)
            .await
            .unwrap();
        assert_eq!(
            a.account
                .storage_test_fixture()
                .message_facts(m.key)
                .await
                .unwrap()
                .jobs,
            0
        );
    }
}
#[tokio::test]
async fn lost_receipt_applied_ack_keeps_job_until_repeated_commit_ack() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    contacts(&b, &a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    a.account
        .message_test_attempt(m.key, false, MessageMutation::None)
        .await
        .unwrap();
    let mut pause = a
        .account
        .message_test_pause(chat_core::account::MessagePoint::Response)
        .await
        .unwrap();
    let account = b.account.clone();
    let task = tokio::spawn(async move {
        account
            .message_test_attempt(m.key, true, MessageMutation::None)
            .await
    });
    pause.wait_entered().await;
    a.clock.set_unix_seconds(NOW + 600);
    pause.release();
    assert!(task.await.unwrap().is_err());
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .receipt_jobs,
        1
    );
    a.clock.set_unix_seconds(NOW);
    b.account.connect_account(a.peer).await.unwrap();
    b.account
        .message_test_attempt(m.key, true, MessageMutation::None)
        .await
        .unwrap();
    assert_eq!(
        b.account
            .storage_test_fixture()
            .message_facts(m.key)
            .await
            .unwrap()
            .receipt_jobs,
        0
    );
}

#[tokio::test]
async fn sender_contact_requirement_keeps_explicit_blocked_job() {
    let (a, b, _) = pair().await;
    a.account.connect_account(b.peer).await.unwrap();
    let m = enqueue(&a, &b).await;
    assert!(
        matches!(a.account.message_test_attempt(m.key,false,MessageMutation::None).await,Err(e) if e.code==ErrorCode::ContactRequired)
    );
    let rows = a
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .load_messages(m.key.conversation, None, 50)
        .await
        .unwrap();
    assert_eq!(rows.items[0].state, DeliveryState::Blocked);
    assert_eq!(rows.items[0].block, Some(BlockReason::ContactRequired));
    let facts = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(facts.jobs, 1);
    assert!(facts.attempted_receivers.is_empty());
}

async fn rejected_prepare_preserves_launched_job(fence_rejection: bool) {
    let (a,b,_)=pair().await;
    contacts(&a,&b).await; contacts(&b,&a).await;
    a.account.connect_account(b.peer).await.unwrap();
    let mut response=a.account.message_test_pause(chat_core::account::MessagePoint::FirstByte).await.unwrap();
    // Arm before eligibility and let only the automatic scheduler own this job.
    let m=enqueue(&a,&b).await;
    tokio::time::timeout(Duration::from_secs(5),response.wait_entered()).await.unwrap();
    let original=a.account.storage_test_fixture().message_facts(m.key).await.unwrap();
    assert_eq!(original.dispatch_state,Some(1));
    assert_eq!(original.attempted_receivers.len(),1);
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0,0);
    // The normal actor path rejects concurrent work before SQL.
    assert_eq!(tokio::time::timeout(Duration::from_secs(5),a.account.message_test_attempt(m.key,false,MessageMutation::None)).await.unwrap().unwrap_err().code,ErrorCode::Busy);
    // Separately exercise actual SQL InFlight rejection and its exact R29 token.
    let repo=a.account.storage_test_fixture().messages().await.unwrap();
    let pause=if fence_rejection {Some(a.account.storage_test_fixture().pause_message_reply().await)} else {None};
    let receiver=original.attempted_receivers[0];
    let rejected=tokio::spawn(async move {repo.rejected_prepare(m.key,receiver,NOW*1000).await.unwrap()});
    let probe=if let Some(pause)=pause {
        tokio::time::timeout(Duration::from_secs(5),pause.wait_entered()).await.unwrap();
        a.account.request_suspend(); pause.release();
        let probe=tokio::time::timeout(Duration::from_secs(5),rejected).await.unwrap().unwrap();
        assert_eq!(probe.code,ErrorCode::StorageLocked);
        restart(&a).await;
        probe
    } else {
        let probe=tokio::time::timeout(Duration::from_secs(5),rejected).await.unwrap().unwrap();
        assert_eq!(probe.code,ErrorCode::Busy);
        probe
    };
    probe.reset().await.unwrap();
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0,0);
    let retained = a
        .account
        .storage_test_fixture()
        .message_facts(m.key)
        .await
        .unwrap();
    assert_eq!(retained.dispatch_state, original.dispatch_state);
    assert_eq!(retained.messages, original.messages);
    assert_eq!(retained.jobs, original.jobs);
    assert_eq!(retained.quota_bytes, original.quota_bytes);
    assert!(retained.wire == original.wire);
    assert!(retained.hash == original.hash);
    assert!(retained.prev_hash == original.prev_hash);
    assert!(retained.attempted_receivers == original.attempted_receivers);
    assert_eq!(retained.receipt_jobs, original.receipt_jobs);
    assert_eq!(retained.processed, original.processed);
    response.release();
    tokio::time::timeout(Duration::from_secs(5), async {
        while a.account.storage_test_fixture().message_facts(m.key).await.unwrap().dispatch_state == Some(1) {
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    if fence_rejection {
        assert_eq!(a.account.message_test_write_starts().await.unwrap().0,0);
        assert_eq!(a.account.storage_test_fixture().message_facts(m.key).await.unwrap().dispatch_state,Some(0));
    } else {
        assert_eq!(a.account.message_test_write_starts().await.unwrap().0,1);
        assert_eq!(a.account.storage_test_fixture().messages().await.unwrap().load_messages(m.key.conversation,None,50).await.unwrap().items[0].state,DeliveryState::Delivered);
    }
}
#[tokio::test]
async fn rejected_prepare_cannot_reset_previously_launched_job() {
    rejected_prepare_preserves_launched_job(false).await;
}
#[tokio::test]
async fn fenced_rejected_prepare_cannot_reset_previously_launched_job() {
    rejected_prepare_preserves_launched_job(true).await;
}
