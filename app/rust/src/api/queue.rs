//! The server queue as the Flutter app shows it (M8 spec 4): the "오프라인 메시지" setting, the
//! mailbox verdict, and the queue server address the account is opened with. Whether this
//! device uses a queue right now also comes as `ChatEvent::ConnectionState { using_queue }`.
use std::sync::Mutex;

use chat_core::account::AccountHandle;

use super::chat::CoreError;

/// The "오프라인 메시지" setting (spec 5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueMode {
    /// A queue only while the account has no qualified mailbox ("자동").
    Auto,
    /// Always a queue ("항상 서버 사용").
    Always,
    /// No queue, and no request to the queue server ("서버 사용 안 함").
    Never,
}

/// Whether the account has a qualified mailbox it saw lately (spec 5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    HasMailbox,
    NoMailbox,
}

/// This device's queue setting and what it sees (spec 4, 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueStateView {
    pub mode: QueueMode,
    pub verdict: Verdict,
    /// This device uses a queue (the home card).
    pub using_queue: bool,
    /// This device's time up as a mailbox within the last 7 days, per mille of them.
    pub uptime_7d_permille: u32,
}

/// The debug address Dart gave (`--queue-url`); `None` takes this build's
/// (`CHATPROJECT_QUEUE_URL`).
static GIVEN: Mutex<Option<String>> = Mutex::new(None);

/// Configures a just opened account with the address, before its actor takes any frame (a
/// `QueueRef` taken before would be refused). Every path that opens the account calls it.
pub(crate) fn configure_opened(account: &AccountHandle) -> Result<(), chat_core::CoreError> {
    let given = GIVEN.lock().unwrap_or_else(|e| e.into_inner()).clone();
    account.queue_configure(given)
}

fn handle() -> Result<AccountHandle, CoreError> {
    crate::global::CORE.account().map_err(Into::into)
}

/// Debug `--queue-url`, before the account opens: the address it is opened with. An account
/// open already takes it now. Release builds take only this build's address (`InvalidConfig`).
pub fn queue_configure(server_url: Option<String>) -> Result<(), CoreError> {
    chat_core::queue::queue_url(server_url.as_deref()).map_err(CoreError::from)?;
    *GIVEN.lock().unwrap_or_else(|e| e.into_inner()) = server_url;
    match crate::global::CORE.account() {
        Ok(account) => configure_opened(&account).map_err(Into::into),
        Err(_) => Ok(()),
    }
}

pub async fn queue_state() -> Result<QueueStateView, CoreError> {
    handle()?
        .queue_state()
        .await
        .map(Into::into)
        .map_err(Into::into)
}

/// Sets "오프라인 메시지". The queues follow at the account's next poll, and `using_queue` with
/// them. Switching to `Never`, a request already on its way still ends (one at most).
pub async fn queue_set_mode(mode: QueueMode) -> Result<(), CoreError> {
    handle()?
        .set_queue_mode(mode.into())
        .await
        .map_err(Into::into)
}
