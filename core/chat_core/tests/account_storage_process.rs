//! Cross-process writer ownership, in its own test binary.
//!
//! Spawning a child duplicates this process's descriptors until the child's
//! exec closes them. On Linux a store opened by a concurrent test in the same
//! binary would keep its flock through that window and see WriterBusy on
//! reopen, so nothing else may run alongside the spawn.
#[path = "support/account_store.rs"]
mod support;

use chat_core::account::storage::{KeyProtector, Store, StoredPlaintext};
use chat_core::account::{Secret32, StoreId};
use chat_core::{CoreError, ErrorCode};
#[cfg(not(target_os = "ios"))]
use std::fs;
#[cfg(not(target_os = "ios"))]
use std::process::Command;
use std::sync::Arc;
#[cfg(not(target_os = "ios"))]
use support::{protector, state};

struct NoSecrets;
impl KeyProtector for NoSecrets {
    fn protect(&self, _: StoreId, _: &Secret32) -> Result<Vec<u8>, CoreError> {
        panic!("protect before ownership")
    }
    fn unprotect(&self, _: StoreId, _: &[u8]) -> Result<Secret32, CoreError> {
        panic!("unprotect before ownership")
    }
    fn delete(&self, _: StoreId) -> Result<(), CoreError> {
        panic!("delete before ownership")
    }
}

#[test]
fn ownership_child() {
    let Some(root) = std::env::var_os("CHAT_A2_OWNERSHIP_CHILD") else {
        return;
    };
    match Store::open(std::path::Path::new(&root), Arc::new(NoSecrets)) {
        Err(e) => assert_eq!(e.code, ErrorCode::WriterBusy),
        Ok(_) => panic!("second process acquired writer ownership"),
    }
}

#[test]
#[cfg(not(target_os = "ios"))]
fn two_processes_canonical_folder_one_writer() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), protector()).unwrap();
    store.commit(&state(b"secret")).unwrap();
    let competing_process = || {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "ownership_child", "--nocapture"])
            .env("CHAT_A2_OWNERSHIP_CHILD", dir.path().join("."))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    competing_process();
    store.suspend().unwrap();
    competing_process();
    store.resume().unwrap();
    fs::write(dir.path().join("account.bin"), b"corrupt").unwrap();
    store.suspend().unwrap();
    assert_eq!(store.resume().unwrap_err().code, ErrorCode::StorageCorrupt);
    competing_process();
    drop(store);
    assert!(Store::open(dir.path(), protector()).is_ok());
}
