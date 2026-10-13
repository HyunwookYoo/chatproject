//! Entry points for native code that runs without Flutter in this process: the Android
//! push receiver (Kotlin, design 12.3). The iOS notification extension runs in its own
//! process and links `core/chat_nse` instead.

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

    // This test exclusively owns starting the process-global core; other tests use local CoreSlots.
    #[tokio::test]
    async fn instance_id_is_none_before_start_and_matches_after() {
        use crate::api::chat;
        assert_eq!(native_instance_id(), None);
        assert_eq!(chat::network_info().await.unwrap_err().code, chat::ErrorCode::CoreNotStarted);
        assert_eq!(chat::network_start().await.unwrap_err().code, chat::ErrorCode::CoreNotStarted);
        assert_eq!(chat::network_stop().await.unwrap_err().code, chat::ErrorCode::CoreNotStarted);
        assert_eq!(chat::connect_peer("unused".into()).await.unwrap_err().code, chat::ErrorCode::CoreNotStarted);
        assert_eq!(chat::send_probe_text("unused".into(), "hello".into()).await.unwrap_err().code, chat::ErrorCode::CoreNotStarted);
        assert_eq!(chat::disconnect_peer("unused".into()).await.unwrap_err().code, chat::ErrorCode::CoreNotStarted);
        let tmp = tempfile::tempdir().unwrap();
        let info = crate::api::chat::core_start(tmp.path().to_string_lossy().into_owned()).unwrap();
        assert_eq!(native_instance_id(), Some(info.instance_id.clone()));
        assert_eq!(native_core_version(), crate::api::chat::core_version());
        let stopped = chat::network_info().await.unwrap();
        assert_eq!(stopped.state, chat::NetworkState::Stopped);
        assert_eq!(stopped.endpoint_id, None);
        assert_eq!(stopped.ticket, None);
        assert!(stopped.peers.is_empty());
        assert_eq!(chat::connect_peer("unused".into()).await.unwrap_err().code, chat::ErrorCode::NetworkNotStarted);
        assert_eq!(chat::send_probe_text("unused".into(), "hello".into()).await.unwrap_err().code, chat::ErrorCode::NetworkNotStarted);
        chat::disconnect_peer("unused".into()).await.unwrap();
        chat::network_stop().await.unwrap();
        chat::network_stop().await.unwrap();
        assert_eq!(chat::network_info().await.unwrap(), stopped);
        assert_eq!(native_instance_id(), Some(info.instance_id));
    }
}
