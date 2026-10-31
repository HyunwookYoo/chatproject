#![cfg(all(feature = "test-hooks", debug_assertions))]
use chat_core::account::{
    AccountHandle, AccountServices, FlowId, Lookup, PairDirection, PairPhase, PeerId,
    RecordBackend, RecordFuture, Secret32, StoreId, clock::FakeClock, storage::KeyProtector,
};
use chat_core::{CoreConfig, CoreError, CoreSlot, EventBus, NetworkOp, NetworkReply};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
const NOW: u64 = 1_800_000_000;

#[tokio::test]
async fn ended_pair_cancel_is_idempotent_and_never_cancels_replacement() {
    let n = Fixture::new(false, Arc::new(Backend::default())).await;
    let old = n.a.pair_begin(PairDirection::NewDisplays, None).await.unwrap();
    n.clock.advance_monotonic_ms(300_000);
    assert_eq!(n.a.pair_confirm_code(old.flow, true).await.unwrap_err().code, chat_core::ErrorCode::PairExpired);
    n.a.pair_cancel(old.flow).await.unwrap();
    n.a.pair_cancel(old.flow).await.unwrap();
    let next = n.a.pair_begin(PairDirection::NewDisplays, None).await.unwrap();
    n.a.pair_cancel(old.flow).await.unwrap();
    assert_eq!(n.a.pair_view().await.unwrap().unwrap().flow, next.flow);
    n.a.pair_cancel(next.flow).await.unwrap();
    assert!(n.a.pair_view().await.unwrap().is_none());
}

#[tokio::test]
async fn remote_close_and_lifecycle_ended_flow_can_be_abandoned() {
    let (e, n, flow, _) = connected(PairDirection::NewDisplays).await;
    e.a.pair_cancel(flow).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while n.a.pair_view().await.unwrap().is_some() { tokio::task::yield_now().await; }
    }).await.unwrap();
    n.a.pair_cancel(flow).await.unwrap();
    let next = n.a.pair_begin(PairDirection::NewDisplays, None).await.unwrap();
    n.a.request_suspend();
    while n.a.lifecycle_status() == 1 { tokio::task::yield_now().await; }
    assert_eq!(n.a.pair_cancel(next.flow).await.unwrap_err().code, chat_core::ErrorCode::StorageLocked);
    n.a.request_resume();
    while n.a.lifecycle_status() == 1 { tokio::task::yield_now().await; }
    n.a.pair_cancel(next.flow).await.unwrap();
    assert!(n.a.pair_view().await.unwrap().is_none());
}
struct Protector;
impl KeyProtector for Protector {
    fn protect(&self, _: StoreId, s: &Secret32) -> Result<Vec<u8>, CoreError> {
        Ok(s.as_bytes().to_vec())
    }
    fn unprotect(&self, _: StoreId, b: &[u8]) -> Result<Secret32, CoreError> {
        Ok(Secret32::from(<[u8; 32]>::try_from(b).unwrap()))
    }
    fn delete(&self, _: StoreId) -> Result<(), CoreError> {
        Ok(())
    }
}
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
struct Fixture {
    _dir: tempfile::TempDir,
    slot: CoreSlot,
    a: AccountHandle,
    clock: Arc<FakeClock>,
}
impl Fixture {
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
                Arc::new(Protector),
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
        Self {
            _dir: dir,
            slot,
            a,
            clock,
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
    assert_eq!(eview.remote_installation, n.a.snapshot().await.unwrap().installation);
    assert_eq!(nview.remote_installation, e.a.snapshot().await.unwrap().installation);
    assert_eq!(eview.remote_kind, Some(chat_core::account::DeviceKind::Desktop));
    assert_eq!(nview.remote_kind, Some(chat_core::account::DeviceKind::Desktop));
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
async fn same_sas_both_local_confirmations() {
    for d in [PairDirection::NewDisplays, PairDirection::ExistingDisplays] {
        let (e, n, f, _) = connected(d).await;
        let ev = e.a.pair_view().await.unwrap().unwrap();
        let nv = n.a.pair_view().await.unwrap().unwrap();
        assert_eq!(ev.sas, nv.sas);
        let sas = ev.sas.unwrap();
        assert_eq!(sas.len(), 9);
        assert_eq!(&sas[4..5], "-");
        assert!(
            sas.chars()
                .filter(|c| *c != '-')
                .all(|c| c.is_ascii_uppercase() || ('2'..='7').contains(&c))
        );
        assert!(e.a.pair_approve(f).await.is_err());
        confirmations(&e, &n, f).await;
        approve(&e, f).await;
        assert!(matches!(e.a.pair_view().await.unwrap().unwrap().phase,
            PairPhase::DisclosurePossible | PairPhase::ReceiptVerified | PairPhase::PublishPending | PairPhase::Observed));
    }
}
#[tokio::test]
async fn zero_gated_bytes_before_existing_local_approval() {
    for d in [PairDirection::NewDisplays, PairDirection::ExistingDisplays] {
        let (e, n, f, _) = connected(d).await;
        let secret = b"bounded synthetic identity/contact/cert fixture".to_vec();
        assert!(e.a.pair_fixture_send(f, secret.clone()).await.is_err());
        assert!(n.a.pair_fixture_received(f).await.unwrap().is_empty());
        confirmations(&e, &n, f).await;
        assert!(e.a.pair_fixture_send(f, secret.clone()).await.is_err());
        assert!(n.a.pair_fixture_received(f).await.unwrap().is_empty());
        approve(&e, f).await;
        let count = e.a.pair_fixture_send(f, secret.clone()).await.unwrap();
        assert!(count > secret.len());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let got = n.a.pair_fixture_received(f).await.unwrap();
                if !got.is_empty() {
                    assert_eq!(got, secret);
                    break;
                }
                tokio::task::yield_now().await
            }
        })
        .await
        .unwrap();
    }
}
#[tokio::test]
async fn remote_confirm_is_not_existing_local_approval() {
    for d in [PairDirection::NewDisplays, PairDirection::ExistingDisplays] {
        let (e, n, f, _) = connected(d).await;
        n.a.pair_confirm_code(f, true).await.unwrap();
        assert!(n.a.pair_approve(f).await.is_err());
        assert!(e.a.pair_approve(f).await.is_err());
        assert!(e.a.pair_fixture_send(f, vec![7; 32]).await.is_err());
        assert_eq!(
            e.a.pair_view().await.unwrap().unwrap().phase,
            PairPhase::Comparing
        );
    }
}
#[tokio::test]
async fn wrong_endpoint_role_qr_candidate_replay_denied() {
    for d in [PairDirection::NewDisplays, PairDirection::ExistingDisplays] {
        let (e, n, f, qr) = connected(d).await;
        let owner = if d == PairDirection::NewDisplays {
            &n
        } else {
            &e
        };
        assert!(owner.a.pair_scan(qr.clone()).await.is_err());
        owner.a.pair_cancel(f).await.unwrap();
        assert!(owner.a.pair_confirm_code(f, true).await.is_err());
        assert!(owner.a.pair_scan(qr).await.is_err());
    }
}
#[tokio::test]
async fn three_failures_and_five_min_expiry() {
    for d in [PairDirection::NewDisplays, PairDirection::ExistingDisplays] {
        let (e, n, f, _) = connected(d).await;
        e.clock.advance_monotonic_ms(300_000);
        n.clock.advance_monotonic_ms(300_000);
        assert!(e.a.pair_confirm_code(f, true).await.is_err());
        assert!(n.a.pair_confirm_code(f, true).await.is_err());
        assert!(e.a.pair_view().await.unwrap().is_none());
        assert!(n.a.pair_view().await.unwrap().is_none());
    }
}

fn altered_psk(qr: &str) -> String {
    let mut b = data_encoding::BASE64URL_NOPAD
        .decode(qr.strip_prefix("chatproject:pair/1/").unwrap().as_bytes())
        .unwrap();
    *b.last_mut().unwrap() ^= 1;
    format!(
        "chatproject:pair/1/{}",
        data_encoding::BASE64URL_NOPAD.encode(&b)
    )
}
#[tokio::test]
async fn three_bad_psk_handshakes_destroy_qr_in_both_directions() {
    for direction in [PairDirection::NewDisplays, PairDirection::ExistingDisplays] {
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
        for attempt in 0..3 {
            assert!(scanner.a.pair_scan(altered_psk(&d.qr)).await.is_err());
            assert!(scanner.a.snapshot().await.unwrap().pair.is_none());
            if attempt < 2 {
                assert!(owner.a.pair_view().await.unwrap().is_some());
            }
        }
        assert!(owner.a.pair_view().await.unwrap().is_none());
        assert!(scanner.a.pair_scan(d.qr.clone()).await.is_err());
    }
}
#[tokio::test]
async fn approval_deadline_rejection_and_lifecycle_close_channels() {
    for direction in [PairDirection::NewDisplays, PairDirection::ExistingDisplays] {
        let (e, n, f, _) = connected(direction).await;
        confirmations(&e, &n, f).await;
        approve(&e, f).await;
        e.clock.advance_monotonic_ms(60_000);
        assert!(e.a.pair_fixture_send(f, vec![1; 32]).await.is_err());
        assert!(e.a.pair_view().await.unwrap().is_none());
        let (e, n, f, _) = connected(direction).await;
        n.a.pair_confirm_code(f, false).await.unwrap();
        assert!(n.a.pair_view().await.unwrap().is_none());
        assert!(e.a.pair_fixture_send(f, vec![1; 32]).await.is_err());
        let (e, n, f, _) = connected(direction).await;
        confirmations(&e, &n, f).await;
        approve(&e, f).await;
        e.a.request_suspend();
        assert!(e.a.pair_fixture_send(f, vec![1; 32]).await.is_err());
    }
}
#[tokio::test]
async fn malformed_expired_qr_and_resume_fail_closed_before_candidate_write() {
    let n = Fixture::new(false, Arc::new(Backend::default())).await;
    for qr in [
        "chatproject:pair/1/AA".into(),
        format!("chatproject:pair/1/{}", "A".repeat(2732)),
        "chatproject:identity-invite/1/AA".into(),
    ] {
        assert!(n.a.pair_scan(qr).await.is_err());
        assert_eq!(
            n.a.snapshot().await.unwrap().phase,
            chat_core::account::AccountPhase::Unconfigured
        );
    }
    assert!(
        n.a.pair_begin(
            PairDirection::NewDisplays,
            Some(chat_core::account::TxId([1; 16]))
        )
        .await
        .is_err()
    );
    let e = Fixture::new(true, Arc::new(Backend::default())).await;
    let d =
        e.a.pair_begin(PairDirection::ExistingDisplays, None)
            .await
            .unwrap();
    n.clock.set_unix_seconds(NOW + 300);
    assert!(n.a.pair_scan(d.qr.clone()).await.is_err());
    assert_eq!(
        n.a.snapshot().await.unwrap().phase,
        chat_core::account::AccountPhase::Unconfigured
    );
}
struct Raw {
    conn: iroh::endpoint::Connection,
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
    noise: snow::TransportState,
    _endpoint: iroh::Endpoint,
}
async fn write_raw(s: &mut iroh::endpoint::SendStream, b: &[u8]) -> Option<()> {
    s.write_all(&(b.len() as u32).to_be_bytes()).await.ok()?;
    s.write_all(b).await.ok()
}
async fn read_raw(r: &mut iroh::endpoint::RecvStream) -> Option<Vec<u8>> {
    let mut l = [0; 4];
    r.read_exact(&mut l).await.ok()?;
    let n = u32::from_be_bytes(l) as usize;
    if n > 65535 {
        return None;
    }
    let mut b = vec![0; n];
    r.read_exact(&mut b).await.ok()?;
    Some(b)
}
#[derive(Clone, Copy)]
enum Mutation {
    None,
    BadProof,
    WrongEndpoint,
    WrongRole,
    NewInstallation,
    NewEndpoint,
    OldHash,
}
async fn raw_new(e: &Fixture, qr: &str, mutation: Mutation) -> Option<Raw> {
    use chat_core::account::{InstallationId, derive_device};
    use ed25519_dalek::{Signer, SigningKey};
    use hkdf::Hkdf;
    use sha2::{Digest, Sha256};
    let b = data_encoding::BASE64URL_NOPAD
        .decode(qr.strip_prefix("chatproject:pair/1/").unwrap().as_bytes())
        .unwrap();
    let mut d = minicbor::Decoder::new(&b);
    assert_eq!(d.array().unwrap(), Some(10));
    assert_eq!(d.u8().unwrap(), 1);
    assert_eq!(d.u8().unwrap(), 1);
    let flow = d.bytes().unwrap().to_vec();
    let et = d.bytes().unwrap().to_vec();
    let ei = d.bytes().unwrap().to_vec();
    let ep = d.bytes().unwrap().to_vec();
    d.null().unwrap();
    d.null().unwrap();
    let expiry = d.u64().unwrap();
    let psk = d.bytes().unwrap();
    let device_seed = if matches!(mutation, Mutation::NewEndpoint) {
        92
    } else {
        91
    };
    let installation_seed = if matches!(mutation, Mutation::NewInstallation) {
        92
    } else {
        device_seed
    };
    let device = derive_device(Secret32::from([installation_seed; 32])).unwrap();
    let InstallationId(ni) = device.installation_id();
    let mut transport = [0; 32];
    Hkdf::<Sha256>::new(Some(b"ChatProject/device-keys/v1"), &[device_seed; 32])
        .expand(b"transport-ed25519", &mut transport)
        .unwrap();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&transport))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let NetworkReply::Info(info) = e
        .slot
        .network()
        .unwrap()
        .execute(NetworkOp::Info)
        .await
        .unwrap()
    else {
        panic!()
    };
    let ticket: iroh_tickets::endpoint::EndpointTicket = info.ticket.unwrap().parse().unwrap();
    let conn = endpoint
        .connect(ticket.endpoint_addr().clone(), b"chatproject/pair/1")
        .await
        .unwrap();
    // A peer the session cannot bind is closed on accept, and that close can
    // arrive before this stream opens; it is the same rejection as a closed read.
    let (mut send, mut recv) = conn.open_bi().await.ok()?;
    let mut claimed = *endpoint.id().as_bytes();
    if matches!(mutation, Mutation::WrongEndpoint) {
        claimed[0] ^= 1;
    }
    let role = if matches!(mutation, Mutation::WrongRole) {
        0
    } else {
        1
    };
    let mut enc = minicbor::Encoder::new(Vec::new());
    enc.array(7)
        .unwrap()
        .u8(1)
        .unwrap()
        .bytes(&flow)
        .unwrap()
        .u8(role)
        .unwrap()
        .bytes(&claimed)
        .unwrap()
        .bytes(&ni)
        .unwrap()
        .null()
        .unwrap()
        .null()
        .unwrap();
    write_raw(&mut send, &enc.into_writer()).await?;
    read_raw(&mut recv).await?;
    let mut enc = minicbor::Encoder::new(Vec::new());
    enc.array(10)
        .unwrap()
        .u8(1)
        .unwrap()
        .u8(1)
        .unwrap()
        .bytes(&flow)
        .unwrap()
        .bytes(&ep)
        .unwrap()
        .bytes(&et)
        .unwrap()
        .bytes(&ei)
        .unwrap()
        .bytes(endpoint.id().as_bytes())
        .unwrap()
        .bytes(&ni)
        .unwrap()
        .null()
        .unwrap()
        .u64(expiry)
        .unwrap();
    let prologue = enc.into_writer();
    let mut noise = snow::Builder::new("Noise_NNpsk0_25519_ChaChaPoly_SHA256".parse().unwrap())
        .psk(0, psk.try_into().unwrap())
        .unwrap()
        .prologue(&prologue)
        .unwrap()
        .build_initiator()
        .unwrap();
    let mut out = vec![0; 65535];
    let size = noise.write_message(&[], &mut out).unwrap();
    write_raw(&mut send, &out[..size]).await?;
    noise
        .read_message(&read_raw(&mut recv).await?, &mut out)
        .ok()?;
    let mut hash = noise.get_handshake_hash().to_vec();
    if matches!(mutation, Mutation::OldHash) {
        hash[0] ^= 1;
    }
    let mut noise = noise.into_transport_mode().unwrap();
    let mut enc = minicbor::Encoder::new(Vec::new());
    enc.array(5)
        .unwrap()
        .u8(1)
        .unwrap()
        .bytes(&flow)
        .unwrap()
        .u8(1)
        .unwrap()
        .bytes(&hash)
        .unwrap()
        .bytes(&Sha256::digest(&prologue))
        .unwrap();
    let mut proof = enc.into_writer();
    let mut msg = b"ChatProject/pair-proof/v1".to_vec();
    msg.push(1);
    msg.extend(&proof);
    let mut installation = [0; 32];
    Hkdf::<Sha256>::new(
        Some(b"ChatProject/device-keys/v1"),
        &[installation_seed; 32],
    )
    .expand(b"installation-ed25519", &mut installation)
    .unwrap();
    let mut sig = SigningKey::from_bytes(&installation).sign(&msg).to_bytes();
    if matches!(mutation, Mutation::BadProof) {
        sig[0] ^= 1;
    }
    proof[0] = 0x86;
    minicbor::Encoder::new(&mut proof).bytes(&sig).unwrap();
    let size = noise.write_message(&proof, &mut out).unwrap();
    write_raw(&mut send, &out[..size]).await?;
    noise
        .read_message(&read_raw(&mut recv).await?, &mut out)
        .ok()?;
    let size = noise.write_message(&[0x82,1,0], &mut out).unwrap();
    if send.write_all(&(size as u32).to_be_bytes()).await.is_err()
        || send.write_all(&out[..size]).await.is_err()
    {
        return None;
    }
    noise
        .read_message(&read_raw(&mut recv).await?, &mut out)
        .ok()?;
    Some(Raw {
        conn,
        send,
        recv,
        noise,
        _endpoint: endpoint,
    })
}
async fn wait_closed(e: &Fixture) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if e.a.pair_view().await.unwrap().is_none() {
                break;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn forged_installation_proof_never_reaches_code_comparison() {
    let e = Fixture::new(true, Arc::new(Backend::default())).await;
    let d =
        e.a.pair_begin(PairDirection::ExistingDisplays, None)
            .await
            .unwrap();
    assert!(raw_new(&e, &d.qr, Mutation::BadProof).await.is_none());
    let view = e.a.pair_view().await.unwrap().unwrap();
    assert_eq!(view.remote_installation, None);
    assert_eq!(view.remote_kind, None);
    assert_eq!(
        e.a.pair_view().await.unwrap().unwrap().phase,
        PairPhase::WaitingQr
    );
}
#[tokio::test]
async fn aead_corruption_and_remote_approval_message_close_channel() {
    for forged_approval in [false, true] {
        let e = Fixture::new(true, Arc::new(Backend::default())).await;
        let d =
            e.a.pair_begin(PairDirection::ExistingDisplays, None)
                .await
                .unwrap();
        let mut raw = raw_new(&e, &d.qr, Mutation::None).await.unwrap();
        let mut out = vec![0; 64];
        let len = raw
            .noise
            .write_message(
                if forged_approval {
                    &[0x82, 1, 1]
                } else {
                    &[0x82, 1, 0]
                },
                &mut out,
            )
            .unwrap();
        if !forged_approval {
            out[len - 1] ^= 1;
        }
        write_raw(&mut raw.send, &out[..len]).await.unwrap();
        wait_closed(&e).await;
        let _ = raw.conn.closed().await;
    }
}
#[tokio::test]
async fn oversized_frame_prefix_closes_without_payload_allocation() {
    let e = Fixture::new(true, Arc::new(Backend::default())).await;
    let d =
        e.a.pair_begin(PairDirection::ExistingDisplays, None)
            .await
            .unwrap();
    let mut raw = raw_new(&e, &d.qr, Mutation::None).await.unwrap();
    raw.send.write_all(&65536u32.to_be_bytes()).await.unwrap();
    wait_closed(&e).await;
    assert!(read_raw(&mut raw.recv).await.is_none());
}

#[tokio::test]
async fn actual_endpoint_role_and_old_handshake_proof_are_bound() {
    for mutation in [
        Mutation::WrongEndpoint,
        Mutation::WrongRole,
        Mutation::OldHash,
    ] {
        let e = Fixture::new(true, Arc::new(Backend::default())).await;
        let d =
            e.a.pair_begin(PairDirection::ExistingDisplays, None)
                .await
                .unwrap();
        assert!(raw_new(&e, &d.qr, mutation).await.is_none());
        assert_eq!(
            e.a.pair_view().await.unwrap().unwrap().phase,
            PairPhase::WaitingQr
        );
    }
}
#[tokio::test]
async fn failed_candidate_cannot_be_replaced_but_same_candidate_can_retry() {
    for mutation in [Mutation::NewEndpoint, Mutation::NewInstallation] {
        let e = Fixture::new(true, Arc::new(Backend::default())).await;
        let d =
            e.a.pair_begin(PairDirection::ExistingDisplays, None)
                .await
                .unwrap();
        assert!(raw_new(&e, &d.qr, Mutation::BadProof).await.is_none());
    let view = e.a.pair_view().await.unwrap().unwrap();
    assert_eq!(view.remote_installation, None);
    assert_eq!(view.remote_kind, None);
        assert!(raw_new(&e, &d.qr, mutation).await.is_none());
        let _raw = raw_new(&e, &d.qr, Mutation::None).await.unwrap();
        assert_eq!(
            e.a.pair_view().await.unwrap().unwrap().phase,
            PairPhase::Comparing
        );
    }
}
#[tokio::test]
async fn noncanonical_qr_extra_fields_and_wrong_owner_role_reject_without_write() {
    let e = Fixture::new(true, Arc::new(Backend::default())).await;
    let d =
        e.a.pair_begin(PairDirection::ExistingDisplays, None)
            .await
            .unwrap();
    let good = data_encoding::BASE64URL_NOPAD
        .decode(d.qr.strip_prefix("chatproject:pair/1/").unwrap().as_bytes())
        .unwrap();
    let mut cases = vec![vec![0; 2049], vec![0xbf, 0xff]];
    let mut trailing = good.clone();
    trailing.push(0);
    cases.push(trailing);
    let mut noncanonical = good.clone();
    noncanonical.splice(1..2, [0x18, 1]);
    cases.push(noncanonical);
    let mut role = good.clone();
    role[2] = 0;
    cases.push(role);
    let mut extra = good.clone();
    extra[0] = 0x8b;
    extra.push(0xf6);
    cases.push(extra);
    for bytes in cases {
        let n = Fixture::new(false, Arc::new(Backend::default())).await;
        let qr = format!(
            "chatproject:pair/1/{}",
            data_encoding::BASE64URL_NOPAD.encode(&bytes)
        );
        assert!(n.a.pair_scan(qr).await.is_err());
        assert_eq!(
            n.a.snapshot().await.unwrap().phase,
            chat_core::account::AccountPhase::Unconfigured
        );
    }
}

#[tokio::test]
async fn cached_existing_record_cannot_replace_fresh_pair_authority() {
    let backend = Arc::new(Backend::default());
    let e = Fixture::new(true, backend.clone()).await;
    backend
        .offline
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        e.a.pair_begin(PairDirection::ExistingDisplays, None)
            .await
            .is_err()
    );
    assert!(e.a.pair_view().await.unwrap().is_none());
    backend
        .offline
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let d =
        e.a.pair_begin(PairDirection::ExistingDisplays, None)
            .await
            .unwrap();
    let n = Fixture::new(false, Arc::new(Backend::default())).await;
    n.route_to(&e).await;
    assert!(n.a.pair_scan(d.qr.clone()).await.is_err());
    assert_eq!(
        n.a.snapshot().await.unwrap().phase,
        chat_core::account::AccountPhase::Candidate
    );
    assert!(n.a.snapshot().await.unwrap().public_peer_id.is_none());
}
#[tokio::test]
async fn replayed_noise_frame_is_rejected_and_new_qr_is_independent() {
    let e = Fixture::new(true, Arc::new(Backend::default())).await;
    let d =
        e.a.pair_begin(PairDirection::ExistingDisplays, None)
            .await
            .unwrap();
    let mut raw = raw_new(&e, &d.qr, Mutation::None).await.unwrap();
    let mut out = vec![0; 32];
    let len = raw.noise.write_message(&[0x82, 1, 0], &mut out).unwrap();
    write_raw(&mut raw.send, &out[..len]).await.unwrap();
    write_raw(&mut raw.send, &out[..len]).await.unwrap();
    wait_closed(&e).await;
    let fresh =
        e.a.pair_begin(PairDirection::ExistingDisplays, None)
            .await
            .unwrap();
    assert_ne!(fresh.flow, d.flow);
    let waiting = e.a.pair_view().await.unwrap().unwrap();
    assert_eq!(waiting.remote_installation, None);
    assert_eq!(waiting.remote_kind, None);
    assert!(raw_new(&e, &d.qr, Mutation::None).await.is_none());
    assert!(e.a.pair_confirm_code(d.flow, true).await.is_err());
}

#[tokio::test]
async fn qr_requires_durable_candidate_and_successful_endpoint_bind() {
    let n = Fixture::new(false, Arc::new(Backend::default())).await;
    n.slot.network().unwrap().fail_next_bind_for_test();
    assert!(
        n.a.pair_begin(PairDirection::NewDisplays, None)
            .await
            .is_err()
    );
    assert_eq!(
        n.a.snapshot().await.unwrap().phase,
        chat_core::account::AccountPhase::Candidate
    );
    assert!(n.a.pair_view().await.unwrap().is_none());
    let before = n.a.snapshot().await.unwrap().installation;
    let display =
        n.a.pair_begin(PairDirection::NewDisplays, None)
            .await
            .unwrap();
    assert_eq!(n.a.snapshot().await.unwrap().installation, before);
    assert_eq!(n.a.snapshot().await.unwrap().pair.map(|p| p.flow), Some(display.flow));
}
#[tokio::test]
async fn restart_keeps_candidate_but_discards_flow_psk_and_approval() {
    let n = Fixture::new(false, Arc::new(Backend::default())).await;
    let display =
        n.a.pair_begin(PairDirection::NewDisplays, None)
            .await
            .unwrap();
    let installation = n.a.snapshot().await.unwrap().installation;
    n.slot
        .network()
        .unwrap()
        .execute(NetworkOp::Stop)
        .await
        .unwrap();
    let storage = n.a.storage_test_fixture();
    let observer = n.a.storage_test_fixture();
    let Fixture {
        _dir: dir,
        slot,
        a,
        clock: _,
    } = n;
    drop(a);
    drop(slot);
    storage.close().await;
    assert_eq!(observer.live_connections(), 0);
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
            Arc::new(Protector),
            AccountServices::new(
                Arc::new(Backend::default()),
                Arc::new(FakeClock::new(NOW, 0)),
            ),
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
    assert_eq!(a.snapshot().await.unwrap().installation, installation);
    assert!(a.pair_view().await.unwrap().is_none());
    assert!(a.pair_confirm_code(display.flow, true).await.is_err());
    let fresh = a
        .pair_begin(PairDirection::NewDisplays, None)
        .await
        .unwrap();
    assert_ne!(display.flow, fresh.flow);
    assert_ne!(display.qr, fresh.qr);
}

#[tokio::test]
async fn existing_display_waits_for_rebound_endpoint_before_returning_qr() {
    let e = Fixture::new(true, Arc::new(Backend::default())).await;
    let network = e.slot.network().unwrap();
    network.execute(NetworkOp::Stop).await.unwrap();
    network.fail_next_bind_for_test();
    assert!(
        e.a.pair_begin(PairDirection::ExistingDisplays, None)
            .await
            .is_err()
    );
    assert!(e.a.pair_view().await.unwrap().is_none());
    e.a.pair_begin(PairDirection::ExistingDisplays, None)
        .await
        .unwrap();
    let NetworkReply::Info(info) = network.execute(NetworkOp::Info).await.unwrap() else {
        panic!()
    };
    assert_eq!(info.state, chat_core::NetworkState::Online);
    assert!(info.ticket.is_some());
}

/// Real lookup await boundary with a deterministic wall-clock change. The packet
/// remains an independently signed public record; no ready/grant state is faked.
struct ClockJumpBackend {
    peer: PeerId,
    packet: pkarr::SignedPacket,
    clock: Arc<FakeClock>,
    after_lookup: u64,
}
impl RecordBackend for ClockJumpBackend {
    fn lookup(&self, peer: PeerId) -> RecordFuture<'_, Lookup> {
        Box::pin(async move {
            assert_eq!(peer, self.peer);
            tokio::task::yield_now().await;
            self.clock.set_unix_seconds(self.after_lookup);
            Ok(Lookup::Found(self.packet.clone()))
        })
    }
    fn publish<'a>(&'a self, _: &'a pkarr::SignedPacket) -> RecordFuture<'a, ()> {
        Box::pin(async { panic!("candidate pairing must not publish") })
    }
}
async fn authority_time_boundary(
    issued_offset: i64,
    expiry_offset: i64,
    sequence_offset: i64,
    after_lookup_offset: i64,
    should_accept: bool,
) {
    use chat_core::account::{
        IdentityRecord, clock::Clock, derive_account, encode_recovery, record_packet,
    };
    let setup_time = chat_core::account::clock::SystemClock::default().unix_seconds();
    let e = Fixture::with_clock(
        false,
        Arc::new(Backend::default()),
        Arc::new(FakeClock::new(setup_time, 0)),
    )
    .await;
    // Test-owned fixed entropy lets this fixture sign a bounded public record
    // with selected timestamps without reading an account's private state.
    let entropy = Secret32::from([42; 32]);
    let keys = derive_account(&entropy).unwrap();
    e.a.recover(encode_recovery(&entropy).unwrap(), true)
        .await
        .unwrap();
    let snapshot = e.a.snapshot().await.unwrap();
    assert_eq!(snapshot.public_peer_id, Some(keys.peer_id()));
    // Account creation signs its certificate using SystemClock. Anchor only the
    // fixture epoch to that public timestamp; all tested changes use FakeClock.
    let invite = e.a.create_invite(None).await.unwrap();
    let now = chat_core::account::parse_invite(&invite, setup_time).unwrap().issuing_cert.created_at;
    e.clock.set_unix_seconds(now);
    e.a.refresh_record(keys.peer_id()).await.unwrap();
    let base = e.a.snapshot().await.unwrap().publication.accepted.unwrap();
    let issued = now.checked_add_signed(issued_offset).unwrap();
    let expires = now.checked_add_signed(expiry_offset).unwrap();
    let sequence_seconds = now.checked_add_signed(sequence_offset).unwrap();
    let after_lookup = now.checked_add_signed(after_lookup_offset).unwrap();
    let packet = record_packet(
        &keys,
        &IdentityRecord {
            issued_at: issued,
            expires_at: expires,
            ..base
        },
        sequence_seconds * 1_000_000,
    )
    .unwrap();
    let clock = Arc::new(FakeClock::new(now, 0));
    let n = Fixture::with_clock(
        false,
        Arc::new(ClockJumpBackend {
            peer: keys.peer_id(),
            packet,
            clock: clock.clone(),
            after_lookup,
        }),
        clock.clone(),
    )
    .await;
    let display =
        e.a.pair_begin(PairDirection::ExistingDisplays, None)
            .await
            .unwrap();
    n.route_to(&e).await;
    let result = n.a.pair_scan(display.qr.clone()).await;
    assert_eq!(clock.unix_seconds(), after_lookup);
    assert!(
        clock.unix_seconds() < display.expires_at,
        "QR must remain live"
    );
    assert_eq!(
        clock.monotonic_ms(),
        0,
        "total pairing deadline must remain live"
    );
    if should_accept {
        result.unwrap();
        assert_eq!(
            n.a.pair_view().await.unwrap().unwrap().phase,
            PairPhase::Comparing
        );
    } else {
        assert!(
            result.is_err(),
            "N accepted authority invalid at lookup completion"
        );
        assert!(
            n.a.pair_view()
                .await
                .unwrap()
                .is_none_or(|v| v.phase != PairPhase::Comparing)
        );
    }
}
#[tokio::test]
async fn authority_time_expiry_during_lookup_rejects_while_qr_live() {
    authority_time_boundary(0, 5, 0, 6, false).await;
}
#[tokio::test]
async fn authority_time_rollback_rechecks_certificate_creation() {
    // Record times remain acceptable; only E's certificate is now too far ahead.
    authority_time_boundary(-500, 100, -500, -301, false).await;
}
#[tokio::test]
async fn authority_time_rollback_rechecks_record_issue_time() {
    // Certificate and sequence remain acceptable; record issuance exceeds +300s.
    authority_time_boundary(100, 200, 0, -201, false).await;
}
#[tokio::test]
async fn authority_time_rollback_rechecks_record_sequence() {
    // Certificate and issuance remain acceptable; sequence exceeds +300s.
    authority_time_boundary(0, 200, 100, -201, false).await;
}
#[tokio::test]
async fn authority_time_fresh_after_lookup_still_compares() {
    authority_time_boundary(0, 100, 0, 1, true).await;
}
