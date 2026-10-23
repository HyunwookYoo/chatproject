#![cfg(all(feature = "test-hooks", debug_assertions))]
use chat_core::account::{
    AccountHandle, AccountServices, AccountView, DeviceKind, Generation, IdentityRecord, Lookup,
    PeerId, RecordBackend, RecordDevice, RecordFuture, Secret32, StoreId,
    clock::FakeClock,
    derive_account, derive_device, record_packet, sign_cert,
    storage::{KeyProtector, Store, StoredPlaintext},
};
use chat_core::{
    CoreConfig, CoreError, CoreEvent, CoreSlot, ErrorCode, EventBus, EventSink, NetworkOp,
    NetworkReply, NetworkState,
};
use hkdf::Hkdf;
use pkarr::SignedPacket;
use sha2::Sha256;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use zeroize::Zeroizing;
#[allow(dead_code)]
#[path = "support/account_store.rs"]
mod support;
const NOW: u64 = 1_800_000_000;
#[derive(Default)]
struct Backend {
    packets: Mutex<BTreeMap<PeerId, SignedPacket>>,
    offline: AtomicBool,
    publish_error: Mutex<Option<ErrorCode>>,
    publishes: Mutex<Vec<SignedPacket>>,
}
impl RecordBackend for Backend {
    fn lookup(&self, peer: PeerId) -> RecordFuture<'_, Lookup> {
        Box::pin(async move {
            if self.offline.load(Ordering::SeqCst) {
                return Err(CoreError::new(
                    ErrorCode::RecordUnavailable,
                    "offline fixture",
                ));
            }
            Ok(self
                .packets
                .lock()
                .unwrap()
                .get(&peer)
                .cloned()
                .map(Lookup::Found)
                .unwrap_or(Lookup::CompletedAbsent))
        })
    }
    fn publish<'a>(&'a self, packet: &'a SignedPacket) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            self.publishes.lock().unwrap().push(packet.clone());
            if let Some(code) = *self.publish_error.lock().unwrap() {
                return Err(CoreError::new(code, "injected publish failure"));
            }
            let peer = PeerId(packet.as_bytes()[..32].try_into().unwrap());
            self.packets.lock().unwrap().insert(peer, packet.clone());
            Ok(())
        })
    }
}
#[derive(Default)]
struct Events(Mutex<Vec<CoreEvent>>);
impl EventSink for Events {
    fn send(&self, event: CoreEvent) -> bool {
        self.0.lock().unwrap().push(event);
        true
    }
}
struct Fixture {
    // Rust drops fields in declaration order; join SQL stop before TempDir cleanup.
    _sql: sql_store::Store,
    _dir: tempfile::TempDir,
    slot: CoreSlot,
    account: AccountHandle,
    peer: PeerId,
    clock: Arc<FakeClock>,
}
fn packet(
    account_seed: u8,
    device_seeds: &[u8],
    generation: u8,
    issued: u64,
    expires: u64,
    sequence: u64,
) -> SignedPacket {
    let keys = derive_account(&Secret32::from([account_seed; 32])).unwrap();
    let mut devices: Vec<_> = device_seeds
        .iter()
        .map(|seed| {
            let d = derive_device(Secret32::from([*seed; 32])).unwrap();
            RecordDevice {
                installation_id: d.installation_id(),
                transport_public_key: d.transport_public_key(),
                kind: DeviceKind::Desktop,
            }
        })
        .collect();
    devices.sort_by_key(|d| d.installation_id);
    record_packet(
        &keys,
        &IdentityRecord {
            generation: Generation([generation; 16]),
            issued_at: issued,
            expires_at: expires,
            devices,
        },
        sequence,
    )
    .unwrap()
}
impl Fixture {
    async fn new(seed: u8, device_seed: u8, signed: &SignedPacket, backend: Arc<Backend>) -> Self {
        Self::with_protector(seed, device_seed, signed, backend, support::protector()).await
    }
    async fn with_protector(
        seed: u8,
        device_seed: u8,
        signed: &SignedPacket,
        backend: Arc<Backend>,
        protector: Arc<dyn KeyProtector>,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let keys = derive_account(&Secret32::from([seed; 32])).unwrap();
        let peer = keys.peer_id();
        let device = derive_device(Secret32::from([device_seed; 32])).unwrap();
        let cert = sign_cert(&keys, &device, DeviceKind::Desktop, NOW).unwrap();
        let record = chat_core::account::parse_record(signed, peer, NOW).unwrap();
        let seq = u64::from_be_bytes(signed.as_bytes()[96..104].try_into().unwrap());
        let mut records = minicbor::Encoder::new(Vec::new());
        records
            .array(9)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&record.generation.0)
            .unwrap()
            .u8(2)
            .unwrap()
            .u64(seq)
            .unwrap()
            .u64(seq)
            .unwrap()
            .bytes(signed.as_bytes())
            .unwrap()
            .null()
            .unwrap()
            .array(0)
            .unwrap()
            .array(0)
            .unwrap();
        let mut identity = Zeroizing::new([0; 32]);
        Hkdf::<Sha256>::new(Some(b"ChatProject/account-keys/v1"), &[seed; 32])
            .expand(b"identity-ed25519", &mut *identity)
            .unwrap();
        let mut payload = minicbor::Encoder::new(Vec::new());
        payload
            .array(11)
            .unwrap()
            .u8(1)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&*identity)
            .unwrap()
            .bytes(&[device_seed; 32])
            .unwrap()
            .bytes(&cert.to_bytes())
            .unwrap()
            .u64(1)
            .unwrap()
            .u8(0)
            .unwrap()
            .bytes(&records.into_writer())
            .unwrap()
            .bytes(&[0x80])
            .unwrap()
            .bytes(&[0x80])
            .unwrap()
            .bytes(&[0x83, 1, 0x80, 0xf6])
            .unwrap();
        {
            let mut store = Store::open(dir.path(), protector.clone()).unwrap();
            store
                .commit(&StoredPlaintext {
                    installation: device.installation_id().0,
                    payload: Zeroizing::new(payload.into_writer()),
                })
                .unwrap();
        }
        let slot = CoreSlot::new();
        let bus = EventBus::new();
        let events = Arc::new(Events::default());
        bus.subscribe(events.clone());
        slot.start(
            CoreConfig {
                data_dir: dir.path().into(),
            },
            &bus,
        )
        .unwrap();
        let network = slot.network().unwrap();
        network.configure_local(None).await.unwrap();
        let clock = Arc::new(FakeClock::new(NOW, 0));
        let account = slot
            .open_account_with_services(protector, AccountServices::new(backend, clock.clone()))
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
        network.execute(NetworkOp::Start).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let NetworkReply::Info(info) = network.execute(NetworkOp::Info).await.unwrap()
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
        Self {
            _sql: sql_store::Store::from_fixture(account.storage_test_fixture()),
            _dir: dir,
            slot,
            account,
            peer,
            clock,
        }
    }
    async fn route(&self, other: &Self) {
        let NetworkReply::Info(info) = other
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
            .account_target_for_test(info.ticket.unwrap())
            .await
            .unwrap();
    }
    async fn view(&self) -> AccountView {
        self.account.snapshot().await.unwrap()
    }
}
async fn pair() -> (Fixture, Fixture, Arc<Backend>) {
    let a = packet(1, &[11], 1, NOW, NOW + 600, NOW * 1_000_000);
    let b = packet(2, &[12], 2, NOW, NOW + 600, NOW * 1_000_000);
    let backend = Arc::new(Backend {
        packets: Mutex::new(BTreeMap::new()),
        offline: AtomicBool::new(false),
        ..Default::default()
    });
    for (seed, p) in [(1, a.clone()), (2, b.clone())] {
        backend.packets.lock().unwrap().insert(
            derive_account(&Secret32::from([seed; 32]))
                .unwrap()
                .peer_id(),
            p,
        );
    }
    let a = Fixture::new(1, 11, &a, backend.clone()).await;
    let b = Fixture::new(2, 12, &b, backend.clone()).await;
    a.route(&b).await;
    b.route(&a).await;
    (a, b, backend)
}

#[path = "support/sql_account_store.rs"]
mod sql_store;
use chat_core::storage_fixture::{BlockReason, DeliveryState, MessageId, MessageMutation};
async fn enqueue(a: &Fixture, b: &Fixture) -> chat_core::storage_fixture::MessageView {
    a.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .enqueue(
            b.peer,
            MessageId([9; 16]),
            "private text".into(),
            NOW * 1000,
        )
        .await
        .unwrap()
}
async fn contacts(a: &Fixture, b: &Fixture) {
    let invite = b.account.create_invite(None).await.unwrap();
    a.account
        .accept_invite(invite, "remote".into())
        .await
        .unwrap();
}
#[tokio::test]
async fn before_auth_no_message_bytes() {
    let (a, b, _) = pair().await;
    contacts(&a, &b).await;
    let m = enqueue(&a, &b).await;
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
    let result = a
        .account
        .message_test_attempt(m.key, false, MessageMutation::None)
        .await;
    assert!(matches!(result,Err(e) if e.code==ErrorCode::AccountRestricted));
    assert_eq!(a.account.message_test_write_starts().await.unwrap().0, 0);
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
