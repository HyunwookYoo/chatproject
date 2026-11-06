//! Key resources and legacy fixtures shared by the SQL migration test binaries.
use crate::support::{protector, state};
use chat_core::account::storage::{KeyProtector, Store};
use chat_core::{
    CoreError, ErrorCode,
    account::{Secret32, StoreId},
};
use std::fs;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

pub struct Resources {
    pub root: PathBuf,
    calls: AtomicUsize,
    pub fail_at: AtomicUsize,
    pub fail_delete: AtomicBool,
}
pub fn resources(root: &Path) -> Arc<Resources> {
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
    pub fn slot(&self, id: StoreId) -> PathBuf {
        self.root
            .join(id.0.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }
    pub fn count(&self) -> usize {
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

pub fn legacy(root: &Path) {
    let mut old = Store::open(root, resources(root)).unwrap();
    old.commit(&state(17)).unwrap();
}
pub fn cleanup_fixture(dir: tempfile::TempDir, case: &str) {
    let root = fs::canonicalize(dir.path()).unwrap();
    assert!(root.starts_with(fs::canonicalize(std::env::temp_dir()).unwrap()));
    dir.close().unwrap();
    assert!(!root.exists());
    println!("owned fixture cleanup case={case} removed=true");
}
