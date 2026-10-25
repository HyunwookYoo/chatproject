use super::*;
use crate::account::{
    AccountKeys, DeviceKind, Generation, IdentityRecord, Lookup, RecordBackend, RecordDevice,
    RecordFuture, RevocationNotifier, RevokeIntent, Secret32, StoreId,
    clock::FakeClock,
    connection::RemoteRecord,
    publication::{PacketIntent, PacketOperation, SignedRecord},
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize},
};

const NOW: u64 = 1000;
const ACCEPTED_SEQ: u64 = 1_000_000_000;
const OLD_SEQ: u64 = 900_000_000;

struct Protector;
impl KeyProtector for Protector {
    fn protect(&self, _: StoreId, secret: &Secret32) -> Result<Vec<u8>, CoreError> {
        Ok(secret.as_bytes().iter().map(|b| b ^ 0xa7).collect())
    }
    fn unprotect(&self, _: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
        Ok(Secret32::from(std::array::from_fn(|i| blob[i] ^ 0xa7)))
    }
    fn delete(&self, _: StoreId) -> Result<(), CoreError> {
        Ok(())
    }
}

struct Backend {
    lookup: Mutex<Option<pkarr::SignedPacket>>,
    published: AtomicUsize,
    notified: AtomicUsize,
    clock_after_lookup: Mutex<Option<(Arc<FakeClock>, u64)>>,
    unavailable: AtomicBool,
}
impl RecordBackend for Backend {
    fn lookup(&self, _: PeerId) -> RecordFuture<'_, Lookup> {
        Box::pin(async {
            tokio::task::yield_now().await;
            if let Some((clock, time)) = self.clock_after_lookup.lock().unwrap().take() {
                clock.set_unix_seconds(time);
            }
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(unavailable(ErrorCode::RecordUnavailable));
            }
            Ok(self
                .lookup
                .lock()
                .unwrap()
                .clone()
                .map_or(Lookup::CompletedAbsent, Lookup::Found))
        })
    }
    fn publish<'a>(&'a self, _: &'a pkarr::SignedPacket) -> RecordFuture<'a, ()> {
        Box::pin(async {
            self.published.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
impl RevocationNotifier for Backend {
    fn notify<'a>(&'a self, _: &'a RevokeIntent) -> RecordFuture<'a, ()> {
        Box::pin(async {
            self.notified.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
#[derive(Default)]
struct Events(Mutex<Vec<crate::CoreEvent>>);
impl crate::EventSink for Events {
    fn send(&self, event: crate::CoreEvent) -> bool {
        self.0.lock().unwrap().push(event);
        true
    }
}

fn signed(keys: &AccountKeys, record: &IdentityRecord, seq: u64) -> SignedRecord {
    SignedRecord::new(
        crate::account::record_packet(keys, record, seq).unwrap(),
        keys.peer_id(),
    )
    .unwrap()
}
fn local_fixture(seed: u8) -> LocalAccount {
    let mut local = crate::account::recovery::local_account(
        crate::account::derive_account(&Secret32::from([seed; 32])).unwrap(),
        PublishIntent::LocalCreate,
        1,
    )
    .unwrap();
    let record = IdentityRecord {
        generation: Generation([seed; 16]),
        issued_at: 900,
        expires_at: 2000,
        devices: vec![RecordDevice {
            installation_id: local.device.installation_id(),
            transport_public_key: local.device.transport_public_key(),
            kind: DeviceKind::Desktop,
        }],
    };
    let accepted = signed(&local.keys, &record, ACCEPTED_SEQ);
    let unrelated = crate::account::derive_device(Secret32::from([seed + 20; 32])).unwrap();
    local.records = RecordState {
        generation: record.generation,
        accepted: Some(accepted.clone()),
        conflict: None,
        observed_seq: ACCEPTED_SEQ,
        allocated_seq: ACCEPTED_SEQ + 20,
        tombstones: vec![
            RevokeIntent::sign(
                &local.keys,
                record.generation,
                accepted.hash,
                &RecordDevice {
                    installation_id: unrelated.installation_id(),
                    transport_public_key: unrelated.transport_public_key(),
                    kind: DeviceKind::Desktop,
                },
                NOW,
            )
            .unwrap(),
        ],
        pending: vec![],
        state: PublicationState::Observed,
    };
    local
}

struct Harness {
    manager: Manager,
    backend: Arc<Backend>,
    clock: Arc<FakeClock>,
    events: Arc<Events>,
    _dir: tempfile::TempDir,
}
impl Harness {
    async fn new(local: LocalAccount, packet: Option<pkarr::SignedPacket>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        let (store, _) = tokio::task::spawn_blocking(move || {
            StorageHandle::open_blocking(root, Arc::new(Protector))
        })
        .await
        .unwrap()
        .unwrap();
        let bus = crate::EventBus::new();
        let events = Arc::new(Events::default());
        bus.subscribe(events.clone());
        // No endpoint is bound: these tests exercise the account/storage boundary only.
        let network = NetworkHandle::spawn(
            &tokio::runtime::Handle::current(),
            crate::transport::NetworkProfile::Public,
            bus.clone(),
        );
        let ticket = network.begin_account();
        store.invalidate(ticket);
        let request = Request {
            ticket,
            suspend: false,
        };
        let (requests, _) = watch::channel(request);
        let shared = Arc::new(Shared {
            store: store.clone(),
            network,
            requests,
            status: AtomicU64::new((ticket << 2) | 1),
        });
        let backend = Arc::new(Backend {
            lookup: Mutex::new(packet),
            published: AtomicUsize::new(0),
            notified: AtomicUsize::new(0),
            clock_after_lookup: Mutex::new(None),
            unavailable: AtomicBool::new(false),
        });
        let clock = Arc::new(FakeClock::new(NOW, 0));
        let state = AccountState::Active(Box::new(local));
        store.resume().await.unwrap();
        store.commit_account(state.encode()).await.unwrap();
        let manager = Manager {
            messages: Default::default(),
            pair: Default::default(),
            auth: Default::default(),
            bus,
            store,
            state: Some(state),
            creation: None,
            cleanup_pending: false,
            revision: 1,
            public_revision: 0,
            last_public_view: None,
            lock_error: None,
            shared,
            services: AccountServices {
                backend: Some(backend.clone()),
                clock: clock.clone(),
                notifier: Some(backend.clone()),
            },
        };
        Self {
            manager,
            backend,
            clock,
            events,
            _dir: dir,
        }
    }
    fn bytes(&self) -> Vec<u8> {
        self.manager
            .state
            .as_ref()
            .unwrap()
            .encode()
            .payload
            .to_vec()
    }
    fn no_effects(&self) {
        assert_eq!(self.backend.published.load(Ordering::SeqCst), 0);
        assert_eq!(self.backend.notified.load(Ordering::SeqCst), 0);
    }
    async fn reload(&mut self) {
        self.manager.state = None;
        self.manager.store.suspend().await.unwrap();
        self.manager.store.resume().await.unwrap();
        let stored = self.manager.store.load_account().await.unwrap().unwrap();
        self.manager.state = Some(AccountState::decode(&stored).unwrap());
    }
}

#[tokio::test]
async fn stale_lookup_own_unavailable_preserves_durable_authority_and_tombstone() {
    let local = local_fixture(1);
    let old = signed(
        &local.keys,
        &local.records.accepted.as_ref().unwrap().record,
        OLD_SEQ,
    );
    let mut h = Harness::new(local, Some(old.packet.clone())).await;
    let before = h.bytes();
    assert!(h.manager.local_authorized());
    assert_eq!(
        h.manager
            .publication(Action::Refresh)
            .await
            .unwrap_err()
            .code,
        ErrorCode::RecordUnavailable
    );
    assert_eq!(h.bytes(), before);
    assert_eq!(h.manager.revision, 1);
    assert!(h.manager.local_authorized());
    h.no_effects();
    h.reload().await;
    assert_eq!(h.bytes(), before);
    assert!(h.manager.local_authorized());
    assert_eq!(h.manager.local().unwrap().records.tombstones.len(), 1);
    // A privileged operation also requires a fresh lookup base.
    let target = h.manager.local().unwrap().records.tombstones[0].target_installation;
    assert_eq!(
        h.manager
            .publication(Action::Revoke(target))
            .await
            .unwrap_err()
            .code,
        ErrorCode::RecordUnavailable
    );
    assert_eq!(h.bytes(), before);
    h.no_effects();
}

#[tokio::test]
async fn stale_lookup_remote_uses_cached_provenance_without_installing_grant() {
    let mut local = local_fixture(1);
    let remote = local_fixture(2);
    let accepted = remote.records.accepted.as_ref().unwrap().clone();
    let old = signed(&remote.keys, &accepted.record, OLD_SEQ);
    let peer = remote.keys.peer_id();
    local.remote.push(RemoteRecord {
        peer,
        accepted: accepted.clone(),
        floor: ACCEPTED_SEQ,
        conflict: None,
    });
    let mut h = Harness::new(local, Some(old.packet.clone())).await;
    let before = h.bytes();
    let result = h.manager.remote_record(peer).await.unwrap();
    assert_eq!(result.hash, accepted.hash);
    assert_eq!(h.bytes(), before);
    assert!(h.manager.auth.cached_lookups.contains(&peer));
    assert!(
        h.manager.auth.registry.0.lock().unwrap().is_empty(),
        "lookup cannot install fresh-proof authority"
    );
    assert!(h.events.0.lock().unwrap().iter().any(|e| matches!(e, crate::CoreEvent::AccountCacheUsed { peer_id } if peer_id == &peer.to_string())));
    h.no_effects();
    h.reload().await;
    assert_eq!(h.bytes(), before);
    // The shared observer used by authenticated control still rejects this packet.
    let request = h.manager.ready().unwrap();
    assert_eq!(
        h.manager
            .observe_remote(peer, old, request)
            .await
            .unwrap_err()
            .code,
        ErrorCode::RecordConflict
    );
    let saved = &h.manager.local().unwrap().remote[0];
    assert_eq!(saved.floor, ACCEPTED_SEQ);
    assert!(saved.conflict.is_some());
}

#[tokio::test]
async fn stale_lookup_strict_candidate_matrix_preserves_conflict_paths() {
    // Every row must retain evidence rather than use the accepted cache.
    for case in [
        "equal",
        "generation",
        "membership",
        "future",
        "expired",
        "above_accepted_below_floor",
    ] {
        let mut local = local_fixture(1);
        let mut record = local.records.accepted.as_ref().unwrap().record.clone();
        let mut seq = OLD_SEQ;
        match case {
            "equal" => {
                seq = ACCEPTED_SEQ;
                record.issued_at = 901;
            }
            "generation" => record.generation = Generation([9; 16]),
            "membership" => record.devices[0].kind = DeviceKind::Mobile,
            "future" => record.issued_at = 1400,
            "expired" => record.expires_at = NOW,
            "above_accepted_below_floor" => {
                seq = ACCEPTED_SEQ + 1;
                local.records.observed_seq = ACCEPTED_SEQ + 2;
            }
            _ => unreachable!(),
        }
        let candidate = signed(&local.keys, &record, seq);
        let mut h = Harness::new(local, Some(candidate.packet.clone())).await;
        let result = h
            .manager
            .publication(Action::Refresh)
            .await
            .unwrap_err()
            .code;
        assert_eq!(
            result,
            if case == "future" {
                ErrorCode::AccountRestricted
            } else {
                ErrorCode::RecordConflict
            },
            "{case}"
        );
        assert_eq!(
            h.manager
                .local()
                .unwrap()
                .records
                .conflict
                .as_ref()
                .unwrap()
                .hash,
            candidate.hash,
            "{case}"
        );
        assert!(!h.manager.local_authorized(), "{case}");
        h.no_effects();

        let remote = local_fixture(2);
        let accepted = remote.records.accepted.as_ref().unwrap().clone();
        let mut record = accepted.record.clone();
        match case {
            "equal" => record.issued_at = 901,
            "generation" => record.generation = Generation([9; 16]),
            "membership" => record.devices[0].kind = DeviceKind::Mobile,
            "future" => record.issued_at = 1400,
            "expired" => record.expires_at = NOW,
            _ => {}
        }
        let candidate = signed(&remote.keys, &record, seq);
        let peer = remote.keys.peer_id();
        let mut local = local_fixture(1);
        let floor = if case == "above_accepted_below_floor" {
            ACCEPTED_SEQ + 2
        } else {
            ACCEPTED_SEQ
        };
        local.remote.push(RemoteRecord {
            peer,
            accepted,
            floor,
            conflict: None,
        });
        let mut h = Harness::new(local, Some(candidate.packet)).await;
        assert_eq!(
            h.manager.remote_record(peer).await.err().unwrap().code,
            ErrorCode::RecordConflict,
            "{case}"
        );
        let entry = &h.manager.local().unwrap().remote[0];
        assert_eq!(
            entry.conflict.as_ref().unwrap().hash,
            candidate.hash,
            "{case}"
        );
        assert_eq!(entry.floor, floor, "{case}");
        assert!(!h.manager.auth.cached_lookups.contains(&peer));
        h.no_effects();
    }
}

#[tokio::test]
async fn stale_lookup_ineligible_authority_keeps_own_strict_path() {
    for case in [
        "accepted_expired",
        "accepted_future",
        "higher_floor",
        "retained",
        "conflict_state",
        "pending",
        "current_tombstone",
    ] {
        let mut local = local_fixture(1);
        let mut accepted = local.records.accepted.as_ref().unwrap().record.clone();
        let candidate = signed(&local.keys, &accepted, OLD_SEQ);
        match case {
            "accepted_expired" => accepted.expires_at = NOW,
            "accepted_future" => accepted.issued_at = 1400,
            "higher_floor" => local.records.observed_seq = ACCEPTED_SEQ + 1,
            "retained" => {
                local.records.conflict = Some(candidate.clone());
                local.records.state = PublicationState::Conflict;
            }
            "conflict_state" => local.records.state = PublicationState::Conflict,
            "pending" => local.records.pending.push(PacketIntent {
                signed: local.records.accepted.as_ref().unwrap().clone(),
                base_hash: None,
                operation: PacketOperation::Renew,
                status: PacketStatus::Ready,
                revoke_nonce: None,
                recipients: None,
            }),
            "current_tombstone" => local.records.tombstones.push(
                RevokeIntent::sign(
                    &local.keys,
                    accepted.generation,
                    local.records.accepted.as_ref().unwrap().hash,
                    &accepted.devices[0],
                    NOW,
                )
                .unwrap(),
            ),
            _ => unreachable!(),
        }
        local.records.accepted = Some(signed(&local.keys, &accepted, ACCEPTED_SEQ));
        let mut h = Harness::new(local, Some(candidate.packet)).await;
        let before = h.bytes();
        let code = h
            .manager
            .publication(Action::Refresh)
            .await
            .unwrap_err()
            .code;
        assert_eq!(
            code,
            if case == "current_tombstone" {
                ErrorCode::AccountRestricted
            } else {
                ErrorCode::RecordConflict
            },
            "{case}"
        );
        if case == "current_tombstone" {
            assert_eq!(h.bytes(), before);
        } else {
            assert!(
                h.manager.local().unwrap().records.conflict.is_some(),
                "{case}"
            );
        }
        if case == "pending" {
            assert!(h.manager.local().unwrap().records.pending[0].status == PacketStatus::Held);
        }
        h.no_effects();
    }
}

#[tokio::test]
async fn stale_lookup_ineligible_remote_authority_keeps_strict_path() {
    for case in [
        "accepted_expired",
        "accepted_future",
        "higher_floor",
        "retained",
    ] {
        let remote = local_fixture(2);
        let mut accepted = remote.records.accepted.as_ref().unwrap().record.clone();
        let candidate = signed(&remote.keys, &accepted, OLD_SEQ);
        if case == "accepted_expired" {
            accepted.expires_at = NOW;
        }
        if case == "accepted_future" {
            accepted.issued_at = 1400;
        }
        let peer = remote.keys.peer_id();
        let mut local = local_fixture(1);
        local.remote.push(RemoteRecord {
            peer,
            accepted: signed(&remote.keys, &accepted, ACCEPTED_SEQ),
            floor: if case == "higher_floor" {
                ACCEPTED_SEQ + 1
            } else {
                ACCEPTED_SEQ
            },
            conflict: if case == "retained" {
                Some(candidate.clone())
            } else {
                None
            },
        });
        let mut h = Harness::new(local, Some(candidate.packet)).await;
        assert_eq!(
            h.manager.remote_record(peer).await.err().unwrap().code,
            ErrorCode::RecordConflict,
            "{case}"
        );
        assert!(h.manager.local().unwrap().remote[0].conflict.is_some());
        assert!(!h.manager.auth.cached_lookups.contains(&peer));
        h.no_effects();
    }
}

#[tokio::test]
async fn stale_lookup_invalid_account_or_signature_never_falls_back() {
    for bad_signature in [false, true] {
        let local = local_fixture(1);
        let remote = local_fixture(2);
        let peer = remote.keys.peer_id();
        let accepted = remote.records.accepted.as_ref().unwrap().clone();
        let valid = signed(
            &local.keys,
            &local.records.accepted.as_ref().unwrap().record,
            OLD_SEQ,
        )
        .packet;
        let invalid = if bad_signature {
            let mut serialized = vec![0; 8];
            serialized.extend_from_slice(valid.as_bytes());
            serialized[40] ^= 1;
            pkarr::SignedPacket::deserialize(&serialized).unwrap()
        } else {
            signed(&remote.keys, &accepted.record, OLD_SEQ).packet
        };
        let mut h = Harness::new(local, Some(invalid)).await;
        let before = h.bytes();
        assert_eq!(
            h.manager
                .publication(Action::Refresh)
                .await
                .unwrap_err()
                .code,
            ErrorCode::AuthFailed
        );
        assert_eq!(h.bytes(), before);
        h.no_effects();
        let mut local = local_fixture(1);
        local.remote.push(RemoteRecord {
            peer,
            accepted: accepted.clone(),
            floor: ACCEPTED_SEQ,
            conflict: None,
        });
        let packet = if bad_signature {
            let mut bytes = vec![0; 8];
            bytes.extend_from_slice(accepted.packet.as_bytes());
            bytes[40] ^= 1;
            pkarr::SignedPacket::deserialize(&bytes).unwrap()
        } else {
            valid
        };
        let mut h = Harness::new(local, Some(packet)).await;
        let before = h.bytes();
        assert_eq!(
            h.manager.remote_record(peer).await.err().unwrap().code,
            ErrorCode::AuthFailed
        );
        assert_eq!(h.bytes(), before);
        assert!(!h.manager.auth.cached_lookups.contains(&peer));
        h.no_effects();
    }
}

#[tokio::test]
async fn stale_lookup_revalidates_time_after_lookup_await() {
    let local = local_fixture(1);
    let candidate = signed(
        &local.keys,
        &local.records.accepted.as_ref().unwrap().record,
        OLD_SEQ,
    );
    let mut h = Harness::new(local, Some(candidate.packet)).await;
    *h.backend.clock_after_lookup.lock().unwrap() = Some((h.clock.clone(), 2000));
    assert_eq!(
        h.manager
            .publication(Action::Refresh)
            .await
            .unwrap_err()
            .code,
        ErrorCode::RecordConflict
    );
    assert!(h.manager.local().unwrap().records.conflict.is_some());
    h.no_effects();
    let mut local = local_fixture(1);
    let remote = local_fixture(2);
    let accepted = remote.records.accepted.as_ref().unwrap().clone();
    let candidate = signed(&remote.keys, &accepted.record, OLD_SEQ);
    let peer = remote.keys.peer_id();
    local.remote.push(RemoteRecord {
        peer,
        accepted,
        floor: ACCEPTED_SEQ,
        conflict: None,
    });
    let mut h = Harness::new(local, Some(candidate.packet)).await;
    *h.backend.clock_after_lookup.lock().unwrap() = Some((h.clock.clone(), 2000));
    assert_eq!(
        h.manager.remote_record(peer).await.err().unwrap().code,
        ErrorCode::RecordConflict
    );
    assert!(!h.manager.auth.cached_lookups.contains(&peer));
    h.no_effects();
}

#[tokio::test]
async fn stale_lookup_exact_accepted_and_completed_absence_keep_distinct_paths() {
    let local = local_fixture(1);
    let packet = local.records.accepted.as_ref().unwrap().packet.clone();
    let mut h = Harness::new(local, Some(packet)).await;
    let before = h.bytes();
    h.manager.publication(Action::Refresh).await.unwrap();
    assert_eq!(h.bytes(), before);
    h.no_effects();
    h.backend.unavailable.store(true, Ordering::SeqCst);
    assert_eq!(
        h.manager
            .publication(Action::Refresh)
            .await
            .unwrap_err()
            .code,
        ErrorCode::RecordUnavailable
    );
    assert_eq!(
        h.manager.local().unwrap().records.state,
        PublicationState::LocalBlocked
    );
    assert_eq!(
        h.manager.local().unwrap().records.allocated_seq,
        ACCEPTED_SEQ + 20
    );
    h.no_effects();
    h.backend.unavailable.store(false, Ordering::SeqCst);
    *h.backend.lookup.lock().unwrap() = None;
    h.manager.publication(Action::Refresh).await.unwrap();
    assert_eq!(
        h.backend.published.load(Ordering::SeqCst),
        1,
        "completed absence keeps existing renewal policy"
    );
    assert!(h.manager.local().unwrap().records.allocated_seq > ACCEPTED_SEQ + 20);

    let mut local = local_fixture(1);
    let remote = local_fixture(2);
    let peer = remote.keys.peer_id();
    let accepted = remote.records.accepted.as_ref().unwrap().clone();
    local.remote.push(RemoteRecord {
        peer,
        accepted: accepted.clone(),
        floor: ACCEPTED_SEQ,
        conflict: None,
    });
    let mut h = Harness::new(local, Some(accepted.packet)).await;
    h.manager.auth.cached_lookups.insert(peer);
    h.manager.remote_record(peer).await.unwrap();
    assert!(!h.manager.auth.cached_lookups.contains(&peer));
    *h.backend.lookup.lock().unwrap() = None;
    h.manager.remote_record(peer).await.unwrap();
    assert!(h.manager.auth.cached_lookups.contains(&peer));
    h.no_effects();
}

#[tokio::test]
async fn stale_lookup_exact_ready_pending_keeps_acceptance_path() {
    let mut local = local_fixture(1);
    let pending = signed(
        &local.keys,
        &local.records.accepted.as_ref().unwrap().record,
        ACCEPTED_SEQ + 20,
    );
    local.records.pending.push(PacketIntent {
        signed: pending.clone(),
        base_hash: local.records.accepted.as_ref().map(|r| r.hash),
        operation: PacketOperation::Renew,
        status: PacketStatus::Ready,
        revoke_nonce: None,
        recipients: None,
    });
    local.records.state = PublicationState::Pending;
    let mut h = Harness::new(local, Some(pending.packet)).await;
    h.manager.publication(Action::Refresh).await.unwrap();
    let records = &h.manager.local().unwrap().records;
    assert_eq!(records.accepted.as_ref().unwrap().hash, pending.hash);
    assert_eq!(records.observed_seq, ACCEPTED_SEQ + 20);
    assert!(records.pending.is_empty());
    assert!(h.manager.local_authorized());
    h.no_effects();
}
