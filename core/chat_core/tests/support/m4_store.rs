use chat_core::account::storage::{KeyProtector, StoredPlaintext};
use chat_core::account::{Secret32, StoreId, derive_device};
use chat_core::{CoreError, ErrorCode};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use zeroize::Zeroizing;

#[derive(Default)]
pub struct Protector {
    pub deny_salt: AtomicBool,
    pub fail_protect: AtomicBool,
    pub oversized_wrap: AtomicBool,
}
impl KeyProtector for Protector {
    fn protect(&self, id: StoreId, secret: &Secret32) -> Result<Vec<u8>, CoreError> {
        if self.fail_protect.load(Ordering::SeqCst) {
            return Err(CoreError::new(
                ErrorCode::StorageLocked,
                "fixture_os_protection_failed",
            ));
        }
        let mut out = id.0.to_vec();
        out.extend(secret.as_bytes().iter().map(|b| b ^ 0xa7));
        if self.oversized_wrap.load(Ordering::SeqCst) {
            out.resize(16_385, 0);
        }
        Ok(out)
    }
    fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
        if blob.len() != 48
            || blob[..16] != id.0
            || (self.deny_salt.load(Ordering::SeqCst) && blob[32..] == [0xa7; 16])
        {
            return Err(CoreError::new(
                ErrorCode::StorageMissing,
                "fixture_resource_missing",
            ));
        }
        Ok(Secret32::from(std::array::from_fn(|i| blob[16 + i] ^ 0xa7)))
    }
    fn delete(&self, _: StoreId) -> Result<(), CoreError> {
        Ok(())
    }
}
pub fn protector() -> Arc<Protector> {
    Arc::new(Protector::default())
}
pub fn state(seed: u8) -> StoredPlaintext {
    let device = derive_device(Secret32::from([seed; 32])).unwrap();
    let mut payload = vec![0x83, 1, 0, 0x58, 32];
    payload.extend_from_slice(&[seed; 32]);
    StoredPlaintext {
        installation: device.installation_id().0,
        payload: Zeroizing::new(payload),
    }
}
