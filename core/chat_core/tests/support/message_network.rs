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
#[path = "account_store.rs"]
mod support;
const NOW: u64 = 1_800_000_000;
#[derive(Default)]
struct Backend {
    packets: Mutex<BTreeMap<PeerId, SignedPacket>>,
    offline: AtomicBool,
    publish_error: Mutex<Option<ErrorCode>>,
    publishes: Mutex<Vec<SignedPacket>>,
    lookup_holds: Mutex<BTreeMap<PeerId,Arc<tokio::sync::Notify>>>,
    lookup_starts: Mutex<Vec<PeerId>>,
}
impl RecordBackend for Backend {
    fn lookup(&self, peer: PeerId) -> RecordFuture<'_, Lookup> {
        Box::pin(async move {
            let hold=self.lookup_holds.lock().unwrap().get(&peer).cloned();
            if let Some(hold)=hold {
                self.lookup_starts.lock().unwrap().push(peer);
                hold.notified().await;
            }
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
    #[allow(dead_code)]
    bus: EventBus,
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
            bus,
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
    #[allow(dead_code)] // Shared harness; only authentication tests inspect the account view.
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

#[path = "sql_account_store.rs"]
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
