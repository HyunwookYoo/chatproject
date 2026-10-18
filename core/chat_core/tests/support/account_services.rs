use chat_core::{CoreError, ErrorCode};
use std::sync::Arc;

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub fn offline() -> chat_core::account::AccountServices {
    struct Offline;
    impl chat_core::account::RecordBackend for Offline {
        fn lookup(
            &self,
            _: chat_core::account::PeerId,
        ) -> chat_core::account::RecordFuture<'_, chat_core::account::Lookup> {
            Box::pin(async { Err(CoreError::new(ErrorCode::RecordUnavailable, "test offline")) })
        }
        fn publish<'a>(
            &'a self,
            _: &'a pkarr::SignedPacket,
        ) -> chat_core::account::RecordFuture<'a, ()> {
            Box::pin(async { panic!("offline lookup must never publish") })
        }
    }
    chat_core::account::AccountServices::new(
        Arc::new(Offline),
        Arc::new(chat_core::account::clock::SystemClock::default()),
    )
}
