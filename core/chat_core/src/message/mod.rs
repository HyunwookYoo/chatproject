//! Durable messages; account actor authority and the one storage worker stay private.
#![allow(dead_code)] // Private producer methods are also consumed by storage fixtures.
pub(crate) mod store;
pub(crate) mod types;
pub(crate) mod wire;
pub(crate) mod scheduler;
pub(crate) mod service;
pub use service::MessageHandle;
pub use types::*;
