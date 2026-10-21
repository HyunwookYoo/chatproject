#![cfg(all(feature = "test-hooks", debug_assertions))]
#[path = "support/m4_store.rs"]
mod support;
use chat_core::{
    CoreError, ErrorCode,
    account::{Secret32, StoreId, storage::KeyProtector},
};
use chat_core::{
    account::storage::Store,
    storage_fixture::{Fixture, assert_account_preserved, migration_states},
};
use sha2::{Digest, Sha256};
use std::fs;
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use subtle::ConstantTimeEq;
use support::{protector, state};

struct Resources {
    root: PathBuf,
    calls: AtomicUsize,
    fail_at: AtomicUsize,
    fail_delete: AtomicBool,
}
fn resources(root: &Path) -> Arc<Resources> {
    let root = root.join("fixture-resources");
    fs::create_dir_all(&root).unwrap();
    Arc::new(Resources {
        root,
        calls: AtomicUsize::new(0),
        fail_at: AtomicUsize::new(0),
        fail_delete: AtomicBool::new(false),
    })
}
impl Resources {
    fn slot(&self, id: StoreId) -> PathBuf {
        self.root
            .join(id.0.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }
    fn count(&self) -> usize {
        fs::read_dir(&self.root).unwrap().count()
    }
}
impl KeyProtector for Resources {
    fn protect(&self, id: StoreId, secret: &Secret32) -> Result<Vec<u8>, CoreError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.fail_at.load(Ordering::SeqCst) {
            return Err(CoreError::new(
                ErrorCode::StorageLocked,
                "fixture_protect_failed",
            ));
        }
        fs::write(self.slot(id), b"owned-fixture-resource").unwrap();
        protector().protect(id, secret)
    }
    fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
        if !self.slot(id).is_file() {
            return Err(CoreError::new(
                ErrorCode::StorageMissing,
                "fixture_resource_missing",
            ));
        }
        protector().unprotect(id, blob)
    }
    fn delete(&self, id: StoreId) -> Result<(), CoreError> {
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err(CoreError::new(
                ErrorCode::StorageLocked,
                "fixture_delete_failed",
            ));
        }
        match fs::remove_file(self.slot(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => panic!("fixture resource deletion failed"),
        }
    }
}

fn legacy(root: &Path) {
    let mut old = Store::open(root, resources(root)).unwrap();
    old.commit(&state(17)).unwrap();
}
fn cleanup_fixture(dir: tempfile::TempDir, case: &str) {
    let root = fs::canonicalize(dir.path()).unwrap();
    assert!(root.starts_with(fs::canonicalize(std::env::temp_dir()).unwrap()));
    dir.close().unwrap();
    assert!(!root.exists());
    println!("owned fixture cleanup case={case} removed=true");
}
fn kill_at(root: &Path, stage: &str) {
    let marker = root.join("migration.checkpoint");
    let _ = fs::remove_file(&marker);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "migration_child", "--nocapture"])
        .env("CHAT_M4_MIGRATION_ROOT", root)
        .env("CHAT_M4_MIGRATION_STAGE", stage)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let start = Instant::now();
    while !marker.is_file() {
        if child.try_wait().unwrap().is_some() || start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "child {pid} missed {stage}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fs::read_to_string(&marker).unwrap(), stage);
    assert!(
        child.try_wait().unwrap().is_none(),
        "checkpoint child exited before kill"
    );
    child.kill().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    println!(
        "owned child pid={pid} stage={stage} killed status={}",
        output.status
    );
}

#[test]
#[ignore = "owned process-kill child only"]
fn migration_child() {
    let root = std::env::var_os("CHAT_M4_MIGRATION_ROOT").expect("owned child root");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let f = Fixture::open(Path::new(&root), resources(Path::new(&root)))
            .await
            .unwrap();
        f.close().await;
    });
    panic!("checkpoint not reached");
}

#[tokio::test]
async fn every_migration_boundary_restarts_idempotently() {
    for stage in [
        "Prepared",
        "KeyProtected",
        "KeysPrepared",
        "BeforeImport",
        "AfterImport",
        "Reopened",
        "BeforeMode",
        "AfterMode",
        "BeforeKeyDelete",
        "AfterKeyDelete",
        "CleanupComplete",
    ] {
        let dir = tempfile::tempdir().unwrap();
        legacy(dir.path());
        let guard = fs::read(dir.path().join("store.id")).unwrap();
        kill_at(dir.path(), stage);
        for _ in 0..2 {
            let f = Fixture::open(dir.path(), resources(dir.path()))
                .await
                .unwrap();
            let loaded = f.handle.load_account().await.unwrap().unwrap();
            assert!(
                bool::from(loaded.payload.ct_eq(&state(17).payload)),
                "{stage}"
            );
            assert_eq!(fs::read(dir.path().join("store.id")).unwrap(), guard);
            f.close().await;
        }
        assert!(!dir.path().join("account.bin").exists());
        cleanup_fixture(dir, stage);
    }
}

#[tokio::test]
async fn sql_authority_never_falls_back() {
    for (mode_published, damage) in [
        (false, "missing"),
        (true, "missing"),
        (false, "truncated"),
        (true, "truncated"),
        (false, "corrupt"),
        (true, "missing_keys"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        legacy(dir.path());
        kill_at(
            dir.path(),
            if mode_published {
                "AfterMode"
            } else {
                "AfterImport"
            },
        );
        let before = fs::read(dir.path().join("account.bin")).unwrap();
        let db = dir.path().join("db.sqlite3");
        match damage {
            "missing" => fs::remove_file(&db).unwrap(),
            "missing_keys" => fs::remove_file(dir.path().join("db.keys")).unwrap(),
            "truncated" => {
                fs::write(&db, []).unwrap();
                for name in ["db.sqlite3-wal", "db.sqlite3-shm"] {
                    let _ = fs::remove_file(dir.path().join(name));
                }
            }
            "corrupt" => {
                fs::write(&db, [0x37; 4096]).unwrap();
                for name in ["db.sqlite3-wal", "db.sqlite3-shm"] {
                    let _ = fs::remove_file(dir.path().join(name));
                }
            }
            _ => unreachable!(),
        }
        let damaged_db = fs::read(&db).ok();
        assert!(
            Fixture::open(dir.path(), resources(dir.path()))
                .await
                .is_err(),
            "damaged SQL authority accepted: {mode_published}/{damage}"
        );
        assert!(
            fs::read(&db).ok() == damaged_db,
            "rejection changed DB {mode_published}/{damage}"
        );
        assert_eq!(fs::read(dir.path().join("account.bin")).unwrap(), before);
        cleanup_fixture(dir, damage);
    }
}

#[tokio::test]
async fn committed_sql_wal_recovers_original_identity_and_state() {
    let dir = tempfile::tempdir().unwrap();
    legacy(dir.path());
    kill_at(dir.path(), "AfterImport");
    assert!(
        fs::metadata(dir.path().join("db.sqlite3-wal"))
            .unwrap()
            .len()
            > 32
    );
    let material = fs::read(dir.path().join("db.keys")).unwrap();
    fs::write(dir.path().join("db.sqlite3"), [0x37; 4096]).unwrap();
    let f = Fixture::open(dir.path(), resources(dir.path()))
        .await
        .unwrap();
    let loaded = f.handle.load_account().await.unwrap().unwrap();
    assert_eq!(loaded.installation, state(17).installation);
    assert!(bool::from(loaded.payload.ct_eq(&state(17).payload)));
    assert!(fs::read(dir.path().join("db.keys")).unwrap() == material);
    f.close().await;
    cleanup_fixture(dir, "committed-wal");
}

#[tokio::test]
async fn uncertain_precreation_and_precommit_gaps_preserve_artifacts() {
    for stage in ["DatabaseStarted", "CommitStarted"] {
        let dir = tempfile::tempdir().unwrap();
        legacy(dir.path());
        kill_at(dir.path(), stage);
        let before = fs::read(dir.path().join("account.bin")).unwrap();
        let keys = fs::read(dir.path().join("db.keys")).unwrap();
        for _ in 0..2 {
            assert!(
                Fixture::open(dir.path(), resources(dir.path()))
                    .await
                    .is_err(),
                "{stage}"
            );
            assert_eq!(fs::read(dir.path().join("account.bin")).unwrap(), before);
            assert!(fs::read(dir.path().join("db.keys")).unwrap() == keys);
        }
        println!("uncertainty stage={stage} rejected repeatedly; source/material preserved");
        cleanup_fixture(dir, stage);
    }
}

#[tokio::test]
async fn unbound_sql_artifact_during_key_preparation_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    legacy(dir.path());
    kill_at(dir.path(), "KeysPrepared");
    fs::write(dir.path().join("db.sqlite3"), []).unwrap();
    assert!(
        Fixture::open(dir.path(), resources(dir.path()))
            .await
            .is_err()
    );
    assert_eq!(
        fs::metadata(dir.path().join("db.sqlite3")).unwrap().len(),
        0
    );
    assert!(dir.path().join("account.bin").is_file());
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
    fs::remove_dir(&alias).unwrap(); // Remove only the owned link, never recursively follow it.
}

#[tokio::test]
async fn changed_legacy_source_is_preserved_and_rejected() {
    for stage in ["Prepared", "AfterImport", "AfterMode"] {
        let dir = tempfile::tempdir().unwrap();
        legacy(dir.path());
        kill_at(dir.path(), stage);
        let path = dir.path().join("account.bin");
        let mut changed = fs::read(&path).unwrap();
        let end = changed.len() - 1;
        changed[end] ^= 1;
        fs::write(&path, &changed).unwrap();
        assert!(
            Fixture::open(dir.path(), resources(dir.path()))
                .await
                .is_err(),
            "{stage}"
        );
        assert_eq!(fs::read(path).unwrap(), changed);
        cleanup_fixture(dir, stage);
    }
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
        let mut old = Store::open(dir.path(), p.clone()).unwrap();
        assert!(old.load().is_err());
        assert!(old.commit(&state(18)).is_err());
        drop(old);
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
    let mut old = Store::open(dir.path(), resources(dir.path())).unwrap();
    assert!(old.load().is_err());
    assert!(old.commit(&state(18)).is_err());
}
