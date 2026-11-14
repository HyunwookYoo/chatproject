//! Stateless push proxy (spec 9): seals PushHandles at `/handle`, opens them at
//! `/push` and hands each item to FCM or APNs after a random delay. Keeps
//! nothing beyond in-memory rate-limit counters.

mod apns;
mod app;
mod config;
mod fcm;
mod jwt;
mod limit;
mod log;
mod serve;
mod upstream;

pub use apns::{APNS_PRODUCTION, APNS_SANDBOX, ApnsUpstream};
pub use app::{AppState, router};
pub use config::{ApnsConfig, ApnsEnv, Config, ConfigError};
pub use fcm::{FCM_ENDPOINT, FcmUpstream, GOOGLE_TOKEN_URL};
pub use limit::Limits;
pub use log::{LogSink, stderr_sink};
pub use reqwest::Url;
pub use serve::serve;
pub use upstream::{
    Clock, Delivery, DeliveryKind, FakeUpstream, SenderError, Upstream, UpstreamResult,
};
