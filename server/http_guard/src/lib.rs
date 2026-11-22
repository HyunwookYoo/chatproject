//! What the public HTTP servers share: bounded connection serving and
//! in-memory token buckets keyed per client.

mod bucket;
mod client;
mod serve;

pub use bucket::Buckets;
pub use client::{client_ip, client_key};
pub use serve::{ServeLimits, serve};
