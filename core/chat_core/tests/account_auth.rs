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
    capture: Mutex<Option<(Vec<std::path::PathBuf>, chat_core::account::InstallationId)>>,
    observed: Mutex<Vec<Vec<chat_core::account::RevokeIntent>>>,
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
            if let Some((paths, target)) = self.capture.lock().unwrap().as_ref() {
                let observed: Vec<_> = paths.iter().map(|path| durable_tombstones(path)).collect();
                assert!(
                    observed.iter().all(|intents| intents
                        .iter()
                        .any(|intent| intent.target_installation == *target)),
                    "public mutation preceded durable receipt"
                );
                *self.observed.lock().unwrap() = observed;
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
    _dir: tempfile::TempDir,
    slot: CoreSlot,
    account: AccountHandle,
    peer: PeerId,
    clock: Arc<FakeClock>,
    events: Arc<Events>,
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
            .array(10)
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
            _dir: dir,
            slot,
            account,
            peer,
            clock,
            events,
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
        capture: Mutex::new(None),
        observed: Mutex::new(vec![]),
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
#[tokio::test]
async fn valid_cert_actual_endpoint_installation_proof_required() {
    let (a, b, _) = pair().await;
    a.account.connect_account(b.peer).await.unwrap();
    assert!(
        a.events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, CoreEvent::AccountAuthChanged { ready: true, .. }))
    );
}
#[tokio::test]
async fn cached_unexpired_allows_new_proof_but_not_mutation() {
    let (a, b, backend) = pair().await;
    a.account.connect_account(b.peer).await.unwrap();
    backend.offline.store(true, Ordering::SeqCst);
    a.account.connect_account(b.peer).await.unwrap();
    assert!(
        a.events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, CoreEvent::AccountCacheUsed { .. }))
    );
    let local = (a.view().await).installation.unwrap();
    assert!(b.account.revoke_device(local).await.is_err());
}
#[tokio::test]
async fn expiry_restricts_then_new_proof() {
    let (a, b, backend) = pair().await;
    a.account.connect_account(b.peer).await.unwrap();
    a.clock.set_unix_seconds(NOW + 600);
    b.clock.set_unix_seconds(NOW + 600);
    assert!(a.account.connect_account(b.peer).await.is_err());
    let restricted = || {
        a.events
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, CoreEvent::AccountAuthChanged { ready: false, .. }))
            .count()
    };
    let count = restricted();
    assert!(count > 0);
    a.clock.set_unix_seconds(NOW);
    assert!(a.account.connect_account(PeerId([0; 32])).await.is_err());
    a.clock.set_unix_seconds(NOW + 600);
    assert!(a.account.connect_account(b.peer).await.is_err());
    assert_eq!(
        restricted(),
        count,
        "clock rollback must not resurrect a grant without proof"
    );
    backend.packets.lock().unwrap().remove(&a.peer);
    backend.packets.lock().unwrap().remove(&b.peer);
    a.account.refresh_record(a.peer).await.unwrap();
    a.account.refresh_record(a.peer).await.unwrap();
    b.account.refresh_record(b.peer).await.unwrap();
    b.account.refresh_record(b.peer).await.unwrap();
    a.account.connect_account(b.peer).await.unwrap();
}
#[tokio::test]
async fn removal_generation_conflict_invalidates_existing_grants() {
    let (a, b, backend) = pair().await;
    a.account.connect_account(b.peer).await.unwrap();
    backend.packets.lock().unwrap().insert(
        b.peer,
        packet(2, &[12], 8, NOW + 1, NOW + 601, (NOW + 1) * 1_000_000),
    );
    assert!(a.account.refresh_record(b.peer).await.is_err());
    assert!(a.account.connect_account(b.peer).await.is_err());
    assert!(
        a.events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, CoreEvent::AccountAuthChanged { ready: false, .. }))
    );
}
#[tokio::test]
async fn own_removal_stops_account_activity() {
    let (a, b, backend) = pair().await;
    a.account.connect_account(b.peer).await.unwrap();
    backend.packets.lock().unwrap().insert(
        a.peer,
        packet(1, &[19], 1, NOW + 1, NOW + 601, (NOW + 1) * 1_000_000),
    );
    assert!(a.account.refresh_record(a.peer).await.is_err());
    assert!(a.account.connect_account(b.peer).await.is_err());
}

fn durable_snapshot(root: &std::path::Path) -> StoredPlaintext {
    // Copy the committed encrypted snapshot into an isolated read-only fixture.
    // Never open a second Store on a live actor's folder.
    let mirror = tempfile::tempdir().unwrap();
    for name in ["store.id", "key.wrap", "account.bin"] {
        std::fs::copy(root.join(name), mirror.path().join(name)).unwrap();
    }
    let mut store = Store::open(mirror.path(), support::protector()).unwrap();
    store.load().unwrap().unwrap()
}
fn durable_tombstones(root: &std::path::Path) -> Vec<chat_core::account::RevokeIntent> {
    let state = durable_snapshot(root);
    let mut d = minicbor::Decoder::new(&state.payload);
    assert_eq!(d.array().unwrap(), Some(10));
    for _ in 0..7 {
        d.skip().unwrap();
    }
    let mut records = minicbor::Decoder::new(d.bytes().unwrap());
    assert_eq!(records.array().unwrap(), Some(9));
    for _ in 0..7 {
        records.skip().unwrap();
    }
    let count = records.array().unwrap().unwrap();
    (0..count)
        .map(|_| chat_core::account::RevokeIntent::from_bytes(records.bytes().unwrap()).unwrap())
        .collect()
}
#[derive(Default)]
struct ReceiptOrderProtector {
    owner: Mutex<Option<(Arc<Events>, chat_core::account::InstallationId)>>,
    checked: AtomicBool,
}
impl KeyProtector for ReceiptOrderProtector {
    fn protect(&self, id: StoreId, key: &Secret32) -> Result<Vec<u8>, CoreError> {
        support::protector().protect(id, key)
    }
    fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
        if let Some((events, target)) = self.owner.lock().unwrap().as_ref() {
            assert!(
                events.0.lock().unwrap().iter().any(|event| matches!(event,
                CoreEvent::AccountAuthChanged { installation_id, ready: false, .. }
                if installation_id == &target.to_string())),
                "receiver persistence started before sender closed the target"
            );
            self.checked.store(true, Ordering::SeqCst);
        }
        support::protector().unprotect(id, blob)
    }
    fn delete(&self, id: StoreId) -> Result<(), CoreError> {
        support::protector().delete(id)
    }
}
#[tokio::test]
async fn real_revoke_closes_target_then_confirms_remaining_peer_before_public_mutation() {
    let signed = packet(3, &[21, 22, 23], 3, NOW, NOW + 600, NOW * 1_000_000);
    let peer = derive_account(&Secret32::from([3; 32])).unwrap().peer_id();
    let backend = Arc::new(Backend {
        packets: Mutex::new(BTreeMap::from([(peer, signed.clone())])),
        offline: AtomicBool::new(false),
        capture: Mutex::new(None),
        observed: Mutex::new(vec![]),
        ..Default::default()
    });
    let a = Fixture::new(3, 21, &signed, backend.clone()).await;
    let order = Arc::new(ReceiptOrderProtector::default());
    let b = Fixture::with_protector(3, 22, &signed, backend.clone(), order.clone()).await;
    let c = Fixture::new(3, 23, &signed, backend.clone()).await;
    b.route(&a).await;
    b.route(&c).await;
    c.route(&a).await;
    c.route(&b).await;
    a.route(&b).await;
    a.route(&c).await;
    // Connect both peers to the owner explicitly via test-local address selection.
    b.account.connect_account(peer).await.unwrap();
    c.account.connect_account(peer).await.unwrap();
    a.account.connect_account(peer).await.unwrap();
    let target = c.view().await.installation.unwrap();
    *backend.capture.lock().unwrap() =
        Some((vec![a._dir.path().into(), b._dir.path().into()], target));
    *order.owner.lock().unwrap() = Some((a.events.clone(), target));
    a.account.revoke_device(target).await.unwrap();
    assert!(order.checked.load(Ordering::SeqCst));
    assert_eq!(backend.observed.lock().unwrap().len(), 2);
    assert_eq!(a.view().await.publication.pending_notifications.len(), 0);
    assert_eq!(
        durable_tombstones(b._dir.path())[0].target_installation,
        target
    );
    assert!(a.events.0.lock().unwrap().iter().any(|event|matches!(event,CoreEvent::AccountAuthChanged{installation_id,ready:false,..} if installation_id==&target.to_string())));
}
fn derive_seed(seed: u8, salt: &[u8], label: &[u8]) -> [u8; 32] {
    let mut out = [0; 32];
    Hkdf::<Sha256>::new(Some(salt), &[seed; 32])
        .expand(label, &mut out)
        .unwrap();
    out
}
fn control(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(3).unwrap().u8(1).unwrap().u8(kind).unwrap();
    if kind == 0 {
        let mut bytes = e.into_writer();
        bytes.extend(payload);
        bytes
    } else {
        e.bytes(payload).unwrap();
        e.into_writer()
    }
}
async fn send_control(send: &mut iroh::endpoint::SendStream, body: &[u8]) {
    send.write_all(&(body.len() as u32).to_be_bytes())
        .await
        .unwrap();
    send.write_all(body).await.unwrap();
}
async fn read_control(recv: &mut iroh::endpoint::RecvStream) -> Vec<u8> {
    let mut len = [0; 4];
    recv.read_exact(&mut len).await.unwrap();
    let n = u32::from_be_bytes(len) as usize;
    assert!(n <= 65536);
    let mut body = vec![0; n];
    recv.read_exact(&mut body).await.unwrap();
    body
}
fn hello_parts(
    bytes: &[u8],
) -> (
    PeerId,
    Generation,
    chat_core::account::InstallationCert,
    [u8; 32],
) {
    let mut d = minicbor::Decoder::new(bytes);
    assert_eq!(d.array().unwrap(), Some(3));
    assert_eq!(d.u8().unwrap(), 1);
    assert_eq!(d.u8().unwrap(), 0);
    assert_eq!(d.array().unwrap(), Some(5));
    assert_eq!(d.u8().unwrap(), 1);
    let peer = PeerId(d.bytes().unwrap().try_into().unwrap());
    let generation = Generation(d.bytes().unwrap().try_into().unwrap());
    let begin = d.position();
    d.skip().unwrap();
    let cert =
        chat_core::account::InstallationCert::from_bytes(&bytes[begin..d.position()]).unwrap();
    let nonce = d.bytes().unwrap().try_into().unwrap();
    (peer, generation, cert, nonce)
}
#[tokio::test]
async fn replay_other_connection_role_nonce_rejected() {
    use ed25519_dalek::{Signer, SigningKey};
    for mode in ["replay", "role", "nonce", "identity_key", "endpoint"] {
        let a_packet = packet(1, &[11], 1, NOW, NOW + 600, NOW * 1_000_000);
        let b_packet = packet(2, &[12], 2, NOW, NOW + 600, NOW * 1_000_000);
        let a_peer = derive_account(&Secret32::from([1; 32])).unwrap().peer_id();
        let b_keys = derive_account(&Secret32::from([2; 32])).unwrap();
        let b_peer = b_keys.peer_id();
        let backend = Arc::new(Backend {
            packets: Mutex::new(BTreeMap::from([
                (a_peer, a_packet.clone()),
                (b_peer, b_packet.clone()),
            ])),
            offline: AtomicBool::new(false),
            capture: Mutex::new(None),
            observed: Mutex::new(vec![]),
            ..Default::default()
        });
        let a = Fixture::new(1, 11, &a_packet, backend).await;
        let raw = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&derive_seed(
                12,
                b"ChatProject/device-keys/v1",
                b"transport-ed25519",
            )))
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![b"chatproject/account/1".to_vec()])
            .bind()
            .await
            .unwrap();
        let addresses = raw.bound_sockets().into_iter().map(|mut socket| {
            if socket.ip().is_unspecified() {
                socket.set_ip(if socket.is_ipv4() {
                    std::net::Ipv4Addr::LOCALHOST.into()
                } else {
                    std::net::Ipv6Addr::LOCALHOST.into()
                });
            }
            iroh::TransportAddr::Ip(socket)
        });
        let ticket = iroh_tickets::endpoint::EndpointTicket::new(iroh::EndpointAddr::from_parts(
            raw.id(),
            addresses,
        ))
        .to_string();
        a.slot
            .network()
            .unwrap()
            .account_target_for_test(ticket)
            .await
            .unwrap();
        let b_device = derive_device(Secret32::from(
            [if mode == "endpoint" { 13 } else { 12 }; 32],
        ))
        .unwrap();
        let cert = sign_cert(&b_keys, &b_device, DeviceKind::Desktop, NOW).unwrap();
        let endpoint = raw.clone();
        let task = tokio::spawn(async move {
            let mut previous = None;
            let mut connections = vec![];
            for attempt in 0..if mode == "replay" { 2 } else { 1 } {
                let conn = endpoint.accept().await.unwrap().await.unwrap();
                let (mut send, mut recv) = conn.accept_bi().await.unwrap();
                let hello = read_control(&mut recv).await;
                let mut remote = hello_parts(&hello);
                let own_nonce = [44; 32];
                let mut e = minicbor::Encoder::new(Vec::new());
                e.array(5)
                    .unwrap()
                    .u8(1)
                    .unwrap()
                    .bytes(&b_peer.0)
                    .unwrap()
                    .bytes(&[2; 16])
                    .unwrap();
                let mut own = e.into_writer();
                own.extend(cert.to_bytes());
                minicbor::Encoder::new(&mut own).bytes(&own_nonce).unwrap();
                send_control(&mut send, &control(0, &own)).await;
                if mode == "endpoint" {
                    connections.push(conn);
                    break;
                }
                let _ = read_control(&mut recv).await;
                send_control(&mut send, &control(2, b_packet.as_bytes())).await;
                if mode == "nonce" {
                    remote.3[0] ^= 1;
                }
                let own = (b_peer, Generation([2; 16]), cert.clone(), own_nonce);
                let roles = if mode == "role" {
                    [&own, &remote]
                } else {
                    [&remote, &own]
                };
                let mut e = minicbor::Encoder::new(Vec::new());
                e.array(3).unwrap().u8(1).unwrap();
                for (p, g, c, n) in roles {
                    e.array(6)
                        .unwrap()
                        .bytes(&p.0)
                        .unwrap()
                        .bytes(&c.transport_public_key)
                        .unwrap()
                        .bytes(&c.installation_id.0)
                        .unwrap()
                        .bytes(&c.hash())
                        .unwrap()
                        .bytes(&g.0)
                        .unwrap()
                        .bytes(n)
                        .unwrap();
                }
                let context = e.into_writer();
                let mut exporter = [0; 32];
                conn.export_keying_material(
                    &mut exporter,
                    b"ChatProject/account-exporter/v1",
                    &context,
                )
                .unwrap();
                let mut message = b"ChatProject/account-auth/v1".to_vec();
                message.extend(context);
                message.extend(exporter);
                let key = if mode == "identity_key" {
                    derive_seed(2, b"ChatProject/account-keys/v1", b"identity-ed25519")
                } else {
                    derive_seed(12, b"ChatProject/device-keys/v1", b"installation-ed25519")
                };
                let sig = if attempt == 1 {
                    previous.unwrap()
                } else {
                    SigningKey::from_bytes(&key).sign(&message).to_bytes()
                };
                previous = Some(sig);
                let _ = read_control(&mut recv).await;
                send_control(&mut send, &control(1, &sig)).await;
                let _ = send.finish();
                connections.push(conn);
            }
            connections
        });
        if mode == "replay" {
            a.account.connect_account(b_peer).await.unwrap();
        }
        assert!(
            a.account.connect_account(b_peer).await.is_err(),
            "accepted {mode}"
        );
        task.await.unwrap();
        raw.close().await;
    }
}

#[tokio::test]
async fn remote_high_water_conflict_survives_store_reload() {
    let (a, b, backend) = pair().await;
    a.account.connect_account(b.peer).await.unwrap();
    let old = backend
        .packets
        .lock()
        .unwrap()
        .get(&b.peer)
        .unwrap()
        .clone();
    backend.packets.lock().unwrap().insert(
        b.peer,
        packet(2, &[12], 9, NOW + 1, NOW + 601, (NOW + 1) * 1_000_000),
    );
    assert!(a.account.refresh_record(b.peer).await.is_err());
    a.account.request_suspend();
    tokio::time::timeout(Duration::from_secs(10), async {
        while a.account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    backend.packets.lock().unwrap().insert(b.peer, old);
    a.account.request_resume();
    tokio::time::timeout(Duration::from_secs(10), async {
        while a.account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    a.account.snapshot().await.unwrap();
    assert!(a.account.connect_account(b.peer).await.is_err());
}
#[tokio::test]
async fn previous_array8_and9_snapshots_are_rejected_without_file_mutation() {
    for field_count in [8, 9] {
        let (a, _b, _backend) = pair().await;
        a.account.request_suspend();
        tokio::time::timeout(Duration::from_secs(10), async {
            while a.account.lifecycle_status() == 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let path = a._dir.path().to_path_buf();
        drop(a.account);
        drop(a.slot);
        let mut store = Store::open(&path, support::protector()).unwrap();
        let mut state = store.load().unwrap().unwrap();
        let mut d = minicbor::Decoder::new(&state.payload);
        d.array().unwrap();
        for _ in 0..field_count {
            d.skip().unwrap();
        }
        let end = d.position();
        state.payload.truncate(end);
        state.payload[0] = 0x80 + field_count;
        store.commit(&state).unwrap();
        drop(store);
        let before = std::fs::read(path.join("account.bin")).unwrap();
        let slot = CoreSlot::new();
        slot.start(
            CoreConfig {
                data_dir: path.clone(),
            },
            &EventBus::new(),
        )
        .unwrap();
        slot.network().unwrap().configure_local(None).await.unwrap();
        let backend = Arc::new(Backend {
            packets: Mutex::new(BTreeMap::new()),
            offline: AtomicBool::new(true),
            capture: Mutex::new(None),
            observed: Mutex::new(vec![]),
            ..Default::default()
        });
        assert!(
            slot.open_account_with_services(
                support::protector(),
                AccountServices::new(backend, Arc::new(FakeClock::new(NOW, 0)))
            )
            .is_err()
        );
        assert_eq!(std::fs::read(path.join("account.bin")).unwrap(), before);
    }
}
#[tokio::test]
async fn lifecycle_suspend_invalidates_ready_event_and_cancels_account_work() {
    let (a, b, _) = pair().await;
    a.account.connect_account(b.peer).await.unwrap();
    a.account.request_suspend();
    tokio::time::timeout(Duration::from_secs(10), async {
        while a.account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(a.account.connect_account(b.peer).await.is_err());
    assert!(
        a.events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, CoreEvent::AccountAuthChanged { ready: false, .. }))
    );
}
#[tokio::test]
async fn expired_session_accepts_public_record_sync_without_mutation_or_grant() {
    let signed = packet(4, &[31, 32], 4, NOW, NOW + 600, NOW * 1_000_000);
    let peer = derive_account(&Secret32::from([4; 32])).unwrap().peer_id();
    let backend = Arc::new(Backend {
        packets: Mutex::new(BTreeMap::from([(peer, signed.clone())])),
        offline: AtomicBool::new(false),
        capture: Mutex::new(None),
        observed: Mutex::new(vec![]),
        ..Default::default()
    });
    let a = Fixture::new(4, 31, &signed, backend.clone()).await;
    let b = Fixture::new(4, 32, &signed, backend.clone()).await;
    a.route(&b).await;
    b.route(&a).await;
    a.account.connect_account(peer).await.unwrap();
    a.clock.set_unix_seconds(NOW + 600);
    b.clock.set_unix_seconds(NOW + 600);
    backend.packets.lock().unwrap().remove(&peer);
    a.account.refresh_record(peer).await.unwrap();
    a.account.refresh_record(peer).await.unwrap();
    let expected = a.view().await.publication.accepted_hash;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let view = b.view().await;
            if view.publication.accepted_hash == expected {
                assert_eq!(view.publication.pending_count, 0);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        b.events
            .0
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|e| matches!(e, CoreEvent::AccountAuthChanged { .. })),
        Some(CoreEvent::AccountAuthChanged { ready: false, .. })
    ));
    a.account.connect_account(peer).await.unwrap();
}

struct FailingProtector(AtomicBool);
impl KeyProtector for FailingProtector {
    fn protect(&self, id: StoreId, key: &Secret32) -> Result<Vec<u8>, CoreError> {
        support::protector().protect(id, key)
    }
    fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
        if self.0.load(Ordering::SeqCst) {
            Err(CoreError::new(
                ErrorCode::StorageLocked,
                "receiver fixture unavailable",
            ))
        } else {
            support::protector().unprotect(id, blob)
        }
    }
    fn delete(&self, id: StoreId) -> Result<(), CoreError> {
        support::protector().delete(id)
    }
}
#[tokio::test]
async fn receiver_store_failure_keeps_real_notification_pending_and_never_publishes() {
    let signed = packet(5, &[41, 42, 43], 5, NOW, NOW + 600, NOW * 1_000_000);
    let peer = derive_account(&Secret32::from([5; 32])).unwrap().peer_id();
    let backend = Arc::new(Backend {
        packets: Mutex::new(BTreeMap::from([(peer, signed.clone())])),
        offline: AtomicBool::new(false),
        capture: Mutex::new(None),
        observed: Mutex::new(vec![]),
        ..Default::default()
    });
    let protector = Arc::new(FailingProtector(AtomicBool::new(false)));
    let mut seeds = [41, 42, 43];
    seeds.sort_by_key(|seed| {
        derive_device(Secret32::from([*seed; 32]))
            .unwrap()
            .installation_id()
    });
    let a = Fixture::new(5, seeds[0], &signed, backend.clone()).await;
    let b = Fixture::with_protector(5, seeds[1], &signed, backend.clone(), protector.clone()).await;
    let c = Fixture::new(5, seeds[2], &signed, backend.clone()).await;
    a.route(&b).await;
    a.route(&c).await;
    b.route(&a).await;
    b.route(&c).await;
    c.route(&a).await;
    c.route(&b).await;
    b.account.connect_account(peer).await.unwrap();
    c.account.connect_account(peer).await.unwrap();
    a.account.connect_account(peer).await.unwrap();
    protector.0.store(true, Ordering::SeqCst);
    let target = c.view().await.installation.unwrap();
    assert!(a.account.revoke_device(target).await.is_err());
    assert_eq!(a.view().await.publication.pending_notifications.len(), 1);
    assert_eq!(
        backend.packets.lock().unwrap()[&peer].as_bytes(),
        signed.as_bytes()
    );
    assert_eq!(durable_tombstones(a._dir.path()).len(), 1);
    assert_eq!(durable_tombstones(b._dir.path()).len(), 0);
    // A receiver that could not commit closes. Retrying must not silently
    // replace the original expected recipient with today's empty registry.
    assert!(a.account.refresh_record(peer).await.is_err());
    a.account.request_suspend();
    tokio::time::timeout(Duration::from_secs(10), async {
        while a.account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    a.account.request_resume();
    tokio::time::timeout(Duration::from_secs(10), async {
        while a.account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    a.account.snapshot().await.unwrap();
    assert!(a.account.refresh_record(peer).await.is_err());
    assert_eq!(a.view().await.publication.pending_notifications.len(), 1);
    assert_eq!(
        backend.packets.lock().unwrap()[&peer].as_bytes(),
        signed.as_bytes()
    );
}
#[tokio::test]
async fn future_remote_observation_never_becomes_cache_authority_after_clock_catchup() {
    let (a, b, backend) = pair().await;
    a.account.connect_account(b.peer).await.unwrap();
    backend.packets.lock().unwrap().insert(
        b.peer,
        packet(2, &[12], 2, NOW + 600, NOW + 1200, (NOW + 600) * 1_000_000),
    );
    assert!(a.account.refresh_record(b.peer).await.is_err());
    a.clock.set_unix_seconds(NOW + 600);
    backend.offline.store(true, Ordering::SeqCst);
    assert!(a.account.refresh_record(b.peer).await.is_err());
}

// A separately owned loopback installation performs the actual exporter proof,
// then can exercise controls a normal revoked target is deliberately not sent.
async fn raw_peer(
    receiver: &Fixture,
    seed: u8,
    device_seed: u8,
    signed: SignedPacket,
) -> (iroh::Endpoint, iroh::endpoint::Connection) {
    use ed25519_dalek::{Signer, SigningKey};
    let keys = derive_account(&Secret32::from([seed; 32])).unwrap();
    let device = derive_device(Secret32::from([device_seed; 32])).unwrap();
    let cert = sign_cert(&keys, &device, DeviceKind::Desktop, NOW).unwrap();
    let peer = keys.peer_id();
    let record = chat_core::account::parse_record(&signed, peer, NOW).unwrap();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&derive_seed(
            device_seed,
            b"ChatProject/device-keys/v1",
            b"transport-ed25519",
        )))
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![b"chatproject/account/1".to_vec()])
        .bind()
        .await
        .unwrap();
    let addresses = endpoint.bound_sockets().into_iter().map(|mut socket| {
        if socket.ip().is_unspecified() {
            socket.set_ip(if socket.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        iroh::TransportAddr::Ip(socket)
    });
    let ticket = iroh_tickets::endpoint::EndpointTicket::new(iroh::EndpointAddr::from_parts(
        endpoint.id(),
        addresses,
    ))
    .to_string();
    receiver
        .slot
        .network()
        .unwrap()
        .account_target_for_test(ticket)
        .await
        .unwrap();
    let raw = endpoint.clone();
    let task = tokio::spawn(async move {
        let conn = raw.accept().await.unwrap().await.unwrap();
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        let remote = hello_parts(&read_control(&mut recv).await);
        let nonce = [87; 32];
        let mut e = minicbor::Encoder::new(Vec::new());
        e.array(5)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&peer.0)
            .unwrap()
            .bytes(&record.generation.0)
            .unwrap();
        let mut hello = e.into_writer();
        hello.extend(cert.to_bytes());
        minicbor::Encoder::new(&mut hello).bytes(&nonce).unwrap();
        send_control(&mut send, &control(0, &hello)).await;
        let _ = read_control(&mut recv).await;
        send_control(&mut send, &control(2, signed.as_bytes())).await;
        let own = (peer, record.generation, cert, nonce);
        let mut e = minicbor::Encoder::new(Vec::new());
        e.array(3).unwrap().u8(1).unwrap();
        for (p, g, c, n) in [&remote, &own] {
            e.array(6)
                .unwrap()
                .bytes(&p.0)
                .unwrap()
                .bytes(&c.transport_public_key)
                .unwrap()
                .bytes(&c.installation_id.0)
                .unwrap()
                .bytes(&c.hash())
                .unwrap()
                .bytes(&g.0)
                .unwrap()
                .bytes(n)
                .unwrap();
        }
        let context = e.into_writer();
        let mut exporter = [0; 32];
        conn.export_keying_material(&mut exporter, b"ChatProject/account-exporter/v1", &context)
            .unwrap();
        let mut message = b"ChatProject/account-auth/v1".to_vec();
        message.extend(context);
        message.extend(exporter);
        let sig = SigningKey::from_bytes(&derive_seed(
            device_seed,
            b"ChatProject/device-keys/v1",
            b"installation-ed25519",
        ))
        .sign(&message)
        .to_bytes();
        let _ = read_control(&mut recv).await;
        send_control(&mut send, &control(1, &sig)).await;
        send.finish().unwrap();
        conn
    });
    receiver.account.connect_account(peer).await.unwrap();
    (endpoint, task.await.unwrap())
}
fn signed_revoke(
    seed: u8,
    signed: &SignedPacket,
    target_seed: u8,
) -> chat_core::account::RevokeIntent {
    use ed25519_dalek::{Signer, SigningKey};
    let peer = derive_account(&Secret32::from([seed; 32]))
        .unwrap()
        .peer_id();
    let record = chat_core::account::parse_record(signed, peer, NOW).unwrap();
    let target = derive_device(Secret32::from([target_seed; 32])).unwrap();
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(8)
        .unwrap()
        .u8(1)
        .unwrap()
        .bytes(&peer.0)
        .unwrap()
        .bytes(&record.generation.0)
        .unwrap()
        .bytes(&chat_core::account::record_hash(signed))
        .unwrap()
        .bytes(&target.installation_id().0)
        .unwrap()
        .bytes(&target.transport_public_key())
        .unwrap()
        .bytes(&[88; 16])
        .unwrap()
        .u64(NOW)
        .unwrap();
    let unsigned = e.into_writer();
    let mut message = b"ChatProject/revoke/v1".to_vec();
    message.push(1);
    message.extend(&unsigned);
    let sig = SigningKey::from_bytes(&derive_seed(
        seed,
        b"ChatProject/account-keys/v1",
        b"identity-ed25519",
    ))
    .sign(&message)
    .to_bytes();
    let mut bytes = unsigned;
    bytes[0] = 0x89;
    minicbor::Encoder::new(&mut bytes).bytes(&sig).unwrap();
    let intent = chat_core::account::RevokeIntent::from_bytes(&bytes).unwrap();
    intent.verify(peer).unwrap();
    intent
}
async fn reload(fixture: &Fixture) {
    for suspend in [true, false] {
        if suspend {
            fixture.account.request_suspend();
        } else {
            fixture.account.request_resume();
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            while fixture.account.lifecycle_status() == 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    fixture.account.snapshot().await.unwrap();
}
#[tokio::test]
async fn received_revoke_holds_ready_packet_after_reload() {
    received_revoke_with_pending(false).await;
}
#[tokio::test]
async fn received_own_removal_stops_ready_publication_after_reload() {
    received_revoke_with_pending(true).await;
}
async fn received_revoke_with_pending(removes_receiver: bool) {
    let signed = packet(6, &[51, 52, 53], 6, NOW, NOW + 600, NOW * 1_000_000);
    let peer = derive_account(&Secret32::from([6; 32])).unwrap().peer_id();
    let backend = Arc::new(Backend {
        packets: Mutex::new(BTreeMap::from([(peer, signed.clone())])),
        ..Default::default()
    });
    let receiver = Fixture::new(6, 51, &signed, backend.clone()).await;
    let (raw, conn) = raw_peer(&receiver, 6, 52, signed.clone()).await;
    backend.packets.lock().unwrap().remove(&peer);
    *backend.publish_error.lock().unwrap() = Some(ErrorCode::RecordUnavailable);
    assert!(receiver.account.refresh_record(peer).await.is_err());
    assert_eq!(receiver.view().await.publication.pending_count, 1);
    assert_eq!(backend.publishes.lock().unwrap().len(), 1);
    backend.packets.lock().unwrap().insert(peer, signed.clone());
    let intent = signed_revoke(6, &signed, if removes_receiver { 51 } else { 53 });
    let frame = intent.to_frame();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    send.write_all(&frame).await.unwrap();
    send.finish().unwrap();
    if removes_receiver {
        tokio::time::timeout(Duration::from_secs(5), conn.closed())
            .await
            .unwrap();
    } else {
        assert_eq!(read_control(&mut recv).await, frame[4..]);
    }
    assert_eq!(durable_tombstones(receiver._dir.path()), vec![intent]);
    backend.publishes.lock().unwrap().clear();
    *backend.publish_error.lock().unwrap() = None;
    for absent in [false, true] {
        if absent {
            backend.packets.lock().unwrap().remove(&peer);
        }
        let _ = receiver.account.refresh_record(peer).await;
        assert!(
            backend.publishes.lock().unwrap().is_empty(),
            "stale Ready packet published after received revoke (own={removes_receiver}, absent={absent})"
        );
    }
    reload(&receiver).await;
    let _ = receiver.account.refresh_record(peer).await;
    assert!(
        backend.publishes.lock().unwrap().is_empty(),
        "reload forgot publication restriction"
    );
    assert_eq!(receiver.view().await.publication.pending_count, 1);
    raw.close().await;
}

#[tokio::test]
async fn publish_conflict_without_signed_evidence_closes_and_denies_account_grants() {
    let (a, b, backend) = pair().await;
    a.account.connect_account(b.peer).await.unwrap();
    let accepted = backend.packets.lock().unwrap().remove(&a.peer).unwrap();
    *backend.publish_error.lock().unwrap() = Some(ErrorCode::RecordConflict);
    assert_eq!(
        a.account.refresh_record(a.peer).await.unwrap_err().code,
        ErrorCode::RecordConflict
    );
    let view = a.view().await;
    assert_eq!(
        view.publication.state,
        chat_core::account::PublicationState::Conflict
    );
    assert!(view.publication.conflict.is_none());
    assert!(
        a.events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, CoreEvent::AccountAuthChanged { ready: false, .. })),
        "known publish conflict retained the active grant"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !b
            .events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, CoreEvent::AccountAuthChanged { ready: false, .. }))
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("conflicted connection was not closed");
    assert!(a.account.connect_account(b.peer).await.is_err());
    backend.offline.store(true, Ordering::SeqCst);
    assert!(a.account.refresh_record(a.peer).await.is_err());
    assert!(a.account.connect_account(b.peer).await.is_err());
    assert_eq!(
        a.view().await.publication.state,
        chat_core::account::PublicationState::Conflict
    );
    backend.offline.store(false, Ordering::SeqCst);
    backend.packets.lock().unwrap().insert(a.peer, accepted);
    a.clock.set_unix_seconds(NOW - 600);
    assert!(a.account.refresh_record(a.peer).await.is_err());
    assert_eq!(
        a.view().await.publication.state,
        chat_core::account::PublicationState::Conflict,
        "clock-only rejection erased known publication conflict"
    );
    a.clock.set_unix_seconds(NOW);
    *backend.publish_error.lock().unwrap() = None;
    let competing = packet(1, &[11], 1, NOW + 1, NOW + 601, (NOW + 1) * 1_000_000);
    backend
        .packets
        .lock()
        .unwrap()
        .insert(a.peer, competing.clone());
    assert!(a.account.refresh_record(a.peer).await.is_err());
    a.account
        .confirm_record_conflict(chat_core::account::record_hash(&competing))
        .await
        .unwrap();
    a.account.refresh_record(a.peer).await.unwrap();
    a.account.connect_account(b.peer).await.unwrap();
}

fn durable_remote(root: &std::path::Path, peer: PeerId) -> Option<(Vec<u8>, u64)> {
    let state = durable_snapshot(root);
    let mut d = minicbor::Decoder::new(&state.payload);
    assert_eq!(d.array().unwrap(), Some(10));
    for _ in 0..8 {
        d.skip().unwrap();
    }
    let mut cache = minicbor::Decoder::new(d.bytes().unwrap());
    for _ in 0..cache.array().unwrap().unwrap() {
        assert_eq!(cache.array().unwrap(), Some(4));
        let id = cache.bytes().unwrap();
        let packet = cache.bytes().unwrap().to_vec();
        let floor = cache.u64().unwrap();
        cache.skip().unwrap();
        if id == peer.0 {
            return Some((packet, floor));
        }
    }
    None
}
#[tokio::test]
async fn different_accounts_exchange_expired_public_records_durably_before_fresh_proof() {
    let (a, b, backend) = pair().await;
    a.account.connect_account(b.peer).await.unwrap();
    let ready = |fixture: &Fixture| {
        fixture
            .events
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, CoreEvent::AccountAuthChanged { ready: true, .. }))
            .count()
    };
    let before = (ready(&a), ready(&b));
    a.clock.set_unix_seconds(NOW + 600);
    b.clock.set_unix_seconds(NOW + 600);
    assert!(a.account.connect_account(b.peer).await.is_err());
    assert!(b.account.connect_account(a.peer).await.is_err());
    backend.packets.lock().unwrap().remove(&a.peer);
    backend.packets.lock().unwrap().remove(&b.peer);
    a.account.refresh_record(a.peer).await.unwrap();
    a.account.refresh_record(a.peer).await.unwrap();
    let renewed_a = backend.packets.lock().unwrap()[&a.peer].clone();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if durable_remote(b._dir.path(), a.peer)
                .is_some_and(|(bytes, _)| bytes.as_slice() == renewed_a.as_bytes().as_ref())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("different-account public renewal never reached durable remote cache");
    b.account.refresh_record(b.peer).await.unwrap();
    b.account.refresh_record(b.peer).await.unwrap();
    let renewed_b = backend.packets.lock().unwrap()[&b.peer].clone();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if durable_remote(a._dir.path(), b.peer)
                .is_some_and(|(bytes, _)| bytes.as_slice() == renewed_b.as_bytes().as_ref())
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("reverse public renewal never reached durable remote cache");
    for (receiver, peer, packet) in [(&a, b.peer, &renewed_b), (&b, a.peer, &renewed_a)] {
        let (bytes, floor) = durable_remote(receiver._dir.path(), peer).unwrap();
        assert_eq!(bytes.as_slice(), packet.as_bytes().as_ref());
        assert_eq!(
            floor,
            u64::from_be_bytes(packet.as_bytes()[96..104].try_into().unwrap())
        );
        assert_eq!(receiver.view().await.publication.pending_count, 0);
    }
    assert_eq!(
        (ready(&a), ready(&b)),
        before,
        "public-only exchange granted authority"
    );
    assert_eq!(
        backend.publishes.lock().unwrap().len(),
        2,
        "remote observation allocated a mutation"
    );
    backend.offline.store(true, Ordering::SeqCst);
    a.account.connect_account(b.peer).await.unwrap();
    assert_eq!(ready(&a), before.0 + 1, "new proof did not grant readiness");
}

#[tokio::test]
async fn cross_account_public_controls_bind_peer_and_preserve_observation_floor() {
    for mode in ["rollback", "future", "wrong_peer"] {
        let own = packet(1, &[11], 1, NOW, NOW + 600, NOW * 1_000_000);
        let remote = packet(2, &[12], 2, NOW, NOW + 600, NOW * 1_000_000);
        let a_peer = derive_account(&Secret32::from([1; 32])).unwrap().peer_id();
        let b_peer = derive_account(&Secret32::from([2; 32])).unwrap().peer_id();
        let backend = Arc::new(Backend {
            packets: Mutex::new(BTreeMap::from([
                (a_peer, own.clone()),
                (b_peer, remote.clone()),
            ])),
            ..Default::default()
        });
        let a = Fixture::new(1, 11, &own, backend).await;
        let (raw, conn) = raw_peer(&a, 2, 12, remote.clone()).await;
        let renewal = packet(2, &[12], 2, NOW + 1, NOW + 601, (NOW + 1) * 1_000_000);
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        send_control(&mut send, &control(2, renewal.as_bytes())).await;
        send.finish().unwrap();
        let _ = read_control(&mut recv).await;
        let unexpected = match mode {
            "rollback" => remote,
            "future" => packet(2, &[12], 2, NOW + 600, NOW + 1200, (NOW + 600) * 1_000_000),
            _ => own,
        };
        let (mut send, _recv) = conn.open_bi().await.unwrap();
        send_control(&mut send, &control(2, unexpected.as_bytes())).await;
        send.finish().unwrap();
        tokio::time::timeout(Duration::from_secs(5), conn.closed())
            .await
            .unwrap();
        let (accepted, floor) = durable_remote(a._dir.path(), b_peer).unwrap();
        assert_eq!(accepted.as_slice(), renewal.as_bytes().as_ref());
        let expected_floor = if mode == "future" {
            (NOW + 600) * 1_000_000
        } else {
            (NOW + 1) * 1_000_000
        };
        assert_eq!(
            floor, expected_floor,
            "public control lost {mode} observation policy"
        );
        assert!(durable_remote(a._dir.path(), a_peer).is_none());
        raw.close().await;
    }
}
#[tokio::test]
async fn contact_device_status_requires_live_fresh_proof_and_preserves_safety() {
    let (a, b, backend) = pair().await;
    let old_packet = packet(2, &[13], 2, NOW, NOW + 600, NOW * 1_000_000);
    let isolated = Arc::new(Backend::default());
    isolated
        .packets
        .lock()
        .unwrap()
        .insert(b.peer, old_packet.clone());
    let old = Fixture::new(2, 13, &old_packet, isolated).await;
    let invite = old
        .account
        .create_invite(Some("untrusted claim".into()))
        .await
        .unwrap();
    let contact = a
        .account
        .accept_invite(invite.clone(), "local".into())
        .await
        .unwrap();
    assert!(contact.device_unverified);
    assert!(!contact.verified);
    assert!(!contact.new_device);
    let digest = chat_core::account::safety_digest(a.peer, b.peer);
    a.account.confirm_safety(b.peer, digest).await.unwrap();
    // Fresh proof must resolve current public record, not trust the stale invite endpoint.
    a.account.connect_account(b.peer).await.unwrap();
    let contact = a.account.contacts().await.unwrap().remove(0);
    assert!(!contact.device_unverified);
    assert!(contact.new_device);
    assert!(contact.verified);
    assert_eq!(contact.alias, "local");
    let duplicate = a
        .account
        .accept_invite(invite, "overwrite".into())
        .await
        .unwrap();
    assert_eq!(duplicate, contact);
    reload(&a).await;
    let offline = a.account.contacts().await.unwrap().remove(0);
    assert!(offline.device_unverified);
    assert!(offline.new_device);
    assert!(offline.verified);

    let c_packet = packet(3, &[14], 3, NOW, NOW + 600, NOW * 1_000_000);
    let c_peer = derive_account(&Secret32::from([3; 32])).unwrap().peer_id();
    backend
        .packets
        .lock()
        .unwrap()
        .insert(c_peer, c_packet.clone());
    let c = Fixture::new(3, 14, &c_packet, backend).await;
    let different = a
        .account
        .accept_invite(c.account.create_invite(None).await.unwrap(), "other".into())
        .await
        .unwrap();
    assert!(!different.verified);
    assert!(different.device_unverified);
    assert!(!different.new_device);
    assert_ne!(chat_core::account::safety_digest(a.peer, c.peer), digest);
}
#[tokio::test]
async fn invited_current_installation_proof_does_not_report_new_device() {
    let (a, b, _) = pair().await;
    a.account
        .accept_invite(b.account.create_invite(None).await.unwrap(), "b".into())
        .await
        .unwrap();
    a.account.connect_account(b.peer).await.unwrap();
    let contact = a.account.contacts().await.unwrap().remove(0);
    assert!(!contact.device_unverified);
    assert!(!contact.new_device);
    assert!(!contact.verified);
    b.clock.set_unix_seconds(NOW + 601);
    a.clock.set_unix_seconds(NOW + 601);
    assert!(a.account.contacts().await.unwrap()[0].device_unverified);
}
