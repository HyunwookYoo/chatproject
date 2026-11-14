//! Stateless push proxy (spec 9): seals PushHandles at `/handle`, opens them at
//! `/push` and hands each item to FCM or APNs after a random delay. Keeps
//! nothing beyond in-memory rate-limit counters.

mod app;
mod config;
mod limit;
mod log;
mod upstream;

pub use app::{AppState, router};
pub use config::{ApnsConfig, ApnsEnv, Config, ConfigError};
pub use limit::Limits;
pub use log::{LogSink, stderr_sink};
pub use upstream::{Delivery, DeliveryKind, FakeUpstream, Upstream, UpstreamResult};
