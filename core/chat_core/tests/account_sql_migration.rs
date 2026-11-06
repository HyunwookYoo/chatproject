#![cfg(all(feature = "test-hooks", debug_assertions))]
//! The migration tests that kill a child process mid-migration live in
//! account_sql_migration_process.rs: a spawn here would let the child's copy
//! of a store's lock descriptor outlive that store's close (see there).
#[path = "support/m4_store.rs"]
mod support;
#[path = "support/sql_migration.rs"]
mod sql_migration;
use chat_core::{
    CoreError, ErrorCode,
    account::{Secret32, StoreId, storage::KeyProtector},
};
use chat_core::{
    account::storage::Store,
    storage_fixture::{Fixture, assert_account_preserved, migration_states},
};
use sha2::{Digest, Sha256};
use sql_migration::{Resources, cleanup_fixture, legacy, resources};
use std::fs;
use std::path::Path;
#[cfg(windows)]
use std::process::Command;
use std::sync::{Arc, Mutex, atomic::Ordering};
use subtle::ConstantTimeEq;
use support::{protector, state};

#[derive(Clone, Copy)]
enum RecoveryFault {
    None,
    MissingKey,
    MissingSalt,
    WrongKey,
    WrongSalt,
}
struct PersistedProtector {
    resources: Arc<Resources>,
    legacy: StoreId,
    fault: RecoveryFault,
    slots: Mutex<Vec<StoreId>>,
    events: Mutex<Vec<&'static str>>,
}
impl KeyProtector for PersistedProtector {
    fn protect(&self, id: StoreId, secret: &Secret32) -> Result<Vec<u8>, CoreError> {
        let blob = self.resources.protect(id, secret)?;
        self.slots.lock().unwrap().push(id);
        Ok(blob)
    }
    fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
        if id == self.legacy {
            return self.resources.unprotect(id, blob);
        }
        let slot = self
            .slots
            .lock()
            .unwrap()
            .iter()
            .position(|s| *s == id)
            .unwrap();
        self.events
            .lock()
            .unwrap()
            .push(if slot == 0 { "read-key" } else { "read-salt" });
        if matches!(
            (self.fault, slot),
            (RecoveryFault::MissingKey, 0) | (RecoveryFault::MissingSalt, 1)
        ) {
            return Err(CoreError::new(
                ErrorCode::StorageMissing,
                "fixture_saved_material_unreadable",
            ));
        }
        let secret = self.resources.unprotect(id, blob)?;
        if matches!(
            (self.fault, slot),
            (RecoveryFault::WrongKey, 0) | (RecoveryFault::WrongSalt, 1)
        ) {
            return Ok(Secret32::from(std::array::from_fn(|i| {
                secret.as_bytes()[i] ^ u8::from(i == 0)
            })));
        }
        Ok(secret)
    }
    fn delete(&self, id: StoreId) -> Result<(), CoreError> {
        if id == self.legacy {
            self.events.lock().unwrap().push("delete-legacy");
        }
        self.resources.delete(id)
    }
}
fn persisted_protector(root: &Path, fault: RecoveryFault) -> Arc<PersistedProtector> {
    legacy(root);
    Arc::new(PersistedProtector {
        resources: resources(root),
        legacy: StoreId(fs::read(root.join("store.id")).unwrap().try_into().unwrap()),
        fault,
        slots: Mutex::new(Vec::new()),
        events: Mutex::new(Vec::new()),
    })
}
async fn rejected_persisted_recovery(fault: RecoveryFault) {
    let dir = tempfile::tempdir().unwrap();
    let p = persisted_protector(dir.path(), fault);
    let snapshot = fs::read(dir.path().join("account.bin")).unwrap();
    let wrapping = fs::read(dir.path().join("key.wrap")).unwrap();
    let result = Fixture::open(dir.path(), p.clone()).await;
    if let Ok(f) = result {
        f.close().await;
        panic!("unrecoverable saved DB material accepted");
    }
    assert!(fs::read(dir.path().join("account.bin")).unwrap() == snapshot);
    assert!(fs::read(dir.path().join("key.wrap")).unwrap() == wrapping);
    assert_eq!(fs::read(dir.path().join("store.id")).unwrap(), p.legacy.0);
    assert!(p.resources.slot(p.legacy).is_file());
    assert!(!dir.path().join("storage.mode").exists());
    let events = p.events.lock().unwrap();
    assert!(events.contains(&"read-key"));
    assert!(!events.contains(&"delete-legacy"));
    drop(events);
    assert_eq!(p.slots.lock().unwrap().len(), 2);
    for slot in p.slots.lock().unwrap().iter() {
        assert!(p.resources.slot(*slot).is_file());
    }
    cleanup_fixture(dir, "unrecoverable-persisted-material");
}
#[tokio::test]
async fn persisted_db_material_missing_key_preserves_legacy() {
    rejected_persisted_recovery(RecoveryFault::MissingKey).await;
}
#[tokio::test]
async fn persisted_db_material_missing_salt_preserves_legacy() {
    rejected_persisted_recovery(RecoveryFault::MissingSalt).await;
}
#[tokio::test]
async fn persisted_db_material_wrong_key_preserves_legacy() {
    rejected_persisted_recovery(RecoveryFault::WrongKey).await;
}
#[tokio::test]
async fn persisted_db_material_wrong_salt_preserves_legacy() {
    rejected_persisted_recovery(RecoveryFault::WrongSalt).await;
}
#[tokio::test]
async fn persisted_db_material_is_recovered_before_legacy_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let p = persisted_protector(dir.path(), RecoveryFault::None);
    let f = Fixture::open(dir.path(), p.clone()).await.unwrap();
    let loaded = f.handle.load_account().await.unwrap().unwrap();
    assert_eq!(loaded.installation, state(17).installation);
    assert!(bool::from(loaded.payload.ct_eq(&state(17).payload)));
    f.close().await;
    assert_eq!(
        *p.events.lock().unwrap(),
        vec!["read-key", "read-salt", "delete-legacy"]
    );
    assert!(!p.resources.slot(p.legacy).exists());
    assert_eq!(p.resources.count(), 2);
    assert!(dir.path().join("storage.mode").is_file());
    cleanup_fixture(dir, "verified-persisted-material");
}


#[tokio::test]
async fn partial_key_resources_and_cleanup_failure_resume_only_owned_work() {
    let dir = tempfile::tempdir().unwrap();
    let p = resources(dir.path());
    let mut old = Store::open(dir.path(), p.clone()).unwrap();
    old.commit(&state(17)).unwrap();
    drop(old);
    let original_id = StoreId(
        fs::read(dir.path().join("store.id"))
            .unwrap()
            .try_into()
            .unwrap(),
    );
    let foreign = p.root.join("foreign-resource");
    fs::write(&foreign, b"retain-unowned-resource").unwrap();
    p.fail_at.store(3, Ordering::SeqCst); // SQL key succeeded, SQL salt failed.
    assert!(Fixture::open(dir.path(), p.clone()).await.is_err());
    assert_eq!(p.count(), 3);
    assert!(dir.path().join("db.keys.pending").is_file());
    let pending = fs::read(dir.path().join("db.keys.pending")).unwrap();
    let mut d = minicbor::Decoder::new(&pending);
    d.array().unwrap();
    d.u8().unwrap();
    d.bytes().unwrap();
    let partial_id = StoreId(d.bytes().unwrap().try_into().unwrap());
    let absent_salt = StoreId(d.bytes().unwrap().try_into().unwrap());
    assert!(p.slot(original_id).is_file());
    assert!(p.slot(partial_id).is_file());
    assert!(!p.slot(absent_salt).exists());
    p.fail_at.store(0, Ordering::SeqCst);
    p.fail_delete.store(true, Ordering::SeqCst);
    assert!(Fixture::open(dir.path(), p.clone()).await.is_err()); // Own pending resources cannot yet be removed.
    assert!(dir.path().join("account.bin").is_file());
    p.fail_delete.store(false, Ordering::SeqCst);
    let f = Fixture::open(dir.path(), p.clone()).await.unwrap();
    f.close().await;
    assert_eq!(p.count(), 3); // Two NEW DB slots plus the unrelated external resource.
    assert!(!p.slot(original_id).exists());
    assert!(!p.slot(partial_id).exists());
    assert_eq!(fs::read(&foreign).unwrap(), b"retain-unowned-resource");
    let keys = fs::read(dir.path().join("db.keys")).unwrap();
    let mut d = minicbor::Decoder::new(&keys);
    d.array().unwrap();
    d.u8().unwrap();
    d.bytes().unwrap();
    for _ in 0..2 {
        assert!(
            p.slot(StoreId(d.bytes().unwrap().try_into().unwrap()))
                .is_file()
        );
    }
    fs::write(dir.path().join("unrelated"), b"keep").unwrap();
    let f = Fixture::open(dir.path(), p.clone()).await.unwrap();
    f.close().await;
    assert_eq!(fs::read(dir.path().join("unrelated")).unwrap(), b"keep");
    assert_eq!(p.count(), 3);

    let dir = tempfile::tempdir().unwrap();
    legacy(dir.path());
    let p = resources(dir.path());
    p.fail_delete.store(true, Ordering::SeqCst);
    assert!(Fixture::open(dir.path(), p.clone()).await.is_err());
    assert!(dir.path().join("storage.mode").is_file());
    assert!(dir.path().join("store.id").is_file());
    assert!(!dir.path().join("account.bin").exists());
    p.fail_delete.store(false, Ordering::SeqCst);
    let f = Fixture::open(dir.path(), p.clone()).await.unwrap();
    assert!(bool::from(
        f.handle
            .load_account()
            .await
            .unwrap()
            .unwrap()
            .payload
            .ct_eq(&state(17).payload)
    ));
    f.close().await;
    assert_eq!(p.count(), 2);
}

#[tokio::test]
async fn active_discard_is_rejected_and_never_active_phases_clear_transactionally() {
    for (index, state) in migration_states().into_iter().enumerate().skip(1) {
        let dir = tempfile::tempdir().unwrap();
        let p = resources(dir.path());
        let state = state.unwrap();
        let mut old = Store::open(dir.path(), p.clone()).unwrap();
        old.commit(&state).unwrap();
        drop(old);
        let f = Fixture::open(dir.path(), p.clone()).await.unwrap();
        if index == 4 {
            assert_eq!(
                f.handle.discard_pending().await.unwrap_err().code,
                ErrorCode::AccountExists
            );
            assert_account_preserved(&state, &f.handle.load_account().await.unwrap().unwrap());
        } else {
            f.handle.discard_pending().await.unwrap();
            assert!(f.handle.load_account().await.unwrap().is_none());
        }
        f.close().await;
        let f = Fixture::open(dir.path(), p).await.unwrap();
        if index == 4 {
            assert_account_preserved(&state, &f.handle.load_account().await.unwrap().unwrap());
        } else {
            assert!(f.handle.load_account().await.unwrap().is_none());
        }
        f.close().await;
    }
}

#[tokio::test]
async fn unknown_schema_is_preserved_and_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let p = resources(dir.path());
    let f = Fixture::open(dir.path(), p.clone()).await.unwrap();
    f.handle.commit_account(state(17)).await.unwrap();
    f.close().await;
    Fixture::set_unknown_schema(dir.path(), p.clone()).unwrap();
    let keys = fs::read(dir.path().join("db.keys")).unwrap();
    assert!(Fixture::open(dir.path(), p).await.is_err());
    assert!(fs::read(dir.path().join("db.keys")).unwrap() == keys);
}

#[tokio::test]
async fn canonical_and_symlink_aliases_share_writer_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir
        .path()
        .join("한글")
        .join("a".repeat(140))
        .join("b".repeat(140));
    fs::create_dir_all(&root).unwrap();
    legacy(&root);
    let alias = dir.path().join("alias");
    let target = dir.path().join("한글");
    assert!(!alias.starts_with(&target));
    let owned_parent = fs::canonicalize(dir.path()).unwrap();
    assert!(
        fs::canonicalize(&target)
            .unwrap()
            .starts_with(&owned_parent)
    );
    assert!(
        fs::canonicalize(alias.parent().unwrap())
            .unwrap()
            .starts_with(&owned_parent)
    );
    #[cfg(windows)]
    if let Err(error) = std::os::windows::fs::symlink_dir(&target, &alias) {
        assert_eq!(error.raw_os_error(), Some(1314));
        let output = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&alias)
            .arg(&target)
            .output()
            .unwrap();
        assert!(output.status.success(), "owned junction failed");
        println!(
            "native symlink privilege1314; owned directory junction exercises canonical alias instead"
        );
    }
    #[cfg(windows)]
    println!(
        "alias creation succeeded; directory reparse/symlink={}",
        fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    let alias_root = alias.join("a".repeat(140)).join("b".repeat(140));
    assert_eq!(
        fs::canonicalize(&alias_root).unwrap(),
        fs::canonicalize(&root).unwrap()
    );
    let p = resources(&root);
    let f = Fixture::open(&root.join("."), p.clone()).await.unwrap();
    assert_eq!(
        Fixture::open_blocking_error(&alias_root, p.clone()),
        ErrorCode::WriterBusy
    );
    f.close().await;
    let f = Fixture::open(&alias_root, p.clone()).await.unwrap();
    assert!(bool::from(
        f.handle
            .load_account()
            .await
            .unwrap()
            .unwrap()
            .payload
            .ct_eq(&state(17).payload)
    ));
    f.handle.suspend().await.unwrap();
    assert_eq!(
        Fixture::open_blocking_error(&root, p.clone()),
        ErrorCode::WriterBusy
    );
    f.handle.resume().await.unwrap();
    f.close().await;
    let preserved = fs::read(root.join("db.sqlite3")).unwrap();
    // Unlink only the exact owned alias, never recursively follow its target.
    #[cfg(unix)]
    fs::remove_file(&alias).unwrap();
    #[cfg(windows)]
    fs::remove_dir(&alias).unwrap();
    assert!(fs::symlink_metadata(&alias).is_err());
    assert!(target.is_dir());
    assert!(root.is_dir());
    assert_eq!(fs::read(root.join("db.sqlite3")).unwrap(), preserved);
}

#[tokio::test]
async fn pending_discard_keeps_sql_authority() {
    let dir = tempfile::tempdir().unwrap();
    legacy(dir.path());
    let f = Fixture::open(dir.path(), resources(dir.path()))
        .await
        .unwrap();
    let keys = fs::read(dir.path().join("db.keys")).unwrap();
    let mode = fs::read(dir.path().join("storage.mode")).unwrap();
    let guard = fs::read(dir.path().join("store.id")).unwrap();
    f.handle.discard_pending().await.unwrap();
    assert!(f.handle.load_account().await.unwrap().is_none());
    assert_eq!(f.account_rows().await, 0);
    f.close().await;
    let f = Fixture::open(dir.path(), resources(dir.path()))
        .await
        .unwrap();
    assert!(f.handle.load_account().await.unwrap().is_none());
    assert!(fs::read(dir.path().join("db.keys")).unwrap() == keys);
    assert_eq!(fs::read(dir.path().join("storage.mode")).unwrap(), mode);
    assert_eq!(fs::read(dir.path().join("store.id")).unwrap(), guard);
    f.handle.commit_account(state(18)).await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn all_legacy_states_round_trip() {
    for expected in migration_states() {
        let dir = tempfile::tempdir().unwrap();
        let p = resources(dir.path());
        let mut old = Store::open(dir.path(), p.clone()).unwrap();
        if let Some(expected) = &expected {
            old.commit(expected).unwrap();
        }
        drop(old);
        let guard = fs::read(dir.path().join("store.id")).ok();
        let source_hash = expected.as_ref().map(|_| {
            let mut h = Sha256::new();
            for name in ["store.id", "key.wrap", "account.bin"] {
                let bytes = fs::read(dir.path().join(name)).unwrap();
                h.update((name.len() as u64).to_be_bytes());
                h.update(name.as_bytes());
                h.update((bytes.len() as u64).to_be_bytes());
                h.update(bytes);
            }
            <[u8; 32]>::from(h.finalize())
        });
        let f = Fixture::open(dir.path(), p.clone()).await.unwrap();
        let loaded = f.handle.load_account().await.unwrap();
        match (&expected, &loaded) {
            (None, None) => {}
            (Some(expected), Some(loaded)) => assert_account_preserved(expected, loaded),
            _ => panic!("account phase lost"),
        }
        if let Some(guard) = &guard {
            assert_eq!(&fs::read(dir.path().join("store.id")).unwrap(), guard);
        }
        assert!(dir.path().join("storage.mode").is_file());
        let bytes = fs::read(dir.path().join("storage.mode")).unwrap();
        let mut d = minicbor::Decoder::new(&bytes);
        assert_eq!(d.array().unwrap(), Some(3));
        assert_eq!(d.u8().unwrap(), 1);
        let db = d.bytes().unwrap().to_vec();
        assert_eq!(db.len(), 16);
        if let Some(h) = source_hash {
            assert_eq!(d.bytes().unwrap(), h);
        } else {
            d.null().unwrap();
        }
        assert_eq!(d.position(), bytes.len());
        let keys = fs::read(dir.path().join("db.keys")).unwrap();
        let mut d = minicbor::Decoder::new(&keys);
        assert_eq!(d.array().unwrap(), Some(6));
        assert_eq!(d.u8().unwrap(), 1);
        assert_eq!(d.bytes().unwrap(), db);
        f.close().await;
        assert!(!dir.path().join("account.bin").exists());
        assert!(!dir.path().join("key.wrap").exists());
        assert!(Store::open(dir.path(), p.clone()).is_err());
        let f = Fixture::open(dir.path(), p).await.unwrap();
        let loaded = f.handle.load_account().await.unwrap();
        if let Some(expected) = &expected {
            assert_account_preserved(expected, loaded.as_ref().unwrap());
        } else {
            assert!(loaded.is_none());
        }
        f.close().await;
    }
}

#[tokio::test]
async fn old_binary_guard_survives_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::open(dir.path(), resources(dir.path()))
        .await
        .unwrap();
    assert_eq!(fs::read(dir.path().join("store.id")).unwrap().len(), 16);
    assert!(dir.path().join("storage.mode").exists());
    f.close().await;
    assert!(Store::open(dir.path(), resources(dir.path())).is_err());
}

// ---- schema v1 -> v2 (M5) -------------------------------------------------------------

/// A real M4-shaped database: one conversation, three messages, two codec-1 outbox
/// rows (distinct retry facts) and one cancelled message. Built by the real message
/// repo on a fresh DB, then rewritten to the exact v1 shape by the narrow fixture.
async fn v1_database(root: &Path, p: Arc<dyn KeyProtector>) {
    use chat_core::account::PeerId;
    use chat_core::storage_fixture::MessageId;
    let f = Fixture::open(root, p.clone()).await.unwrap();
    f.handle
        .commit_account(migration_states().pop().unwrap().unwrap())
        .await
        .unwrap();
    let r = f.messages().await.unwrap();
    let peer = PeerId([2; 32]);
    let a = r
        .enqueue(peer, MessageId([1; 16]), "first".into(), 1)
        .await
        .unwrap();
    let b = r
        .enqueue(peer, MessageId([2; 16]), "second".into(), 2)
        .await
        .unwrap();
    let c = r
        .enqueue(peer, MessageId([3; 16]), "cancelled".into(), 3)
        .await
        .unwrap();
    r.cancel_send(c.key).await.unwrap();
    f.saturate_message_attempts(a.key, false).await.unwrap();
    f.set_outbox_retry(b.key, 3, 4000, 123_456).await.unwrap();
    f.close().await;
    Fixture::downgrade_to_v1(root, p).unwrap();
}

#[tokio::test]
async fn v1_db_upgrades_to_v2_preserving_messages_and_outbox() {
    let dir = tempfile::tempdir().unwrap();
    let p: Arc<dyn KeyProtector> = protector();
    v1_database(dir.path(), p.clone()).await;
    let before = Fixture::probe_database(dir.path(), p.clone()).unwrap();
    assert_eq!((before.user_version, before.schema_version), (1, 1));
    assert!(!before.tables.iter().any(|t| t.starts_with("mls_")));
    assert_eq!(before.message_count, 3);
    assert_eq!(before.outbox.len(), 2);
    assert!(
        before
            .outbox
            .iter()
            .all(|o| o.codec == 1 && !o.wire.is_empty() && o.reencrypts.is_none())
    );

    let f = Fixture::open(dir.path(), p.clone()).await.unwrap();
    let r = f.messages().await.unwrap();
    assert_eq!(r.list_conversations(None, 50).await.unwrap().items.len(), 1);
    f.close().await;
    let after = Fixture::probe_database(dir.path(), p.clone()).unwrap();
    assert_eq!((after.user_version, after.schema_version), (2, 2));
    for table in [
        "mls_kv",
        "mls_conversation",
        "mls_delivery",
        "mls_hold",
        "mls_peer",
        "mls_receipt",
    ] {
        assert!(after.tables.iter().any(|t| t == table), "{table}");
    }
    // `message` rows (cancelled one included) are byte-for-byte unchanged.
    assert_eq!(after.message_rows, before.message_rows);
    assert_eq!(after.outbox.len(), 2);
    for (old, new) in before.outbox.iter().zip(&after.outbox) {
        assert_eq!(new.msg_id, old.msg_id);
        assert_eq!(new.codec, 3);
        assert!(new.wire.is_empty());
        assert_eq!(
            (new.attempts, new.retry_delay_ms, new.next_retry_at_ms),
            (old.attempts, old.retry_delay_ms, old.next_retry_at_ms)
        );
        assert_eq!(new.dispatch_state, old.dispatch_state);
        assert_eq!((new.reencrypts, new.waiting_peer_update), (Some(0), Some(0)));
    }
    assert_eq!(after.outbox[0].attempts, u32::MAX as i64);
    assert_eq!(after.outbox[1].next_retry_at_ms, 123_456);

    // A second open is a no-op on a v2 database.
    Fixture::open(dir.path(), p.clone()).await.unwrap().close().await;
    assert_eq!(Fixture::probe_database(dir.path(), p).unwrap(), after);
}

#[tokio::test]
async fn upgraded_schema_matches_a_fresh_v2_schema() {
    let upgraded = tempfile::tempdir().unwrap();
    let fresh = tempfile::tempdir().unwrap();
    let p: Arc<dyn KeyProtector> = protector();
    v1_database(upgraded.path(), p.clone()).await;
    Fixture::open(upgraded.path(), p.clone()).await.unwrap().close().await;
    Fixture::open(fresh.path(), p.clone()).await.unwrap().close().await;
    let a = Fixture::probe_database(upgraded.path(), p.clone()).unwrap();
    let b = Fixture::probe_database(fresh.path(), p).unwrap();
    assert!(a.schema.len() > 10);
    assert_eq!(a.schema, b.schema);
}

#[tokio::test]
async fn v2_db_rejected_by_v1_reader() {
    let dir = tempfile::tempdir().unwrap();
    let p: Arc<dyn KeyProtector> = protector();
    Fixture::open(dir.path(), p.clone()).await.unwrap().close().await;
    let error = Fixture::v1_reader_check(dir.path(), p.clone()).unwrap_err();
    assert_eq!(error.code, ErrorCode::StorageCorrupt);
    // The replica is a real v1 reader: it accepts a genuine v1 database.
    let v1 = tempfile::tempdir().unwrap();
    v1_database(v1.path(), p.clone()).await;
    Fixture::v1_reader_check(v1.path(), p).unwrap();
}

#[tokio::test]
async fn upgrade_interrupted_keeps_v1() {
    let dir = tempfile::tempdir().unwrap();
    let p: Arc<dyn KeyProtector> = protector();
    v1_database(dir.path(), p.clone()).await;
    let before = Fixture::probe_database(dir.path(), p.clone()).unwrap();
    Fixture::fail_next_upgrade(dir.path());
    assert!(Fixture::open(dir.path(), p.clone()).await.is_err());
    // Everything the failed transaction touched was rolled back.
    let interrupted = Fixture::probe_database(dir.path(), p.clone()).unwrap();
    assert_eq!(interrupted, before);
    assert_eq!(interrupted.user_version, 1);
    // The next open migrates again and succeeds.
    Fixture::open(dir.path(), p.clone()).await.unwrap().close().await;
    let after = Fixture::probe_database(dir.path(), p).unwrap();
    assert_eq!((after.user_version, after.schema_version), (2, 2));
    assert_eq!(after.message_rows, before.message_rows);
    assert!(after.outbox.iter().all(|o| o.codec == 3));
}
