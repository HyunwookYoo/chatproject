//! Chat core. All app state lives here; the UI only renders what this crate reports
//! (design 12.1).

pub mod account;
pub mod error;
pub mod event;
pub mod runtime;
pub mod transport;

pub use error::{CoreError, ErrorCode};
pub use event::{ConnectionSnapshot, CoreEvent, EventBus, EventSink};
pub use runtime::{CORE_VERSION, CoreConfig, CoreInfo, CoreSlot};
pub use transport::{
    NetworkHandle, NetworkInfo, NetworkOp, NetworkReply, NetworkState, PeerInfo, PeerPath, PeerPhase,
};

#[cfg(all(feature = "test-hooks", debug_assertions))]
pub mod puppet;
