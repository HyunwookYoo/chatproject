//! Queue server (M8 spec 9): holds ciphertexts for devices that are off,
//! in one SQLite file, until the receiving device takes and acks them.

mod store;

pub use store::{Store, StoreError, SweepCounts};
