use serde::{Deserialize, Serialize};

/// Error kinds the UI can branch on. Never match on `detail` (design 12.2 rule 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    /// The start configuration is unusable, e.g. the data folder cannot be created.
    InvalidConfig,
    /// The core already runs in this process with another data folder.
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code:?}: {detail}")]
pub struct CoreError {
    pub code: ErrorCode,
    pub detail: String,
}

impl CoreError {
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self { code, detail: detail.into() }
    }
}
