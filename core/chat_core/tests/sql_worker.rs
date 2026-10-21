#![cfg(all(feature = "test-hooks", debug_assertions))]
#[path = "support/m4_store.rs"]
mod support;
use chat_core::{
    ErrorCode,
    storage_fixture::{CommitFault, Fixture},
};
use std::{fs, sync::atomic::Ordering, time::Duration};
use support::{protector, state};

#[tokio::test(flavor = "current_thread")]
async fn db_key_and_salt_use_distinct_owned_slots() {
    let dir = tempfile::tempdir().unwrap();
    let p = protector();
    let f = Fixture::open(dir.path(), p.clone()).await.unwrap();
    let bytes = fs::read(dir.path().join("db.keys")).unwrap();
    let mut d = minicbor::Decoder::new(&bytes);
    assert_eq!(d.array().unwrap(), Some(6));
    assert_eq!(d.u8().unwrap(), 1);
    let db = d.bytes().unwrap().to_vec();
    let key = d.bytes().unwrap().to_vec();
    let salt = d.bytes().unwrap().to_vec();
    assert_ne!(key, salt);
    assert_ne!(db, key);
    assert_ne!(db, salt);
    let legacy = fs::read(dir.path().join("store.id")).unwrap();
    assert_ne!(key, legacy);
    assert_ne!(salt, legacy);
    assert!(!dir.path().join("db.keys.pending").exists());
    f.handle.commit_account(state(1)).await.unwrap();
    f.close().await;
    p.deny_salt.store(true, Ordering::SeqCst);
    assert!(Fixture::open(dir.path(), p.clone()).await.is_err());
    assert_eq!(bytes, fs::read(dir.path().join("db.keys")).unwrap());
    p.deny_salt.store(false, Ordering::SeqCst);
    let f = Fixture::open(dir.path(), p).await.unwrap();
    assert_eq!(
        &*f.handle.load_account().await.unwrap().unwrap().payload,
        &*state(1).payload
    );
    f.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn worker_queue_busy_does_not_block_network() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::open(dir.path(), protector()).await.unwrap();
    let pause = f.pause().await;
    pause.wait_entered().await;
    let mut jobs = Vec::new();
    for _ in 0..32 {
        let h = f.handle.clone();
        jobs.push(tokio::spawn(async move { h.load_account().await }));
    }
    for _ in 0..1000 {
        if f.remaining_capacity() == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(f.remaining_capacity(), 0);
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(100), f.handle.load_account())
            .await
            .unwrap()
            .err()
            .unwrap()
            .code,
        ErrorCode::Busy
    );
    let receiver = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sender
        .send_to(b"worker-busy-loopback", receiver.local_addr().unwrap())
        .await
        .unwrap();
    let mut packet = [0; 64];
    let (len, _) =
        tokio::time::timeout(Duration::from_millis(100), receiver.recv_from(&mut packet))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(&packet[..len], b"worker-busy-loopback");
    pause.release();
    for j in jobs {
        assert!(j.await.unwrap().is_ok());
    }
    f.close().await;
}

#[tokio::test]
async fn suspend_closes_actual_connection() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::open(dir.path(), protector()).await.unwrap();
    f.handle.commit_account(state(2)).await.unwrap();
    assert_eq!(f.live_connections(), 1);
    f.handle.suspend().await.unwrap();
    assert_eq!(f.live_connections(), 0);
    fs::rename(dir.path().join("db.sqlite3"), dir.path().join("closed.db")).unwrap();
    fs::rename(dir.path().join("closed.db"), dir.path().join("db.sqlite3")).unwrap();
    assert_eq!(
        f.handle.commit_account(state(3)).await.unwrap_err().code,
        ErrorCode::StorageLocked
    );
    f.handle.resume().await.unwrap();
    assert_eq!(f.live_connections(), 1);
    assert_eq!(
        &*f.handle.load_account().await.unwrap().unwrap().payload,
        &*state(2).payload
    );
    let pause = f.pause().await;
    pause.wait_entered().await;
    let h = f.handle.clone();
    let stale = tokio::spawn(async move { h.resume().await });
    wait_capacity(&f, 31).await;
    f.handle.invalidate(100);
    pause.release();
    assert_eq!(
        stale.await.unwrap().unwrap_err().code,
        ErrorCode::StorageLocked
    );
    assert!(f.handle.commit_account(state(3)).await.is_err());
    f.handle.suspend().await.unwrap();
    assert_eq!(f.live_connections(), 0);
    f.close().await;
}

#[tokio::test]
async fn ambiguous_commit_reopens_without_duplicate() {
    for fault in [
        CommitFault::WriteDenied,
        CommitFault::BeforeCommit,
        CommitFault::AfterCommit,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let f = Fixture::open(dir.path(), protector()).await.unwrap();
        f.handle.commit_account(state(4)).await.unwrap();
        let before = f.opens();
        f.fault(fault).await;
        assert_eq!(
            f.handle.commit_account(state(5)).await.unwrap_err().code,
            ErrorCode::StorageUncertain
        );
        assert_eq!(f.live_connections(), 0);
        assert_eq!(
            &*f.handle.load_account().await.unwrap().unwrap().payload,
            &*match fault {
                CommitFault::AfterCommit => state(5),
                _ => state(4),
            }
            .payload
        );
        assert!(f.opens() > before);
        assert_eq!(f.account_rows().await, 1);
        f.handle.commit_account(state(5)).await.unwrap();
        f.close().await;
    }
}

#[tokio::test]
async fn existing_or_incomplete_material_is_never_reinitialized() {
    for artifact in [
        "db.sqlite3",
        "db.keys.pending",
        ".db.keys.tmp",
        "storage.mode",
        "account.bin",
    ] {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(artifact), b"owned-public-fixture").unwrap();
        assert!(
            Fixture::open(dir.path(), protector()).await.is_err(),
            "{artifact}"
        );
        assert_eq!(
            fs::read(dir.path().join(artifact)).unwrap(),
            b"owned-public-fixture"
        );
        assert!(!dir.path().join("db.keys").exists());
    }
}

#[tokio::test]
async fn separate_native_threads_cannot_own_same_writer() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::open(dir.path(), protector()).await.unwrap();
    let root = dir.path().to_owned();
    let code = std::thread::spawn(move || Fixture::open_blocking_error(&root, protector()))
        .join()
        .unwrap();
    assert_eq!(code, ErrorCode::WriterBusy);
    f.close().await;
}

#[tokio::test]
async fn malformed_or_changed_material_fails_closed() {
    for variant in [
        "padding",
        "salt",
        "trailing",
        "noncanonical",
        "oversize",
        "guard",
        "mode",
        "legacy_snapshot",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let p = protector();
        let f = Fixture::open(dir.path(), p.clone()).await.unwrap();
        f.handle.commit_account(state(7)).await.unwrap();
        f.close().await;
        let path = dir.path().join("db.keys");
        let mut bytes = fs::read(&path).unwrap();
        match variant {
            "padding" => {
                *bytes.last_mut().unwrap() ^= 1;
                fs::write(&path, &bytes).unwrap();
            }
            "salt" => {
                let index = bytes.len() - 32;
                bytes[index] ^= 1;
                fs::write(&path, &bytes).unwrap();
            }
            "noncanonical" => {
                bytes.splice(1..2, [0x18, 1]);
                fs::write(&path, &bytes).unwrap();
            }
            "trailing" => {
                bytes.push(0);
                fs::write(&path, &bytes).unwrap();
            }
            "oversize" => {
                fs::write(&path, vec![0; 65_537]).unwrap();
            }
            "guard" => {
                fs::write(dir.path().join("store.id"), [0; 16]).unwrap();
            }
            "mode" => {
                fs::write(dir.path().join("storage.mode"), b"foreign").unwrap();
            }
            "legacy_snapshot" => {
                fs::write(dir.path().join("account.bin"), b"foreign-public-fixture").unwrap();
            }
            _ => unreachable!(),
        }
        let preserved = fs::read(&path).unwrap();
        assert!(Fixture::open(dir.path(), p).await.is_err(), "{variant}");
        assert_eq!(preserved, fs::read(path).unwrap());
    }
}

#[tokio::test]
async fn canonical_non_ascii_long_root_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir
        .path()
        .join("한글 저장소")
        .join("a".repeat(140))
        .join("b".repeat(140));
    let f = Fixture::open(&root, protector()).await.unwrap();
    f.handle.commit_account(state(8)).await.unwrap();
    f.close().await;
    let f = Fixture::open(&root, protector()).await.unwrap();
    assert_eq!(
        &*f.handle.load_account().await.unwrap().unwrap().payload,
        &*state(8).payload
    );
    f.close().await;
}

#[tokio::test(flavor = "current_thread")]
async fn suspend_fences_an_already_queued_resume() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::open(dir.path(), protector()).await.unwrap();
    let pause = f.pause().await;
    pause.wait_entered().await;
    let h = f.handle.clone();
    let resume = tokio::spawn(async move { h.resume().await });
    wait_capacity(&f, 31).await;
    let h = f.handle.clone();
    let suspend = tokio::spawn(async move { h.suspend().await });
    wait_capacity(&f, 30).await;
    pause.release();
    assert_eq!(
        resume.await.unwrap().unwrap_err().code,
        ErrorCode::StorageLocked
    );
    suspend.await.unwrap().unwrap();
    assert_eq!(f.live_connections(), 0);
    assert!(f.handle.commit_account(state(9)).await.is_err());
    f.close().await;
}

#[tokio::test]
async fn external_lifecycle_ticket_is_exact_and_stale_ticket_cannot_resume() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::open(dir.path(), protector()).await.unwrap();
    f.handle.invalidate(10);
    assert_eq!(f.handle.accepted_ticket(), 10);
    f.handle.invalidate(10);
    assert_eq!(f.handle.accepted_ticket(), 10);
    f.handle.suspend().await.unwrap();
    assert_eq!(f.handle.accepted_ticket(), 10);
    f.handle.resume().await.unwrap();
    f.handle.invalidate(9);
    assert_eq!(f.handle.accepted_ticket(), 10);
    assert_eq!(
        f.handle.resume().await.unwrap_err().code,
        ErrorCode::StorageLocked
    );
    f.handle.invalidate(11);
    assert_eq!(f.handle.accepted_ticket(), 11);
    f.handle.resume().await.unwrap();
    f.close().await;
}

#[tokio::test]
async fn failed_os_protection_preserves_owned_pending_ids() {
    let dir = tempfile::tempdir().unwrap();
    let p = protector();
    p.fail_protect.store(true, Ordering::SeqCst);
    assert!(Fixture::open(dir.path(), p.clone()).await.is_err());
    let pending = fs::read(dir.path().join("db.keys.pending")).unwrap();
    let mut d = minicbor::Decoder::new(&pending);
    assert_eq!(d.array().unwrap(), Some(4));
    assert_eq!(d.u8().unwrap(), 1);
    let db = d.bytes().unwrap().to_vec();
    let key = d.bytes().unwrap().to_vec();
    let salt = d.bytes().unwrap().to_vec();
    assert_eq!(db.len(), 16);
    assert_eq!(key.len(), 16);
    assert_eq!(salt.len(), 16);
    assert_ne!(db, key);
    assert_ne!(db, salt);
    assert_ne!(key, salt);
    assert!(!dir.path().join("db.keys").exists());
    p.fail_protect.store(false, Ordering::SeqCst);
    assert!(Fixture::open(dir.path(), p).await.is_err());
    assert_eq!(
        pending,
        fs::read(dir.path().join("db.keys.pending")).unwrap()
    );
}
#[tokio::test]
async fn metadata_io_failure_never_acknowledges_initialization() {
    use chat_core::{
        CoreError,
        account::{Secret32, StoreId, storage::KeyProtector},
    };
    use std::{path::PathBuf, sync::Arc};
    struct ObstructMetadata {
        root: PathBuf,
        inner: support::Protector,
    }
    impl KeyProtector for ObstructMetadata {
        fn protect(&self, id: StoreId, secret: &Secret32) -> Result<Vec<u8>, CoreError> {
            // Actual owned filesystem IO fault at metadata publication, after pending IDs exist.
            assert!(self.root.join("db.keys.pending").is_file());
            fs::create_dir_all(self.root.join(".db.keys.tmp")).unwrap();
            self.inner.protect(id, secret)
        }
        fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
            self.inner.unprotect(id, blob)
        }
        fn delete(&self, id: StoreId) -> Result<(), CoreError> {
            self.inner.delete(id)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let p = Arc::new(ObstructMetadata {
        root: dir.path().to_owned(),
        inner: Default::default(),
    });
    assert_eq!(
        Fixture::open(dir.path(), p).await.err().unwrap().code,
        ErrorCode::StorageFull
    );
    assert!(dir.path().join("db.keys.pending").is_file());
    assert!(!dir.path().join("db.keys").exists());
    assert!(!dir.path().join("db.sqlite3").exists());
    assert!(Fixture::open(dir.path(), protector()).await.is_err());
    assert!(dir.path().join(".db.keys.tmp").is_dir());
}

#[tokio::test]
async fn invalid_account_payload_does_not_replace_committed_state() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::open(dir.path(), protector()).await.unwrap();
    f.handle.commit_account(state(12)).await.unwrap();
    let mut invalid = state(13);
    invalid.installation = [0; 32];
    assert_eq!(
        f.handle.commit_account(invalid).await.unwrap_err().code,
        ErrorCode::StorageCorrupt
    );
    assert_eq!(
        &*f.handle.load_account().await.unwrap().unwrap().payload,
        &*state(12).payload
    );
    f.close().await;
}

async fn wait_capacity(f: &Fixture, capacity: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while f.remaining_capacity() != capacity {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker fixture did not queue request");
}
#[tokio::test(flavor = "current_thread")]
async fn already_queued_reply_is_rejected_after_invalidation() {
    let dir = tempfile::tempdir().unwrap();
    let f = Fixture::open(dir.path(), protector()).await.unwrap();
    let mut resume = Box::pin(f.handle.resume());
    assert!(futures_util::poll!(&mut resume).is_pending());
    let pause = f.pause().await;
    pause.wait_entered().await;
    f.handle.invalidate(20);
    pause.release();
    assert_eq!(resume.await.unwrap_err().code, ErrorCode::StorageLocked);
    assert!(f.handle.commit_account(state(14)).await.is_err());
    f.close().await;
}

#[tokio::test]
async fn oversized_os_wrapping_blob_keeps_pending_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let p = protector();
    p.oversized_wrap.store(true, Ordering::SeqCst);
    assert_eq!(
        Fixture::open(dir.path(), p).await.err().unwrap().code,
        ErrorCode::StorageFull
    );
    assert!(dir.path().join("db.keys.pending").is_file());
    assert!(!dir.path().join("db.keys").exists());
    assert!(Fixture::open(dir.path(), protector()).await.is_err());
}
