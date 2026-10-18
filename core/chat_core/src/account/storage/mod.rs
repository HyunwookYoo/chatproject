//! Internal Rust storage capability; never expose it directly through FFI.
use super::{Secret32, StoreId};
use crate::{CoreError, ErrorCode};
use std::{fs, path::Path, sync::Arc};
use zeroize::Zeroizing;
mod atomic;
mod ownership;
mod schema;

pub trait KeyProtector: Send + Sync {
    fn protect(&self, id: StoreId, secret: &Secret32) -> Result<Vec<u8>, CoreError>;
    fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError>;
    /// Remove an external wrapping resource, if any. Absence is success so that
    /// interrupted initialization and interrupted cleanup can both be retried.
    fn delete(&self, id: StoreId) -> Result<(), CoreError>;
}

pub struct StoredPlaintext {
    pub installation: [u8; 32],
    pub payload: Zeroizing<Vec<u8>>,
}

pub struct Store {
    ownership: ownership::Ownership,
    protector: Arc<dyn KeyProtector>,
}

const ARTIFACTS: &[&str] = &[
    "store.id",
    "key.wrap",
    "account.bin",
    ".store.id.tmp",
    ".key.wrap.tmp",
    ".account.bin.tmp",
];

fn exists(path: &Path) -> Result<bool, CoreError> {
    path.try_exists()
        .map_err(|_| CoreError::new(ErrorCode::StorageMissing, "Cannot inspect account storage"))
}

fn store_id(root: &Path) -> Result<StoreId, CoreError> {
    let bytes = atomic::read_bounded(&root.join("store.id"))?;
    Ok(StoreId(bytes.as_slice().try_into().map_err(|_| {
        CoreError::new(ErrorCode::StorageCorrupt, "Invalid store identifier")
    })?))
}

fn load_locked(
    root: &Path,
    protector: &dyn KeyProtector,
) -> Result<Option<(StoreId, Secret32, StoredPlaintext)>, CoreError> {
    if !exists(&root.join("account.bin"))? {
        for name in ARTIFACTS {
            if exists(&root.join(name))? {
                return Err(CoreError::new(
                    ErrorCode::StorageMissing,
                    "Incomplete account initialization requires explicit cleanup",
                ));
            }
        }
        return Ok(None);
    }
    let id = store_id(root)?;
    let wrapped = atomic::read_bounded(&root.join("key.wrap"))?;
    let encoded = atomic::read_bounded(&root.join("account.bin"))?;
    let secret = protector.unprotect(id, &wrapped)?;
    let state = schema::unseal(id, &secret, &encoded)?;
    Ok(Some((id, secret, state)))
}

impl Store {
    pub fn open(root: &Path, protector: Arc<dyn KeyProtector>) -> Result<Self, CoreError> {
        Ok(Self {
            ownership: ownership::Ownership::open(root)?,
            protector,
        })
    }

    pub fn load(&mut self) -> Result<Option<StoredPlaintext>, CoreError> {
        self.ownership
            .with_lock(|root| Ok(load_locked(root, &*self.protector)?.map(|(_, _, state)| state)))
    }

    pub fn commit(&mut self, state: &StoredPlaintext) -> Result<(), CoreError> {
        self.ownership.with_lock(|root| {
            if state.payload.len() > schema::FILE_LIMIT - 87 {
                return Err(CoreError::new(
                    ErrorCode::StorageFull,
                    "Account snapshot exceeds limit",
                ));
            }
            let (id, secret) = match load_locked(root, &*self.protector)? {
                Some((id, secret, _)) => (id, secret),
                None => {
                    let mut id = StoreId([0; 16]);
                    getrandom::fill(&mut id.0).map_err(|_| {
                        CoreError::new(ErrorCode::NotAvailable, "Secure randomness unavailable")
                    })?;
                    // Publish the ID first: a failed OS protect leaves an explicit orphan,
                    // with the ID needed for a later authorized resource cleanup.
                    atomic::replace(root, "store.id", &id.0)?;
                    let secret = super::random_secret()?;
                    let wrapped = self.protector.protect(id, &secret)?;
                    if wrapped.len() > schema::FILE_LIMIT {
                        return Err(CoreError::new(
                            ErrorCode::StorageFull,
                            "Protected key exceeds limit",
                        ));
                    }
                    atomic::replace(root, "key.wrap", &wrapped)?;
                    (id, secret)
                }
            };
            let encoded = schema::seal(id, &secret, state)?;
            atomic::replace(root, "account.bin", &encoded)
        })
    }

    pub fn suspend(&mut self) -> Result<(), CoreError> {
        self.ownership.suspend();
        Ok(())
    }

    pub fn resume(&mut self) -> Result<(), CoreError> {
        self.ownership.resume()?;
        if let Err(error) = self.load() {
            self.ownership.suspend();
            return Err(error);
        }
        Ok(())
    }

    /// Physical cleanup only. AccountManager/B6 MUST first establish that this
    /// candidate has never been active. Opaque storage cannot authorize deletion.
    #[allow(dead_code)] // Called by the later AccountManager/B6 integration.
    pub(crate) fn discard_pending(&mut self) -> Result<(), CoreError> {
        self.ownership.with_lock(|root| {
            if exists(&root.join("store.id"))? {
                self.protector.delete(store_id(root)?)?;
            } else {
                // ID persistence precedes OS protection. Only the ID's own temp
                // file can therefore be known to predate an external key resource.
                for name in [
                    "key.wrap",
                    ".key.wrap.tmp",
                    "account.bin",
                    ".account.bin.tmp",
                ] {
                    if exists(&root.join(name))? {
                        return Err(CoreError::new(
                            ErrorCode::StorageMissing,
                            "Orphaned account resource has no store identifier",
                        ));
                    }
                }
            }
            // Keep the ID until all other artifacts are removed. A partial failure
            // remains explicitly incomplete and cannot silently generate new keys.
            for name in [
                "account.bin",
                ".account.bin.tmp",
                "key.wrap",
                ".key.wrap.tmp",
                ".store.id.tmp",
                "store.id",
            ] {
                match fs::remove_file(root.join(name)) {
                    Ok(()) => atomic::sync_directory(root)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => {
                        return Err(CoreError::new(
                            ErrorCode::StorageLocked,
                            "Pending account cleanup incomplete",
                        ));
                    }
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "../../../tests/support/account_store.rs"]
mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use test_support::{protector, state};

    #[test]
    fn authorized_pending_snapshot_cleanup_preserves_lock_and_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("unrelated"), b"keep").unwrap();
        let mut store = Store::open(dir.path(), protector()).unwrap();
        store.commit(&state(b"never-active-pending")).unwrap();
        store.discard_pending().unwrap();
        assert!(store.load().unwrap().is_none());
        assert!(!dir.path().join("account.bin").exists());
        assert!(!dir.path().join("key.wrap").exists());
        assert!(!dir.path().join("store.id").exists());
        assert!(dir.path().join(".account.lock").exists());
        assert_eq!(fs::read(dir.path().join("unrelated")).unwrap(), b"keep");
    }

    #[test]
    fn failed_pending_cleanup_remains_incomplete_instead_of_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), protector()).unwrap();
        store.commit(&state(b"pending")).unwrap();
        // A file deletion error after the first snapshot has been removed.
        fs::remove_file(dir.path().join("key.wrap")).unwrap();
        fs::create_dir(dir.path().join("key.wrap")).unwrap();
        assert!(store.discard_pending().is_err());
        assert!(dir.path().join("store.id").exists());
        assert!(store.load().is_err());
        assert!(store.commit(&state(b"fresh")).is_err());
    }

    #[test]
    fn cleanup_pre_protection_orphan_without_committed_id() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".store.id.tmp"), [0x27; 16]).unwrap();
        let mut store = Store::open(dir.path(), protector()).unwrap();
        assert!(store.load().is_err());
        store.discard_pending().unwrap();
        assert!(store.load().unwrap().is_none());
        assert!(!dir.path().join(".store.id.tmp").exists());
        // Without a committed ID, a protected blob/ciphertext is not known pre-protect.
        fs::write(dir.path().join("key.wrap"), b"orphan-resource").unwrap();
        assert!(store.discard_pending().is_err());
        assert_eq!(
            fs::read(dir.path().join("key.wrap")).unwrap(),
            b"orphan-resource"
        );
    }
}
