//! M6 Task 4: the device log (spec 5.1, 5.2). Entries are written inside the M5 seal/open and
//! Commit/Welcome transactions; pages, cursors and the 30-day retention keep it bounded.
#![cfg(all(feature = "test-hooks", debug_assertions))]
#[path = "support/m4_store.rs"]
#[allow(dead_code)]
mod support;
use chat_core::{
    ErrorCode,
    account::{InstallationId, PeerId},
    storage_fixture::{
        CommitFault, ConversationId, EntryBody, Fixture, LogEntry, MAILBOX_MAX_ENTRY_BODY,
        MailboxPage, MailboxState, MessageId, MessageKey, Messages, MlsFrame, MlsReply,
        mls_account, mls_app_from_wire, mls_commit_from_wire,
    },
};

const NOW: u64 = 5_000;
const RETENTION: u64 = 2_592_000_000;
const A: (u8, u8) = (41, 42);
const B: (u8, u8) = (51, 52);
const X: InstallationId = InstallationId([0x58; 32]);
const Y: InstallationId = InstallationId([0x59; 32]);

// Panic cleanup still joins the owned worker before the caller's TempDir drops.
struct OwnedFixture(Option<Fixture>);
impl std::ops::Deref for OwnedFixture {
    type Target = Fixture;
    fn deref(&self) -> &Fixture {
        self.0.as_ref().unwrap()
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
            f: OwnedFixture(Some(f)),
            m,
            _dir: dir,
            peer: account.peer,
            install: account.installation,
        }
    }
    async fn log(&self) -> MailboxState {
        self.f.mailbox_state().await.unwrap()
    }
    /// A `Frame` entry with a distinct body of `n`.
    async fn record(&self, n: u8, now: u64) -> u64 {
        self.f
            .mailbox_record(vec![n; 40], now)
            .await
            .unwrap()
            .expect("recorded")
    }
    async fn page(&self, puller: InstallationId, since_seq: u64, now: u64) -> MailboxPage {
        self.m.mailbox_page(puller, since_seq, now).await.unwrap()
    }
    async fn cursor(&self, device: InstallationId) -> Option<u64> {
        self.log()
            .await
            .cursors
            .iter()
            .find(|(i, _)| *i == device)
            .map(|(_, n)| *n)
    }
}

fn id(n: u8) -> MessageId {
    MessageId([n; 16])
}
fn seqs(entries: &[LogEntry]) -> Vec<u64> {
    entries.iter().map(|e| e.seq).collect()
}
fn ack(reply: &MlsReply) {
    assert!(matches!(reply, MlsReply::Ack { .. }), "expected Ack");
}

/// Two devices sharing a group: B made a KeyPackage, A created the group and B joined through
/// A's Welcome (sent by A's installation, as the actor checks).
async fn linked() -> (Side, Side, ConversationId, Vec<u8>) {
    let a = Side::new(A, B).await;
    let b = Side::new(B, A).await;
    let conversation = a.m.open_dm(a.peer, b.peer).await.unwrap().id;
    assert_eq!(b.m.open_dm(b.peer, a.peer).await.unwrap().id, conversation);
    let key_package = b.m.mls_key_package().await.unwrap();
    let created =
        a.m.mls_create_group(conversation, b.peer, vec![key_package], NOW)
            .await
            .unwrap();
    b.m.mls_join_group_from(
        conversation,
        a.peer,
        a.install,
        created.welcome.clone(),
        created.first_commit_hash,
        NOW,
    )
    .await
    .unwrap();
    (a, b, conversation, created.welcome)
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
/// Seals `n` and returns its MLS ciphertext.
async fn sealed(from: &Side, to: &Side, n: u8, body: &str) -> Vec<u8> {
    let key = send(from, to, n, body).await;
    from.m.mls_seal(key, NOW).await.unwrap().unwrap();
    let facts = from.f.mls_outbox(key).await.unwrap();
    mls_app_from_wire(&facts.wire).unwrap().1
}
async fn open(to: &Side, from: &Side, conversation: ConversationId, ct: &[u8]) -> MlsReply {
    to.m.mls_open_app(from.peer, from.install, conversation, ct.to_vec(), NOW)
        .await
        .unwrap()
}
fn app_body(conversation: ConversationId, ciphertext: &[u8]) -> Vec<u8> {
    MlsFrame::App {
        conversation,
        ciphertext: ciphertext.to_vec(),
    }
    .inner()
    .to_vec()
}

#[tokio::test]
async fn accepted_and_sent_frames_are_logged_in_processing_order() {
    let (a, b, conversation, welcome) = linked().await;
    let ct1 = sealed(&a, &b, 1, "one").await;
    let ct2 = sealed(&a, &b, 2, "two").await;
    ack(&open(&b, &a, conversation, &ct1).await);
    // B's own message, sealed between the two it opens.
    let ct3 = sealed(&b, &a, 3, "from b").await;
    ack(&open(&b, &a, conversation, &ct2).await);
    // A Commit made by A and applied by B.
    let epoch =
        a.m.mls_update(conversation, b.peer, NOW)
            .await
            .unwrap()
            .unwrap();
    let wire =
        a.f.mls_delivery_frame(conversation, b.install, 1, epoch)
            .await
            .unwrap()
            .unwrap();
    let (_, framed_epoch, commit) = mls_commit_from_wire(&wire).unwrap();
    b.m.mls_apply_commit(conversation, a.peer, a.peer, framed_epoch, commit.clone(), NOW)
        .await
        .unwrap();

    let welcome_body = MlsFrame::Welcome {
        conversation,
        welcome,
        sender_install: a.install,
    }
    .inner()
    .to_vec();
    let commit_body = MlsFrame::Commit {
        conversation,
        epoch: framed_epoch,
        commit,
    }
    .inner()
    .to_vec();

    let log = b.log().await;
    let got: Vec<_> = log
        .entries
        .iter()
        .map(|e| (e.kind, e.sender_peer, e.sender_install, e.msg_id, e.body.clone()))
        .collect();
    assert_eq!(
        got,
        vec![
            (1, a.peer, a.install, None, welcome_body.clone()),
            (1, a.peer, a.install, Some(id(1)), app_body(conversation, &ct1)),
            (1, b.peer, b.install, Some(id(3)), app_body(conversation, &ct3)),
            (1, a.peer, a.install, Some(id(2)), app_body(conversation, &ct2)),
            (1, a.peer, a.install, None, commit_body.clone()),
        ],
        "B logs in the order it processed"
    );
    assert!(log.entries.windows(2).all(|w| w[0].seq < w[1].seq));
    assert!(log.entries.iter().all(|e| e.conversation == conversation));
    assert!(log.entries.iter().all(|e| e.recorded_at_ms == NOW));

    // The sending side logs what it made: its Welcome once, its two messages, its Commit once.
    let got: Vec<_> =
        a.log()
            .await
            .entries
            .iter()
            .map(|e| (e.sender_install, e.msg_id, e.body.clone()))
            .collect();
    assert_eq!(
        got,
        vec![
            (a.install, None, welcome_body),
            (a.install, Some(id(1)), app_body(conversation, &ct1)),
            (a.install, Some(id(2)), app_body(conversation, &ct2)),
            (a.install, None, commit_body),
        ]
    );
}

#[tokio::test]
async fn a_receipt_is_logged_once_when_it_is_sealed() {
    let (a, b, conversation, _) = linked().await;
    let key = send(&a, &b, 1, "read me").await;
    a.m.mls_seal(key, NOW).await.unwrap().unwrap();
    let ct = mls_app_from_wire(&a.f.mls_outbox(key).await.unwrap().wire)
        .unwrap()
        .1;
    ack(&open(&b, &a, conversation, &ct).await);
    let before = b.log().await.entries.len();
    // Final review I2: the Read is sealed, and so logged, as the user reads.
    b.m.mark_read(conversation, vec![key], NOW).await.unwrap();
    assert_eq!(b.log().await.entries.len(), before + 1);

    let first =
        b.m.prepare_receipt_attempt(key, a.install, 2_000_000)
            .await
            .unwrap();
    // The attempts send the receipt sealed then: nothing new to log.
    b.m.prepare_receipt_attempt(key, a.install, 2_001_000)
        .await
        .unwrap();
    let log = b.log().await.entries;
    assert_eq!(log.len(), before + 1);
    let (_, receipt_ct) = mls_app_from_wire(&first).unwrap();
    let last = log.last().unwrap();
    assert_eq!(
        (last.sender_install, last.msg_id, last.body.clone()),
        (b.install, None, app_body(conversation, &receipt_ct))
    );

    // The receiver of the receipt logs it as from B's installation, with no message id.
    ack(&a
        .m
        .mls_open_app(b.peer, b.install, conversation, receipt_ct.clone(), NOW)
        .await
        .unwrap());
    let last = a.log().await.entries.pop().unwrap();
    assert_eq!(
        (last.sender_install, last.msg_id, last.body),
        (b.install, None, app_body(conversation, &receipt_ct))
    );
}

#[tokio::test]
async fn decrypt_failure_is_not_logged() {
    let (a, b, conversation, _) = linked().await;
    let before = b.log().await.entries;
    // Not an MLS message at all (class 5).
    assert!(matches!(
        open(&b, &a, conversation, &[7u8; 64]).await,
        MlsReply::DecryptFailed { .. }
    ));
    assert_eq!(b.log().await.entries, before);

    // A device without the group (class 4) logs nothing either.
    let lone = Side::new(B, A).await;
    lone.m.open_dm(lone.peer, a.peer).await.unwrap();
    let ct = sealed(&a, &b, 1, "for a group lone never joined").await;
    assert!(matches!(
        open(&lone, &a, conversation, &ct).await,
        MlsReply::DecryptFailed { .. }
    ));
    assert!(lone.log().await.entries.is_empty());
}

#[tokio::test]
async fn rejected_text_is_not_logged() {
    let (a, b, conversation, _) = linked().await;
    // A lamport far beyond what B has seen: M4 refuses such a text, so B does too.
    a.f.set_message_lamport(conversation, 5_000_000)
        .await
        .unwrap();
    let ct = sealed(&a, &b, 1, "too far ahead").await;
    let before = b.log().await.entries;
    let error =
        b.m.mls_open_app(a.peer, a.install, conversation, ct, NOW)
            .await
            .err()
            .unwrap();
    assert_eq!(error.code, ErrorCode::InvalidMessage);
    assert_eq!(b.log().await.entries, before);
}

#[tokio::test]
async fn same_frame_twice_is_logged_once() {
    let (a, b, conversation, _) = linked().await;
    let ct = sealed(&a, &b, 1, "twice").await;
    let before = b.log().await.entries.len();
    ack(&open(&b, &a, conversation, &ct).await);
    // The second one is answered from `processed_ct`.
    ack(&open(&b, &a, conversation, &ct).await);
    let log = b.log().await.entries;
    assert_eq!(log.len(), before + 1);
    assert_eq!(log.last().unwrap().body, app_body(conversation, &ct));
}

#[tokio::test]
async fn log_write_rolls_back_with_the_message() {
    let (a, b, conversation, _) = linked().await;
    let ct = sealed(&a, &b, 1, "survives the crash").await;
    let before = b.log().await;
    let counts = b.f.message_counts().await.unwrap();

    b.f.message_fault(CommitFault::BeforeCommit).await;
    let error =
        b.m.mls_open_app(a.peer, a.install, conversation, ct.clone(), NOW)
            .await
            .err()
            .unwrap();
    assert_eq!(error.code, ErrorCode::StorageUncertain);
    // Neither the message, its `processed_ct` row, nor the log entry survived.
    assert_eq!(b.f.message_counts().await.unwrap(), counts);
    assert_eq!(b.log().await, before);

    ack(&open(&b, &a, conversation, &ct).await);
    assert_eq!(b.log().await.entries.len(), before.entries.len() + 1);
}

/// The largest text pads to the largest bucket; its real frame still fits one page, so it is
/// logged on both sides (`MAX_ENTRY_BODY` never drops a kind 17 frame).
#[tokio::test]
async fn the_largest_text_is_logged() {
    let (a, b, conversation, _) = linked().await;
    let body = "x".repeat(16_384);
    let ct = sealed(&a, &b, 1, &body).await;
    let frame = app_body(conversation, &ct);
    assert!(frame.len() > 60_416 && frame.len() <= MAILBOX_MAX_ENTRY_BODY);
    assert_eq!(a.log().await.entries.last().unwrap().body, frame);
    ack(&open(&b, &a, conversation, &ct).await);
    assert_eq!(b.log().await.entries.last().unwrap().body, frame);
}

#[tokio::test]
async fn record_refuses_a_body_too_large_for_one_page_and_a_repeat() {
    let s = Side::new(A, B).await;
    assert_eq!(
        s.f.mailbox_record(vec![1; MAILBOX_MAX_ENTRY_BODY + 1], NOW)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        s.f.mailbox_record(vec![1; MAILBOX_MAX_ENTRY_BODY], NOW)
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        s.f.mailbox_record(vec![1; MAILBOX_MAX_ENTRY_BODY], NOW)
            .await
            .unwrap(),
        None
    );
    assert_eq!(s.log().await.entries.len(), 1);
}

#[tokio::test]
async fn page_starts_at_cursor_and_records_it() {
    let s = Side::new(A, B).await;
    s.m.mailbox_devices(vec![X, Y], NOW).await.unwrap();
    for n in 1..=5 {
        assert_eq!(s.record(n, NOW).await, n as u64);
    }
    let page = s.page(X, 3, NOW).await;
    assert_eq!(seqs(&page.entries), vec![3, 4, 5]);
    assert_eq!((page.next_seq, page.more), (6, false));
    assert!(page.entries[0].body == EntryBody::Frame(vec![3; 40]));
    assert_eq!(s.cursor(X).await, Some(3));

    // Below the cursor the page still starts at it, and the cursor never moves back.
    let page = s.page(X, 1, NOW).await;
    assert_eq!(seqs(&page.entries), vec![3, 4, 5]);
    assert_eq!(s.cursor(X).await, Some(3));

    let page = s.page(X, 6, NOW).await;
    assert!(page.entries.is_empty());
    assert_eq!((page.next_seq, page.more), (6, false));
    assert_eq!(s.cursor(X).await, Some(6));
    // Y has taken nothing yet: everything from its cursor on is kept.
    assert_eq!(s.cursor(Y).await, Some(1));
    assert_eq!(seqs(&s.page(Y, 1, NOW).await.entries), vec![1, 2, 3, 4, 5]);
}

#[tokio::test]
async fn a_full_page_says_more() {
    let s = Side::new(A, B).await;
    s.m.mailbox_devices(vec![X], NOW).await.unwrap();
    for n in 1..=70 {
        s.record(n, NOW).await;
    }
    let page = s.page(X, 1, NOW).await;
    assert_eq!(seqs(&page.entries), (1..=64).collect::<Vec<_>>());
    assert_eq!((page.next_seq, page.more), (65, true));
    let page = s.page(X, 65, NOW).await;
    assert_eq!(seqs(&page.entries), (65..=70).collect::<Vec<_>>());
    assert_eq!((page.next_seq, page.more), (71, false));
}

#[tokio::test]
async fn new_device_cursor_starts_after_current_head() {
    let s = Side::new(A, B).await;
    for n in 1..=5 {
        s.record(n, NOW).await;
    }
    s.m.mailbox_devices(vec![X], NOW).await.unwrap();
    assert_eq!(s.cursor(X).await, Some(6));
    assert_eq!(s.m.mailbox_pull_from(X).await.unwrap(), 0);
    let page = s.page(X, 0, NOW).await;
    assert!(page.entries.is_empty());
    assert_eq!((page.next_seq, page.more), (6, false));
    // Only what is recorded after it was linked reaches it.
    s.record(6, NOW).await;
    assert_eq!(seqs(&s.page(X, 6, NOW).await.entries), vec![6]);
}

/// Ruling 8: the next seq comes from the AUTOINCREMENT high-water mark, which survives a log
/// that was pruned empty.
#[tokio::test]
async fn new_device_after_everything_was_pruned_starts_after_the_high_water_mark() {
    let s = Side::new(A, B).await;
    for n in 1..=3 {
        s.record(n, NOW).await;
    }
    // No other device: nothing holds the entries.
    assert_eq!(s.m.mailbox_prune(NOW).await.unwrap(), 3);
    assert!(s.log().await.entries.is_empty());
    s.m.mailbox_devices(vec![X], NOW).await.unwrap();
    assert_eq!(s.cursor(X).await, Some(4));
    let page = s.page(X, 0, NOW).await;
    assert!(page.entries.is_empty());
    assert_eq!(page.next_seq, 4);
    assert_eq!(s.record(4, NOW).await, 4);
}

/// Ruling 4: an own device without a cursor (its link was not seen here yet) starts at the head.
#[tokio::test]
async fn page_for_a_device_without_a_cursor_starts_it_at_the_head() {
    let s = Side::new(A, B).await;
    s.record(1, NOW).await;
    s.record(2, NOW).await;
    let page = s.page(X, 1, NOW).await;
    assert!(page.entries.is_empty());
    assert_eq!((page.next_seq, page.more), (3, false));
    assert_eq!(s.cursor(X).await, Some(3));
}

#[tokio::test]
async fn pulls_remember_how_far_each_holder_was_taken() {
    let s = Side::new(A, B).await;
    assert_eq!(s.m.mailbox_pull_from(X).await.unwrap(), 0);
    assert_eq!(s.m.mailbox_pulled(X, 7).await.unwrap(), 7);
    // Never moves back.
    assert_eq!(s.m.mailbox_pulled(X, 5).await.unwrap(), 7);
    assert_eq!(s.m.mailbox_pull_from(X).await.unwrap(), 7);
    assert_eq!(s.m.mailbox_pull_from(Y).await.unwrap(), 0);
}

#[tokio::test]
async fn entries_all_cursors_passed_are_pruned() {
    let s = Side::new(A, B).await;
    s.m.mailbox_devices(vec![X, Y], NOW).await.unwrap();
    for n in 1..=5 {
        s.record(n, NOW).await;
    }
    s.page(X, 4, NOW).await;
    // Y still holds everything.
    assert_eq!(s.m.mailbox_prune(NOW).await.unwrap(), 0);
    assert_eq!(seqs(&s.page(Y, 1, NOW).await.entries), vec![1, 2, 3, 4, 5]);
    // Serving a page that moved Y's cursor prunes what both have passed.
    s.page(Y, 3, NOW).await;
    let kept: Vec<u64> = s.log().await.entries.iter().map(|e| e.seq).collect();
    assert_eq!(kept, vec![3, 4, 5]);
    s.page(Y, 6, NOW).await;
    let kept: Vec<u64> = s.log().await.entries.iter().map(|e| e.seq).collect();
    assert_eq!(kept, vec![4, 5]);
}

#[tokio::test]
async fn entries_older_than_30_days_are_pruned() {
    let s = Side::new(A, B).await;
    s.m.mailbox_devices(vec![X], NOW).await.unwrap();
    s.record(1, NOW).await;
    s.record(2, NOW + 1_000).await;
    s.f.mailbox_seed(X, NOW).await.unwrap();
    let state = s.log().await;
    assert_eq!((state.handoffs, state.status_ahead), (1, 1));

    // Exactly 30 days old is kept.
    assert_eq!(s.m.mailbox_prune(NOW + RETENTION).await.unwrap(), 0);
    // One millisecond more and it goes, cursor or not.
    assert_eq!(s.m.mailbox_prune(NOW + RETENTION + 1).await.unwrap(), 1);
    let state = s.log().await;
    assert_eq!(
        state.entries.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![2]
    );
    assert_eq!((state.handoffs, state.status_ahead), (0, 0));
    assert_eq!(s.cursor(X).await, Some(1));
}

#[tokio::test]
async fn revoked_device_cursor_is_dropped_and_no_longer_holds_entries() {
    let s = Side::new(A, B).await;
    s.m.mailbox_devices(vec![X, Y], NOW).await.unwrap();
    s.f.mailbox_seed(Y, NOW).await.unwrap();
    for n in 1..=3 {
        s.record(n, NOW).await;
    }
    s.page(X, 4, NOW).await;
    assert_eq!(s.log().await.entries.len(), 3, "Y still holds them");

    // Y is revoked.
    assert_eq!(s.m.mailbox_devices(vec![X], NOW).await.unwrap(), 3);
    let state = s.log().await;
    assert!(state.entries.is_empty());
    assert_eq!(state.cursors, vec![(X, 4)]);
    assert_eq!(state.pulls, vec![(X, 0)]);
    assert_eq!((state.devices, state.handoffs), (0, 0));
}

/// Review Focus 5: a device whose cursor is behind entries that retention removed resumes at
/// the oldest one kept, without stalling or an error.
#[tokio::test]
async fn cursor_behind_pruned_head_resumes_at_oldest_kept() {
    let s = Side::new(A, B).await;
    s.m.mailbox_devices(vec![X], NOW).await.unwrap();
    let later = NOW + 10 * 86_400_000;
    s.record(1, NOW).await;
    s.record(2, NOW).await;
    s.record(3, later).await;
    s.record(4, later).await;
    assert_eq!(s.m.mailbox_prune(NOW + RETENTION + 1).await.unwrap(), 2);

    let page = s.page(X, 1, NOW + RETENTION + 1).await;
    assert_eq!(seqs(&page.entries), vec![3, 4]);
    assert_eq!((page.next_seq, page.more), (5, false));

    // With everything gone it moves on to the head, not back to where it was.
    s.m.mailbox_pulled(X, 3).await.unwrap();
    assert_eq!(s.m.mailbox_prune(later + RETENTION + 1).await.unwrap(), 2);
    let page = s.page(X, 3, later + RETENTION + 1).await;
    assert!(page.entries.is_empty());
    assert_eq!((page.next_seq, page.more), (5, false));
}
