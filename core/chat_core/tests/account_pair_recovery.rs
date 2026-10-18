#![cfg(all(feature = "test-hooks", debug_assertions))]
use chat_core::account::{
    AccountHandle, AccountPhase, AccountServices, FlowId, Lookup, PairDirection, PairTestStop,
    PeerId, RecordBackend, RecordFuture, Secret32, StoreId, TxId, clock::FakeClock,
    storage::KeyProtector,
};
use chat_core::{CoreConfig, CoreError, CoreSlot, EventBus, NetworkOp, NetworkReply};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
const NOW: u64 = 1_800_000_000;
#[derive(Default)]
struct Protector {
    shift_clock: Mutex<Option<(usize, Arc<FakeClock>, u64)>>,
    reads: std::sync::atomic::AtomicUsize,
    fail_read: std::sync::atomic::AtomicUsize,
    fail_delete: std::sync::atomic::AtomicBool,
    deleted: std::sync::atomic::AtomicUsize,
}
impl KeyProtector for Protector {
    fn protect(&self, _: StoreId, s: &Secret32) -> Result<Vec<u8>, CoreError> {
        Ok(s.as_bytes().to_vec())
    }
    fn unprotect(&self, _: StoreId, b: &[u8]) -> Result<Secret32, CoreError> {
        let read = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if let Some((at, clock, time)) = &*self.shift_clock.lock().unwrap()
            && *at == read
        {
            clock.set_unix_seconds(*time);
        }
        if self.fail_read.load(std::sync::atomic::Ordering::SeqCst) == read {
            return Err(CoreError::new(
                chat_core::ErrorCode::StorageLocked,
                "real Store unprotect failure",
            ));
        }
        Ok(Secret32::from(<[u8; 32]>::try_from(b).unwrap()))
    }
    fn delete(&self, _: StoreId) -> Result<(), CoreError> {
        if self.fail_delete.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(CoreError::new(
                chat_core::ErrorCode::StorageLocked,
                "OS resource deletion failed",
            ));
        }
        self.deleted
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}
#[derive(Default)]
struct Backend {
    packets: Mutex<BTreeMap<PeerId, pkarr::SignedPacket>>,
    offline: std::sync::atomic::AtomicBool,
    hold_publish: std::sync::atomic::AtomicBool,
    published: std::sync::atomic::AtomicUsize,
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
            if self.hold_publish.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(CoreError::new(
                    chat_core::ErrorCode::RecordUnavailable,
                    "publish blocked",
                ));
            }
            self.published
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.packets
                .lock()
                .unwrap()
                .insert(PeerId(p.as_bytes()[..32].try_into().unwrap()), p.clone());
            Ok(())
        })
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    slot: CoreSlot,
    a: AccountHandle,
    clock: Arc<FakeClock>,
    protector: Arc<Protector>,
    backend: Arc<dyn RecordBackend>,
}
impl Fixture {
    async fn restart(self) -> Self {
        self.restart_edit(|_| {}).await
    }
    async fn restart_edit(
        self,
        edit: impl FnOnce(&mut chat_core::account::storage::Store),
    ) -> Self {
        let Self {
            _dir,
            slot,
            a,
            clock,
            protector,
            backend,
        } = self;
        drop(a);
        drop(slot);
        let mut store = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match chat_core::account::storage::Store::open(_dir.path(), protector.clone()) {
                    Ok(s) => break s,
                    Err(e) if e.code == chat_core::ErrorCode::WriterBusy => {
                        tokio::time::sleep(Duration::from_millis(10)).await
                    }
                    Err(e) => panic!("store restart: {e}"),
                }
            }
        })
        .await
        .unwrap();
        edit(&mut store);
        drop(store);
        let slot = CoreSlot::new();
        slot.start(
            CoreConfig {
                data_dir: _dir.path().into(),
            },
            &EventBus::new(),
        )
        .unwrap();
        slot.network().unwrap().configure_local(None).await.unwrap();
        let a = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match slot.open_account_with_services(
                    protector.clone(),
                    AccountServices::new(backend.clone(), clock.clone()),
                ) {
                    Ok(a) => break a,
                    Err(e) if e.code == chat_core::ErrorCode::WriterBusy => {
                        tokio::time::sleep(Duration::from_millis(10)).await
                    }
                    Err(e) => panic!("restart: {e}"),
                }
            }
        })
        .await
        .unwrap();
        settled(&a).await;
        Self {
            _dir,
            slot,
            a,
            clock,
            protector,
            backend,
        }
    }
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
        let protector = Arc::new(Protector::default());
        let a = slot
            .open_account_with_services(
                protector.clone(),
                AccountServices::new(backend.clone(), clock.clone()),
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
        Self {
            _dir: dir,
            slot,
            a,
            clock,
            protector,
            backend,
        }
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
async fn settled(a: &AccountHandle) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while a.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(a.lifecycle_status(), 0);
}
async fn fresh(e: &Fixture, n: &Fixture, tx: Option<TxId>) -> FlowId {
    let d =
        n.a.pair_begin(PairDirection::NewDisplays, tx)
            .await
            .unwrap();
    e.route_to(n).await;
    e.a.pair_scan(d.qr.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while n
            .a
            .pair_view()
            .await
            .unwrap()
            .is_none_or(|v| v.sas.is_none())
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    d.flow
}
async fn confirm_wait(e: &Fixture, n: &Fixture, flow: FlowId) {
    confirmations(e, n, flow).await;
    tokio::time::sleep(Duration::from_millis(80)).await;
}
async fn active(n: &Fixture) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if n.a.snapshot().await.unwrap().phase == AccountPhase::Active {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
async fn pending(n: &Fixture) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if n.a.snapshot().await.unwrap().phase == AccountPhase::PendingImport {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
async fn connected(direction: PairDirection) -> (Fixture, Fixture, FlowId, String) {
    let backend = Arc::new(Backend::default());
    let e = Fixture::new(true, backend.clone()).await;
    let n = Fixture::new(false, backend).await;
    let (owner, scanner) = if direction == PairDirection::NewDisplays {
        (&n, &e)
    } else {
        (&e, &n)
    };
    let d = owner.a.pair_begin(direction, None).await.unwrap();
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
    (e, n, d.flow, d.qr.clone())
}
async fn confirmations(e: &Fixture, n: &Fixture, f: FlowId) {
    e.a.pair_confirm_code(f, true).await.unwrap();
    n.a.pair_confirm_code(f, true).await.unwrap();
}
async fn approve(e: &Fixture, f: FlowId) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if e.a.pair_approve(f).await.is_ok() {
                break;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn durable_transfer_activates_exact_candidate() {
    let (e, n, flow, _) = connected(PairDirection::NewDisplays).await;
    let candidate = n.a.snapshot().await.unwrap().installation.unwrap();
    let peer = e.a.snapshot().await.unwrap().public_peer_id.unwrap();
    confirmations(&e, &n, flow).await;
    approve(&e, flow).await;
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let view = n.a.snapshot().await.unwrap();
            if view.phase == chat_core::account::AccountPhase::Active {
                assert_eq!(view.installation, Some(candidate));
                assert_eq!(view.public_peer_id, Some(peer));
                assert_eq!(view.publication.accepted.unwrap().devices.len(), 2);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "durable receipt and observed membership must activate the same candidate"
    );
}

#[tokio::test]
async fn failed_disclosure_commit_sends_zero() {
    use std::sync::atomic::Ordering::SeqCst;
    let (e, n, f, _) = connected(PairDirection::NewDisplays).await;
    confirm_wait(&e, &n, f).await;
    e.protector
        .fail_read
        .store(e.protector.reads.load(SeqCst) + 1, SeqCst);
    assert!(e.a.pair_approve(f).await.is_err());
    assert_eq!(e.a.pair_test_bytes().await.unwrap().0, 0);
    assert_eq!(n.a.pair_test_bytes().await.unwrap().1, 0);
    assert_eq!(n.a.snapshot().await.unwrap().phase, AccountPhase::Candidate);
    assert_eq!(e.a.snapshot().await.unwrap().phase, AccountPhase::Locked);
    let e = e.restart().await;
    assert!(!e.a.snapshot().await.unwrap().disclosure_possible);
}

#[tokio::test]
async fn ack_only_after_durable_import() {
    use std::sync::atomic::Ordering::SeqCst;
    let (e, n, f, _) = connected(PairDirection::NewDisplays).await;
    confirm_wait(&e, &n, f).await;
    // Pin commit succeeds; the second actual Store::commit fails while loading its wrapping key.
    n.protector
        .fail_read
        .store(n.protector.reads.load(SeqCst) + 2, SeqCst);
    e.a.pair_approve(f).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while n.a.snapshot().await.unwrap().phase != AccountPhase::Locked {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(n.a.pair_test_bytes().await.unwrap().1, 0);
    assert!(e.a.snapshot().await.unwrap().disclosure_possible);
    let n = n.restart().await;
    assert_eq!(
        n.a.snapshot().await.unwrap().phase,
        AccountPhase::PendingImport
    );
    assert!(n.a.contacts().await.is_err());
}

#[tokio::test]
async fn resume_same_pins_fresh_approval_only() {
    let (e, n, f, _) = connected(PairDirection::NewDisplays).await;
    confirm_wait(&e, &n, f).await;
    n.a.pair_test_stop(PairTestStop::NAfterImport)
        .await
        .unwrap();
    e.a.pair_approve(f).await.unwrap();
    pending(&n).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(n.a.pair_test_bytes().await.unwrap().1, 0);
    let tx = n.a.snapshot().await.unwrap().pending_pair_transactions[0];
    let e = e.restart().await;
    let n = n.restart().await;
    assert!(e.a.snapshot().await.unwrap().disclosure_possible);
    let new_flow = fresh(&e, &n, Some(tx)).await;
    assert_ne!(new_flow, f);
    assert!(e.a.pair_approve(f).await.is_err());
    assert!(e.a.pair_approve(new_flow).await.is_err());
    assert_eq!(e.a.pair_test_bytes().await.unwrap().0, 0);
    confirm_wait(&e, &n, new_flow).await;
    assert_eq!(e.a.pair_test_bytes().await.unwrap().0, 0);
    e.a.pair_approve(new_flow).await.unwrap();
    active(&n).await;
    assert_eq!(
        n.a.snapshot()
            .await
            .unwrap()
            .publication
            .accepted
            .unwrap()
            .devices
            .len(),
        2
    );
}

#[tokio::test]
async fn unpublished_resume_has_transaction_rights_only() {
    let backend = Arc::new(Backend::default());
    let e = Fixture::new(true, backend.clone()).await;
    let n = Fixture::new(false, backend.clone()).await;
    let f = fresh(&e, &n, None).await;
    confirm_wait(&e, &n, f).await;
    backend
        .hold_publish
        .store(true, std::sync::atomic::Ordering::SeqCst);
    e.a.pair_approve(f).await.unwrap();
    pending(&n).await;
    let peer = e.a.snapshot().await.unwrap().public_peer_id.unwrap();
    assert!(n.a.connect_account(peer).await.is_err());
    assert!(n.a.create_invite(None).await.is_err());
    assert!(n.a.contacts().await.is_err());
    assert!(n.a.begin_create().await.is_err());
    assert!(
        n.a.pair_begin(PairDirection::ExistingDisplays, None)
            .await
            .is_err()
    );
    assert!(
        n.a.revoke_device(e.a.snapshot().await.unwrap().installation.unwrap())
            .await
            .is_err()
    );
    assert_ne!(n.a.snapshot().await.unwrap().phase, AccountPhase::Active);
}

#[tokio::test]
async fn discard_pending_then_phrase_recovery() {
    use std::sync::atomic::Ordering::SeqCst;
    let (e, n, f, _) = connected(PairDirection::NewDisplays).await;
    confirm_wait(&e, &n, f).await;
    n.a.pair_test_stop(PairTestStop::NAfterImport)
        .await
        .unwrap();
    e.a.pair_approve(f).await.unwrap();
    pending(&n).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let old = n.a.snapshot().await.unwrap().installation.unwrap();
    n.protector.fail_delete.store(true, SeqCst);
    assert!(n.a.discard_pending_import().await.is_err());
    assert_eq!(n.a.snapshot().await.unwrap().phase, AccountPhase::Locked);
    assert!(n.a.begin_create().await.is_err());
    n.protector.fail_delete.store(false, SeqCst);
    n.a.discard_pending_import().await.unwrap();
    assert_eq!(
        n.a.snapshot().await.unwrap().phase,
        AccountPhase::Unconfigured
    );
    assert_eq!(n.protector.deleted.load(SeqCst), 1);
    for path in ["store.id", "key.wrap", "account.bin"] {
        assert!(!n._dir.path().join(path).exists());
    }
    assert!(
        n.slot
            .network()
            .unwrap()
            .execute(NetworkOp::Start)
            .await
            .is_err()
    );
    let words = chat_core::account::encode_recovery(&Secret32::from([17; 32])).unwrap();
    n.a.recover(words, true).await.unwrap();
    assert_ne!(n.a.snapshot().await.unwrap().installation, Some(old));
    assert!(e.a.snapshot().await.unwrap().disclosure_possible);
    assert!(n.a.discard_pending_import().await.is_err());
}

#[tokio::test]
async fn already_published_no_duplicate_add() {
    let backend = Arc::new(Backend::default());
    let e = Fixture::new(true, backend.clone()).await;
    let n = Fixture::new(false, backend.clone()).await;
    let f = fresh(&e, &n, None).await;
    confirm_wait(&e, &n, f).await;
    e.a.pair_approve(f).await.unwrap();
    active(&n).await;
    let tx = n.a.pair_view().await.unwrap().unwrap().transaction.unwrap();
    let count = backend.published.load(std::sync::atomic::Ordering::SeqCst);
    let e = e.restart().await;
    let n = n.restart().await;
    let f = fresh(&e, &n, Some(tx)).await;
    confirm_wait(&e, &n, f).await;
    e.a.pair_approve(f).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        n.a.snapshot()
            .await
            .unwrap()
            .publication
            .accepted
            .unwrap()
            .devices
            .len(),
        2
    );
    assert_eq!(
        backend.published.load(std::sync::atomic::Ordering::SeqCst),
        count
    );
    assert!(n.a.discard_pending_import().await.is_err());
}

#[tokio::test]
async fn crash_boundaries_reload_durable_history() {
    for stop in [
        PairTestStop::EBeforeDisclosure,
        PairTestStop::EAfterDisclosure,
        PairTestStop::EAfterFirstChunk,
        PairTestStop::NBeforeImport,
        PairTestStop::NAfterImport,
        PairTestStop::NAfterAck,
        PairTestStop::EBeforeReceipt,
        PairTestStop::EAfterReceipt,
        PairTestStop::EBeforePublish,
        PairTestStop::EAfterPublish,
    ] {
        let (e, n, f, _) = connected(PairDirection::NewDisplays).await;
        confirm_wait(&e, &n, f).await;
        let new = matches!(
            stop,
            PairTestStop::NBeforeImport | PairTestStop::NAfterImport | PairTestStop::NAfterAck
        );
        if new {
            n.a.pair_test_stop(stop).await.unwrap();
        } else {
            e.a.pair_test_stop(stop).await.unwrap();
        }
        let _ = e.a.pair_approve(f).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let bytes = e.a.pair_test_bytes().await.unwrap().0;
        if matches!(
            stop,
            PairTestStop::EBeforeDisclosure | PairTestStop::EAfterDisclosure
        ) {
            assert_eq!(bytes, 0, "{stop:?}");
        }
        if matches!(
            stop,
            PairTestStop::NBeforeImport | PairTestStop::NAfterImport
        ) {
            assert_eq!(n.a.pair_test_bytes().await.unwrap().1, 0, "{stop:?}");
        }
        let e = e.restart().await;
        let n = n.restart().await;
        assert_eq!(
            e.a.snapshot().await.unwrap().disclosure_possible,
            stop != PairTestStop::EBeforeDisclosure,
            "{stop:?}"
        );
        if matches!(
            stop,
            PairTestStop::EBeforeDisclosure | PairTestStop::EAfterDisclosure
        ) {
            assert_eq!(
                n.a.snapshot().await.unwrap().phase,
                AccountPhase::Candidate,
                "{stop:?}"
            );
        }
        if matches!(
            stop,
            PairTestStop::NBeforeImport
                | PairTestStop::NAfterImport
                | PairTestStop::EBeforeReceipt
                | PairTestStop::EAfterReceipt
        ) {
            assert_eq!(
                n.a.snapshot().await.unwrap().phase,
                AccountPhase::PendingImport,
                "{stop:?}"
            );
        }
        assert!(n.a.pair_view().await.unwrap().is_none());
    }
}

#[tokio::test]
async fn changed_payload_or_candidate_rejected() {
    use minicbor::{Decoder, Encoder};
    use zeroize::Zeroizing;
    fn fields(input: &[u8]) -> Vec<Zeroizing<Vec<u8>>> {
        let mut d = Decoder::new(input);
        let n = d.array().unwrap().unwrap();
        (0..n)
            .map(|_| {
                let start = d.position();
                d.skip().unwrap();
                Zeroizing::new(input[start..d.position()].to_vec())
            })
            .collect()
    }
    fn array(parts: &[Zeroizing<Vec<u8>>]) -> Zeroizing<Vec<u8>> {
        let mut e = Encoder::new(Vec::with_capacity(4 * 1024 * 1024));
        e.array(parts.len() as u64).unwrap();
        for p in parts {
            e.writer_mut().extend_from_slice(p);
        }
        Zeroizing::new(e.into_writer())
    }
    fn bytes(input: &[u8]) -> Zeroizing<Vec<u8>> {
        let mut e = Encoder::new(Vec::with_capacity(input.len() + 8));
        e.bytes(input).unwrap();
        Zeroizing::new(e.into_writer())
    }
    let (e, n, f, _) = connected(PairDirection::NewDisplays).await;
    confirm_wait(&e, &n, f).await;
    n.a.pair_test_stop(PairTestStop::NAfterImport)
        .await
        .unwrap();
    e.a.pair_approve(f).await.unwrap();
    pending(&n).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let tx = n.a.snapshot().await.unwrap().pending_pair_transactions[0];
    let other = Fixture::new(false, e.backend.clone()).await;
    assert!(
        other
            .a
            .pair_begin(PairDirection::NewDisplays, Some(tx))
            .await
            .is_err()
    );
    let e = e
        .restart_edit(|store| {
            let mut saved = store.load().unwrap().unwrap();
            let mut active = fields(&saved.payload);
            let mut pair = fields(Decoder::new(&active[10]).bytes().unwrap());
            let mut transactions = fields(&pair[1]);
            let mut transaction = fields(Decoder::new(&transactions[0]).bytes().unwrap());
            let mut payload = fields(Decoder::new(&transaction[5]).bytes().unwrap());
            let mut contact = Encoder::new(Vec::new());
            contact
                .array(1)
                .unwrap()
                .array(4)
                .unwrap()
                .bytes(&[9; 32])
                .unwrap()
                .str("changed contact")
                .unwrap()
                .u8(0)
                .unwrap()
                .null()
                .unwrap();
            payload[6] = Zeroizing::new(contact.into_writer());
            transaction[5] = bytes(&array(&payload));
            transactions[0] = bytes(&array(&transaction));
            pair[1] = array(&transactions);
            active[10] = bytes(&array(&pair));
            saved.payload = array(&active);
            store.commit(&saved).unwrap();
        })
        .await;
    let n = n.restart().await;
    let f = fresh(&e, &n, Some(tx)).await;
    confirm_wait(&e, &n, f).await;
    e.a.pair_approve(f).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(n.a.pair_view().await.unwrap().is_none());
    assert_eq!(n.a.pair_test_bytes().await.unwrap().1, 0);
    assert_eq!(
        n.a.snapshot().await.unwrap().phase,
        AccountPhase::PendingImport
    );
}

#[tokio::test]
async fn discard_recovery_concurrent_suspend_never_reopens_endpoint() {
    let backend = Arc::new(Backend::default());
    let n = Fixture::new(false, backend).await;
    n.a.prepare_candidate().await.unwrap();
    n.a.discard_pending_import().await.unwrap();
    let held = n.slot.network().unwrap().hold_next_bind_for_test();
    let account = n.a.clone();
    let recovery = tokio::spawn(async move {
        account
            .recover(
                chat_core::account::encode_recovery(&Secret32::from([18; 32])).unwrap(),
                true,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(80)).await;
    n.a.request_suspend();
    held.notify_waiters();
    assert!(recovery.await.unwrap().is_err());
    settled(&n.a).await;
    assert_eq!(n.a.snapshot().await.unwrap().phase, AccountPhase::Locked);
    let NetworkReply::Info(info) = n
        .slot
        .network()
        .unwrap()
        .execute(NetworkOp::Info)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(info.state, chat_core::NetworkState::Stopped);
    assert!(
        n.slot
            .network()
            .unwrap()
            .execute(NetworkOp::Start)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn disclosure_before_first_frame_resumes_same_tx_from_existing_display() {
    let (e, n, f, _) = connected(PairDirection::NewDisplays).await;
    let candidate = n.a.snapshot().await.unwrap().installation;
    confirm_wait(&e, &n, f).await;
    e.a.pair_test_stop(PairTestStop::EAfterDisclosure)
        .await
        .unwrap();
    assert!(e.a.pair_approve(f).await.is_err());
    assert_eq!(e.a.pair_test_bytes().await.unwrap().0, 0);
    let tx = e.a.snapshot().await.unwrap().pending_pair_transactions[0];
    let e = e.restart().await;
    let n = n.restart().await;
    assert_eq!(n.a.snapshot().await.unwrap().phase, AccountPhase::Candidate);
    assert!(
        n.a.pair_begin(PairDirection::NewDisplays, Some(tx))
            .await
            .is_err()
    );
    let qr =
        e.a.pair_begin(PairDirection::ExistingDisplays, Some(tx))
            .await
            .unwrap();
    n.route_to(&e).await;
    n.a.pair_scan(qr.qr.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while e
            .a
            .pair_view()
            .await
            .unwrap()
            .is_none_or(|v| v.sas.is_none())
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_ne!(qr.flow, f);
    assert!(e.a.pair_approve(qr.flow).await.is_err());
    assert_eq!(e.a.pair_test_bytes().await.unwrap().0, 0);
    confirm_wait(&e, &n, qr.flow).await;
    assert_eq!(e.a.pair_test_bytes().await.unwrap().0, 0);
    e.a.pair_approve(qr.flow).await.unwrap();
    active(&n).await;
    assert_eq!(n.a.snapshot().await.unwrap().installation, candidate);
    assert_eq!(
        n.a.pair_view().await.unwrap().unwrap().transaction,
        Some(tx)
    );
    assert_eq!(
        n.a.snapshot()
            .await
            .unwrap()
            .publication
            .accepted
            .unwrap()
            .devices
            .len(),
        2
    );
}

#[tokio::test]
async fn thousand_contacts_cross_multiple_real_noise_chunks_without_device_trust() {
    let backend = Arc::new(Backend::default());
    let e = Fixture::new(true, backend.clone()).await;
    let own = e.a.snapshot().await.unwrap().public_peer_id.unwrap();
    let e = e
        .restart_edit(|store| {
            let mut saved = store.load().unwrap().unwrap();
            let mut d = minicbor::Decoder::new(&saved.payload);
            assert_eq!(d.array().unwrap(), Some(11));
            for _ in 0..9 {
                d.skip().unwrap();
            }
            let start = d.position();
            d.skip().unwrap();
            let suffix = saved.payload[d.position()..].to_vec();
            let mut contacts = minicbor::Encoder::new(Vec::with_capacity(128 * 1024));
            contacts.array(1000).unwrap();
            for index in 0u16..1000 {
                let mut peer = [0; 32];
                peer[..2].copy_from_slice(&index.to_be_bytes());
                contacts
                    .array(6)
                    .unwrap()
                    .bytes(&peer)
                    .unwrap()
                    .str(&"a".repeat(64))
                    .unwrap()
                    .u8(1)
                    .unwrap()
                    .bytes(&chat_core::account::safety_digest(own, PeerId(peer)))
                    .unwrap()
                    .null()
                    .unwrap()
                    .u8(0)
                    .unwrap();
            }
            let mut plain = zeroize::Zeroizing::new(Vec::with_capacity(4 * 1024 * 1024));
            plain.extend_from_slice(&saved.payload[..start]);
            minicbor::Encoder::new(&mut *plain)
                .bytes(&contacts.into_writer())
                .unwrap();
            plain.extend_from_slice(&suffix);
            saved.payload = plain;
            store.commit(&saved).unwrap();
        })
        .await;
    let n = Fixture::new(false, backend).await;
    let f = fresh(&e, &n, None).await;
    confirm_wait(&e, &n, f).await;
    e.a.pair_approve(f).await.unwrap();
    active(&n).await;
    let contacts = n.a.contacts().await.unwrap();
    assert_eq!(contacts.len(), 1000);
    assert!(
        contacts
            .iter()
            .all(|c| c.alias.len() == 64 && c.verified && c.device_unverified && !c.new_device)
    );
    assert!(e.a.pair_test_bytes().await.unwrap().0 > 16 * 1024);
    assert!(n.a.pair_test_bytes().await.unwrap().1 > 0);
}

#[tokio::test]
async fn authority_rechecked_after_disclosure_store_commit_before_first_byte() {
    let (e, n, f, _) = connected(PairDirection::NewDisplays).await;
    confirm_wait(&e, &n, f).await;
    let next = e.protector.reads.load(std::sync::atomic::Ordering::SeqCst) + 1;
    *e.protector.shift_clock.lock().unwrap() = Some((next, e.clock.clone(), NOW - 301));
    assert!(e.a.pair_approve(f).await.is_err());
    assert_eq!(e.a.pair_test_bytes().await.unwrap().0, 0);
    assert!(e.a.snapshot().await.unwrap().disclosure_possible);
}

#[tokio::test]
async fn cancel_and_timeout_preserve_durable_disclosure_warning() {
    for timeout in [false, true] {
        let backend = Arc::new(Backend::default());
        let e = Fixture::new(true, backend.clone()).await;
        let n = Fixture::new(false, backend.clone()).await;
        let f = fresh(&e, &n, None).await;
        confirm_wait(&e, &n, f).await;
        backend
            .hold_publish
            .store(true, std::sync::atomic::Ordering::SeqCst);
        e.a.pair_approve(f).await.unwrap();
        if timeout {
            e.clock.advance_monotonic_ms(60_001);
            assert!(e.a.pair_view().await.unwrap().is_none());
        } else {
            e.a.pair_cancel(f).await.unwrap();
        }
        assert!(e.a.snapshot().await.unwrap().disclosure_possible);
        let e = e.restart().await;
        assert!(e.a.snapshot().await.unwrap().disclosure_possible);
        assert_eq!(
            e.a.snapshot()
                .await
                .unwrap()
                .pending_pair_transactions
                .len(),
            1
        );
    }
}
