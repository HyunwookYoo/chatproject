//! Entry points for native code that runs without Flutter: the Android push receiver
//! (Kotlin) now, the iOS notification extension (Swift) in M1b (design 12.3).

use crate::global::CORE;

/// Version of the core in this library.
#[uniffi::export]
pub fn native_core_version() -> String {
    chat_core::CORE_VERSION.to_string()
}

/// Instance id of the core running in this process, or `None` before the first start.
/// Equals the id Dart got from `core_start` when both run in one process.
#[uniffi::export]
pub fn native_instance_id() -> Option<String> {
    CORE.info().map(|info| info.instance_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_is_none_before_start_and_matches_after() {
        assert_eq!(native_instance_id(), None);
        let tmp = tempfile::tempdir().unwrap();
        let info = crate::api::chat::core_start(tmp.path().to_string_lossy().into_owned()).unwrap();
        assert_eq!(native_instance_id(), Some(info.instance_id));
        assert_eq!(native_core_version(), crate::api::chat::core_version());
    }
}
