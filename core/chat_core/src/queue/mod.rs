//! M8 server queue (spec 6, 7, 9): the seam the account actor uses to reach the queue server,
//! and which server address this build takes. The commands, signing bytes and constants come
//! from `queue_wire`; nothing here restates them.
use std::fmt;

use futures_util::future::BoxFuture;
pub use queue_wire::{AckItem, CreateItem, DeleteItem, ItemResult, QueueId, RecvItem, SendItem};

use crate::{CoreError, ErrorCode};

#[cfg(feature = "push-http")]
mod client;
#[cfg(feature = "push-http")]
pub use client::HttpQueueServer;

/// Why one queue server request failed. Carries no request or response content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueError {
    /// No connection, or it broke.
    Network,
    /// No answer within the request's time limit.
    Timeout,
    /// The server answered with this HTTP status (not 200).
    Status(u16),
    /// The answer was not the expected JSON, or not one result per item.
    Malformed,
}

impl fmt::Display for QueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueueError::Network => f.write_str("queue server unreachable"),
            QueueError::Timeout => f.write_str("queue server timed out"),
            QueueError::Status(code) => write!(f, "queue server answered {code}"),
            QueueError::Malformed => f.write_str("queue server answer malformed"),
        }
    }
}

impl std::error::Error for QueueError {}

/// One request of a queue server call: one result per item, in the items' order.
pub type QueueFuture = BoxFuture<'static, Result<Vec<ItemResult>, QueueError>>;

/// The queue server (spec 9.2) as the actor sees it: one batch of at most
/// `queue_wire::MAX_ITEMS` items per call, each bounded in time by the implementation.
pub trait QueueServer: Send + Sync {
    fn create(&self, items: Vec<CreateItem>) -> QueueFuture;
    fn send(&self, items: Vec<SendItem>) -> QueueFuture;
    fn recv(&self, items: Vec<RecvItem>) -> QueueFuture;
    fn ack(&self, items: Vec<AckItem>) -> QueueFuture;
    fn delete(&self, items: Vec<DeleteItem>) -> QueueFuture;
}

/// This build's queue server (`CHATPROJECT_QUEUE_URL` when it was built).
const BUILD_URL: Option<&str> = option_env!("CHATPROJECT_QUEUE_URL");

/// The queue server address this device uses, from the app's `given` one (spec 7.2, Global
/// Constraints): release builds take only this build's address, and only `https://`; debug
/// builds take `given` (`https://` or `http://`, a local server) or else the build's. `None`:
/// the queue is off, and no request of any kind is made. A `given` address a release build
/// does not take, or one that is not a URL this build may use, is `InvalidConfig`.
pub fn queue_url(given: Option<&str>) -> Result<Option<String>, CoreError> {
    resolve(given, !cfg!(debug_assertions), BUILD_URL)
}

fn resolve(
    given: Option<&str>,
    release: bool,
    build: Option<&str>,
) -> Result<Option<String>, CoreError> {
    let invalid = || CoreError::new(ErrorCode::InvalidConfig, "queue_url");
    let url = if release {
        if given.is_some_and(|g| Some(g) != build) {
            return Err(invalid());
        }
        build
    } else {
        given.or(build)
    };
    let Some(url) = url else {
        return Ok(None);
    };
    crate::push::check_scheme(url, !release).map_err(|_| invalid())?;
    Ok(Some(url.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_takes_only_the_build_https_url() {
        let build = Some("https://queue.example");
        assert_eq!(resolve(None, true, build).unwrap().as_deref(), build);
        assert_eq!(
            resolve(Some("https://queue.example"), true, build)
                .unwrap()
                .as_deref(),
            build
        );
        for other in ["https://other.example", "http://queue.example"] {
            assert_eq!(
                resolve(Some(other), true, build).unwrap_err().code,
                ErrorCode::InvalidConfig
            );
        }
        // A build without an address has the queue off; one with a plain-HTTP address too.
        assert_eq!(resolve(None, true, None).unwrap(), None);
        assert!(resolve(None, true, Some("http://queue.example")).is_err());
        assert!(resolve(Some("https://queue.example"), true, None).is_err());
    }

    #[test]
    fn debug_takes_a_given_url_over_the_build_one() {
        let build = Some("https://queue.example");
        assert_eq!(
            resolve(Some("http://127.0.0.1:8788"), false, build)
                .unwrap()
                .as_deref(),
            Some("http://127.0.0.1:8788")
        );
        assert_eq!(resolve(None, false, build).unwrap().as_deref(), build);
        assert_eq!(resolve(None, false, None).unwrap(), None);
        for bad in ["", "ftp://queue.example", "queue.example"] {
            assert!(resolve(Some(bad), false, build).is_err());
        }
    }
}
