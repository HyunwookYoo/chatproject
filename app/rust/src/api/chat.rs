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
    InvalidRecovery,
    StorageLocked,
    StorageMissing,
    StorageCorrupt,
    StorageFull,
    AccountExists,
    WriterBusy,
    InvalidInvite,
    InviteExpired,
    DeviceLimit,
    PairMismatch,
    PairExpired,
    RecordConflict,
    RecordUnavailable,
    AuthFailed,
    AccountRestricted,
    PairHistoryFull,
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
    AccountChanged { revision: u64, view: Box<super::account::AccountView> },
    AccountAuthChanged { peer_id: String, installation_id: String, ready: bool },
    AccountCacheUsed { peer_id: String },
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

async fn run_network(op: chat_core::NetworkOp) -> Result<chat_core::NetworkReply, CoreError> {
    let network = CORE.network().map_err(CoreError::from)?;
    network.execute(op).await.map_err(CoreError::from)
}

fn unexpected_network_reply() -> CoreError {
    CoreError { code: ErrorCode::TransportFailed, detail: "unexpected network reply".into() }
}

fn network_info_reply(reply: chat_core::NetworkReply) -> Result<NetworkInfo, CoreError> {
    match reply {
        chat_core::NetworkReply::Info(info) => Ok(info.into()),
        chat_core::NetworkReply::Peer(_) | chat_core::NetworkReply::Done => {
            Err(unexpected_network_reply())
        }
    }
}

fn done_reply(reply: chat_core::NetworkReply) -> Result<(), CoreError> {
    match reply {
        chat_core::NetworkReply::Done => Ok(()),
        chat_core::NetworkReply::Info(_) | chat_core::NetworkReply::Peer(_) => {
            Err(unexpected_network_reply())
        }
    }
}

pub async fn network_start() -> Result<NetworkInfo, CoreError> {
    network_info_reply(run_network(chat_core::NetworkOp::Start).await?)
}

pub async fn network_info() -> Result<NetworkInfo, CoreError> {
    network_info_reply(run_network(chat_core::NetworkOp::Info).await?)
}

pub async fn connect_peer(target: String) -> Result<PeerInfo, CoreError> {
    match run_network(chat_core::NetworkOp::Connect { target }).await? {
        chat_core::NetworkReply::Peer(peer) => Ok(peer.into()),
        chat_core::NetworkReply::Info(_) | chat_core::NetworkReply::Done => {
            Err(unexpected_network_reply())
        }
    }
}

pub async fn send_probe_text(peer_id: String, text: String) -> Result<(), CoreError> {
    done_reply(run_network(chat_core::NetworkOp::Send { peer_id, text }).await?)
}

pub async fn disconnect_peer(peer_id: String) -> Result<(), CoreError> {
    done_reply(run_network(chat_core::NetworkOp::Disconnect { peer_id }).await?)
}

pub async fn network_stop() -> Result<(), CoreError> {
    done_reply(run_network(chat_core::NetworkOp::Stop).await?)
}

/// The single event stream of the UI (design 12.2 rule 1).
pub fn events(sink: StreamSink<ChatEvent>) -> Result<(), CoreError> {
    BUS.subscribe(Arc::new(FrbSink(sink)));
    Ok(())
}

/// Debug control startup waits until this core has an isolated Local profile.
pub async fn test_hooks_start(control_url: String, name: String) -> Result<(), CoreError> {
    CORE.test_hooks_start(control_url, name).await.map_err(CoreError::from)
}
