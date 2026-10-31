use super::{status, MAX_BLOB};
use chat_core::{
    account::{storage::KeyProtector, Secret32, StoreId},
    CoreError, ErrorCode,
};
use std::sync::OnceLock;
use zeroize::Zeroizing;

type KeyTransform = unsafe extern "C" fn(
    id: *const u8,
    input: *const u8,
    len: usize,
    out: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> i32;
type KeyDelete = unsafe extern "C" fn(id: *const u8) -> i32;

struct NativeKeys {
    protect: KeyTransform,
    unprotect: KeyTransform,
    delete: KeyDelete,
}

// The Keychain functions live in Runner, but this library is linked into the
// chat_ffi framework (and a cdylib), where Runner symbols cannot be resolved at
// link time. Runner hands them over through cp_key_register before Flutter starts.
static NATIVE_KEYS: OnceLock<NativeKeys> = OnceLock::new();

/// 0 registered, 1 already registered (the first set is kept), 3 a null pointer.
#[no_mangle]
pub extern "C" fn cp_key_register(
    protect: Option<KeyTransform>,
    unprotect: Option<KeyTransform>,
    delete: Option<KeyDelete>,
) -> i32 {
    let (Some(protect), Some(unprotect), Some(delete)) = (protect, unprotect, delete) else {
        return 3;
    };
    match NATIVE_KEYS.set(NativeKeys {
        protect,
        unprotect,
        delete,
    }) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn native_keys() -> Result<&'static NativeKeys, CoreError> {
    NATIVE_KEYS
        .get()
        .ok_or_else(|| super::error(ErrorCode::NotAvailable))
}

pub(super) struct IosProtector;

impl KeyProtector for IosProtector {
    fn protect(&self, id: StoreId, secret: &Secret32) -> Result<Vec<u8>, CoreError> {
        let keys = native_keys()?;
        let mut output = vec![0; MAX_BLOB];
        let mut length = 0;
        status(unsafe {
            (keys.protect)(
                id.0.as_ptr(),
                secret.as_bytes().as_ptr(),
                32,
                output.as_mut_ptr(),
                output.len(),
                &mut length,
            )
        })?;
        if length == 0 || length > output.len() {
            return Err(super::error(ErrorCode::StorageCorrupt));
        }
        output.truncate(length);
        Ok(output)
    }
    fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
        super::validate_blob(blob)?;
        let keys = native_keys()?;
        let mut output = Zeroizing::new([0u8; 32]);
        let mut length = 0;
        status(unsafe {
            (keys.unprotect)(
                id.0.as_ptr(),
                blob.as_ptr(),
                blob.len(),
                output.as_mut_ptr(),
                output.len(),
                &mut length,
            )
        })?;
        if length != 32 {
            return Err(super::error(ErrorCode::StorageCorrupt));
        }
        Ok(Secret32::from(*output))
    }
    fn delete(&self, id: StoreId) -> Result<(), CoreError> {
        let keys = native_keys()?;
        status(unsafe { (keys.delete)(id.0.as_ptr()) })
    }
}

/// Lifecycle ABI status is distinct from the KeyProtector ABI above:
/// 0 completed, 1 pending (keep gated), 2 failed (keep gated).
#[no_mangle]
pub extern "C" fn cp_account_suspend() -> i32 {
    std::panic::catch_unwind(|| crate::global::CORE.account_suspend()).unwrap_or(2)
}

#[no_mangle]
pub extern "C" fn cp_account_resume() -> i32 {
    std::panic::catch_unwind(|| crate::global::CORE.account_resume()).unwrap_or(2)
}

// Apple-target source coverage. Windows execution of core lifecycle tests is
// deliberately not presented as execution of this ABI or UIKit callbacks.
#[cfg(all(test, feature = "test-hooks", debug_assertions))]
mod lifecycle_tests {
    use super::*;
    use crate::global::{BUS, CORE};
    use chat_core::{CoreConfig, NetworkOp};
    use std::{sync::Arc, time::Duration};
    struct Protector;
    impl KeyProtector for Protector {
        fn protect(&self, _: StoreId, key: &Secret32) -> Result<Vec<u8>, CoreError> {
            Ok(key.as_bytes().iter().map(|b| b ^ 0x96).collect())
        }
        fn unprotect(&self, _: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
            if blob.len() != 32 { return Err(super::super::error(ErrorCode::StorageCorrupt)); }
            Ok(Secret32::from(std::array::from_fn(|i| blob[i] ^ 0x96)))
        }
        fn delete(&self, _: StoreId) -> Result<(), CoreError> { Ok(()) }
    }
    #[tokio::test]
    async fn lifecycle_abi_controls_the_same_core_actor() {
        let dir = tempfile::tempdir().unwrap();
        CORE.start(CoreConfig { data_dir: dir.path().into() }, &BUS).unwrap();
        let network = CORE.network().unwrap();
        network.configure_local(None).await.unwrap();
        let account = CORE.open_account(Arc::new(Protector)).unwrap();
        account.prepare_candidate().await.unwrap();
        assert_eq!(cp_account_suspend(), 1);
        assert_eq!(network.execute(NetworkOp::Start).await.unwrap_err().code, ErrorCode::Busy);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while cp_account_suspend() == 1 {
            assert!(std::time::Instant::now() < deadline);
            tokio::task::yield_now().await;
        }
        assert_eq!(cp_account_suspend(), 0);
        assert_eq!(cp_account_resume(), 1);
        while cp_account_resume() == 1 {
            assert!(std::time::Instant::now() < deadline);
            tokio::task::yield_now().await;
        }
        assert_eq!(cp_account_resume(), 0);
        network.execute(NetworkOp::Start).await.unwrap();
        network.execute(NetworkOp::Stop).await.unwrap();
    }
}
