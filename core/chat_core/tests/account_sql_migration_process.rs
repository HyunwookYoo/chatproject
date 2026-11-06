#![cfg(all(feature = "test-hooks", debug_assertions))]
//! SQL migration tests that kill a child process mid-migration, in their own
//! test binary and one at a time.
//!
//! A spawned child holds copies of this process's descriptors until its exec
//! closes them. On Linux that copy keeps a store's flock alive after the store
//! closed, so a test that closes and reopens a store while another test spawns
//! sees WriterBusy (or its child does). Every test here spawns, so each holds
//! `ONE_AT_A_TIME` for its whole run; tests that never spawn belong in
//! account_sql_migration.rs.
#[path = "support/m4_store.rs"]
#[allow(dead_code)]
mod support;
#[path = "support/sql_migration.rs"]
#[allow(dead_code)]
mod sql_migration;
use chat_core::storage_fixture::Fixture;
use sql_migration::{cleanup_fixture, legacy, resources};
use std::fs;
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use support::state;

static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
    let _serial = ONE_AT_A_TIME.lock().await;
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
    let _serial = ONE_AT_A_TIME.lock().await;
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
    let _serial = ONE_AT_A_TIME.lock().await;
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
    let _serial = ONE_AT_A_TIME.lock().await;
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
    let _serial = ONE_AT_A_TIME.lock().await;
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
async fn changed_legacy_source_is_preserved_and_rejected() {
    let _serial = ONE_AT_A_TIME.lock().await;
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
