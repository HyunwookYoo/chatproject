#![cfg(all(feature = "test-hooks", debug_assertions))]
use chat_core::account::{
    AccountHandle, AccountPhase, AccountServices, AccountView, Generation, IdentityRecord, Lookup,
    PeerId, PublicationState, RecordBackend, RecordFuture, Secret32, StoreId,
    clock::FakeClock,
    derive_account, encode_recovery, parse_record, record_hash, record_packet,
    storage::{KeyProtector, StoredPlaintext},
};
use chat_core::{CoreConfig, CoreError, CoreSlot, ErrorCode, EventBus};
use pkarr::SignedPacket;
use std::sync::{Arc, Mutex};
#[allow(dead_code)]
#[path = "support/account_store.rs"]
mod support;

#[derive(Clone)]
enum Answer {
    Unavailable,
    Absent,
    Found(SignedPacket),
    Blocked(Arc<tokio::sync::Notify>),
}
struct Backend {
    answer: Mutex<Answer>,
    sent: Mutex<Vec<SignedPacket>>,
}
impl Backend {
    fn new() -> Self {
        Self {
            answer: Mutex::new(Answer::Unavailable),
            sent: Mutex::new(vec![]),
        }
    }
    fn answer(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }
    fn last(&self) -> SignedPacket {
        self.sent.lock().unwrap().last().unwrap().clone()
    }
}
impl RecordBackend for Backend {
    fn lookup(&self, _: PeerId) -> RecordFuture<'_, Lookup> {
        let answer = self.answer.lock().unwrap().clone();
        Box::pin(async move {
            match answer {
                Answer::Unavailable => Err(CoreError::new(
                    ErrorCode::RecordUnavailable,
                    "fixture offline",
                )),
                Answer::Absent => Ok(Lookup::CompletedAbsent),
                Answer::Found(packet) => Ok(Lookup::Found(packet)),
                Answer::Blocked(entered) => {
                    entered.notify_one();
                    std::future::pending().await
                }
            }
        })
    }
    fn publish<'a>(&'a self, packet: &'a SignedPacket) -> RecordFuture<'a, ()> {
        Box::pin(async move {
            self.sent.lock().unwrap().push(packet.clone());
            Ok(())
        })
    }
}
struct Fixture {
    _sql: sql_store::Store,
    _dir: tempfile::TempDir,
    slot: CoreSlot,
    account: AccountHandle,
    backend: Arc<Backend>,
    clock: Arc<FakeClock>,
}
#[tokio::test]
async fn fixture_drop_closes_sql_before_owned_tempdir() {
    let fixture = Fixture::new().await;
    let path = fixture._dir.path().canonicalize().unwrap();
    let storage = fixture.account.storage_test_fixture();
    drop(fixture);
    sql_store::assert_closed_and_removed(path, storage);
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(Backend::new());
        let clock = Arc::new(FakeClock::new(1_800_000_000, 0));
        let (slot, account) = open(dir.path(), backend.clone(), clock.clone()).await;
        Self {
            _sql: sql_store::Store::from_fixture(account.storage_test_fixture()),
            _dir: dir,
            slot,
            account,
            backend,
            clock,
        }
    }
    async fn recover(&self) {
        self.account
            .recover(encode_recovery(&Secret32::from([0; 32])).unwrap(), true)
            .await
            .unwrap();
    }
    async fn view(&self) -> AccountView {
        self.account.snapshot().await.unwrap()
    }
    async fn refresh(&self) -> Result<(), CoreError> {
        self.account
            .refresh_record(self.view().await.public_peer_id.unwrap())
            .await
    }
    async fn published(&self) -> SignedPacket {
        self.backend.answer(Answer::Absent);
        self.refresh().await.unwrap();
        let packet = self.backend.last();
        self.backend.answer(Answer::Found(packet.clone()));
        self.refresh().await.unwrap();
        assert_eq!(
            self.view().await.publication.state,
            PublicationState::Observed
        );
        packet
    }
}
async fn open(
    path: &std::path::Path,
    backend: Arc<Backend>,
    clock: Arc<FakeClock>,
) -> (CoreSlot, AccountHandle) {
    open_services(path, AccountServices::new(backend, clock)).await
}
async fn open_services(
    path: &std::path::Path,
    services: AccountServices,
) -> (CoreSlot, AccountHandle) {
    let slot = CoreSlot::new();
    slot.start(
        CoreConfig {
            data_dir: path.into(),
        },
        &EventBus::new(),
    )
    .unwrap();
    slot.network().unwrap().configure_local(None).await.unwrap();
    let account = slot
        .open_account_with_services(support::protector(), services)
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(account.lifecycle_status(), 0);
    (slot, account)
}

#[tokio::test]
async fn unavailable_is_not_absent() {
    let f = Fixture::new().await;
    f.recover().await;
    assert_eq!(
        f.refresh().await.unwrap_err().code,
        ErrorCode::RecordUnavailable
    );
    assert!(f.backend.sent.lock().unwrap().is_empty());
    assert_eq!(f.view().await.publication.allocated_seq, 0);
    assert_eq!(f.view().await.phase, AccountPhase::PublishPending);
}

#[tokio::test]
async fn future_initial_recovery_waits_for_fresh_current_base_across_restart() {
    let f = Fixture::new().await;
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let record = IdentityRecord {
        generation: Generation([3; 16]),
        issued_at: 1_800_000_600,
        expires_at: 1_800_600_000,
        devices: vec![],
    };
    let future = record_packet(&keys, &record, 1_800_000_600_000_000).unwrap();
    f.backend.answer(Answer::Found(future.clone()));
    f.recover().await;
    let view = f.view().await;
    assert_eq!(view.publication.observed_seq, 1_800_000_600_000_000);
    assert_eq!(view.publication.state, PublicationState::LocalBlocked);
    assert_eq!(view.publication.conflict_hash, Some(record_hash(&future)));
    assert!(f.backend.sent.lock().unwrap().is_empty());
    f.backend.answer(Answer::Absent);
    assert_eq!(
        f.refresh().await.unwrap_err().code,
        ErrorCode::AccountRestricted
    );
    assert_eq!(f.view().await.publication.allocated_seq, 0);
    f.clock.set_unix_seconds(1_800_000_600);
    assert_eq!(
        f.refresh().await.unwrap_err().code,
        ErrorCode::AccountRestricted
    );
    f.account.request_suspend();
    while f.account.lifecycle_status() == 1 {
        tokio::task::yield_now().await;
    }
    let Fixture {
        _sql,
        _dir: dir,
        slot,
        account,
        backend,
        clock,
    } = f;
    drop(account);
    drop(slot);
    drop(_sql);
    let (_slot, account) = open(dir.path(), backend.clone(), clock).await;
    let _sql = sql_store::Store::from_fixture(account.storage_test_fixture());
    let restored = account.snapshot().await.unwrap();
    assert_eq!(restored.publication.state, PublicationState::LocalBlocked);
    assert_eq!(
        restored.publication.conflict_hash,
        Some(record_hash(&future))
    );
    assert_eq!(restored.publication.allocated_seq, 0);
    assert!(backend.sent.lock().unwrap().is_empty());
    let older = IdentityRecord {
        issued_at: 1_800_000_000,
        ..record.clone()
    };
    backend.answer(Answer::Found(
        record_packet(&keys, &older, 1_800_000_599_000_000).unwrap(),
    ));
    assert_eq!(
        account
            .refresh_record(keys.peer_id())
            .await
            .unwrap_err()
            .code,
        ErrorCode::AccountRestricted
    );
    assert_eq!(
        account.snapshot().await.unwrap().publication.conflict_hash,
        Some(record_hash(&future))
    );
    backend.answer(Answer::Found(future.clone()));
    assert_eq!(
        account
            .confirm_record_conflict(record_hash(&future))
            .await
            .unwrap_err()
            .code,
        ErrorCode::AccountRestricted
    );
    assert_eq!(
        account.snapshot().await.unwrap().publication.conflict_hash,
        Some(record_hash(&future))
    );
    backend.answer(Answer::Found(future));
    account.refresh_record(keys.peer_id()).await.unwrap();
    let replacement = parse_record(&backend.last(), keys.peer_id(), 1_800_000_600).unwrap();
    assert_ne!(replacement.generation, record.generation);
    assert_eq!(replacement.devices.len(), 1);
    assert_eq!(
        Some(replacement.devices[0].installation_id),
        restored.installation
    );
    let published = account.snapshot().await.unwrap();
    assert_eq!(published.publication.conflict_hash, None);
    assert!(published.publication.allocated_seq > 1_800_000_600_000_000);
    assert_eq!(published.publication.state, PublicationState::Pending);
}

#[tokio::test]
async fn future_competitor_holds_ready_pending_across_absence() {
    let f = Fixture::new().await;
    f.recover().await;
    let accepted = f.published().await;
    f.backend.answer(Answer::Absent);
    f.refresh().await.unwrap();
    let before = f.view().await;
    let sent = f.backend.sent.lock().unwrap().len();
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let mut competing = parse_record(&accepted, keys.peer_id(), 1_800_000_000).unwrap();
    competing.generation = Generation([8; 16]);
    competing.issued_at = 1_800_000_600;
    competing.expires_at = 1_800_605_400;
    let future = record_packet(&keys, &competing, 1_800_000_600_000_000).unwrap();
    f.backend.answer(Answer::Found(future.clone()));
    assert_eq!(
        f.refresh().await.unwrap_err().code,
        ErrorCode::AccountRestricted
    );
    f.backend.answer(Answer::Absent);
    assert_eq!(
        f.refresh().await.unwrap_err().code,
        ErrorCode::RecordConflict
    );
    let held = f.view().await;
    assert_eq!(held.publication.state, PublicationState::Conflict);
    assert_eq!(held.publication.conflict_hash, Some(record_hash(&future)));
    assert_eq!(held.publication.conflict, Some(competing));
    assert_eq!(
        held.publication.accepted_hash,
        before.publication.accepted_hash
    );
    assert_eq!(
        held.publication.allocated_seq,
        before.publication.allocated_seq
    );
    assert_eq!(held.publication.pending_count, 1);
    assert_eq!(f.backend.sent.lock().unwrap().len(), sent);
}

#[tokio::test]
async fn future_excluded_writer_stays_held_after_clock_catches_up() {
    let f = Fixture::new().await;
    f.recover().await;
    let accepted = f.published().await;
    let before = f.view().await;
    let sent = f.backend.sent.lock().unwrap().len();
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let mut competing = parse_record(&accepted, keys.peer_id(), 1_800_000_000).unwrap();
    competing.devices.clear();
    competing.issued_at = 1_800_000_600;
    competing.expires_at = 1_800_605_400;
    let future = record_packet(&keys, &competing, 1_800_000_600_000_000).unwrap();
    f.backend.answer(Answer::Found(future.clone()));
    assert_eq!(
        f.refresh().await.unwrap_err().code,
        ErrorCode::AccountRestricted
    );
    f.clock.set_unix_seconds(1_800_000_600);
    f.backend.answer(Answer::Absent);
    assert_eq!(
        f.refresh().await.unwrap_err().code,
        ErrorCode::RecordConflict
    );
    assert_eq!(
        f.view().await.publication.allocated_seq,
        before.publication.allocated_seq
    );
    assert_eq!(f.backend.sent.lock().unwrap().len(), sent);
    f.backend.answer(Answer::Found(future.clone()));
    assert_eq!(
        f.account
            .confirm_record_conflict(record_hash(&future))
            .await
            .unwrap_err()
            .code,
        ErrorCode::AccountRestricted
    );
    assert_eq!(f.view().await.publication.conflict, Some(competing));
}

#[tokio::test]
async fn future_same_sequence_conflict_and_pending_hold_survive_restart() {
    let f = Fixture::new().await;
    f.recover().await;
    let accepted = f.published().await;
    f.backend.answer(Answer::Absent);
    f.refresh().await.unwrap();
    let before = f.view().await;
    let sent = f.backend.sent.lock().unwrap().len();
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let mut competing = parse_record(&accepted, keys.peer_id(), 1_800_000_000).unwrap();
    competing.issued_at = 1_800_000_600;
    competing.expires_at = 1_800_605_400;
    let future = record_packet(&keys, &competing, before.publication.observed_seq).unwrap();
    f.backend.answer(Answer::Found(future.clone()));
    assert_eq!(
        f.refresh().await.unwrap_err().code,
        ErrorCode::AccountRestricted
    );
    f.account.request_suspend();
    while f.account.lifecycle_status() == 1 {
        tokio::task::yield_now().await;
    }
    let Fixture {
        _sql,
        _dir: dir,
        slot,
        account,
        backend,
        clock,
    } = f;
    drop(account);
    drop(slot);
    drop(_sql);
    clock.set_unix_seconds(1_800_000_600);
    backend.answer(Answer::Absent);
    let (_slot, account) = open(dir.path(), backend.clone(), clock).await;
    let _sql = sql_store::Store::from_fixture(account.storage_test_fixture());
    let restored = account.snapshot().await.unwrap();
    assert_eq!(backend.sent.lock().unwrap().len(), sent);
    assert_eq!(restored.publication.state, PublicationState::Conflict);
    assert_eq!(
        restored.publication.conflict_hash,
        Some(record_hash(&future))
    );
    assert_eq!(restored.publication.conflict, Some(competing));
    assert_eq!(
        restored.publication.accepted_hash,
        before.publication.accepted_hash
    );
    assert_eq!(
        restored.publication.allocated_seq,
        before.publication.allocated_seq
    );
    assert_eq!(restored.publication.pending_count, 1);
}

#[tokio::test]
async fn exact_known_packet_under_rollback_does_not_invent_conflict() {
    for pending in [false, true] {
        let f = Fixture::new().await;
        f.recover().await;
        let accepted = f.published().await;
        let known = if pending {
            f.backend.answer(Answer::Absent);
            f.refresh().await.unwrap();
            f.backend.last()
        } else {
            accepted
        };
        f.clock.set_unix_seconds(1_700_000_000);
        f.backend.answer(Answer::Found(known.clone()));
        assert_eq!(
            f.refresh().await.unwrap_err().code,
            ErrorCode::AccountRestricted
        );
        assert_eq!(
            f.view().await.publication.state,
            PublicationState::LocalBlocked
        );
        assert_eq!(f.view().await.publication.conflict_hash, None);
        f.clock.set_unix_seconds(1_800_000_000);
        f.backend.answer(if pending {
            Answer::Absent
        } else {
            Answer::Found(known.clone())
        });
        f.refresh().await.unwrap();
        assert_eq!(f.view().await.publication.conflict_hash, None);
        if pending {
            assert_eq!(f.backend.last().as_bytes(), known.as_bytes());
        }
    }
}

#[tokio::test]
async fn first_publish_after_confirmation_and_ack_is_not_observation() {
    let f = Fixture::new().await;
    f.backend.answer(Answer::Absent);
    let display = f.account.begin_create().await.unwrap();
    assert!(f.backend.sent.lock().unwrap().is_empty());
    let answers = display
        .positions
        .map(|p| display.words[p as usize - 1].clone());
    f.account
        .confirm_create(display.flow, answers)
        .await
        .unwrap();
    let view = f.view().await;
    assert_eq!(view.publication.state, PublicationState::Pending);
    assert_eq!(view.publication.allocated_seq, 1_800_000_000_000_000);
    assert_ne!(view.phase, AccountPhase::Active);
    let packet = f.backend.last();
    f.refresh().await.unwrap();
    assert_eq!(f.backend.last().as_bytes(), packet.as_bytes());
    f.backend.answer(Answer::Found(packet));
    f.refresh().await.unwrap();
    assert_eq!(f.view().await.phase, AccountPhase::Active);
}

#[tokio::test]
async fn expired_seven_days_renew_same_generation_no_readd() {
    let f = Fixture::new().await;
    f.recover().await;
    let first = f.published().await;
    let peer = f.view().await.public_peer_id.unwrap();
    let old = parse_record(&first, peer, 1_800_000_000).unwrap();
    f.clock.set_unix_seconds(1_800_700_000);
    f.refresh().await.unwrap();
    let renewed = parse_record(&f.backend.last(), peer, 1_800_700_000).unwrap();
    assert_eq!(renewed.generation, old.generation);
    assert_eq!(renewed.devices, old.devices);
    assert_eq!(renewed.issued_at, 1_800_700_000);
    assert_eq!(f.view().await.publication.state, PublicationState::Pending);
}

#[tokio::test]
async fn recovery_replacement_one_new_device() {
    let f = Fixture::new().await;
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let record = IdentityRecord {
        generation: Generation([7; 16]),
        issued_at: 1_799_000_000,
        expires_at: 1_799_604_800,
        devices: vec![],
    };
    f.backend.answer(Answer::Found(
        record_packet(&keys, &record, 1_799_000_000_000_000).unwrap(),
    ));
    f.recover().await;
    let view = f.view().await;
    let replacement = parse_record(&f.backend.last(), keys.peer_id(), 1_800_000_000).unwrap();
    assert_ne!(replacement.generation, record.generation);
    assert_eq!(replacement.devices.len(), 1);
    assert_eq!(
        Some(replacement.devices[0].installation_id),
        view.installation
    );
    assert!(view.publication.observed_seq >= 1_799_000_000_000_000);
}

#[tokio::test]
async fn same_seq_different_payload_conflict() {
    let f = Fixture::new().await;
    f.recover().await;
    let packet = f.published().await;
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let mut record = parse_record(&packet, keys.peer_id(), 1_800_000_000).unwrap();
    record.generation = Generation([9; 16]);
    let other = record_packet(&keys, &record, 1_800_000_000_000_000).unwrap();
    f.backend.answer(Answer::Found(other.clone()));
    assert_eq!(
        f.refresh().await.unwrap_err().code,
        ErrorCode::RecordConflict
    );
    let view = f.view().await;
    assert_eq!(view.phase, AccountPhase::Conflict);
    assert_eq!(view.publication.conflict_hash, Some(record_hash(&other)));
    assert_eq!(view.publication.accepted_hash, Some(record_hash(&packet)));
    assert_eq!(
        f.account
            .confirm_record_conflict(record_hash(&packet))
            .await
            .unwrap_err()
            .code,
        ErrorCode::RecordConflict
    );
    assert_eq!(
        f.account
            .confirm_record_conflict(record_hash(&other))
            .await
            .unwrap_err()
            .code,
        ErrorCode::AccountRestricted
    );
}

#[tokio::test]
async fn durable_floor_survives_restart_and_clock_rollback() {
    let f = Fixture::new().await;
    f.recover().await;
    f.published().await;
    let before = f.view().await;
    f.account.request_suspend();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while f.account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let Fixture {
        _sql,
        _dir: dir,
        slot,
        account,
        backend,
        clock,
    } = f;
    drop(account);
    drop(slot);
    drop(_sql);
    clock.set_unix_seconds(1_700_000_000);
    backend.answer(Answer::Absent);
    let (_slot, account) = open(dir.path(), backend, clock).await;
    let _sql = sql_store::Store::from_fixture(account.storage_test_fixture());
    let after = account.snapshot().await.unwrap();
    assert_ne!(after.phase, AccountPhase::Locked);
    assert_eq!(
        after.publication.observed_seq,
        before.publication.observed_seq
    );
    assert_eq!(
        after.publication.allocated_seq,
        before.publication.allocated_seq
    );
    assert_eq!(after.publication.state, PublicationState::LocalBlocked);
}

#[tokio::test]
async fn concurrent_add_revoke_recovery_no_silent_merge() {
    let f = Fixture::new().await;
    f.recover().await;
    let packet = f.published().await;
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let mut record = parse_record(&packet, keys.peer_id(), 1_800_000_000).unwrap();
    let remote = chat_core::account::derive_device(Secret32::from([3; 32])).unwrap();
    record.devices.push(chat_core::account::RecordDevice {
        installation_id: remote.installation_id(),
        transport_public_key: remote.transport_public_key(),
        kind: chat_core::account::DeviceKind::Desktop,
    });
    record.devices.sort_by_key(|d| d.installation_id);
    let addition = record_packet(&keys, &record, 1_800_000_000_000_010).unwrap();
    f.backend.answer(Answer::Found(addition.clone()));
    assert_eq!(
        f.refresh().await.unwrap_err().code,
        ErrorCode::RecordConflict
    );
    f.account
        .confirm_record_conflict(record_hash(&addition))
        .await
        .unwrap();
    f.backend.answer(Answer::Found(f.backend.last()));
    f.refresh().await.unwrap();
    let sent = f.backend.sent.lock().unwrap().len();
    f.account
        .revoke_device(remote.installation_id())
        .await
        .unwrap();
    let revoked = f.view().await;
    assert_eq!(revoked.publication.tombstones.len(), 1);
    assert_eq!(revoked.publication.pending_notifications.len(), 0);
    assert_eq!(
        f.backend.sent.lock().unwrap().len(),
        sent + 1,
        "installed empty authenticated registry has no recipients"
    );
    let replacement = IdentityRecord {
        generation: Generation([6; 16]),
        devices: vec![],
        ..record
    };
    f.backend.answer(Answer::Found(
        record_packet(&keys, &replacement, 1_800_000_000_000_100).unwrap(),
    ));
    assert_eq!(
        f.refresh().await.unwrap_err().code,
        ErrorCode::RecordConflict
    );
    assert_eq!(f.view().await.publication.tombstones.len(), 1);
    assert_eq!(f.view().await.publication.pending_notifications.len(), 1);
    assert_eq!(f.backend.sent.lock().unwrap().len(), sent + 1);
    assert_eq!(
        f.account
            .revoke_device(f.view().await.installation.unwrap())
            .await
            .unwrap_err()
            .code,
        ErrorCode::AccountRestricted
    );
}

#[tokio::test]
async fn completed_absence_republishes_a_known_base_without_claiming_observed() {
    let f = Fixture::new().await;
    f.recover().await;
    let first = f.published().await;
    f.backend.answer(Answer::Absent);
    f.refresh().await.unwrap();
    assert_eq!(f.view().await.publication.state, PublicationState::Pending);
    assert_ne!(f.backend.last().as_bytes(), first.as_bytes());
    assert!(f.view().await.publication.allocated_seq > 1_800_000_000_000_000);
}

#[tokio::test]
async fn expired_unobserved_packet_is_reconciled_without_reusing_sequence() {
    let f = Fixture::new().await;
    f.recover().await;
    f.backend.answer(Answer::Absent);
    f.refresh().await.unwrap();
    let old = f.backend.last();
    f.clock.set_unix_seconds(1_800_700_000);
    f.refresh().await.unwrap();
    assert_ne!(old.as_bytes(), f.backend.last().as_bytes());
    let peer = f.view().await.public_peer_id.unwrap();
    let record = parse_record(&f.backend.last(), peer, 1_800_700_000).unwrap();
    assert_eq!(record.issued_at, 1_800_700_000);
    assert_eq!(
        record.devices,
        parse_record(&old, peer, 1_800_700_000).unwrap().devices
    );
}

#[tokio::test]
async fn conflict_confirm_cannot_readd_an_excluded_writer() {
    let f = Fixture::new().await;
    f.recover().await;
    let first = f.published().await;
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let mut record = parse_record(&first, keys.peer_id(), 1_800_000_000).unwrap();
    record.devices.clear();
    let competing = record_packet(&keys, &record, 1_800_000_000_000_001).unwrap();
    f.backend.answer(Answer::Found(competing.clone()));
    assert_eq!(
        f.refresh().await.unwrap_err().code,
        ErrorCode::RecordConflict
    );
    assert_eq!(
        f.account
            .confirm_record_conflict(record_hash(&competing))
            .await
            .unwrap_err()
            .code,
        ErrorCode::AccountRestricted
    );
    assert_eq!(
        f.view().await.publication.conflict_hash,
        Some(record_hash(&competing))
    );
}

#[tokio::test]
async fn suspend_cancels_lookup_before_allocation_or_send() {
    let f = Fixture::new().await;
    f.recover().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    f.backend.answer(Answer::Blocked(entered.clone()));
    let account = f.account.clone();
    let peer = f.view().await.public_peer_id.unwrap();
    let task = tokio::spawn(async move { account.refresh_record(peer).await });
    entered.notified().await;
    f.account.request_suspend();
    assert_eq!(
        task.await.unwrap().unwrap_err().code,
        ErrorCode::StorageLocked
    );
    assert!(f.backend.sent.lock().unwrap().is_empty());
    f.backend.answer(Answer::Absent);
    f.account.request_resume();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while f.account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        f.view().await.publication.allocated_seq,
        1_800_000_000_000_000
    );
}

#[tokio::test]
async fn restart_retries_exact_durable_packet() {
    let f = Fixture::new().await;
    f.recover().await;
    f.backend.answer(Answer::Absent);
    f.refresh().await.unwrap();
    let first = f.backend.last();
    f.account.request_suspend();
    while f.account.lifecycle_status() == 1 {
        tokio::task::yield_now().await;
    }
    let Fixture {
        _sql,
        _dir: dir,
        slot,
        account,
        backend,
        clock,
    } = f;
    drop(account);
    drop(slot);
    drop(_sql);
    let (_slot, account) = open(dir.path(), backend.clone(), clock).await;
    let _sql = sql_store::Store::from_fixture(account.storage_test_fixture());
    let view = account.snapshot().await.unwrap();
    assert_eq!(backend.last().as_bytes(), first.as_bytes());
    assert_eq!(view.publication.pending_count, 1);
    assert_eq!(view.publication.allocated_seq, 1_800_000_000_000_000);
}

#[tokio::test]
async fn legacy_a5_snapshot_is_rejected_without_file_modification() {
    let f = Fixture::new().await;
    f.recover().await;
    f.account.request_suspend();
    while f.account.lifecycle_status() == 1 {
        tokio::task::yield_now().await;
    }
    let Fixture {
        _sql,
        _dir: dir,
        slot,
        account,
        backend,
        clock,
    } = f;
    drop(account);
    drop(slot);
    drop(_sql);
    let mut store =
        sql_store::Store::open(dir.path(), support::protector()).unwrap();
    let mut stored = store.load().unwrap().unwrap();
    let mut decoder = minicbor::Decoder::new(&stored.payload);
    assert_eq!(decoder.array().unwrap(), Some(11));
    for _ in 0..7 {
        decoder.skip().unwrap();
    }
    let end = decoder.position();
    stored.payload.truncate(end);
    stored.payload[0] = 0x87;
    drop(store);
    let dir = tempfile::tempdir().unwrap();
    chat_core::account::storage::Store::open(dir.path(), support::protector()).unwrap().commit(&stored).unwrap();
    let before = std::fs::read(dir.path().join("account.bin")).unwrap();
    let slot = CoreSlot::new();
    slot.start(
        CoreConfig {
            data_dir: dir.path().into(),
        },
        &EventBus::new(),
    )
    .unwrap();
    let error = slot
        .open_account_with_services(support::protector(), AccountServices::new(backend, clock))
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::StorageCorrupt);
    assert_eq!(
        std::fs::read(dir.path().join("account.bin")).unwrap(),
        before
    );
}

#[tokio::test]
async fn revoke_is_durable_before_notification_and_failed_notification_blocks_publish() {
    use chat_core::account::{RevocationNotifier, RevokeIntent};
    use std::sync::atomic::{AtomicBool, Ordering};
    fn committed_notification(stored: &StoredPlaintext, intent: &RevokeIntent) -> bool {
        let mut account = minicbor::Decoder::new(&stored.payload);
        assert_eq!(account.array().unwrap(), Some(11));
        for _ in 0..7 {
            account.skip().unwrap();
        }
        let mut records = minicbor::Decoder::new(account.bytes().unwrap());
        assert_eq!(records.array().unwrap(), Some(9));
        for _ in 0..7 {
            records.skip().unwrap();
        }
        let expected = intent.to_bytes();
        let mut tombstone = false;
        for _ in 0..records.array().unwrap().unwrap() {
            tombstone |= records.bytes().unwrap() == expected;
        }
        let mut notification = false;
        for _ in 0..records.array().unwrap().unwrap() {
            assert_eq!(records.array().unwrap(), Some(6));
            records.skip().unwrap(); // Exact committed signed packet is validated by worker load.
            records.skip().unwrap(); // Packet base hash.
            let operation = records.u8().unwrap(); // Revoke = 3.
            let status = records.u8().unwrap(); // AwaitingNotification = 1.
            let nonce = if records.datatype().unwrap() == minicbor::data::Type::Null {
                records.null().unwrap();
                None
            } else {
                Some(records.bytes().unwrap())
            };
            let start = records.position();
            records.skip().unwrap();
            let empty_recipients = &records.input()[start..records.position()] == [0x80];
            notification |= operation == 3
                && status == 1
                && nonce == Some(intent.nonce.as_slice())
                && empty_recipients;
        }
        tombstone && notification
    }
    struct Notifier {
        store: tokio::sync::watch::Sender<Option<chat_core::storage_fixture::Handle>>,
        clock: Arc<FakeClock>,
        fail: AtomicBool,
        calls: Mutex<Vec<(RevokeIntent, StoredPlaintext)>>,
    }
    impl RevocationNotifier for Notifier {
        fn notify<'a>(&'a self, intent: &'a RevokeIntent) -> RecordFuture<'a, ()> {
            Box::pin(async move {
                let mut binding = self.store.subscribe();
                let handle = loop {
                    if let Some(handle) = binding.borrow_and_update().clone() {
                        break handle;
                    }
                    binding.changed().await.unwrap();
                };
                let stored = handle.load_account().await.unwrap().unwrap();
                assert!(committed_notification(&stored, intent), "notification preceded its committed tombstone/obligation");
                self.calls
                    .lock()
                    .unwrap()
                    .push((intent.clone(), stored));
                if self.fail.load(Ordering::SeqCst) {
                    Err(CoreError::new(
                        ErrorCode::RecordUnavailable,
                        "notification offline",
                    ))
                } else {
                    self.clock.set_unix_seconds(1_700_000_000);
                    Ok(())
                }
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(Backend::new());
    let clock = Arc::new(FakeClock::new(1_800_000_000, 0));
    let notifier = Arc::new(Notifier {
        store: tokio::sync::watch::channel(None).0,
        clock: clock.clone(),
        fail: AtomicBool::new(true),
        calls: Mutex::new(vec![]),
    });
    let (slot, account) = open_services(
        dir.path(),
        AccountServices::new(backend.clone(), clock.clone()).with_notifier(notifier.clone()),
    )
    .await;
    notifier.store.send_replace(Some(account.storage_test_fixture().handle));
    let f = Fixture {
        _sql: sql_store::Store::from_fixture(account.storage_test_fixture()),
        _dir: dir,
        slot,
        account,
        backend,
        clock,
    };
    f.recover().await;
    let packet = f.published().await;
    let keys = derive_account(&Secret32::from([0; 32])).unwrap();
    let mut record = parse_record(&packet, keys.peer_id(), 1_800_000_000).unwrap();
    let target = chat_core::account::RecordDevice {
        installation_id: chat_core::account::InstallationId([7; 32]),
        transport_public_key: [8; 32],
        kind: chat_core::account::DeviceKind::Desktop,
    };
    record.devices.push(target.clone());
    record.devices.sort_by_key(|d| d.installation_id);
    let addition = record_packet(&keys, &record, 1_800_000_000_000_010).unwrap();
    f.backend.answer(Answer::Found(addition.clone()));
    f.refresh().await.unwrap_err();
    f.account
        .confirm_record_conflict(record_hash(&addition))
        .await
        .unwrap();
    f.backend.answer(Answer::Found(f.backend.last()));
    f.refresh().await.unwrap();
    let before = f.account.storage_test_fixture().handle.load_account().await.unwrap().unwrap();
    let sent = f.backend.sent.lock().unwrap().len();
    assert_eq!(
        f.account
            .revoke_device(target.installation_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::RecordUnavailable
    );
    let view = f.view().await;
    assert_eq!(view.publication.pending_notifications.len(), 1);
    assert_eq!(f.backend.sent.lock().unwrap().len(), sent);
    let after = f.account.storage_test_fixture().handle.load_account().await.unwrap().unwrap();
    {
        let calls = notifier.calls.lock().unwrap();
        let (intent, at_notification) = &calls[0];
        assert!(!committed_notification(&before, intent));
        let mut wrong = intent.clone();
        wrong.nonce[0] ^= 1;
        assert!(!committed_notification(at_notification, &wrong));
        assert!(at_notification.payload != before.payload);
        assert!(at_notification.payload == after.payload);
    }
    f.account.request_suspend();
    while f.account.lifecycle_status() == 1 {
        tokio::task::yield_now().await;
    }
    // Verify the real encrypted snapshot, not only an in-memory notification.
    let Fixture {
        _sql,
        _dir: dir,
        slot,
        account,
        backend,
        clock,
    } = f;
    drop(account);
    drop(slot);
    drop(_sql);
    let mut store =
        sql_store::Store::open(dir.path(), support::protector()).unwrap();
    let stored = store.load().unwrap().unwrap();
    let signed_intent = view.publication.tombstones[0].to_bytes();
    assert!(
        stored
            .payload
            .windows(signed_intent.len())
            .any(|w| w == signed_intent)
    );
    drop(store);
    notifier.store.send_replace(None);
    notifier.fail.store(false, Ordering::SeqCst);
    let (_slot, account) = open_services(
        dir.path(),
        AccountServices::new(backend.clone(), clock.clone()).with_notifier(notifier.clone()),
    )
    .await;
    notifier.store.send_replace(Some(account.storage_test_fixture().handle));
    let _sql = sql_store::Store::from_fixture(account.storage_test_fixture());
    let resumed = account.snapshot().await.unwrap();
    assert_eq!(resumed.publication.pending_notifications.len(), 0);
    assert!(notifier.calls.lock().unwrap().len() >= 2);
    assert_eq!(
        backend.sent.lock().unwrap().len(),
        sent,
        "clock rollback during notification must block publication"
    );
    assert_eq!(resumed.publication.state, PublicationState::LocalBlocked);
    clock.set_unix_seconds(1_800_000_000);
    account.refresh_record(keys.peer_id()).await.unwrap();
    let public = parse_record(&backend.last(), keys.peer_id(), 1_800_000_000).unwrap();
    assert!(
        !public
            .devices
            .iter()
            .any(|d| d.installation_id == target.installation_id)
    );
    assert_eq!(
        account.snapshot().await.unwrap().publication.state,
        PublicationState::Pending
    );
}

#[path = "support/sql_account_store.rs"]
mod sql_store;
