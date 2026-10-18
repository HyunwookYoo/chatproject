//! OS key protection stays entirely on the Rust/native side of the bridge.
#[cfg(not(windows))]
use chat_core::ErrorCode;
use chat_core::{account::storage::KeyProtector, CoreError};
use std::{path::Path, sync::Arc};

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "ios")]
mod ios;
#[cfg(windows)]
mod windows;

#[cfg(any(target_os = "android", target_os = "ios"))]
const MAX_BLOB: usize = 16_384;

#[cfg(any(target_os = "android", target_os = "ios"))]
fn error(code: ErrorCode) -> CoreError {
    CoreError::new(code, "Cannot access this device's protected account key")
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn status(code: i32) -> Result<(), CoreError> {
    match code {
        0 => Ok(()),
        1 => Err(error(ErrorCode::StorageLocked)),
        2 => Err(error(ErrorCode::StorageMissing)),
        3 => Err(error(ErrorCode::StorageCorrupt)),
        5 => Err(error(ErrorCode::StorageFull)),
        _ => Err(error(ErrorCode::NotAvailable)),
    }
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn validate_blob(blob: &[u8]) -> Result<(), CoreError> {
    if blob.is_empty() || blob.len() > MAX_BLOB {
        return Err(error(ErrorCode::StorageCorrupt));
    }
    Ok(())
}

pub fn provider(_root: &Path) -> Result<Arc<dyn KeyProtector>, CoreError> {
    #[cfg(windows)]
    return Ok(Arc::new(windows::WindowsProtector));
    #[cfg(target_os = "android")]
    return android::provider();
    #[cfg(target_os = "ios")]
    return Ok(Arc::new(ios::IosProtector));
    #[cfg(not(any(windows, target_os = "android", target_os = "ios")))]
    Err(CoreError::new(
        ErrorCode::NotAvailable,
        "OS key protection unavailable",
    ))
}
