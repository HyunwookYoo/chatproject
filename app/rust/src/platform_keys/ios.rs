use super::{status, MAX_BLOB};
use chat_core::{
    account::{storage::KeyProtector, Secret32, StoreId},
    CoreError, ErrorCode,
};
use zeroize::Zeroizing;

extern "C" {
    fn cp_key_protect(
        id: *const u8,
        input: *const u8,
        len: usize,
        out: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32;
    fn cp_key_unprotect(
        id: *const u8,
        input: *const u8,
        len: usize,
        out: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32;
    fn cp_key_delete(id: *const u8) -> i32;
}

pub(super) struct IosProtector;

impl KeyProtector for IosProtector {
    fn protect(&self, id: StoreId, secret: &Secret32) -> Result<Vec<u8>, CoreError> {
        let mut output = vec![0; MAX_BLOB];
        let mut length = 0;
        status(unsafe {
            cp_key_protect(
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
        let mut output = Zeroizing::new([0u8; 32]);
        let mut length = 0;
        status(unsafe {
            cp_key_unprotect(
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
        status(unsafe { cp_key_delete(id.0.as_ptr()) })
    }
}
