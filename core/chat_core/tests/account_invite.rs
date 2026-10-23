use chat_core::account::{
    DeviceKind, Secret32, derive_account, derive_device, parse_invite, safety_digest,
    safety_display, sign_cert,
};
use ed25519_dalek::{Signer, SigningKey};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
const NOW: u64 = 1_800_000_000;
fn invite(name: Option<&str>, issued: u64, expires: u64, relay: Option<&str>) -> String {
    let keys = derive_account(&Secret32::from([7; 32])).unwrap();
    let device = derive_device(Secret32::from([8; 32])).unwrap();
    let cert = sign_cert(&keys, &device, DeviceKind::Desktop, NOW).unwrap();
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(8)
        .unwrap()
        .u8(1)
        .unwrap()
        .bytes(&keys.peer_id().0)
        .unwrap();
    let mut body = e.into_writer();
    body.extend(cert.to_bytes());
    let mut e = minicbor::Encoder::new(body);
    e.array(2)
        .unwrap()
        .bytes(&device.transport_public_key())
        .unwrap();
    if let Some(v) = relay {
        e.str(v).unwrap();
    } else {
        e.null().unwrap();
    }
    e.bytes(&[9; 16]).unwrap();
    if let Some(v) = name {
        e.str(v).unwrap();
    } else {
        e.null().unwrap();
    }
    e.u64(issued).unwrap().u64(expires).unwrap();
    let mut body = e.into_writer();
    let mut message = b"ChatProject/identity-invite/v1".to_vec();
    message.push(1);
    message.extend(&body);
    let mut seed = [0; 32];
    Hkdf::<Sha256>::new(Some(b"ChatProject/account-keys/v1"), &[7; 32])
        .expand(b"identity-ed25519", &mut seed)
        .unwrap();
    let sig = SigningKey::from_bytes(&seed).sign(&message).to_bytes();
    body[0] = 0x89;
    minicbor::Encoder::new(&mut body).bytes(&sig).unwrap();
    format!(
        "chatproject:identity-invite/1/{}",
        data_encoding::BASE64URL_NOPAD.encode(&body)
    )
}
#[test]
fn name_64_bytes_accept_65_reject() {
    assert!(parse_invite(&invite(Some(&"가".repeat(21)), NOW, NOW + 86400, None), NOW).is_ok());
    assert!(parse_invite(&invite(Some(&"a".repeat(64)), NOW, NOW + 86400, None), NOW).is_ok());
    assert!(parse_invite(&invite(Some(&"a".repeat(65)), NOW, NOW + 86400, None), NOW).is_err());
    assert!(parse_invite(&invite(Some(&"가".repeat(22)), NOW, NOW + 86400, None), NOW).is_err());
}

#[test]
fn public_invite_errors_distinguish_verified_expiry_from_invalid() {
    let expired = invite(None, NOW - 86400, NOW, None);
    assert_eq!(parse_invite(&expired, NOW).unwrap_err().code, chat_core::ErrorCode::InviteExpired);
    assert_eq!(parse_invite("malformed", NOW).unwrap_err().code, chat_core::ErrorCode::InvalidInvite);
    let mut bytes = data_encoding::BASE64URL_NOPAD.decode(expired.rsplit('/').next().unwrap().as_bytes()).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    let forged = format!("chatproject:identity-invite/1/{}", data_encoding::BASE64URL_NOPAD.encode(&bytes));
    assert_eq!(parse_invite(&forged, NOW).unwrap_err().code, chat_core::ErrorCode::InvalidInvite);
}
#[test]
fn unicode_controls_bidi_not_trusted_name() {
    for name in ["a\nb", "a\0b", "a\u{0085}b"] {
        assert!(parse_invite(&invite(Some(name), NOW, NOW + 86400, None), NOW).is_err());
    }
    let p = parse_invite(
        &invite(Some("claimed\u{202e}name"), NOW, NOW + 86400, None),
        NOW,
    )
    .unwrap();
    assert_eq!(p.display_name.as_deref(), Some("claimed\u{202e}name"));
}
#[test]
fn unknown_scheme_no_network_or_storage() {
    for input in [
        "https://example.org/",
        "chatproject:identity-invite/2/AA",
        "chatproject:pair/1/AA",
    ] {
        assert!(parse_invite(input, NOW).is_err());
    }
}
#[test]
fn expiry_future_and_all_fields_signed() {
    for (issued, expires) in [
        (NOW + 301, NOW + 86400),
        (NOW, NOW),
        (NOW + 1, NOW),
        (NOW, NOW + 604801),
        (NOW - 86400, NOW),
    ] {
        assert!(parse_invite(&invite(None, issued, expires, None), NOW).is_err());
    }
    assert!(parse_invite(&invite(None, NOW + 300, NOW + 86400, None), NOW).is_ok());
    let original = invite(Some("name"), NOW, NOW + 86400, None);
    let bytes = data_encoding::BASE64URL_NOPAD
        .decode(original.rsplit('/').next().unwrap().as_bytes())
        .unwrap();
    for i in 0..bytes.len() {
        let mut bad = bytes.clone();
        bad[i] ^= 1;
        let text = format!(
            "chatproject:identity-invite/1/{}",
            data_encoding::BASE64URL_NOPAD.encode(&bad)
        );
        assert!(parse_invite(&text, NOW).is_err(), "unsigned byte {i}");
    }
}
#[test]
fn malformed_canonical_size_and_relay() {
    for relay in [
        "http://relay.example/",
        "https://127.0.0.1/",
        "https://[::1]/",
        "https://user:pass@relay.example/",
        "https://relay.example/#frag",
    ] {
        assert!(
            parse_invite(&invite(None, NOW, NOW + 86400, Some(relay)), NOW).is_err(),
            "{relay}"
        );
    }
    assert!(
        parse_invite(
            &invite(None, NOW, NOW + 86400, Some("https://relay.example/")),
            NOW
        )
        .is_ok()
    );
    assert!(
        parse_invite(
            &invite(
                None,
                NOW,
                NOW + 86400,
                Some(&format!("https://relay.example/{}", "x".repeat(256)))
            ),
            NOW
        )
        .is_err()
    );
    let valid = invite(None, NOW, NOW + 86400, None);
    let bytes = data_encoding::BASE64URL_NOPAD
        .decode(valid.rsplit('/').next().unwrap().as_bytes())
        .unwrap();
    for bad in [
        vec![0; 4097],
        {
            let mut b = bytes.clone();
            b.push(0);
            b
        },
        {
            let mut b = bytes.clone();
            b.splice(1..2, [0x18, 1]);
            b
        },
        {
            let mut b = bytes.clone();
            b[0] = 0x9f;
            b.push(0xff);
            b
        },
    ] {
        assert!(
            parse_invite(
                &format!(
                    "chatproject:identity-invite/1/{}",
                    data_encoding::BASE64URL_NOPAD.encode(&bad)
                ),
                NOW
            )
            .is_err()
        );
    }
}
#[test]
fn symmetric_full_digest_and_same_peer_recovery() {
    let a = derive_account(&Secret32::from([1; 32])).unwrap().peer_id();
    let b = derive_account(&Secret32::from([7; 32])).unwrap().peer_id();
    assert_eq!(safety_digest(a, b), safety_digest(b, a));
    let mut h = Sha256::new();
    h.update(b"ChatProject/safety/v1");
    h.update(a.min(b).0);
    h.update(a.max(b).0);
    let expected: [u8; 32] = h.finalize().into();
    assert_eq!(safety_digest(a, b), expected);
    let display = safety_display(a, b);
    assert_eq!(display.digest, expected);
    assert_eq!(display.groups.split(' ').count(), 8);
    assert_eq!(display.groups.replace(' ', "").len(), 64);
    assert_eq!(
        display.groups.replace(' ', ""),
        data_encoding::HEXLOWER.encode(&expected)
    );
    assert_eq!(display, safety_display(b, a));
    let recovered = derive_account(&Secret32::from([7; 32])).unwrap().peer_id();
    assert_eq!(safety_digest(a, recovered), expected);
    let qr = data_encoding::BASE64URL_NOPAD
        .decode(display.qr.rsplit('/').next().unwrap().as_bytes())
        .unwrap();
    let mut d = minicbor::Decoder::new(&qr);
    assert_eq!(d.array().unwrap(), Some(4));
    assert_eq!(d.u8().unwrap(), 1);
    assert_eq!(d.bytes().unwrap(), a.min(b).0);
    assert_eq!(d.bytes().unwrap(), a.max(b).0);
    assert_eq!(d.bytes().unwrap(), expected);
    assert_eq!(d.position(), qr.len());
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
use chat_core::account::{
    StoreId,
    storage::{KeyProtector, StoredPlaintext},
};
#[cfg(all(feature = "test-hooks", debug_assertions))]
use chat_core::{CoreError, ErrorCode};
#[cfg(all(feature = "test-hooks", debug_assertions))]
#[allow(dead_code)]
#[path = "support/account_store.rs"]
mod support;
#[cfg(all(feature = "test-hooks", debug_assertions))]
mod actor {
    use super::*;
    use chat_core::account::{
        AccountHandle, AccountServices, Lookup, PeerId, RecordBackend, RecordFuture,
        clock::FakeClock,
    };
    use chat_core::{CoreConfig, CoreSlot, ErrorCode, EventBus};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Backend(AtomicUsize);
    impl RecordBackend for Backend {
        fn lookup(&self, _: PeerId) -> RecordFuture<'_, Lookup> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(chat_core::CoreError::new(
                    ErrorCode::RecordUnavailable,
                    "offline",
                ))
            })
        }
        fn publish<'a>(&'a self, _: &'a pkarr::SignedPacket) -> RecordFuture<'a, ()> {
            Box::pin(async { panic!("no publication") })
        }
    }
    async fn open(path: &std::path::Path) -> (CoreSlot, AccountHandle, Arc<Backend>) {
        let slot = CoreSlot::new();
        slot.start(
            CoreConfig {
                data_dir: path.into(),
            },
            &EventBus::new(),
        )
        .unwrap();
        slot.network().unwrap().configure_local(None).await.unwrap();
        let backend = Arc::new(Backend(AtomicUsize::new(0)));
        let account = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match slot.open_account_with_services(
                    support::protector(),
                    AccountServices::new(backend.clone(), Arc::new(FakeClock::new(NOW, 0))),
                ) {
                    Ok(a) => break a,
                    Err(e) if e.code == ErrorCode::WriterBusy => tokio::task::yield_now().await,
                    Err(e) => panic!("{e}"),
                }
            }
        })
        .await
        .unwrap();
        settled(&account).await;
        (slot, account, backend)
    }
    async fn settled(account: &AccountHandle) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while account.lifecycle_status() == 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(account.lifecycle_status(), 0);
    }
    async fn setup(a: &AccountHandle) {
        a.recover(
            chat_core::account::encode_recovery(&Secret32::from([1; 32])).unwrap(),
            true,
        )
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn duplicate_keeps_alias_and_verified() {
        let dir = tempfile::tempdir().unwrap();
        let (slot, a, backend) = open(dir.path()).await;
        setup(&a).await;
        let input = invite(Some("remote name"), NOW, NOW + 86400, None);
        let before = a.storage_test_fixture().handle.load_account().await.unwrap().unwrap().payload;
        let calls = backend.0.load(Ordering::SeqCst);
        assert!(parse_invite("https://bad/", NOW).is_err());
        assert!(
            a.accept_invite("https://bad/".into(), "alias".into())
                .await
                .is_err()
        );
        assert!(a.storage_test_fixture().handle.load_account().await.unwrap().unwrap().payload == before);
        assert_eq!(backend.0.load(Ordering::SeqCst), calls);
        let contact = a
            .accept_invite(input.clone(), "local alias".into())
            .await
            .unwrap();
        assert!(!contact.verified);
        assert!(contact.device_unverified);
        assert!(!contact.new_device);
        let own = a.snapshot().await.unwrap().public_peer_id.unwrap();
        let digest = safety_digest(own, contact.peer_id);
        assert!(a.confirm_safety(contact.peer_id, [0; 32]).await.is_err());
        a.confirm_safety(contact.peer_id, digest).await.unwrap();
        let duplicate = a
            .accept_invite(input.clone(), "replacement".into())
            .await
            .unwrap();
        assert_eq!(duplicate.alias, "local alias");
        assert!(duplicate.verified);
        assert_eq!(a.snapshot().await.unwrap().contacts, vec![chat_core::account::ContactView { verified: true, ..contact.clone() }]);
        assert_eq!(a.contacts().await.unwrap(), vec![duplicate.clone()]);
        for alias in ["x".repeat(65), "a\nb".into(), "a\u{0085}b".into()] {
            assert!(a.accept_invite(input.clone(), alias).await.is_err());
        }
        assert!(
            a.accept_invite(invite(None, NOW - 86400, NOW, None), "invalid".into())
                .await
                .is_err()
        );
        a.request_suspend();
        settled(&a).await;
        a.request_resume();
        settled(&a).await;
        assert_eq!(a.contacts().await.unwrap(), vec![duplicate]);
        let own_invite = a.create_invite(Some("가".repeat(21))).await.unwrap();
        let preview = parse_invite(&own_invite, NOW).unwrap();
        assert_eq!(preview.peer_id, own);
        assert_eq!(preview.expires_at, NOW + 86400);
        assert!(a.create_invite(Some("a".repeat(65))).await.is_err());
        assert!(a.accept_invite(own_invite, "self".into()).await.is_err());
        assert_eq!(a.contacts().await.unwrap().len(), 1);
        let expected = a.contacts().await.unwrap();
        slot.network()
            .unwrap()
            .execute(chat_core::NetworkOp::Stop)
            .await
            .unwrap();
        drop(a);
        drop(slot);
        let (_slot, reopened, _) = open(dir.path()).await;
        assert_eq!(reopened.contacts().await.unwrap(), expected);
    }
    #[tokio::test]
    async fn contact_capacity_rejects_without_eviction_or_store_change() {
        use crate::sql_store::Store;
        let dir = tempfile::tempdir().unwrap();
        let (slot, a, _) = open(dir.path()).await;
        setup(&a).await;
        slot.network()
            .unwrap()
            .execute(chat_core::NetworkOp::Stop)
            .await
            .unwrap();
        drop(a);
        drop(slot);
        let mut store = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match Store::open(dir.path(), support::protector()) {
                    Ok(s) => break s,
                    Err(e) if e.code == ErrorCode::WriterBusy => tokio::task::yield_now().await,
                    Err(e) => panic!("{e}"),
                }
            }
        })
        .await
        .unwrap();
        let mut state = store.load().unwrap().unwrap();
        let mut d = minicbor::Decoder::new(&state.payload);
        assert_eq!(d.array().unwrap(), Some(11));
        for _ in 0..9 {
            d.skip().unwrap();
        }
        let end = d.position();
        d.skip().unwrap();
        let pair_state = d.bytes().unwrap().to_vec();
        let mut e = minicbor::Encoder::new(Vec::new());
        e.array(1000).unwrap();
        for n in 0u16..1000 {
            let mut peer = [0; 32];
            peer[..2].copy_from_slice(&n.to_be_bytes());
            e.array(6)
                .unwrap()
                .bytes(&peer)
                .unwrap()
                .str("local")
                .unwrap()
                .u8(0)
                .unwrap()
                .null()
                .unwrap()
                .null()
                .unwrap()
                .u8(0)
                .unwrap();
        }
        state.payload.truncate(end);
        minicbor::Encoder::new(&mut *state.payload)
            .bytes(&e.into_writer())
            .unwrap()
            .bytes(&pair_state)
            .unwrap();
        store.commit(&state).unwrap();
        drop(store);
        let (_slot, a, _) = open(dir.path()).await;
        assert_eq!(a.contacts().await.unwrap().len(), 1000);
        assert_eq!(a.snapshot().await.unwrap().contacts.len(), 1000);
        let before = a.storage_test_fixture().handle.load_account().await.unwrap().unwrap().payload;
        assert_eq!(
            a.accept_invite(invite(None, NOW, NOW + 86400, None), "new".into())
                .await
                .unwrap_err()
                .code,
            ErrorCode::StorageFull
        );
        assert_eq!(a.contacts().await.unwrap().len(), 1000);
        assert!(a.storage_test_fixture().handle.load_account().await.unwrap().unwrap().payload == before);
    }
}

#[path = "support/sql_account_store.rs"]
mod sql_store;
