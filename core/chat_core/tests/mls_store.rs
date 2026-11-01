//! Task 6: seal and open run inside one SQL transaction with the MLS state they change.
#![cfg(all(feature = "test-hooks", debug_assertions))]
#[path = "support/m4_store.rs"]
#[allow(dead_code)]
mod support;
use chat_core::{
    ErrorCode,
    account::{InstallationId, PeerId},
    storage_fixture::{
        CommitFault, ConversationId, DeliveryState, Fixture, MessageId, MessageKey, Messages,
        MlsReply, mls_account, mls_app_from_wire,
    },
};
use sha2::{Digest, Sha256};

const NOW: u64 = 5_000;
const A: (u8, u8) = (41, 42);
const B: (u8, u8) = (51, 52);

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

/// One device: its own database, with the other device's account in its contacts.
struct Side {
    f: OwnedFixture,
    m: Messages,
    dir: tempfile::TempDir,
    peer: PeerId,
    install: InstallationId,
    seeds: ((u8, u8), (u8, u8)),
}
impl Side {
    async fn new(own: (u8, u8), other: (u8, u8)) -> Side {
        let dir = tempfile::tempdir().unwrap();
        let account = mls_account(own, other);
        let f = Fixture::open(dir.path(), support::protector())
            .await
            .unwrap();
        f.handle.commit_account(account.state).await.unwrap();
        let m = f.messages().await.unwrap();
        Side {
            f: OwnedFixture(Some(f)),
            m,
            dir,
            peer: account.peer,
            install: account.installation,
            seeds: (own, other),
        }
    }
    /// Closes the store, optionally rewrites it to the M4 (v1) shape, and opens it again.
    async fn reopen(self, downgrade: bool) -> Side {
        let Side {
            f,
            dir,
            peer,
            install,
            seeds,
            ..
        } = self;
        f.close().await;
        if downgrade {
            Fixture::downgrade_to_v1(dir.path(), support::protector()).unwrap();
        }
        let f = Fixture::open(dir.path(), support::protector())
            .await
            .unwrap();
        let m = f.messages().await.unwrap();
        Side {
            f: OwnedFixture(Some(f)),
            m,
            dir,
            peer,
            install,
            seeds,
        }
    }
}

fn id(n: u8) -> MessageId {
    MessageId([n; 16])
}
fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn ack(reply: &MlsReply) -> [u8; 32] {
    match reply {
        MlsReply::Ack { ct_hash } => *ct_hash,
        MlsReply::DecryptFailed { class, epoch } => {
            panic!("expected Ack, got DecryptFailed class {class} epoch {epoch}")
        }
        _ => panic!("expected Ack"),
    }
}
fn decrypt_failed(reply: &MlsReply) -> (u8, u64) {
    match reply {
        MlsReply::DecryptFailed { class, epoch } => (*class, *epoch),
        _ => panic!("expected DecryptFailed"),
    }
}

/// Two devices that already share a group: B made a KeyPackage, A created the group and B joined.
async fn linked_pair(a: Side) -> (Side, Side, ConversationId) {
    let b = Side::new(B, A).await;
    link(a, b).await
}
async fn link(a: Side, b: Side) -> (Side, Side, ConversationId) {
    let conversation = a.m.open_dm(a.peer, b.peer).await.unwrap().id;
    assert_eq!(b.m.open_dm(b.peer, a.peer).await.unwrap().id, conversation);
    let key_package = b.m.mls_key_package().await.unwrap();
    let created =
        a.m.mls_create_group(conversation, b.peer, vec![key_package], NOW)
            .await
            .unwrap();
    b.m.mls_join_group(
        conversation,
        a.peer,
        created.welcome,
        created.first_commit_hash,
        NOW,
    )
    .await
    .unwrap();
    (a, b, conversation)
}
async fn linked() -> (Side, Side, ConversationId) {
    linked_pair(Side::new(A, B).await).await
}

/// An M4-shaped outgoing message turned into the "awaiting encryption" (codec 3) row.
async fn send(from: &Side, to: &Side, n: u8, body: &str) -> MessageKey {
    let view = from
        .m
        .enqueue(to.peer, id(n), body.into(), NOW)
        .await
        .unwrap();
    from.f.mls_promote_outbox(view.key).await.unwrap();
    view.key
}
/// The MLS ciphertext of a sealed outbox row.
async fn ciphertext(from: &Side, key: MessageKey) -> Vec<u8> {
    let facts = from.f.mls_outbox(key).await.unwrap();
    assert_eq!(facts.codec, Some(2));
    mls_app_from_wire(&facts.wire).unwrap().1
}
async fn open(to: &Side, from: &Side, conversation: ConversationId, ct: &[u8]) -> MlsReply {
    to.m.mls_open_app(from.peer, from.install, conversation, ct.to_vec(), NOW)
        .await
        .unwrap()
}

#[tokio::test]
async fn seal_then_open_stores_once_and_acks() {
    let (a, b, conversation) = linked().await;
    let key = send(&a, &b, 1, "hello mls").await;
    let sealed = a.m.mls_seal(key, NOW).await.unwrap().expect("group exists");

    let facts = a.f.mls_outbox(key).await.unwrap();
    assert_eq!(facts.codec, Some(2));
    assert_eq!((facts.reencrypts, facts.waiting_peer_update), (0, false));
    assert!(!facts.wire.windows(9).any(|w| w == b"hello mls"));
    let (framed_conversation, ct) = mls_app_from_wire(&facts.wire).unwrap();
    assert_eq!(framed_conversation, conversation);
    assert_eq!(hash(&ct), sealed);
    // Sealing makes it ready, not delivered.
    let sent = a.m.load_messages(conversation, None, 50).await.unwrap();
    assert_eq!(sent.items[0].state, DeliveryState::Queued);

    let reply = open(&b, &a, conversation, &ct).await;
    assert_eq!(ack(&reply), sealed);
    let page = b.m.load_messages(conversation, None, 50).await.unwrap();
    assert_eq!(page.items.len(), 1);
    let stored = &page.items[0];
    assert_eq!(stored.key, key);
    assert_eq!(stored.body, "hello mls");
    assert!(!stored.outgoing);
    assert_eq!(stored.lamport, sent.items[0].lamport);
    assert_eq!(
        b.m.list_conversations(None, 50).await.unwrap().items[0].unread,
        1
    );
    let state = b.f.mls_state(conversation).await.unwrap();
    assert_eq!(state.processed_mls, 1);
    assert_eq!(state.peer_mls, Some(true));

    // The same ciphertext again changes nothing and is acknowledged again.
    let counts = b.f.message_counts().await.unwrap();
    let again = open(&b, &a, conversation, &ct).await;
    assert_eq!(ack(&again), sealed);
    assert_eq!(b.f.message_counts().await.unwrap(), counts);
    assert_eq!(
        b.m.list_conversations(None, 50).await.unwrap().items[0].unread,
        1
    );
}

#[tokio::test]
async fn crash_between_open_and_store_rolls_back_both() {
    let (a, b, conversation) = linked().await;
    let key = send(&a, &b, 1, "survives the crash").await;
    a.m.mls_seal(key, NOW).await.unwrap().unwrap();
    let ct = ciphertext(&a, key).await;
    let before = b.f.mls_state(conversation).await.unwrap();
    let counts = b.f.message_counts().await.unwrap();

    // The process dies after the ratchet moved and the message row was written, before commit.
    b.f.message_fault(CommitFault::BeforeCommit).await;
    let error =
        b.m.mls_open_app(a.peer, a.install, conversation, ct.clone(), NOW)
            .await
            .err()
            .unwrap();
    assert_eq!(error.code, ErrorCode::StorageUncertain);

    // Neither the ratchet nor the message nor the duplicate marker survived.
    let after = b.f.mls_state(conversation).await.unwrap();
    assert_eq!(after.kv_digest, before.kv_digest);
    assert_eq!(after.processed_mls, 0);
    assert_eq!(b.f.message_counts().await.unwrap(), counts);

    // So the very same ciphertext opens again, and is stored exactly once.
    let reply = open(&b, &a, conversation, &ct).await;
    assert_eq!(ack(&reply), hash(&ct));
    let page = b.m.load_messages(conversation, None, 50).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].body, "survives the crash");
    assert_ne!(
        b.f.mls_state(conversation).await.unwrap().kv_digest,
        before.kv_digest
    );
}

#[tokio::test]
async fn reencrypted_resend_stores_once() {
    let (a, b, conversation) = linked().await;
    let key = send(&a, &b, 1, "said twice").await;
    a.m.mls_seal(key, NOW).await.unwrap().unwrap();
    let first = ciphertext(&a, key).await;
    assert_eq!(ack(&open(&b, &a, conversation, &first).await), hash(&first));

    // The receiver reported a decrypt failure, so the sender seals the same message again.
    a.m.mls_reencrypt(key, NOW + 1).await.unwrap();
    let facts = a.f.mls_outbox(key).await.unwrap();
    assert_eq!((facts.codec, facts.reencrypts), (Some(2), 1));
    let second = ciphertext(&a, key).await;
    assert_ne!(first, second);

    let reply = open(&b, &a, conversation, &second).await;
    assert_eq!(ack(&reply), hash(&second));
    let page = b.m.load_messages(conversation, None, 50).await.unwrap();
    assert_eq!(page.items.len(), 1, "same msg_id must stay one row");
    assert_eq!(page.items[0].body, "said twice");
    assert_eq!(
        b.m.list_conversations(None, 50).await.unwrap().items[0].unread,
        1
    );
    // Both ciphertexts are remembered, so neither is opened again.
    assert_eq!(b.f.mls_state(conversation).await.unwrap().processed_mls, 2);
}

#[tokio::test]
async fn duplicate_ciphertext_is_dropped_before_decrypt() {
    let (a, b, conversation) = linked().await;
    let key = send(&a, &b, 1, "once").await;
    a.m.mls_seal(key, NOW).await.unwrap().unwrap();
    let ct = ciphertext(&a, key).await;

    assert_eq!(b.f.mls_state(conversation).await.unwrap().open_calls, 0);
    ack(&open(&b, &a, conversation, &ct).await);
    assert_eq!(b.f.mls_state(conversation).await.unwrap().open_calls, 1);
    let digest = b.f.mls_state(conversation).await.unwrap().kv_digest;

    // Opening it again would be a replay (secret reuse); it must not even reach MLS.
    ack(&open(&b, &a, conversation, &ct).await);
    let state = b.f.mls_state(conversation).await.unwrap();
    assert_eq!(state.open_calls, 1);
    assert_eq!(state.kv_digest, digest);
    assert_eq!(
        b.m.load_messages(conversation, None, 50)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
}

#[tokio::test]
async fn own_echo_is_dropped() {
    let (a, b, conversation) = linked().await;
    let key = send(&a, &b, 1, "mine").await;
    a.m.mls_seal(key, NOW).await.unwrap().unwrap();
    let ct = ciphertext(&a, key).await;
    let counts = a.f.message_counts().await.unwrap();
    let digest = a.f.mls_state(conversation).await.unwrap().kv_digest;

    // The relay hands this device its own ciphertext back.
    let reply = open(&a, &a, conversation, &ct).await;
    assert_eq!(ack(&reply), hash(&ct));
    let state = a.f.mls_state(conversation).await.unwrap();
    assert_eq!(state.open_calls, 0);
    assert_eq!(state.kv_digest, digest);
    assert_eq!(a.f.message_counts().await.unwrap(), counts);
}

#[tokio::test]
async fn reencrypt_cap_marks_failed() {
    let (a, b, _) = linked().await;
    let key = send(&a, &b, 1, "never accepted").await;
    a.m.mls_seal(key, NOW).await.unwrap().unwrap();
    let state = |a: &Side| {
        let (m, key) = (a.m.clone(), key);
        async move {
            m.load_messages(key.conversation, None, 50)
                .await
                .unwrap()
                .items[0]
                .state
        }
    };

    for n in 1..=2u64 {
        a.m.mls_reencrypt(key, NOW + n).await.unwrap();
        assert_eq!(a.f.mls_outbox(key).await.unwrap().reencrypts, n);
        assert_eq!(state(&a).await, DeliveryState::Queued);
    }
    a.m.mls_reencrypt(key, NOW + 3).await.unwrap();
    let facts = a.f.mls_outbox(key).await.unwrap();
    assert_eq!((facts.codec, facts.reencrypts), (Some(2), 3));
    assert_eq!(state(&a).await, DeliveryState::Failed);

    // The user's Retry starts the count over and makes it sendable again.
    let view = a.m.retry_send(key, 2_000_000).await.unwrap();
    assert_eq!(view.state, DeliveryState::Queued);
    let facts = a.f.mls_outbox(key).await.unwrap();
    assert_eq!((facts.codec, facts.reencrypts), (Some(2), 0));
    assert_eq!(facts.dispatch_state, 0);
}

#[tokio::test]
async fn promoted_m4_outbox_seals_with_same_id_and_lamport() {
    // A queued M4 plaintext message, then the app updates: v1 -> v2 promotes it to codec 3.
    let a = Side::new(A, B).await;
    let b = Side::new(B, A).await;
    let m4 =
        a.m.enqueue(b.peer, id(7), "queued before the update".into(), 1_000)
            .await
            .unwrap();
    let a = a.reopen(true).await;
    let facts = a.f.mls_outbox(m4.key).await.unwrap();
    assert_eq!(facts.codec, Some(3));
    assert!(facts.wire.is_empty());

    let (a, b, conversation) = {
        // Group set-up on the promoted store.
        let conversation = a.m.open_dm(a.peer, b.peer).await.unwrap().id;
        b.m.open_dm(b.peer, a.peer).await.unwrap();
        let kp = b.m.mls_key_package().await.unwrap();
        let created =
            a.m.mls_create_group(conversation, b.peer, vec![kp], NOW)
                .await
                .unwrap();
        b.m.mls_join_group(
            conversation,
            a.peer,
            created.welcome,
            created.first_commit_hash,
            NOW,
        )
        .await
        .unwrap();
        (a, b, conversation)
    };
    // A device that joined later seals everything that was waiting.
    assert_eq!(
        a.m.mls_seal_pending(conversation, NOW).await.unwrap(),
        Some(1)
    );
    let ct = ciphertext(&a, m4.key).await;
    ack(&open(&b, &a, conversation, &ct).await);

    let stored =
        &b.m.load_messages(conversation, None, 50)
            .await
            .unwrap()
            .items[0];
    assert_eq!(stored.key, m4.key, "same message id");
    assert_eq!(stored.lamport, m4.lamport, "same lamport");
    assert_eq!(stored.body, "queued before the update");
    assert_eq!(stored.sent_at_ms, m4.sent_at_ms);
    let _ = (&a.dir, a.seeds);
}

#[tokio::test]
async fn seal_never_produces_codec1() {
    let (a, b, conversation) = linked().await;
    // An M4 plaintext job that has not been promoted is not sealed, and stays what it was.
    let legacy =
        a.m.enqueue(b.peer, id(9), "legacy plaintext job".into(), NOW)
            .await
            .unwrap()
            .key;
    let before = a.f.mls_outbox(legacy).await.unwrap();
    assert_eq!(before.codec, Some(1));
    assert_eq!(
        a.m.mls_seal(legacy, NOW).await.unwrap_err().code,
        ErrorCode::InvalidMessage
    );
    assert_eq!(
        a.m.mls_seal_pending(conversation, NOW).await.unwrap(),
        Some(0)
    );
    let after = a.f.mls_outbox(legacy).await.unwrap();
    assert_eq!((after.codec, &after.wire), (Some(1), &before.wire));

    // Everything that is sealed ends up as codec 2, however it gets there.
    let one = send(&a, &b, 1, "one").await;
    let two = send(&a, &b, 2, "two").await;
    a.m.mls_seal(one, NOW).await.unwrap().unwrap();
    assert_eq!(
        a.m.mls_seal_pending(conversation, NOW).await.unwrap(),
        Some(1)
    );
    a.m.mls_reencrypt(two, NOW).await.unwrap();
    for key in [one, two] {
        let facts = a.f.mls_outbox(key).await.unwrap();
        assert_eq!(facts.codec, Some(2));
        assert!(!facts.wire.is_empty());
    }
    // Re-sealing a codec 3 row would be sealing; Reencrypt only works on a sealed row.
    let three = send(&a, &b, 3, "three").await;
    assert_eq!(
        a.m.mls_reencrypt(three, NOW).await.unwrap_err().code,
        ErrorCode::InvalidMessage
    );
    assert_eq!(a.f.mls_outbox(three).await.unwrap().codec, Some(3));
    // The legacy job never changed through any of it.
    assert_eq!(a.f.mls_outbox(legacy).await.unwrap().codec, Some(1));
}

#[tokio::test]
async fn seal_without_a_group_asks_for_one() {
    let a = Side::new(A, B).await;
    let peer = mls_account(B, A).peer;
    let conversation = a.m.open_dm(a.peer, peer).await.unwrap().id;
    let view =
        a.m.enqueue(peer, id(1), "no group yet".into(), NOW)
            .await
            .unwrap();
    a.f.mls_promote_outbox(view.key).await.unwrap();
    assert_eq!(a.m.mls_seal(view.key, NOW).await.unwrap(), None);
    assert_eq!(a.m.mls_seal_pending(conversation, NOW).await.unwrap(), None);
    let facts = a.f.mls_outbox(view.key).await.unwrap();
    assert_eq!((facts.codec, facts.wire.len()), (Some(3), 0));
}

#[tokio::test]
async fn seal_pending_seals_waiting_rows_in_lamport_order() {
    let (a, b, conversation) = linked().await;
    let keys = [
        send(&a, &b, 3, "first").await,
        send(&a, &b, 1, "second").await,
        send(&a, &b, 2, "third").await,
    ];
    assert_eq!(
        a.m.mls_seal_pending(conversation, NOW).await.unwrap(),
        Some(3)
    );
    // Opening in sealing order works only if the ratchet was walked in lamport order.
    for key in keys {
        let ct = ciphertext(&a, key).await;
        ack(&open(&b, &a, conversation, &ct).await);
    }
    let bodies: Vec<_> =
        b.m.load_messages(conversation, None, 50)
            .await
            .unwrap()
            .items
            .into_iter()
            .rev()
            .map(|m| m.body)
            .collect();
    assert_eq!(bodies, ["first", "second", "third"]);
    // Nothing is left to seal.
    assert_eq!(
        a.m.mls_seal_pending(conversation, NOW).await.unwrap(),
        Some(0)
    );
}

#[tokio::test]
async fn decrypt_failures_report_a_class_and_change_nothing() {
    let (a, b, conversation) = linked().await;
    let before = b.f.mls_state(conversation).await.unwrap();
    let counts = b.f.message_counts().await.unwrap();

    // Not an MLS message at all.
    let reply = open(&b, &a, conversation, &[7u8; 64]).await;
    assert_eq!(decrypt_failed(&reply), (5, 1));

    // A device that has no group for the conversation.
    let lone = Side::new(B, A).await;
    let lone_conversation = lone.m.open_dm(lone.peer, a.peer).await.unwrap().id;
    assert_eq!(lone_conversation, conversation);
    let key = send(&a, &b, 1, "for a group lone never joined").await;
    a.m.mls_seal(key, NOW).await.unwrap().unwrap();
    let ct = ciphertext(&a, key).await;
    let before_lone = lone.f.mls_state(conversation).await.unwrap();
    let reply = open(&lone, &a, conversation, &ct).await;
    assert_eq!(decrypt_failed(&reply), (4, 0));
    assert_eq!(
        lone.f.mls_state(conversation).await.unwrap().kv_digest,
        before_lone.kv_digest
    );

    // The failures left no trace on b either: the real message still opens afterwards.
    let after = b.f.mls_state(conversation).await.unwrap();
    assert_eq!(
        (after.kv_digest, after.processed_mls),
        (before.kv_digest, 0)
    );
    assert_eq!(b.f.message_counts().await.unwrap(), counts);
    ack(&open(&b, &a, conversation, &ct).await);
}

#[tokio::test]
async fn a_message_from_another_sender_than_its_transport_is_rejected() {
    let (a, b, conversation) = linked().await;
    let key = send(&a, &b, 1, "claimed by someone else").await;
    a.m.mls_seal(key, NOW).await.unwrap().unwrap();
    let ct = ciphertext(&a, key).await;
    let before = b.f.mls_state(conversation).await.unwrap();

    // Transport says the sender is another installation of A's account.
    let error =
        b.m.mls_open_app(
            a.peer,
            InstallationId([9; 32]),
            conversation,
            ct.clone(),
            NOW,
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::InvalidMessage);
    // Transport says the sender is not in this conversation.
    let error =
        b.m.mls_open_app(PeerId([8; 32]), a.install, conversation, ct.clone(), NOW)
            .await
            .err()
            .unwrap();
    assert_eq!(error.code, ErrorCode::InvalidMessage);

    let after = b.f.mls_state(conversation).await.unwrap();
    assert_eq!(
        (after.kv_digest, after.processed_mls),
        (before.kv_digest, 0)
    );
    ack(&open(&b, &a, conversation, &ct).await);
}

#[tokio::test]
async fn waiting_peer_update_is_a_flag_on_the_outbox_row() {
    let (a, b, _) = linked().await;
    let key = send(&a, &b, 1, "needs a newer peer").await;
    a.m.mls_mark_waiting(key, true).await.unwrap();
    assert!(a.f.mls_outbox(key).await.unwrap().waiting_peer_update);
    a.m.mls_mark_waiting(key, false).await.unwrap();
    assert!(!a.f.mls_outbox(key).await.unwrap().waiting_peer_update);
}

#[tokio::test]
async fn group_rows_are_kept_and_dropped_with_their_state() {
    let (a, b, conversation) = linked().await;
    let state = a.f.mls_state(conversation).await.unwrap();
    assert_eq!(state.group_epoch, Some(1));
    assert_eq!(state.peer_mls, Some(false));
    assert!(state.kv_rows > 0);
    let joined = b.f.mls_state(conversation).await.unwrap();
    assert_eq!(joined.group_epoch, Some(1));

    // Creating a second group for the same conversation is refused; it never overwrites.
    let kp = b.m.mls_key_package().await.unwrap();
    assert!(
        a.m.mls_create_group(conversation, b.peer, vec![kp], NOW)
            .await
            .is_err()
    );
    assert_eq!(
        a.f.mls_state(conversation).await.unwrap().kv_digest,
        state.kv_digest
    );

    a.m.mls_drop_group(conversation).await.unwrap();
    let state = a.f.mls_state(conversation).await.unwrap();
    assert_eq!(state.group_epoch, None);
    let key = send(&a, &b, 1, "after the drop").await;
    assert_eq!(a.m.mls_seal(key, NOW).await.unwrap(), None);
}

#[tokio::test]
async fn an_uncertain_seal_is_settled_against_the_stored_row() {
    let (a, b, conversation) = linked().await;

    // Rolled back: the row is still waiting, and the next operation (which first reconciles
    // the uncertain one) seals it normally.
    let waiting = send(&a, &b, 1, "sealed after a rollback").await;
    a.f.message_fault(CommitFault::BeforeCommit).await;
    assert_eq!(
        a.m.mls_seal(waiting, NOW).await.unwrap_err().code,
        ErrorCode::StorageUncertain
    );
    assert_eq!(a.f.mls_outbox(waiting).await.unwrap().codec, Some(3));
    let hash = a.m.mls_seal(waiting, NOW).await.unwrap().unwrap();
    let ct = ciphertext(&a, waiting).await;
    assert_eq!(ack(&open(&b, &a, conversation, &ct).await), hash);

    // Committed, but the answer was lost: sealing again reports the ciphertext that is stored.
    let committed = send(&a, &b, 2, "sealed before the answer was lost").await;
    a.f.message_fault(CommitFault::AfterCommit).await;
    assert_eq!(
        a.m.mls_seal(committed, NOW).await.unwrap_err().code,
        ErrorCode::StorageUncertain
    );
    let stored = ciphertext(&a, committed).await;
    let again = a.m.mls_seal(committed, NOW).await.unwrap().unwrap();
    assert_eq!(again, hash_of(&stored));
    assert_eq!(ciphertext(&a, committed).await, stored, "sealed once only");
    assert_eq!(ack(&open(&b, &a, conversation, &stored).await), again);
}
fn hash_of(bytes: &[u8]) -> [u8; 32] {
    hash(bytes)
}

#[tokio::test]
async fn sealing_keeps_the_m4_hash_so_old_receipts_still_apply() {
    // A message attempted under M4, then promoted by the update and sealed: the receipt the
    // peer's pre-update app sends for the canonical M4 text hash must still apply.
    let a = Side::new(A, B).await;
    let b = Side::new(B, A).await;
    let key =
        a.m.enqueue(b.peer, id(5), "attempted before the update".into(), 1_000)
            .await
            .unwrap()
            .key;
    a.m.prepare_attempt(key, b.install, 2_000_000)
        .await
        .unwrap();
    a.m.settle_attempt(key, true).await.unwrap();
    let canonical = a.f.message_facts(key).await.unwrap().hash;
    let a = a.reopen(true).await;
    assert_eq!(a.f.mls_outbox(key).await.unwrap().codec, Some(3));

    let (a, b, conversation) = link(a, b).await;
    assert_eq!(
        a.m.mls_seal_pending(conversation, NOW).await.unwrap(),
        Some(1)
    );
    a.m.mls_reencrypt(key, NOW).await.unwrap();
    assert_eq!(a.f.message_facts(key).await.unwrap().hash, canonical);

    a.m.apply_delivered_receipt(key, canonical, b.install, b.peer, 2_000_001)
        .await
        .unwrap();
    let page = a.m.load_messages(conversation, None, 50).await.unwrap();
    assert_eq!(page.items[0].state, DeliveryState::Delivered);
}

#[tokio::test]
async fn sealing_respects_the_outbox_byte_cap() {
    let (a, b, conversation) = linked().await;
    let first = send(&a, &b, 1, "first").await;
    let second = send(&a, &b, 2, "second").await;
    let third = send(&a, &b, 3, "third").await;
    a.m.mls_seal(first, NOW).await.unwrap().unwrap();
    let frame = a.f.mls_outbox(first).await.unwrap().wire.len() as u64;
    let digest = a.f.mls_state(conversation).await.unwrap().kv_digest;
    let processed = a.f.mls_state(conversation).await.unwrap().processed_mls;

    // One more frame does not fit: nothing is written and the ratchet did not move.
    a.f.mls_outbox_cap(Some(frame + 10)).await;
    assert_eq!(
        a.m.mls_seal(second, NOW).await.unwrap_err().code,
        ErrorCode::StorageFull
    );
    // The batch is atomic: the first of two rows would fit, but then neither is sealed.
    a.f.mls_outbox_cap(Some(2 * frame + 10)).await;
    assert_eq!(
        a.m.mls_seal_pending(conversation, NOW)
            .await
            .unwrap_err()
            .code,
        ErrorCode::StorageFull
    );
    // Sealing again must not grow the outbox either.
    a.f.mls_outbox_cap(Some(frame - 1)).await;
    assert_eq!(
        a.m.mls_reencrypt(first, NOW).await.unwrap_err().code,
        ErrorCode::StorageFull
    );
    for key in [second, third] {
        let facts = a.f.mls_outbox(key).await.unwrap();
        assert_eq!((facts.codec, facts.wire.len()), (Some(3), 0));
    }
    assert_eq!(a.f.mls_outbox(first).await.unwrap().reencrypts, 0);
    let state = a.f.mls_state(conversation).await.unwrap();
    assert_eq!((state.kv_digest, state.processed_mls), (digest, processed));

    // With room again everything goes through.
    a.f.mls_outbox_cap(None).await;
    assert_eq!(
        a.m.mls_seal_pending(conversation, NOW).await.unwrap(),
        Some(2)
    );
    a.m.mls_reencrypt(first, NOW).await.unwrap();
}

#[tokio::test]
async fn reencrypt_waits_for_an_attempt_and_a_failed_message_stays_failed() {
    let (a, b, conversation) = linked().await;
    let key = send(&a, &b, 1, "being sent").await;
    a.m.mls_seal(key, NOW).await.unwrap().unwrap();
    let state = |a: &Side| {
        let m = a.m.clone();
        async move { m.load_messages(conversation, None, 50).await.unwrap().items[0].state }
    };

    // An attempt is in flight: re-encrypting now would race its outcome.
    a.m.prepare_attempt(key, b.install, 2_000_000)
        .await
        .unwrap();
    let before = a.f.mls_outbox(key).await.unwrap();
    assert_eq!(before.dispatch_state, 1);
    let digest = a.f.mls_state(conversation).await.unwrap().kv_digest;
    assert_eq!(
        a.m.mls_reencrypt(key, NOW).await.unwrap_err().code,
        ErrorCode::Busy
    );
    let after = a.f.mls_outbox(key).await.unwrap();
    assert_eq!(
        (after.reencrypts, after.wire, after.dispatch_state),
        (0, before.wire, 1)
    );
    assert_eq!(a.f.mls_state(conversation).await.unwrap().kv_digest, digest);

    // Once settled it can be re-encrypted until it is given up on.
    a.m.settle_attempt(key, false).await.unwrap();
    for n in 1..=3 {
        a.m.mls_reencrypt(key, NOW + n).await.unwrap();
    }
    assert_eq!(state(&a).await, DeliveryState::Failed);

    // A late attempt outcome cannot revive the failed message.
    a.m.settle_attempt(key, true).await.unwrap();
    assert_eq!(state(&a).await, DeliveryState::Failed);
    let facts = a.f.mls_outbox(key).await.unwrap();
    assert_eq!((facts.reencrypts, facts.dispatch_state), (3, 2));
}

#[tokio::test]
async fn due_never_returns_rows_waiting_for_encryption() {
    let (a, b, _) = linked().await;
    // An M4 plaintext job is due as before.
    let view =
        a.m.enqueue(b.peer, id(1), "plaintext job".into(), NOW)
            .await
            .unwrap();
    assert_eq!(a.m.due_keys(10).await.unwrap(), [view.key]);
    assert!(a.f.due_hint(10).await.unwrap());

    // Promoted, it waits for encryption: not handed out, and no scheduling hint.
    a.f.mls_promote_outbox(view.key).await.unwrap();
    assert!(a.m.due_keys(10).await.unwrap().is_empty());
    assert!(!a.f.due_hint(10).await.unwrap());

    // Sealed, it is due.
    a.m.mls_seal(view.key, NOW).await.unwrap().unwrap();
    assert_eq!(a.m.due_keys(10).await.unwrap(), [view.key]);
    assert!(a.f.due_hint(10).await.unwrap());
}
