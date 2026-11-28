//! M7 Task 4: a push payload's ciphertext carries no conversation id, and a received
//! `PushHandle` goes into `push_peer`, never into the messages.
#![cfg(all(feature = "test-hooks", debug_assertions))]
#[path = "support/m4_store.rs"]
#[allow(dead_code)]
mod support;
use chat_core::{
    account::{InstallationId, PeerId},
    storage_fixture::{
        ConversationId, Fixture, MessageId, MessageKey, Messages, MlsReply, PushCtError,
        PushPeerRow, QueueRefBody, mls_account, mls_app_from_wire, restore_group_id,
        strip_group_id,
    },
};
use sha2::{Digest, Sha256};

const NOW: u64 = 5_000;
const A: (u8, u8) = (41, 42);
const B: (u8, u8) = (51, 52);

/// One device: its own database, with the other device's account in its contacts.
struct Side {
    f: Option<Fixture>,
    m: Messages,
    _dir: tempfile::TempDir,
    peer: PeerId,
    install: InstallationId,
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
            f: Some(f),
            m,
            _dir: dir,
            peer: account.peer,
            install: account.installation,
        }
    }
    fn f(&self) -> &Fixture {
        self.f.as_ref().unwrap()
    }
}
// Panic cleanup still joins the owned worker before the TempDir drops.
impl Drop for Side {
    fn drop(&mut self) {
        if let Some(f) = self.f.take() {
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

/// A and B share a group: B made a KeyPackage, A created the group, B joined. Also returns the
/// group id.
async fn linked() -> (Side, Side, ConversationId, Vec<u8>) {
    let (a, b) = (Side::new(A, B).await, Side::new(B, A).await);
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
    (a, b, conversation, created.group_id)
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn ack(reply: &MlsReply) -> [u8; 32] {
    match reply {
        MlsReply::Ack { ct_hash } => *ct_hash,
        _ => panic!("expected Ack"),
    }
}
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The MLS ciphertext of a text of `body` from `from` to `to`, sealed.
async fn sealed_text(from: &Side, to: &Side, n: u8, body: &str) -> (MessageKey, Vec<u8>) {
    let view = from
        .m
        .enqueue(to.peer, MessageId([n; 16]), body.into(), NOW)
        .await
        .unwrap();
    from.f().mls_promote_outbox(view.key).await.unwrap();
    from.m.mls_seal(view.key, NOW).await.unwrap().unwrap();
    let facts = from.f().mls_outbox(view.key).await.unwrap();
    (view.key, mls_app_from_wire(&facts.wire).unwrap().1)
}

#[tokio::test]
async fn stripped_ciphertext_has_no_conversation_id_and_restores() {
    let (a, b, conversation, group_id) = linked().await;
    assert!(group_id.starts_with(&conversation.0));
    let (key, ct) = sealed_text(&a, &b, 1, "where is the conversation id").await;
    assert!(
        contains(&ct, &conversation.0),
        "the plain ciphertext has it"
    );

    let stripped = strip_group_id(&ct).unwrap();
    assert!(!contains(&stripped, &conversation.0));
    assert!(!contains(&stripped, &group_id[32..]), "nor the random tail");
    let restored = restore_group_id(&stripped, &group_id).unwrap();
    assert_eq!(restored, ct);

    // The restored bytes are the sealed message: B opens and stores it.
    let reply =
        b.m.mls_open_app(a.peer, a.install, conversation, restored, NOW)
            .await
            .unwrap();
    assert_eq!(ack(&reply), hash(&ct));
    let page = b.m.load_messages(conversation, None, 10).await.unwrap();
    assert_eq!(page.items[0].key, key);

    // Another group id does not open (the AAD covers it).
    let (_, second) = sealed_text(&a, &b, 2, "second").await;
    let mut wrong = group_id.clone();
    wrong[40] ^= 1;
    let forged = restore_group_id(&strip_group_id(&second).unwrap(), &wrong).unwrap();
    let reply =
        b.m.mls_open_app(a.peer, a.install, conversation, forged, NOW)
            .await
            .unwrap();
    assert!(matches!(reply, MlsReply::DecryptFailed { .. }));
}

#[tokio::test]
async fn stripped_is_never_longer() {
    let (a, b, _, group_id) = linked().await;
    // One text per plaintext bucket: 1 KiB, the push bucket, 16 KiB.
    for (n, len) in [(1u8, 10usize), (2, 1_500), (3, 10_000)] {
        let (_, ct) = sealed_text(&a, &b, n, &"x".repeat(len)).await;
        let stripped = strip_group_id(&ct).unwrap();
        assert_eq!(stripped.len(), ct.len() - group_id.len(), "{len}");
        assert_eq!(restore_group_id(&stripped, &group_id).unwrap(), ct);
    }
    // Handshake messages are no PrivateMessage.
    let key_package = b.m.mls_key_package().await.unwrap();
    assert_eq!(strip_group_id(&key_package), Err(PushCtError::NotPrivate));
    assert_eq!(strip_group_id(&[1, 2]), Err(PushCtError::Malformed));
}

/// What B's store holds of A's handles, and how many notification marks.
async fn peers(side: &Side) -> Vec<PushPeerRow> {
    let state = side.f().push_peer_state().await.unwrap();
    assert_eq!(state.mailbox_notify, 0, "a handle is never notified");
    state.peers
}

#[tokio::test]
async fn push_handle_opens_into_push_peer_not_messages() {
    let (a, b, conversation, _) = linked().await;
    let counts = b.f().message_counts().await.unwrap();
    let log = b.f().mailbox_state().await.unwrap().entries.len();
    let unread = |listed: chat_core::ConversationPage| listed.items[0].unread;
    let unread_before = unread(b.m.list_conversations(None, 50).await.unwrap());
    // Brought by a pull, so that a text would be marked for its notification (Ruling 35).
    let holder = InstallationId([9; 32]);
    let open = |ct: Vec<u8>| {
        b.m.mls_open_app_via(
            a.peer,
            a.install,
            vec![a.install],
            Some(holder),
            conversation,
            ct,
            NOW,
        )
    };

    let handle = vec![0x5a; 300];
    let ct =
        a.m.mls_seal_push_handle(conversation, handle.clone(), 1_000)
            .await
            .unwrap();
    assert_eq!(ack(&open(ct.clone()).await.unwrap()), hash(&ct));
    let row = |handle: Option<Vec<u8>>, sent_at_ms: u64| PushPeerRow {
        conversation,
        installation: a.install,
        handle,
        sent_at_ms,
        received_at_ms: NOW,
    };
    assert_eq!(peers(&b).await, vec![row(Some(handle.clone()), 1_000)]);
    // Not a message: no row, nothing unread, nothing to notify. Taken (processed_ct) and in
    // the device log like any frame.
    let after = b.f().message_counts().await.unwrap();
    assert_eq!((after.0, after.2), (counts.0, counts.2));
    assert_eq!(after.1, counts.1 + 1);
    assert_eq!(
        unread(b.m.list_conversations(None, 50).await.unwrap()),
        unread_before
    );
    assert_eq!(b.f().mailbox_state().await.unwrap().entries.len(), log + 1);

    // The same ciphertext again: acknowledged, nothing changes.
    assert_eq!(ack(&open(ct.clone()).await.unwrap()), hash(&ct));
    assert_eq!(peers(&b).await, vec![row(Some(handle), 1_000)]);
    assert_eq!(b.f().message_counts().await.unwrap(), after);

    // A newer handle replaces it; the longest allowed one is kept.
    let longest = vec![0x6b; 1_024];
    let ct =
        a.m.mls_seal_push_handle(conversation, longest.clone(), 2_000)
            .await
            .unwrap();
    ack(&open(ct).await.unwrap());
    assert_eq!(peers(&b).await, vec![row(Some(longest.clone()), 2_000)]);

    // A handle longer than `push_peer` takes is acknowledged and ignored.
    let ct =
        a.m.mls_seal_push_handle(conversation, vec![0x7c; 1_025], 3_000)
            .await
            .unwrap();
    ack(&open(ct).await.unwrap());
    assert_eq!(peers(&b).await, vec![row(Some(longest), 2_000)]);

    // An empty handle withdraws it: the row stays as a tombstone (handle NULL).
    let ct =
        a.m.mls_seal_push_handle(conversation, Vec::new(), 4_000)
            .await
            .unwrap();
    ack(&open(ct).await.unwrap());
    assert_eq!(peers(&b).await, vec![row(None, 4_000)]);
    let last = b.f().message_counts().await.unwrap();
    assert_eq!((last.0, last.2), (counts.0, counts.2));
    assert_eq!(
        unread(b.m.list_conversations(None, 50).await.unwrap()),
        unread_before
    );
}

/// M8 spec 7.1: a `QueueRef`, like a `PushHandle`, is no text. Brought by a pull it is taken
/// (processed_ct), logged and acknowledged, with no message row, nothing unread and no
/// notification mark; a withdrawal likewise.
#[tokio::test]
async fn queue_ref_is_not_a_text() {
    let (a, b, conversation, _) = linked().await;
    let counts = b.f().message_counts().await.unwrap();
    let log = b.f().mailbox_state().await.unwrap().entries.len();
    let unread = |listed: chat_core::ConversationPage| listed.items[0].unread;
    let unread_before = unread(b.m.list_conversations(None, 50).await.unwrap());
    let holder = InstallationId([9; 32]);
    let open = |ct: Vec<u8>| {
        b.m.mls_open_app_via(
            a.peer,
            a.install,
            vec![a.install],
            Some(holder),
            conversation,
            ct,
            NOW,
        )
    };
    let queue = QueueRefBody {
        server_url: "https://queue.example".into(),
        queue_id: [0x31; 32],
        sender_secret: [0x32; 32].into(),
    };
    let sealed = [
        a.m.mls_seal_queue_ref(conversation, Some(queue), 1_000)
            .await
            .unwrap(),
        a.m.mls_seal_queue_ref(conversation, None, 2_000)
            .await
            .unwrap(),
    ];
    for (n, ct) in sealed.into_iter().enumerate() {
        assert_eq!(ack(&open(ct.clone()).await.unwrap()), hash(&ct));
        let after = b.f().message_counts().await.unwrap();
        assert_eq!((after.0, after.2), (counts.0, counts.2));
        assert_eq!(after.1, counts.1 + 1 + n as u64);
        assert_eq!(
            unread(b.m.list_conversations(None, 50).await.unwrap()),
            unread_before
        );
        assert_eq!(
            b.f().mailbox_state().await.unwrap().entries.len(),
            log + 1 + n
        );
        // Not a push handle either.
        assert!(peers(&b).await.is_empty());
    }
}

/// `to` opens `from`'s ciphertext (acknowledged) and returns its `push_peer` rows.
async fn open_then_peers(
    from: &Side,
    to: &Side,
    conversation: ConversationId,
    ct: Vec<u8>,
) -> Vec<PushPeerRow> {
    let reply =
        to.m.mls_open_app(from.peer, from.install, conversation, ct, NOW)
            .await
            .unwrap();
    ack(&reply);
    peers(to).await
}

/// Ruling R10: frames arrive out of order (the device log, a hand-over); a `PushHandle` sent
/// earlier than the stored one, or at the same time, never replaces it, whether that is a handle
/// or a withdrawal.
#[tokio::test]
async fn older_push_handles_never_replace_newer_or_withdrawn() {
    let (a, b, conversation, _) = linked().await;
    let seal = |handle: &[u8], sent_at: u64| {
        a.m.mls_seal_push_handle(conversation, handle.to_vec(), sent_at)
    };
    // Sealed in this order, opened below in another.
    let h1 = seal(b"handle one", 1_000).await.unwrap();
    let h2 = seal(b"handle two", 2_000).await.unwrap();
    let h3 = seal(b"handle three", 2_500).await.unwrap();
    let withdrawn = seal(b"", 3_000).await.unwrap();
    let h4 = seal(b"handle four", 4_000).await.unwrap();
    let h5 = seal(b"handle five", 4_000).await.unwrap();
    let open = |ct: Vec<u8>| open_then_peers(&a, &b, conversation, ct);
    let row = |handle: Option<&[u8]>, sent_at_ms: u64| {
        vec![PushPeerRow {
            conversation,
            installation: a.install,
            handle: handle.map(<[u8]>::to_vec),
            sent_at_ms,
            received_at_ms: NOW,
        }]
    };

    // An older handle after a newer one: the newer stays.
    assert_eq!(open(h2).await, row(Some(b"handle two"), 2_000));
    assert_eq!(open(h1).await, row(Some(b"handle two"), 2_000));
    // An older handle after a withdrawal: it stays withdrawn.
    assert_eq!(open(withdrawn).await, row(None, 3_000));
    assert_eq!(open(h3).await, row(None, 3_000));
    // A newer handle after the withdrawal replaces it.
    assert_eq!(open(h4).await, row(Some(b"handle four"), 4_000));
    // The same send time: the stored row stays.
    assert_eq!(open(h5).await, row(Some(b"handle four"), 4_000));
}
