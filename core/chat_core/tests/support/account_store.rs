use std::sync::Arc;

use super::{CoreError, ErrorCode, KeyProtector, Secret32, StoreId, StoredPlaintext};
use zeroize::Zeroizing;

// Test-only stand-in for an OS protector. It binds blobs to the public store ID.
pub struct FakeProtector;

impl KeyProtector for FakeProtector {
    fn protect(&self, id: StoreId, key: &Secret32) -> Result<Vec<u8>, CoreError> {
        let mut blob = id.0.to_vec();
        blob.extend(key.as_bytes().iter().map(|b| b ^ 0xa7));
        Ok(blob)
    }

    fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
        if blob.len() != 48 || blob[..16] != id.0 {
            return Err(CoreError::new(
                ErrorCode::StorageLocked,
                "test key unavailable",
            ));
        }
        Ok(Secret32::from(std::array::from_fn(|i| blob[16 + i] ^ 0xa7)))
    }

    fn delete(&self, _: StoreId) -> Result<(), CoreError> {
        Ok(())
    }
}

pub fn protector() -> Arc<dyn KeyProtector> {
    Arc::new(FakeProtector)
}

pub fn state(payload: &[u8]) -> StoredPlaintext {
    StoredPlaintext {
        installation: [0x31; 32],
        payload: Zeroizing::new(payload.to_vec()),
    }
}
