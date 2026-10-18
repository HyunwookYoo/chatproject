use chat_core::{
    account::{storage::KeyProtector, Secret32, StoreId},
    CoreError, ErrorCode,
};
use std::ptr;
use windows_sys::Win32::{
    Foundation::{GetLastError, LocalFree},
    Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    },
};
use zeroize::{Zeroize, Zeroizing};

pub(super) struct WindowsProtector;

impl KeyProtector for WindowsProtector {
    fn protect(&self, id: StoreId, secret: &Secret32) -> Result<Vec<u8>, CoreError> {
        Ok(crypt(id, secret.as_bytes(), true)?.to_vec())
    }
    fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
        let bytes = crypt(id, blob, false)?;
        let key: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| error(ErrorCode::StorageCorrupt))?;
        Ok(Secret32::from(key))
    }
    // DPAPI has no per-store external resource. The Store owns removal of key.wrap.
    fn delete(&self, _id: StoreId) -> Result<(), CoreError> {
        Ok(())
    }
}

fn error(code: ErrorCode) -> CoreError {
    CoreError::new(code, "Cannot access this device's protected account key")
}

fn crypt(id: StoreId, input: &[u8], protect: bool) -> Result<Zeroizing<Vec<u8>>, CoreError> {
    if input.is_empty() || input.len() > 16_384 {
        return Err(error(ErrorCode::StorageCorrupt));
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr().cast_mut(),
    };
    let mut scope = b"ChatProject.account.v1".to_vec();
    scope.extend_from_slice(&id.0);
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: scope.len() as u32,
        pbData: scope.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    // Input/entropy are borrowed only during this synchronous call. Never use LOCAL_MACHINE.
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &input,
                ptr::null(),
                &entropy,
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                ptr::null_mut(),
                &entropy,
                ptr::null(),
                ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        let code = match unsafe { GetLastError() } {
            5 | 1326 | 1907 => ErrorCode::StorageLocked,
            13 | 87 => ErrorCode::StorageCorrupt,
            _ if !protect => ErrorCode::StorageCorrupt,
            _ => ErrorCode::NotAvailable,
        };
        return Err(error(code));
    }
    // DPAPI owns the returned allocation. Erase plaintext before LocalFree even on bad length.
    let result = unsafe {
        let bytes = std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize);
        let result = if bytes.is_empty() || bytes.len() > 16_384 {
            Err(error(ErrorCode::StorageCorrupt))
        } else {
            Ok(Zeroizing::new(bytes.to_vec()))
        };
        bytes.zeroize();
        LocalFree(output.pbData.cast());
        result
    };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};

    #[test]
    fn round_trip_randomized_and_scope_bound() {
        let secret = Secret32::from([0x56; 32]);
        let id = StoreId([7; 16]);
        let a = WindowsProtector.protect(id, &secret).unwrap();
        let b = WindowsProtector.protect(id, &secret).unwrap();
        assert_ne!(a, b);
        assert!(!a.windows(32).any(|w| w == secret.as_bytes()));
        assert_eq!(
            WindowsProtector.unprotect(id, &a).unwrap().as_bytes(),
            secret.as_bytes()
        );
        assert_eq!(
            WindowsProtector
                .unprotect(StoreId([8; 16]), &a)
                .err()
                .unwrap()
                .code,
            ErrorCode::StorageCorrupt
        );
        let mut corrupt = a;
        let last = corrupt.len() - 1;
        corrupt[last] ^= 1;
        assert_eq!(
            WindowsProtector.unprotect(id, &corrupt).err().unwrap().code,
            ErrorCode::StorageCorrupt
        );
    }

    #[test]
    fn restart_reads_persisted_blob() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wrapped");
        fs::write(
            &path,
            WindowsProtector
                .protect(StoreId([9; 16]), &Secret32::from([0x39; 32]))
                .unwrap(),
        )
        .unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "platform_keys::windows::tests::restart_child",
                "--ignored",
            ])
            .env("CHAT_DPAPI_TEST_BLOB", path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    #[ignore = "spawned by restart_reads_persisted_blob"]
    fn restart_child() {
        let blob = fs::read(std::env::var_os("CHAT_DPAPI_TEST_BLOB").unwrap()).unwrap();
        assert_eq!(
            WindowsProtector
                .unprotect(StoreId([9; 16]), &blob)
                .unwrap()
                .as_bytes(),
            &[0x39; 32]
        );
    }

    #[test]
    fn missing_wrapped_key_cannot_reopen_store() {
        use chat_core::account::storage::{Store, StoredPlaintext};
        let dir = tempfile::tempdir().unwrap();
        let provider = || super::super::provider(dir.path()).unwrap();
        let mut store = Store::open(dir.path(), provider()).unwrap();
        store
            .commit(&StoredPlaintext {
                installation: [2; 32],
                payload: b"account".to_vec().into(),
            })
            .unwrap();
        drop(store);
        fs::remove_file(dir.path().join("key.wrap")).unwrap();
        let mut store = Store::open(dir.path(), provider()).unwrap();
        assert_eq!(store.load().err().unwrap().code, ErrorCode::StorageMissing);
    }

    #[test]
    fn deletion_is_idempotent_and_invalid_blob_rejected() {
        WindowsProtector.delete(StoreId([1; 16])).unwrap();
        WindowsProtector.delete(StoreId([1; 16])).unwrap();
        for blob in [vec![], vec![0; 16_385]] {
            assert_eq!(
                WindowsProtector
                    .unprotect(StoreId([1; 16]), &blob)
                    .err()
                    .unwrap()
                    .code,
                ErrorCode::StorageCorrupt
            );
        }
    }
}
