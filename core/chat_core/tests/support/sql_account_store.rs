//! Synchronous fixture facade over the real SQL worker, for existing disk-codec tests.
#![allow(dead_code)]
use chat_core::{
    CoreError,
    account::storage::{KeyProtector, StoredPlaintext},
    storage_fixture::Fixture,
};
use std::{path::Path, sync::Arc};

fn blocking<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> T {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    })
    .join()
    .unwrap()
}
pub struct Store(Option<Fixture>);
impl Store {
    pub fn from_fixture(fixture: Fixture) -> Self {
        Self(Some(fixture))
    }
    pub fn open(root: &Path, protector: Arc<dyn KeyProtector>) -> Result<Self, CoreError> {
        let root = root.to_owned();
        blocking(async move { Fixture::open(&root, protector).await }).map(|f| Self(Some(f)))
    }
    pub fn load(&mut self) -> Result<Option<StoredPlaintext>, CoreError> {
        let h = self.0.as_ref().unwrap().handle.clone();
        blocking(async move { h.load_account().await })
    }
    pub fn commit(&mut self, state: &StoredPlaintext) -> Result<(), CoreError> {
        let h = self.0.as_ref().unwrap().handle.clone();
        let state = StoredPlaintext {
            installation: state.installation,
            payload: state.payload.clone(),
        };
        blocking(async move { h.commit_account(state).await })
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            blocking(f.close());
        }
    }
}

pub fn assert_closed_and_removed(path: std::path::PathBuf, source: Fixture) {
    let observed = (source.live_connections(), path.exists());
    if observed.1 {
        // Clean only the exact owned RED-test root after joining its SQL stop.
        blocking(source.close());
        let resolved = path.canonicalize().unwrap();
        let temp_root = std::env::temp_dir().canonicalize().unwrap();
        assert!(resolved == path && resolved.starts_with(&temp_root) && resolved != temp_root);
        std::fs::remove_dir_all(resolved).unwrap();
    }
    assert_eq!(observed, (0, false), "fixture SQL/root outlived its owner");
}

/// The failed source stays fenced without a concurrent reopen; observe actual
/// connection closure before copying its persistent DB/WAL and protection files.
pub fn snapshot_closed(root: &Path, protector: Arc<dyn KeyProtector>, source: &Fixture) -> StoredPlaintext {
    assert_eq!(source.live_connections(), 0);
    let mirror = tempfile::tempdir().unwrap();
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file()
            && entry.file_name() != ".account.lock"
            && entry.file_name() != "db.sqlite3-shm"
        {
            std::fs::copy(entry.path(), mirror.path().join(entry.file_name())).unwrap();
        }
    }
    Store::open(mirror.path(), protector)
        .unwrap()
        .load()
        .unwrap()
        .unwrap()
}
