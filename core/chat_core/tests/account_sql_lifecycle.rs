#![cfg(all(feature = "test-hooks", debug_assertions))]
use chat_core::account::{
    AccountHandle, AccountPhase, Secret32, StoreId,
    storage::{KeyProtector, Store, StoredPlaintext},
};
use chat_core::storage_fixture::{Fixture, migration_states};
use chat_core::{CoreConfig, CoreError, CoreSlot, ErrorCode, EventBus};
use std::{sync::Arc, time::Duration};
#[path = "support/account_services.rs"]
mod services;
#[path = "support/account_store.rs"]
#[allow(dead_code)]
mod support;

async fn open(root: &std::path::Path) -> (Arc<CoreSlot>, AccountHandle) {
    let slot = Arc::new(CoreSlot::new());
    slot.start(
        CoreConfig {
            data_dir: root.into(),
        },
        &EventBus::new(),
    )
    .unwrap();
    slot.network().unwrap().configure_local(None).await.unwrap();
    let owned = slot.clone();
    let account = tokio::task::spawn_blocking(move || {
        owned.open_account_with_services(support::protector(), services::offline())
    })
    .await
    .unwrap()
    .unwrap();
    settled(&account).await;
    (slot, account)
}
async fn settled(account: &AccountHandle) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(account.lifecycle_status(), 0);
}

#[tokio::test]
async fn sql_pending_import_receipt_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let pending = migration_states().remove(3).unwrap();
    Store::open(dir.path(), support::protector())
        .unwrap()
        .commit(&pending)
        .unwrap();
    let (slot, account) = open(dir.path()).await;
    let before = account.snapshot().await.unwrap();
    assert_eq!(before.phase, AccountPhase::PendingImport);
    let peer = chat_core::account::PeerId([7; 32]);
    assert_eq!(account.messages().open_dm(peer).await.unwrap_err().code, ErrorCode::AccountRestricted);
    assert_eq!(account.messages().send_text(peer, chat_core::MessageId([7; 16]), "pending fixture".into()).await.unwrap_err().code, ErrorCode::AccountRestricted);
    assert_eq!(account.storage_test_fixture().message_counts().await.unwrap(), (0, 0, 0, 0));
    assert!(!before.pending_pair_transactions.is_empty());
    chat_core::storage_fixture::assert_account_preserved(
        &pending,
        &account
            .storage_test_fixture()
            .handle
            .load_account()
            .await
            .unwrap()
            .unwrap(),
    );
    assert!(
        dir.path().join("storage.mode").is_file(),
        "actor startup must migrate to SQL"
    );
    account.request_suspend();
    settled(&account).await;
    drop(account);
    drop(slot);
    let (_slot, account) = open(dir.path()).await;
    let after = account.snapshot().await.unwrap();
    assert_eq!(
        after.pending_pair_transactions,
        before.pending_pair_transactions
    );
    assert_eq!(after.phase, AccountPhase::PendingImport);
    assert_eq!(after.installation, before.installation);
    chat_core::storage_fixture::assert_account_preserved(
        &pending,
        &account
            .storage_test_fixture()
            .handle
            .load_account()
            .await
            .unwrap()
            .unwrap(),
    );
    assert!(Store::open(dir.path(), support::protector()).is_err());
}

#[tokio::test]
async fn account_open_errors_still_reach_locked_ui() {
    let dir = tempfile::tempdir().unwrap();
    Fixture::set_unknown_schema(dir.path(), support::protector()).unwrap();
    let slot = Arc::new(CoreSlot::new());
    slot.start(
        CoreConfig {
            data_dir: dir.path().into(),
        },
        &EventBus::new(),
    )
    .unwrap();
    slot.network().unwrap().configure_local(None).await.unwrap();
    let owned = slot.clone();
    let result = tokio::task::spawn_blocking(move || {
        owned.open_account_with_services(support::protector(), services::offline())
    })
    .await
    .unwrap();
    assert!(
        matches!(result, Err(e) if e.code == ErrorCode::StorageCorrupt),
        "SQL open errors must reach the existing UI error boundary"
    );
    assert!(slot.account().is_err());
}

#[tokio::test]
async fn legacy_store_rejects_sql_metadata_without_becoming_writer() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("storage.mode"), [1, 2, 3]).unwrap();
    assert!(
        Store::open(dir.path(), support::protector()).is_err(),
        "legacy Store must reject SQL authority at open"
    );
}

#[tokio::test]
async fn lifecycle_fence_wins_during_sql_wait() {
    let dir = tempfile::tempdir().unwrap();
    let (_slot, account) = open(dir.path()).await;
    let storage = account.storage_test_fixture();
    let pause = storage.pause().await;
    pause.wait_entered().await;
    let a = account.clone();
    let pending = tokio::spawn(async move { a.prepare_candidate().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while storage.remaining_capacity() == 32 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!pending.is_finished());
    assert_eq!(account.request_suspend(), 1);
    assert_eq!(account.request_resume(), 1);
    pause.release();
    assert_eq!(
        pending.await.unwrap().unwrap_err().code,
        ErrorCode::StorageLocked
    );
    settled(&account).await;
    assert_eq!(
        account.snapshot().await.unwrap().phase,
        AccountPhase::Unconfigured
    );
    assert_eq!(account.pair_test_bytes().await.unwrap(), (0, 0));
    assert_eq!(storage.account_rows().await, 0);
}

#[tokio::test]
async fn private_reply_already_sent_is_rejected_after_lifecycle_invalidation() {
    use std::{future::Future, task::Poll};
    let dir = tempfile::tempdir().unwrap();
    let (_slot, account) = open(dir.path()).await;
    let storage = account.storage_test_fixture();
    let pause = storage.pause().await;
    pause.wait_entered().await;
    let a = account.clone();
    let preparing = tokio::spawn(async move { a.prepare_candidate().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while storage.remaining_capacity() == 32 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let display = account.begin_create();
    tokio::pin!(display);
    assert!(std::future::poll_fn(|cx| Poll::Ready(display.as_mut().poll(cx).is_pending())).await);
    let projection = account.snapshot();
    tokio::pin!(projection);
    assert!(std::future::poll_fn(|cx| Poll::Ready(projection.as_mut().poll(cx).is_pending())).await);
    pause.release();
    preparing.await.unwrap().unwrap();
    // Same actor barrier proves Begin's private response has already been sent.
    account.snapshot().await.unwrap();
    account.request_suspend();
    assert!(matches!(display.await, Err(e) if e.code == ErrorCode::StorageLocked));
    assert!(matches!(projection.await, Err(e) if e.code == ErrorCode::StorageLocked));
    settled(&account).await;
}

#[tokio::test]
async fn initial_validated_resume_stays_pending_and_new_suspend_wins() {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    struct PausedResume {
        first_resume: AtomicBool,
        entered: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl KeyProtector for PausedResume {
        fn protect(&self, id: StoreId, s: &Secret32) -> Result<Vec<u8>, CoreError> {
            support::protector().protect(id, s)
        }
        fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
            // Initial open runs on the caller pool; resume reopens on the sole writer.
            if std::thread::current().name() == Some("chat-storage-writer")
                && self.first_resume.swap(false, Ordering::SeqCst)
            {
                self.entered.send(()).unwrap();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
            }
            support::protector().unprotect(id, blob)
        }
        fn delete(&self, id: StoreId) -> Result<(), CoreError> {
            support::protector().delete(id)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    Fixture::open(dir.path(), support::protector())
        .await
        .unwrap()
        .close()
        .await;
    let slot = Arc::new(CoreSlot::new());
    slot.start(
        CoreConfig {
            data_dir: dir.path().into(),
        },
        &EventBus::new(),
    )
    .unwrap();
    slot.network().unwrap().configure_local(None).await.unwrap();
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, receive) = std::sync::mpsc::channel();
    let protector = Arc::new(PausedResume {
        first_resume: AtomicBool::new(true),
        entered,
        release: Mutex::new(receive),
    });
    let owned = slot.clone();
    let account = tokio::task::spawn_blocking(move || {
        owned.open_account_with_services(protector, services::offline())
    })
    .await
    .unwrap()
    .unwrap();
    tokio::task::spawn_blocking(move || waiting.recv_timeout(Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    assert_eq!(account.lifecycle_status(), 1);
    assert_eq!(
        slot.network()
            .unwrap()
            .execute(NetworkOp::Start)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    let begin = account.begin_create();
    tokio::pin!(begin);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            std::future::Future::poll(begin.as_mut(), cx).is_pending()
        ))
        .await
    );
    account.request_suspend();
    release.send(()).unwrap();
    assert!(matches!(begin.await, Err(e) if e.code == ErrorCode::StorageLocked));
    settled(&account).await;
    assert_eq!(
        account.snapshot().await.unwrap().phase,
        AccountPhase::Locked
    );
    assert_eq!(account.storage_test_fixture().live_connections(), 0);
}

use chat_core::account::{
    AccountServices, FlowId, Lookup, PairDirection, PeerId, RecordBackend, RecordFuture,
    clock::FakeClock,
};
use chat_core::{NetworkOp, NetworkReply};
use std::{collections::BTreeMap, sync::Mutex};
const NOW: u64 = 1_800_000_000;
#[derive(Default)]
struct Backend {
    packets: Mutex<BTreeMap<PeerId, pkarr::SignedPacket>>,
    offline: std::sync::atomic::AtomicBool,
}
impl RecordBackend for Backend {
    fn lookup(&self, p: PeerId) -> RecordFuture<'_, Lookup> {
        Box::pin(async move {
            if self.offline.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(CoreError::new(
                    chat_core::ErrorCode::RecordUnavailable,
                    "offline fixture",
                ));
            }
            Ok(self
                .packets
                .lock()
                .unwrap()
                .get(&p)
                .cloned()
                .map(Lookup::Found)
                .unwrap_or(Lookup::CompletedAbsent))
        })
    }
    fn publish<'a>(&'a self, p: &'a pkarr::SignedPacket) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            self.packets
                .lock()
                .unwrap()
                .insert(PeerId(p.as_bytes()[..32].try_into().unwrap()), p.clone());
            Ok(())
        })
    }
}
struct PairFixture {
    _sql: sql_store::Store,
    _dir: tempfile::TempDir,
    slot: CoreSlot,
    a: AccountHandle,
}
#[path = "support/sql_account_store.rs"]
mod sql_store;
#[tokio::test]
async fn fixture_drop_closes_sql_before_owned_tempdir() {
    let fixture = PairFixture::new(false, Arc::new(Backend::default())).await;
    let path = fixture._dir.path().canonicalize().unwrap();
    let storage = fixture.a.storage_test_fixture();
    drop(fixture);
    sql_store::assert_closed_and_removed(path, storage);
}
impl PairFixture {
    async fn new(existing: bool, backend: Arc<dyn RecordBackend>) -> Self {
        Self::with_clock(existing, backend, Arc::new(FakeClock::new(NOW, 0))).await
    }
    async fn with_clock(
        existing: bool,
        backend: Arc<dyn RecordBackend>,
        clock: Arc<FakeClock>,
    ) -> Self {
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
        let a = slot
            .open_account_with_services(
                support::protector(),
                AccountServices::new(backend, clock.clone()),
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while a.lifecycle_status() == 1 {
                tokio::task::yield_now().await
            }
        })
        .await
        .unwrap();
        assert_eq!(a.lifecycle_status(), 0);
        if existing {
            let d = a.begin_create().await.unwrap();
            let answers = d.positions.map(|p| d.words[usize::from(p - 1)].clone());
            a.confirm_create(d.flow, answers).await.unwrap();
        }
        Self { _sql: sql_store::Store::from_fixture(a.storage_test_fixture()), _dir: dir, slot, a }
    }
    async fn route_to(&self, other: &Self) {
        let NetworkReply::Info(i) = other
            .slot
            .network()
            .unwrap()
            .execute(NetworkOp::Info)
            .await
            .unwrap()
        else {
            panic!()
        };
        self.slot
            .network()
            .unwrap()
            .account_target_for_test(i.ticket.unwrap())
            .await
            .unwrap();
    }
}
async fn connected(direction: PairDirection) -> (PairFixture, PairFixture, FlowId, String) {
    let backend = Arc::new(Backend::default());
    let e = PairFixture::new(true, backend.clone()).await;
    let n = PairFixture::new(false, backend).await;
    let (owner, scanner) = if direction == PairDirection::NewDisplays {
        (&n, &e)
    } else {
        (&e, &n)
    };
    let d = owner.a.pair_begin(direction, None).await.unwrap();
    let waiting = owner.a.pair_view().await.unwrap().unwrap();
    assert_eq!(waiting.remote_installation, None);
    assert_eq!(waiting.remote_kind, None);
    scanner.route_to(owner).await;
    scanner.a.pair_scan(d.qr.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if owner
                .a
                .pair_view()
                .await
                .unwrap()
                .is_some_and(|v| v.sas.is_some())
            {
                break;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    let eview = e.a.pair_view().await.unwrap().unwrap();
    let nview = n.a.pair_view().await.unwrap().unwrap();
    assert_eq!(
        eview.remote_installation,
        n.a.snapshot().await.unwrap().installation
    );
    assert_eq!(
        nview.remote_installation,
        e.a.snapshot().await.unwrap().installation
    );
    assert_eq!(
        eview.remote_kind,
        Some(chat_core::account::DeviceKind::Desktop)
    );
    assert_eq!(
        nview.remote_kind,
        Some(chat_core::account::DeviceKind::Desktop)
    );
    (e, n, d.flow, d.qr.clone())
}
async fn confirmations(e: &PairFixture, n: &PairFixture, f: FlowId) {
    e.a.pair_confirm_code(f, true).await.unwrap();
    n.a.pair_confirm_code(f, true).await.unwrap();
}

#[tokio::test]
async fn sql_commit_precedes_pair_disclosure() {
    for stale in [false, true] {
        let (e, n, flow, _) = connected(PairDirection::NewDisplays).await;
        confirmations(&e, &n, flow).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while e.a.pair_view().await.unwrap().unwrap().phase
                != chat_core::account::PairPhase::AwaitingApproval
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let sql = e.a.storage_test_fixture();
        let pause = sql.pause().await;
        pause.wait_entered().await;
        let a = e.a.clone();
        let pending = tokio::spawn(async move { a.pair_approve(flow).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while sql.remaining_capacity() == 32 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!pending.is_finished());
        assert_eq!(n.a.pair_test_bytes().await.unwrap(), (0, 0));
        if stale {
            e.a.request_suspend();
            e.a.request_resume();
        }
        pause.release();
        let result = pending.await.unwrap();
        if stale {
            assert!(result.is_err());
            settled(&e.a).await;
            assert_eq!(e.a.pair_test_bytes().await.unwrap(), (0, 0));
            assert_eq!(n.a.pair_test_bytes().await.unwrap(), (0, 0));
        } else {
            result.unwrap();
            assert!(e.a.pair_test_bytes().await.unwrap().0 > 0);
        }
    }
}
