#[path = "support/account_store.rs"]
mod support;

use chat_core::account::storage::{KeyProtector, Store, StoredPlaintext};
use chat_core::account::{Secret32, StoreId};
use chat_core::{CoreError, ErrorCode};
#[cfg(not(target_os = "ios"))]
use std::process::Command;
use std::{fs, sync::Arc};
use support::{protector, state};

const LIMIT: usize = 4 * 1024 * 1024;

fn open_error(path: &std::path::Path) -> ErrorCode {
    match Store::open(path, protector()) {
        Ok(mut store) => match store.load() {
            Ok(_) => panic!("invalid store accepted"),
            Err(e) => e.code,
        },
        Err(e) => e.code,
    }
}

#[test]
fn encrypted_round_trip_random_nonce_and_relocated_store() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("original");
    let mut store = Store::open(&root, protector()).unwrap();
    assert!(store.load().unwrap().is_none());
    assert!(!root.join("store.id").exists());
    store.commit(&state(b"sensitive-payload-sentinel")).unwrap();
    let first = fs::read(root.join("account.bin")).unwrap();
    assert!(
        !first
            .windows(26)
            .any(|w| w == b"sensitive-payload-sentinel")
    );
    store.commit(&state(b"sensitive-payload-sentinel")).unwrap();
    assert_ne!(first, fs::read(root.join("account.bin")).unwrap());
    drop(store);
    let moved = parent.path().join("relocated");
    fs::rename(root, &moved).unwrap();
    let mut store = Store::open(&moved, protector()).unwrap();
    let loaded = store.load().unwrap().unwrap();
    assert_eq!(loaded.installation, [0x31; 32]);
    assert_eq!(&*loaded.payload, b"sensitive-payload-sentinel");
}

#[test]
fn envelope_decrypts_with_independently_constructed_required_aad() {
    use aes_gcm::{
        Aes256Gcm, KeyInit, Nonce,
        aead::{Aead, Payload},
    };
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), protector()).unwrap();
    store.commit(&state(b"known plaintext")).unwrap();
    let wrapped = fs::read(dir.path().join("key.wrap")).unwrap();
    // Decode the test protector format, without calling storage's key/codec helpers.
    let key: [u8; 32] = std::array::from_fn(|i| wrapped[16 + i] ^ 0xa7);
    let encoded = fs::read(dir.path().join("account.bin")).unwrap();
    let mut d = minicbor::Decoder::new(&encoded);
    assert_eq!(d.array().unwrap(), Some(5));
    assert_eq!(d.u8().unwrap(), 1);
    let id = d.bytes().unwrap();
    assert_eq!(id, fs::read(dir.path().join("store.id")).unwrap());
    assert_eq!(id.len(), 16);
    let installation = d.bytes().unwrap();
    assert_eq!(installation, [0x31; 32]);
    let nonce = d.bytes().unwrap();
    assert_eq!(nonce.len(), 12);
    let ciphertext = d.bytes().unwrap();
    let mut aad = b"ChatProject/store/v1\x01".to_vec();
    aad.extend_from_slice(id);
    aad.extend_from_slice(&[0x31; 32]);
    let plaintext = Aes256Gcm::new_from_slice(&key)
        .unwrap()
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .unwrap();
    assert_eq!(plaintext, b"known plaintext");
    assert_eq!(d.position(), encoded.len());
}

#[test]
fn tamper_aad_nonce_version_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), protector()).unwrap();
    store.commit(&state(b"private")).unwrap();
    drop(store);
    let file = dir.path().join("account.bin");
    let original = fs::read(&file).unwrap();
    // Decode only the public envelope to find each field independently.
    let mut d = minicbor::Decoder::new(&original);
    assert_eq!(d.array().unwrap(), Some(5));
    let version = d.position();
    d.u8().unwrap();
    let id = d.bytes().unwrap().as_ptr() as usize - original.as_ptr() as usize;
    let installation = d.bytes().unwrap().as_ptr() as usize - original.as_ptr() as usize;
    let nonce = d.bytes().unwrap().as_ptr() as usize - original.as_ptr() as usize;
    let cipher = d.bytes().unwrap().as_ptr() as usize - original.as_ptr() as usize;
    for offset in [version, id, installation, nonce, cipher, original.len() - 1] {
        let mut changed = original.clone();
        changed[offset] ^= 1;
        fs::write(&file, changed).unwrap();
        assert_eq!(open_error(dir.path()), ErrorCode::StorageCorrupt);
    }
    fs::write(&file, [&original[..], &[0][..]].concat()).unwrap();
    assert_eq!(open_error(dir.path()), ErrorCode::StorageCorrupt);
}

#[test]
fn limits_preserve_old_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), protector()).unwrap();
    store.commit(&state(b"old")).unwrap();
    let original = fs::read(dir.path().join("account.bin")).unwrap();
    // Envelope overhead is 87 bytes at the largest valid payload size.
    let largest = state(&vec![0x55; LIMIT - 87]);
    store.commit(&largest).unwrap();
    assert_eq!(
        fs::metadata(dir.path().join("account.bin")).unwrap().len(),
        LIMIT as u64
    );
    assert_eq!(
        store
            .commit(&state(&vec![0x55; LIMIT - 86]))
            .unwrap_err()
            .code,
        ErrorCode::StorageFull
    );
    assert_eq!(store.load().unwrap().unwrap().payload.len(), LIMIT - 87);
    fs::write(dir.path().join("account.bin"), original).unwrap();
    assert_eq!(
        store.commit(&state(&vec![0; LIMIT])).unwrap_err().code,
        ErrorCode::StorageFull
    );
    assert_eq!(&*store.load().unwrap().unwrap().payload, b"old");
    drop(store);
    fs::File::create(dir.path().join("account.bin"))
        .unwrap()
        .set_len(LIMIT as u64 + 1)
        .unwrap();
    assert_eq!(open_error(dir.path()), ErrorCode::StorageFull);
}

#[test]
fn missing_wrapping_key_never_creates_new_account() {
    for missing in ["key.wrap", "store.id"] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), protector()).unwrap();
        store.commit(&state(b"existing-account")).unwrap();
        drop(store);
        let before = fs::read(dir.path().join("account.bin")).unwrap();
        fs::remove_file(dir.path().join(missing)).unwrap();
        assert!(matches!(
            open_error(dir.path()),
            ErrorCode::StorageMissing | ErrorCode::StorageCorrupt
        ));
        assert_eq!(fs::read(dir.path().join("account.bin")).unwrap(), before);
    }
}

#[test]
fn incomplete_initialization_is_not_fresh_and_unknown_files_survive() {
    for artifact in ["store.id", "key.wrap", ".account.bin.tmp"] {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(artifact), [7; 16]).unwrap();
        fs::write(dir.path().join("unrelated"), b"keep").unwrap();
        assert!(matches!(
            open_error(dir.path()),
            ErrorCode::StorageMissing | ErrorCode::StorageCorrupt
        ));
        assert_eq!(fs::read(dir.path().join(artifact)).unwrap(), [7; 16]);
        assert_eq!(fs::read(dir.path().join("unrelated")).unwrap(), b"keep");
    }
}

#[test]
fn corrupt_snapshot_cannot_be_overwritten_by_commit() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), protector()).unwrap();
    store.commit(&state(b"original")).unwrap();
    fs::write(dir.path().join("account.bin"), b"corrupt").unwrap();
    assert_eq!(
        store.commit(&state(b"replacement")).unwrap_err().code,
        ErrorCode::StorageCorrupt
    );
    assert_eq!(
        fs::read(dir.path().join("account.bin")).unwrap(),
        b"corrupt"
    );
}

struct FailedProtection;
impl KeyProtector for FailedProtection {
    fn protect(&self, _: StoreId, _: &Secret32) -> Result<Vec<u8>, CoreError> {
        Err(CoreError::new(
            ErrorCode::StorageLocked,
            "OS key unavailable",
        ))
    }
    fn unprotect(&self, _: StoreId, _: &[u8]) -> Result<Secret32, CoreError> {
        panic!("unexpected key read")
    }
    fn delete(&self, _: StoreId) -> Result<(), CoreError> {
        Ok(())
    }
}

#[test]
fn protection_failure_leaves_explicit_pending_initialization() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), Arc::new(FailedProtection)).unwrap();
    assert_eq!(
        store.commit(&state(b"candidate")).unwrap_err().code,
        ErrorCode::StorageLocked
    );
    drop(store);
    assert_eq!(fs::metadata(dir.path().join("store.id")).unwrap().len(), 16);
    assert!(!dir.path().join("account.bin").exists());
    assert_eq!(open_error(dir.path()), ErrorCode::StorageMissing);
}

struct UnavailableKey;
impl KeyProtector for UnavailableKey {
    fn protect(&self, _: StoreId, _: &Secret32) -> Result<Vec<u8>, CoreError> {
        panic!("must never replace unavailable key")
    }
    fn unprotect(&self, _: StoreId, _: &[u8]) -> Result<Secret32, CoreError> {
        Err(CoreError::new(
            ErrorCode::StorageLocked,
            "OS key unavailable",
        ))
    }
    fn delete(&self, _: StoreId) -> Result<(), CoreError> {
        panic!("must never delete unavailable key")
    }
}

#[test]
fn unavailable_os_key_never_regenerates_or_overwrites_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), protector()).unwrap();
    store.commit(&state(b"existing")).unwrap();
    drop(store);
    let original = fs::read(dir.path().join("account.bin")).unwrap();
    let wrapped = fs::read(dir.path().join("key.wrap")).unwrap();
    let mut locked = Store::open(dir.path(), Arc::new(UnavailableKey)).unwrap();
    assert_eq!(locked.load().err().unwrap().code, ErrorCode::StorageLocked);
    assert_eq!(
        locked.commit(&state(b"new")).unwrap_err().code,
        ErrorCode::StorageLocked
    );
    locked.suspend().unwrap();
    assert_eq!(locked.resume().unwrap_err().code, ErrorCode::StorageLocked);
    // Failed validation must not hand persistent ownership to another Store.
    #[cfg(not(target_os = "ios"))]
    assert!(matches!(
        Store::open(dir.path(), protector()),
        Err(CoreError {
            code: ErrorCode::WriterBusy,
            ..
        })
    ));
    assert_eq!(locked.load().err().unwrap().code, ErrorCode::StorageLocked);
    assert_eq!(fs::read(dir.path().join("account.bin")).unwrap(), original);
    assert_eq!(fs::read(dir.path().join("key.wrap")).unwrap(), wrapped);
    drop(locked);
    let mut other = Store::open(dir.path(), protector()).unwrap();
    assert_eq!(&*other.load().unwrap().unwrap().payload, b"existing");
}

#[test]
fn all_storage_files_are_bounded_before_secret_access() {
    for name in ["store.id", "key.wrap", "account.bin"] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), protector()).unwrap();
        store.commit(&state(b"existing")).unwrap();
        drop(store);
        fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join(name))
            .unwrap()
            .set_len(LIMIT as u64 + 1)
            .unwrap();
        let mut store = Store::open(dir.path(), Arc::new(NoSecrets)).unwrap();
        assert_eq!(store.load().err().unwrap().code, ErrorCode::StorageFull);
    }
}

#[test]
#[cfg(not(target_os = "ios"))]
fn suspend_blocks_access_and_retains_writer_until_drop() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), protector()).unwrap();
    store.commit(&state(b"saved")).unwrap();
    store.suspend().unwrap();
    assert_eq!(store.load().err().unwrap().code, ErrorCode::StorageLocked);
    assert_eq!(
        store.commit(&state(b"blocked")).unwrap_err().code,
        ErrorCode::StorageLocked
    );
    assert!(matches!(
        Store::open(dir.path(), protector()),
        Err(CoreError {
            code: ErrorCode::WriterBusy,
            ..
        })
    ));
    store.resume().unwrap();
    assert_eq!(&*store.load().unwrap().unwrap().payload, b"saved");
    assert!(matches!(
        Store::open(dir.path(), protector()),
        Err(CoreError {
            code: ErrorCode::WriterBusy,
            ..
        })
    ));
    drop(store);
    let mut other = Store::open(dir.path(), protector()).unwrap();
    assert_eq!(&*other.load().unwrap().unwrap().payload, b"saved");
}

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
