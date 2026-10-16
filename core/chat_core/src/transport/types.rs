use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkState {
    Stopped,
    Starting,
    Online,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerPhase {
    Connecting,
    Connected,
    Reconnecting,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerPath {
    Unknown,
    Relay,
    Direct,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInfo {
    pub revision: u64,
    pub state: NetworkState,
    pub endpoint_id: Option<String>,
    pub ticket: Option<String>,
    pub peers: Vec<PeerInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub peer_id: String,
    pub phase: PeerPhase,
    pub path: PeerPath,
    pub connect_ms: Option<u64>,
    pub direct_ms: Option<u64>,
}
