#![cfg(all(feature = "test-hooks", debug_assertions))]
//! The account endpoint test that runs a competing writer in a child process,
//! in its own test binary.
//!
//! A spawned child holds copies of this process's descriptors until its exec
//! closes them. On Linux that copy keeps a store's flock alive after the store
//! closed, so a concurrent test in the same binary that reopens a store would
//! see WriterBusy. Nothing else may run alongside the spawn.
use chat_core::account::{
    Secret32, StoreId,
    storage::{KeyProtector, Store, StoredPlaintext},
};
use chat_core::{CoreConfig, CoreError, CoreSlot, ErrorCode, EventBus, NetworkOp};
use std::time::Duration;
#[allow(dead_code)]
#[path = "support/account_store.rs"]
mod support;
#[path = "support/account_services.rs"]
mod services;

fn core(dir: &std::path::Path) -> CoreSlot {
    let core = CoreSlot::new();
    core.start(
        CoreConfig {
            data_dir: dir.into(),
        },
        &EventBus::new(),
    )
    .unwrap();
    core
}
async fn settled(account: &chat_core::account::AccountHandle) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(account.lifecycle_status(), 0);
}

#[tokio::test]
async fn suspend_blocks_immediately_and_failed_resume_keeps_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let slot = core(dir.path());
    let net = slot.network().unwrap();
    net.configure_local(None).await.unwrap();
    let account = slot.open_account_with_services(support::protector(), services::offline()).unwrap();
    account.prepare_candidate().await.unwrap();
    competing_writer(dir.path());
    assert_eq!(account.request_suspend(), 1);
    assert_eq!(
        net.execute(NetworkOp::Start).await.unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(
        account.prepare_candidate().await.unwrap_err().code,
        ErrorCode::StorageLocked
    );
    settled(&account).await;
    competing_writer(dir.path());
    #[cfg(not(target_os = "ios"))]
    assert_eq!(
        Store::open(dir.path(), support::protector())
            .err()
            .unwrap()
            .code,
        ErrorCode::WriterBusy
    );
    std::fs::write(dir.path().join("storage.mode"), b"corrupt").unwrap();
    assert_eq!(account.request_resume(), 1);
    tokio::time::timeout(Duration::from_secs(10), async {
        while account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(account.lifecycle_status(), 2);
    competing_writer(dir.path());
    assert_eq!(
        net.execute(NetworkOp::Start).await.unwrap_err().code,
        ErrorCode::Busy
    );
    #[cfg(not(target_os = "ios"))]
    assert_eq!(
        Store::open(dir.path(), support::protector())
            .err()
            .unwrap()
            .code,
        ErrorCode::WriterBusy
    );
}

#[test]
fn account_writer_child() {
    let Some(path) = std::env::var_os("CP_A4_WRITER_PATH") else {
        return;
    };
    assert_eq!(
        Store::open(std::path::Path::new(&path), support::protector())
            .err()
            .unwrap()
            .code,
        ErrorCode::WriterBusy
    );
}
fn competing_writer(path: &std::path::Path) {
    #[cfg(not(target_os = "ios"))]
    {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "account_writer_child", "--nocapture"])
            .env("CP_A4_WRITER_PATH", path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let _ = path;
}
