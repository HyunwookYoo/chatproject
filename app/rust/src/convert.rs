//! Conversions between chat_core types and the types Dart sees.

use std::sync::Mutex;

use crate::api::chat::{ChatEvent, CoreError, CoreInfo, ErrorCode};
use crate::frb_generated::StreamSink;

impl From<chat_core::CoreInfo> for CoreInfo {
    fn from(info: chat_core::CoreInfo) -> Self {
        Self { version: info.version, instance_id: info.instance_id }
    }
}

impl From<chat_core::ErrorCode> for ErrorCode {
    fn from(code: chat_core::ErrorCode) -> Self {
        match code {
            chat_core::ErrorCode::InvalidConfig => Self::InvalidConfig,
            chat_core::ErrorCode::AlreadyStartedDifferentConfig => Self::AlreadyStartedDifferentConfig,
        }
    }
}

impl From<chat_core::CoreError> for CoreError {
    fn from(err: chat_core::CoreError) -> Self {
        Self { code: err.code.into(), detail: err.detail }
    }
}

impl From<chat_core::CoreEvent> for ChatEvent {
    fn from(event: chat_core::CoreEvent) -> Self {
        match event {
            chat_core::CoreEvent::ConnectionState(s) => Self::ConnectionState {
                direct_peers: s.direct_peers,
                mailbox_ok: s.mailbox_ok,
                queue_ok: s.queue_ok,
            },
            chat_core::CoreEvent::Error { code, detail } => Self::Error { code: code.into(), detail },
        }
    }
}

/// Forwards core events into a Dart stream. A closed Dart stream ends the subscription.
pub(crate) struct FrbSink(pub(crate) Mutex<StreamSink<ChatEvent>>);

impl chat_core::EventSink for FrbSink {
    fn send(&self, event: chat_core::CoreEvent) -> bool {
        let sink = self.0.lock().expect("stream sink lock poisoned");
        sink.add(ChatEvent::from(event)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_state_keeps_every_field() {
        let event = chat_core::CoreEvent::ConnectionState(chat_core::ConnectionSnapshot {
            direct_peers: 2,
            mailbox_ok: true,
            queue_ok: Some(false),
        });
        assert_eq!(
            ChatEvent::from(event),
            ChatEvent::ConnectionState { direct_peers: 2, mailbox_ok: true, queue_ok: Some(false) }
        );
    }

    #[test]
    fn error_keeps_code_and_detail() {
        let err = chat_core::CoreError::new(chat_core::ErrorCode::AlreadyStartedDifferentConfig, "x");
        let converted = CoreError::from(err);
        assert_eq!(converted.code, ErrorCode::AlreadyStartedDifferentConfig);
        assert_eq!(converted.detail, "x");
    }
}
