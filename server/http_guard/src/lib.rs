//! What the public HTTP servers share: bounded connection serving and
//! in-memory token buckets keyed per client.

mod bucket;
mod serve;

pub use bucket::{Buckets, client_key};
pub use serve::{ServeLimits, serve};
