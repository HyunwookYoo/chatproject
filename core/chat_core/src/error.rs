/// Error kinds the UI can branch on. Never match on `detail` (design 12.2 rule 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// The start configuration is unusable, e.g. the data folder cannot be created.
    InvalidConfig,
    /// The core already runs in this process with another data folder.
    AlreadyStartedDifferentConfig,
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
