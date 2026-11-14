//! What the proxy hands to FCM or APNs, and the seam the senders plug into.
//!
//! Routes decide; an [`Upstream`] only delivers. Each platform is one
//! implementation, so a new platform adds an implementation, not a branch.

use std::fmt;
use std::sync::{Mutex, PoisonError};

use futures_core::future::BoxFuture;
use push_wire::{Platform, Tag};

/// One push to one device, built from an opened handle and a request item.
#[derive(Clone, PartialEq, Eq)]
pub struct Delivery {
    pub platform: Platform,
    pub token: String,
    pub tag: Tag,
    pub kind: DeliveryKind,
}

#[derive(Clone, PartialEq, Eq)]
pub enum DeliveryKind {
    /// `e` as the sender gave it: base64url of the stripped MLS ciphertext.
    Cipher(String),
    Wake,
    Read,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UpstreamResult {
    Sent,
    /// FCM `UNREGISTERED` / APNs 410. Only counted in the log (spec 9.2).
    TokenGone,
    Failed,
}

pub trait Upstream: Send + Sync {
    fn send(&self, delivery: Delivery) -> BoxFuture<'static, UpstreamResult>;
}

/// Records every delivery in memory and answers a fixed result.
pub struct FakeUpstream {
    received: Mutex<Vec<Delivery>>,
    result: UpstreamResult,
}

impl FakeUpstream {
    pub fn new() -> Self {
        Self::answering(UpstreamResult::Sent)
    }

    pub fn answering(result: UpstreamResult) -> Self {
        FakeUpstream {
            received: Mutex::new(Vec::new()),
            result,
        }
    }

    /// Deliveries received so far, in arrival order.
    pub fn deliveries(&self) -> Vec<Delivery> {
        self.received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Default for FakeUpstream {
    fn default() -> Self {
        Self::new()
    }
}

impl Upstream for FakeUpstream {
    fn send(&self, delivery: Delivery) -> BoxFuture<'static, UpstreamResult> {
        self.received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(delivery);
        let result = self.result;
        Box::pin(async move { result })
    }
}

struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Debug for Delivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Delivery")
            .field("platform", &self.platform)
            .field("token", &Redacted)
            .field("tag", &Redacted)
            .field("kind", &self.kind)
            .finish()
    }
}

impl fmt::Debug for DeliveryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeliveryKind::Cipher(_) => f.debug_tuple("Cipher").field(&Redacted).finish(),
            DeliveryKind::Wake => f.write_str("Wake"),
            DeliveryKind::Read => f.write_str("Read"),
        }
    }
}
