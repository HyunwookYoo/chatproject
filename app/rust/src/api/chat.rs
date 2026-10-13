//! Commands and events the Flutter UI uses (design 12.1). Keep this file thin:
//! logic lives in chat_core, this module only converts types.

use std::path::PathBuf;
use std::sync::Arc;

use crate::convert::FrbSink;
use crate::frb_generated::StreamSink;
use crate::global::{BUS, CORE};

#[flutter_rust_bridge::frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

pub struct CoreInfo {
    pub version: String,
    pub instance_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidConfig,
    AlreadyStartedDifferentConfig,
    CoreNotStarted,
    NetworkNotStarted,
    InvalidPeerAddress,
    SelfConnection,
    PeerNotConnected,
    MessageTooLarge,
    Busy,
    NetworkTimeout,
    TransportFailed,
    NotAvailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkState {
    Stopped,
    Starting,
    Online,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerPhase {
    Connecting,
    Connected,
    Reconnecting,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerPath {
    Unknown,
    Relay,
    Direct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInfo {
    pub revision: u64,
    pub state: NetworkState,
    pub endpoint_id: Option<String>,
    pub ticket: Option<String>,
    pub peers: Vec<PeerInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub peer_id: String,
    pub phase: PeerPhase,
    pub path: PeerPath,
    pub connect_ms: Option<u64>,
    pub direct_ms: Option<u64>,
}

#[derive(Debug)]
pub struct CoreError {
    pub code: ErrorCode,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    ConnectionState {
        direct_peers: u32,
        mailbox_ok: bool,
        queue_ok: Option<bool>,
    },
    Error {
        code: ErrorCode,
        detail: String,
    },
    NetworkChanged {
        info: NetworkInfo,
    },
    PeerChanged {
        revision: u64,
        peer: PeerInfo,
    },
    ProbeTextReceived {
        peer_id: String,
        text: String,
    },
}

#[flutter_rust_bridge::frb(sync)]
pub fn core_version() -> String {
    chat_core::CORE_VERSION.to_string()
}

/// Starts the core, or returns the running one (see `chat_core::CoreSlot::start`).
pub fn core_start(data_dir: String) -> Result<CoreInfo, CoreError> {
    let config = chat_core::CoreConfig { data_dir: PathBuf::from(data_dir) };
    CORE.start(config, &BUS).map(CoreInfo::from).map_err(CoreError::from)
}

/// The single event stream of the UI (design 12.2 rule 1).
pub fn events(sink: StreamSink<ChatEvent>) -> Result<(), CoreError> {
    BUS.subscribe(Arc::new(FrbSink(sink)));
    Ok(())
}
