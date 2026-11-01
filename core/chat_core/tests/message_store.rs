#![cfg(all(feature = "test-hooks", debug_assertions))]
#[path = "support/m4_store.rs"]
#[allow(dead_code)]
mod support;
use chat_core::{
    ErrorCode,
    account::PeerId,
    storage_fixture::{
        CommitFault, ConversationCursor, DeliveryState, Fixture, MessageCursor, MessageId,
    },
};

// Panic cleanup still joins the owned worker before the caller's TempDir drops.
struct OwnedFixture(Option<Fixture>);
impl std::ops::Deref for OwnedFixture {
    type Target = Fixture;
    fn deref(&self) -> &Fixture {
        self.0.as_ref().unwrap()
    }
}
impl OwnedFixture {
    async fn close(mut self) {
        self.0.take().unwrap().close().await;
    }
}
impl Drop for OwnedFixture {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(f.close())
            })
            .join()
            .unwrap();
        }
    }
}
async fn ready() -> (tempfile::TempDir, OwnedFixture) {
    let dir = tempfile::tempdir().unwrap();
    let f = OwnedFixture(Some(
        Fixture::open(dir.path(), support::protector())
            .await
            .unwrap(),
    ));
    f.handle
        .commit_account(
            chat_core::storage_fixture::migration_states()
                .pop()
                .unwrap()
                .unwrap(),
        )
        .await
        .unwrap();
    (dir, f)
}
fn peer(n: u8) -> PeerId {
    PeerId([n; 32])
}
fn id(n: u8) -> MessageId {
    MessageId([n; 16])
}

#[test]
fn schema_enforces_identity_ordering_foreign_keys_and_ranges() {
    // An isolated constraint probe, not an exported SQL capability or product connection.
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    c.execute_batch(include_str!("../src/storage/schema.sql"))
        .unwrap();
    c.execute(
        "INSERT INTO conversation VALUES(?1,?2,0,?3,0,0)",
        rusqlite::params![
            [1u8; 32].as_slice(),
            [2u8; 32].as_slice(),
            [0u8; 32].as_slice()
        ],
    )
    .unwrap();
    let insert = "INSERT INTO message VALUES(?1,?2,?3,?4,1,?5,?5,1,1,'fixture',1,0,NULL,NULL,1,0,NULL,NULL,X'')";
    c.execute(
        insert,
        rusqlite::params![
            [1u8; 32].as_slice(),
            [2u8; 32].as_slice(),
            [3u8; 32].as_slice(),
            [4u8; 16].as_slice(),
            [0u8; 32].as_slice()
        ],
    )
    .unwrap();
    assert!(
        c.execute(
            insert,
            rusqlite::params![
                [1u8; 32].as_slice(),
                [5u8; 32].as_slice(),
                [3u8; 32].as_slice(),
                [4u8; 16].as_slice(),
                [0u8; 32].as_slice()
            ]
        )
        .is_err(),
        "ambiguous ordering tuple accepted"
    );
    assert!(
        c.execute(
            insert,
            rusqlite::params![
                [9u8; 32].as_slice(),
                [2u8; 32].as_slice(),
                [3u8; 32].as_slice(),
                [4u8; 16].as_slice(),
                [0u8; 32].as_slice()
            ]
        )
        .is_err(),
        "missing parent accepted"
    );
    for sql in [
        "UPDATE message SET sender_peer=X'00'",
        "UPDATE message SET lamport=-1",
        "UPDATE message SET sent_at_ms=-1",
        "UPDATE message SET outgoing=2",
        "UPDATE message SET outcome=4",
        "UPDATE message SET block_reason=4",
        "UPDATE message SET failure_code='private arbitrary detail'",
        "UPDATE message SET attempted_receivers=zeroblob(161)",
        "UPDATE message SET attempted_receivers=zeroblob(31)",
        "UPDATE message SET body=''",
        "UPDATE message SET body=CAST(zeroblob(16385) AS TEXT)",
    ] {
        assert!(
            c.execute(sql, []).is_err(),
            "invalid message constraint accepted"
        );
    }
    c.execute("INSERT INTO outbox SELECT conversation,sender_peer,sender_install,msg_id,1,X'01',0,1000,1,0 FROM message",[]).unwrap();
    for sql in [
        "UPDATE outbox SET wire=zeroblob(61441)",
        "UPDATE outbox SET attempts=4294967296",
        "UPDATE outbox SET attempts=-1",
        "UPDATE outbox SET dispatch_state=3",
        "UPDATE outbox SET codec=0",
        "UPDATE outbox SET next_retry_at_ms=-1",
        "UPDATE outbox SET retry_delay_ms=300001",
    ] {
        assert!(
            c.execute(sql, []).is_err(),
            "invalid outbox constraint accepted"
        );
    }
    c.execute(
        "UPDATE outbox SET wire=zeroblob(61440),attempts=4294967295",
        [],
    )
    .unwrap();
    assert!(
        c.execute("DELETE FROM message", []).is_err(),
        "referenced message was deleted"
    );
}

#[tokio::test]
async fn enqueue_commits_text_and_job_once() {
    let (_dir, f) = ready().await;
    let r = f.messages().await.unwrap();
    let c = r.open_dm(r.local_peer(), peer(2)).await.unwrap();
    assert_eq!(
        r.open_dm(peer(2), peer(2)).await.unwrap_err().code,
        ErrorCode::AccountRestricted
    );
    assert_eq!(
        r.open_dm(r.local_peer(), r.local_peer())
            .await
            .unwrap_err()
            .code,
        ErrorCode::SelfConnection
    );
    let m = r
        .enqueue(peer(2), id(1), "exact text \n한".into(), 24)
        .await
        .unwrap();
    assert_eq!(m.key.conversation, c.id);
    assert_eq!(
        (m.lamport, m.sent_at_ms, m.outgoing, m.state),
        (1, 24, true, DeliveryState::Queued)
    );
    let same = r
        .enqueue(peer(2), id(1), m.body.clone(), 999)
        .await
        .unwrap();
    assert_eq!(m, same);
    let facts = f.message_facts(m.key).await.unwrap();
    assert_eq!((facts.messages, facts.jobs), (1, 1));
    assert_eq!(facts.wire.len() as u64, facts.quota_bytes);
    assert!(facts.wire.starts_with(&[0x83, 1, 16]));
    assert_eq!(
        r.enqueue(peer(2), id(1), "different".into(), 24)
            .await
            .unwrap_err()
            .code,
        ErrorCode::MessageConflict
    );
    let page = r.load_messages(c.id, None, 50).await.unwrap();
    assert_eq!(page.items, vec![m]);
    let second = r.enqueue(peer(2), id(2), "chain".into(), 25).await.unwrap();
    let second_facts = f.message_facts(second.key).await.unwrap();
    let mut outer = minicbor::Decoder::new(&second_facts.wire);
    assert_eq!(outer.array().unwrap(), Some(3));
    assert_eq!(outer.u8().unwrap(), 1);
    assert_eq!(outer.u8().unwrap(), 16);
    let inner = outer.bytes().unwrap();
    let mut inner_decoder = minicbor::Decoder::new(inner);
    assert_eq!(inner_decoder.array().unwrap(), Some(10));
    for _ in 0..7 {
        inner_decoder.skip().unwrap();
    }
    assert!(
        inner_decoder.bytes().unwrap() == facts.hash,
        "local text chain changed"
    );
    use sha2::{Digest, Sha256};
    assert_eq!(second_facts.hash, <[u8; 32]>::from(Sha256::digest(inner)));
    f.close().await;
}

#[tokio::test]
async fn strict_tuple_ties_and_invalid_cursor_bounds() {
    let (_dir, f) = ready().await;
    let r = f.messages().await.unwrap();
    let c = r.open_dm(r.local_peer(), peer(2)).await.unwrap().id;
    for n in [1, 3, 2] {
        f.set_message_lamport(c, 0).await.unwrap();
        r.enqueue(peer(2), id(n), "tie".into(), 1).await.unwrap();
    }
    let first = r.load_messages(c, None, 1).await.unwrap();
    assert_eq!(first.items[0].key.id, id(3));
    let second = r.load_messages(c, first.next, 1).await.unwrap();
    assert_eq!(second.items[0].key.id, id(2));
    let third = r.load_messages(c, second.next, 1).await.unwrap();
    assert_eq!(third.items[0].key.id, id(1));
    assert!(third.next.is_none());
    let mut invalid = first.next.unwrap();
    invalid.lamport = i64::MAX as u64 + 1;
    assert_eq!(
        r.load_messages(c, Some(invalid), 1).await.unwrap_err().code,
        ErrorCode::InvalidMessage
    );
    f.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn lifecycle_fences_message_admission_completion_and_consumption() {
    use std::{future::Future, task::Poll};
    let (_dir, f) = ready().await;
    let r = f.messages().await.unwrap();
    let c = r.open_dm(r.local_peer(), peer(2)).await.unwrap().id;
    let initial = f.pause().await;
    initial.wait_entered().await;
    let mut pending = Box::pin(r.enqueue(peer(2), id(1), "committed before fence".into(), 1));
    std::future::poll_fn(|cx| {
        assert!(pending.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    initial.release();
    let barrier = f.pause().await;
    barrier.wait_entered().await;
    f.handle.invalidate(10);
    barrier.release();
    assert_eq!(pending.await.unwrap_err().code, ErrorCode::StorageLocked);
    assert_eq!(
        r.load_messages(c, None, 1).await.unwrap_err().code,
        ErrorCode::StorageLocked
    );
    f.handle.resume().await.unwrap();
    assert_eq!(r.load_messages(c, None, 50).await.unwrap().items.len(), 1);
    let barrier = f.pause().await;
    barrier.wait_entered().await;
    let clone = r.clone();
    let queued = tokio::spawn(async move {
        clone
            .enqueue(peer(2), id(2), "must not commit".into(), 2)
            .await
    });
    for _ in 0..1000 {
        if f.remaining_capacity() == 31 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(f.remaining_capacity(), 31);
    f.handle.invalidate(11);
    barrier.release();
    assert_eq!(
        queued.await.unwrap().unwrap_err().code,
        ErrorCode::StorageLocked
    );
    f.handle.resume().await.unwrap();
    assert_eq!(r.load_messages(c, None, 50).await.unwrap().items.len(), 1);
    f.close().await;
}

#[tokio::test]
async fn account_fault_scope_and_candidate_authority_are_preserved() {
    let (_dir, f) = ready().await;
    let r = f.messages().await.unwrap();
    let account = f.handle.load_account().await.unwrap().unwrap();
    f.fault(CommitFault::AfterCommit).await;
    r.enqueue(
        peer(2),
        id(1),
        "message cannot consume account hook".into(),
        1,
    )
    .await
    .unwrap();
    assert_eq!(
        f.handle.commit_account(account).await.unwrap_err().code,
        ErrorCode::StorageUncertain
    );
    assert_eq!(f.live_connections(), 0);
    r.enqueue(peer(2), id(2), "reopen validates active account".into(), 2)
        .await
        .unwrap();
    f.close().await;
    let dir = tempfile::tempdir().unwrap();
    let f = OwnedFixture(Some(
        Fixture::open(dir.path(), support::protector())
            .await
            .unwrap(),
    ));
    f.handle.commit_account(support::state(1)).await.unwrap();
    assert_eq!(
        f.messages().await.err().unwrap().code,
        ErrorCode::AccountRestricted
    );
    f.close().await;
}

#[tokio::test]
async fn idempotent_retry_before_quota() {
    let (_dir, f) = ready().await;
    let r = f.messages().await.unwrap();
    let m = r
        .enqueue(peer(2), id(1), "original".into(), 1)
        .await
        .unwrap();
    f.fill_message_quota(m.key, 10_000, None).await.unwrap();
    assert_eq!(
        r.enqueue(peer(2), id(1), "original".into(), 2)
            .await
            .unwrap(),
        m
    );
    assert_eq!(
        r.enqueue(peer(2), id(1), "conflict".into(), 2)
            .await
            .unwrap_err()
            .code,
        ErrorCode::MessageConflict
    );
    f.deliver_message(m.key).await.unwrap();
    let revision = r.list_conversations(None, 50).await.unwrap().revision;
    let mut terminal = m;
    terminal.state = DeliveryState::Delivered;
    assert_eq!(
        r.enqueue(peer(2), id(1), "original".into(), 3)
            .await
            .unwrap(),
        terminal
    );
    assert_eq!(
        r.enqueue(peer(2), id(1), "different after delivery".into(), 3)
            .await
            .unwrap_err()
            .code,
        ErrorCode::MessageConflict
    );
    let facts = f.message_facts(terminal.key).await.unwrap();
    assert_eq!((facts.messages, facts.jobs), (10_000, 9_999));
    assert!(facts.wire.is_empty(), "terminal retry recreated its job");
    assert_eq!(
        r.list_conversations(None, 50).await.unwrap().revision,
        revision
    );
    f.close().await;
}

#[tokio::test]
async fn full_queue_preserves_old_jobs() {
    for bytes in [false, true] {
        let (_dir, f) = ready().await;
        let r = f.messages().await.unwrap();
        let m = r.enqueue(peer(2), id(1), "old".into(), 1).await.unwrap();
        f.fill_message_quota(
            m.key,
            if bytes { 1 } else { 10_000 },
            bytes.then_some(128 * 1024 * 1024),
        )
        .await
        .unwrap();
        let before = f.message_facts(m.key).await.unwrap();
        let revision = r.list_conversations(None, 50).await.unwrap().revision;
        assert_eq!(
            if bytes {
                before.quota_bytes
            } else {
                before.jobs
            },
            if bytes { 128 * 1024 * 1024 } else { 10_000 }
        );
        assert_eq!(
            r.enqueue(peer(3), id(2), "new".into(), 2)
                .await
                .unwrap_err()
                .code,
            ErrorCode::StorageFull
        );
        let after = f.message_facts(m.key).await.unwrap();
        let page = r.list_conversations(None, 50).await.unwrap();
        assert_eq!(
            (page.items.len(), page.revision),
            (1, revision),
            "failed enqueue left conversation/revision changes"
        );
        assert_eq!(
            (before.jobs, before.messages, before.quota_bytes),
            (after.jobs, after.messages, after.quota_bytes)
        );
        assert!(before.wire == after.wire, "immutable fixture wire changed");
        f.close().await;
    }
}

#[tokio::test]
async fn ambiguous_commit_reopens_without_duplicate() {
    for fault in [
        CommitFault::WriteDenied,
        CommitFault::BeforeCommit,
        CommitFault::AfterCommit,
    ] {
        let (dir, f) = ready().await;
        let r = f.messages().await.unwrap();
        let c = r.open_dm(r.local_peer(), peer(2)).await.unwrap();
        let opened = f.opens();
        f.message_fault(fault).await;
        assert_eq!(
            r.enqueue(peer(2), id(1), "original".into(), 1)
                .await
                .unwrap_err()
                .code,
            ErrorCode::StorageUncertain
        );
        assert_eq!(f.live_connections(), 0);
        let recovered = r.load_messages(c.id, None, 50).await.unwrap();
        assert_eq!(
            recovered.items.len(),
            if fault == CommitFault::AfterCommit {
                1
            } else {
                0
            }
        );
        let m = r
            .enqueue(peer(2), id(1), "original".into(), 1)
            .await
            .unwrap();
        assert!(f.opens() > opened);
        assert_eq!(
            r.load_messages(c.id, None, 50).await.unwrap().items.len(),
            1
        );
        assert_eq!(
            (
                f.message_facts(m.key).await.unwrap().jobs,
                f.message_facts(m.key).await.unwrap().messages
            ),
            (1, 1)
        );
        let bytes = f.message_facts(m.key).await.unwrap().wire;
        f.close().await;
        let f = OwnedFixture(Some(
            Fixture::open(dir.path(), support::protector())
                .await
                .unwrap(),
        ));
        let r = f.messages().await.unwrap();
        assert_eq!(
            r.enqueue(peer(2), id(1), "original".into(), 999)
                .await
                .unwrap(),
            m
        );
        assert!(
            f.message_facts(m.key).await.unwrap().wire == bytes,
            "immutable retry wire changed"
        );
        f.close().await;
    }
}

#[tokio::test]
async fn utf8_limits_and_lamport_are_exact() {
    let (_dir, f) = ready().await;
    let r = f.messages().await.unwrap();
    let body = "한".repeat(5461) + "x";
    assert_eq!(body.len(), 16_384);
    let m = r
        .enqueue(peer(2), id(1), body.clone(), i64::MAX as u64)
        .await
        .unwrap();
    assert!(m.body == body, "body preservation failed");
    assert_eq!(
        r.enqueue(peer(2), id(2), body + "x", 1)
            .await
            .unwrap_err()
            .code,
        ErrorCode::MessageTooLarge
    );
    assert_eq!(
        r.enqueue(peer(2), id(2), String::new(), 1)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidMessage
    );
    assert_eq!(
        r.enqueue(peer(2), id(2), "x".into(), i64::MAX as u64 + 1)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidMessage
    );
    f.set_message_lamport(m.key.conversation, i64::MAX as u64 - 1)
        .await
        .unwrap();
    assert_eq!(
        r.enqueue(peer(2), id(2), "last".into(), 1)
            .await
            .unwrap()
            .lamport,
        i64::MAX as u64
    );
    assert_eq!(
        r.enqueue(peer(2), id(3), "overflow".into(), 1)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidMessage
    );
    assert!(!format!("{m:?}").contains(&m.body));
    f.close().await;
}

#[tokio::test]
async fn page_event_race_converges_without_duplicates() {
    let (_dir, f) = ready().await;
    let r = f.messages().await.unwrap();
    let mut all = Vec::new();
    for n in 1..=55 {
        all.push(r.enqueue(peer(2), id(n), format!("{n}"), 1).await.unwrap());
    }
    let c = all[0].key.conversation;
    for limit in [0, 51] {
        assert_eq!(
            r.load_messages(c, None, limit).await.unwrap_err().code,
            ErrorCode::InvalidMessage
        );
        assert_eq!(
            r.list_conversations(None, limit).await.unwrap_err().code,
            ErrorCode::InvalidMessage
        );
    }
    let first = r.load_messages(c, None, 50).await.unwrap();
    assert_eq!(first.items.len(), 50);
    let cursor = first.next.unwrap();
    assert_eq!(
        cursor,
        MessageCursor {
            lamport: 6,
            installation: all[0].key.installation,
            id: id(6)
        }
    );
    let live = r.enqueue(peer(2), id(56), "live".into(), 1).await.unwrap();
    let tail = r.load_messages(c, Some(cursor), 50).await.unwrap();
    let mut merged = first.items;
    merged.extend(tail.items);
    merged.push(live);
    merged.sort_by_key(|m| m.key);
    merged.dedup_by_key(|m| m.key);
    assert_eq!(merged.len(), 56);
    let a = r.list_conversations(None, 1).await.unwrap();
    let anchor = ConversationCursor {
        change_revision: a.items[0].change_revision,
        id: a.items[0].id,
    };
    r.enqueue(peer(2), id(57), "move anchor".into(), 1)
        .await
        .unwrap();
    assert_eq!(
        r.list_conversations(Some(anchor), 1)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidMessage
    );
    r.enqueue(peer(3), id(1), "other conversation".into(), 1)
        .await
        .unwrap();
    let first = r.list_conversations(None, 1).await.unwrap();
    let anchor = first.next.unwrap();
    r.enqueue(peer(2), id(58), "unrelated change".into(), 1)
        .await
        .unwrap();
    let tail = r.list_conversations(Some(anchor), 50).await.unwrap();
    assert!(tail.revision > first.revision);
    let restart = r.list_conversations(None, 50).await.unwrap();
    assert_eq!(restart.items.len(), 2);
    assert_eq!(restart.items[0].peer, peer(2));
    f.close().await;
}
