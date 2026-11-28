//! M7 push (spec 5, 8): the seam the account actor uses to reach the push proxy, and the types
//! of the push API on `AccountHandle` (`account/manager/push.rs`). The proxy's wire formats and
//! constants come from `push_wire`; nothing here restates them.
use std::{fmt, path::PathBuf};

use futures_util::future::BoxFuture;
pub use push_wire::{
    HandleRequest, HandleResponse, Platform, PushItem, PushItemKind, PushRequest, PushResponse,
    PushResult, Tag,
};

use crate::{CoreError, ErrorCode};

#[cfg(feature = "push-http")]
pub(crate) mod client;
pub mod ios_state;
#[cfg(feature = "push-http")]
pub use client::{HttpPushProxy, Url};

/// Why one proxy request failed. Carries no request or response content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushError {
    /// No connection, or it broke.
    Network,
    /// No answer within the request's time limit.
    Timeout,
    /// The proxy answered with this HTTP status (not 200).
    Status(u16),
    /// The answer was not the expected JSON.
    Malformed,
}

impl fmt::Display for PushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PushError::Network => f.write_str("push proxy unreachable"),
            PushError::Timeout => f.write_str("push proxy timed out"),
            PushError::Status(code) => write!(f, "push proxy answered {code}"),
            PushError::Malformed => f.write_str("push proxy answer malformed"),
        }
    }
}

impl std::error::Error for PushError {}

/// One request of a proxy call, answered once.
pub type PushFuture<T> = BoxFuture<'static, Result<T, PushError>>;

/// The push proxy (spec 9.2) as the actor sees it: `/handle` and `/push`, each bounded in time
/// by the implementation.
pub trait PushProxy: Send + Sync {
    fn handle(&self, request: HandleRequest) -> PushFuture<HandleResponse>;
    fn push(&self, request: PushRequest) -> PushFuture<PushResponse>;
}

/// What the app tells the core at every start (plan 2): the proxy address (`None`: push off for
/// this build), the platform this device receives pushes on (`None`: it receives none, e.g.
/// Windows) and the iOS snapshot directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PushConfig {
    pub proxy_url: Option<String>,
    pub platform: Option<Platform>,
    pub snapshot_dir: Option<PathBuf>,
}

/// This device's push setting and handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PushStatus {
    pub enabled: bool,
    pub platform: Option<Platform>,
    /// Conversations with a current handle of this device.
    pub handles: u32,
    pub oldest_issued_at_ms: Option<u64>,
}

/// What one renewal did: handles issued (own-device handle included), PushHandle frames sealed
/// with a handle, and withdrawals sealed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenewSummary {
    pub issued: u32,
    pub announced: u32,
    pub withdrawn: u32,
}

/// Longest a received push may fetch for (spec 5.4: Android's wake budget); `push_received`
/// takes a shorter deadline as it is.
pub const FETCH_MAX_MS: u64 = 20_000;

/// A push this device received (plan 2, spec 5.3), as the platform receiver parsed it: `t` and
/// `e` (base64url-decoded), `t` with `w`, or `t` with `r`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceivedPush {
    /// A text's MLS ciphertext with its group id taken out (`strip_group_id`).
    Cipher { tag: Tag, ciphertext: Vec<u8> },
    /// A text too large to carry: fetched from the sender and my mailbox.
    Wake { tag: Tag },
    /// Messages were read on another of my devices (spec 7). `tag` must be this device's
    /// own-device tag (the one its current `Announce` handle holds); any other is unknown.
    ReadClear { tag: Tag },
}

/// What processing a received push showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    /// A notification that alerts (`NotifyItem::new_messages > 0`).
    Notified,
    /// The conversation's notification was updated or cleared without alerting.
    Updated,
    /// Nothing to show: a text read already, or a push about nothing new.
    Nothing,
    /// The tag is not one of this device's current handles (withdrawn, or the conversation is
    /// gone): nothing is shown.
    UnknownTag,
}

/// A proxy address this build may use: `https://`, or in debug builds also `http://` (Global
/// Constraints). Anything else is `InvalidConfig`.
pub fn check_proxy_url(url: &str) -> Result<(), CoreError> {
    check_scheme(url, cfg!(debug_assertions))
}

/// `https://`, or also `http://` when `allow_http` (debug builds); shared with the queue server
/// address (`queue::queue_url`).
pub(crate) fn check_scheme(url: &str, allow_http: bool) -> Result<(), CoreError> {
    let lower = url.get(..8).map(str::to_ascii_lowercase).unwrap_or_default();
    let http = url.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("http://"));
    if lower == "https://" || (allow_http && http) {
        Ok(())
    } else {
        Err(CoreError::new(ErrorCode::InvalidConfig, "push_proxy_url"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_rule_takes_https_only() {
        assert!(check_scheme("https://push.example", false).is_ok());
        assert!(check_scheme("HTTPS://push.example", false).is_ok());
        assert!(check_scheme("http://push.example", false).is_err());
        assert!(check_scheme("http://push.example", true).is_ok());
        for bad in ["", "ftp://push.example", "push.example", "https:/x"] {
            let error = check_scheme(bad, true).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidConfig);
        }
    }
}
