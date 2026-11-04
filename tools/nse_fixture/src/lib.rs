//! Builds spike B's rev3 group through `chat_core` and exports receiver R's snapshot plus K new
//! ciphertexts from S. The ciphertexts are at generations after R's stored ratchet, so each one
//! decrypts against the same snapshot (the NSE never persists its writes).
//!
//! The group and the snapshot come from `chat_core::mls::nse::fixture`; this crate holds no
//! MLS code of its own.

pub use chat_core::mls::nse::fixture::{Fixture, build_full as build};
