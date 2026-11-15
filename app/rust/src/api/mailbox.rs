//! This device's mailbox role and the app's foreground/background hooks (M6 spec 7, 8).
use super::chat::CoreError;

/// This device's mailbox setting (M6 spec 8.1, 8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxStateView {
    pub enabled: bool,
    /// The first-link screen was shown on this device; it is not shown again.
    pub first_link_shown: bool,
    /// Time up as a mailbox within the last 7 days, per mille of them.
    pub uptime_7d_permille: u32,
}

fn handle() -> Result<chat_core::account::AccountHandle, CoreError> {
    crate::global::CORE.account().map_err(Into::into)
}
pub async fn mailbox_state() -> Result<MailboxStateView, CoreError> {
    handle()?
        .mailbox_state()
        .await
        .map(Into::into)
        .map_err(Into::into)
}
/// Sets this device as a mailbox (or not) and tells my other devices.
pub async fn set_mailbox(enabled: bool) -> Result<(), CoreError> {
    handle()?.set_mailbox(enabled).await.map_err(Into::into)
}
pub async fn mark_first_link_shown() -> Result<(), CoreError> {
    handle()?.mark_first_link_shown().await.map_err(Into::into)
}
/// The app came to the foreground with an active account (also a launch on screen): the network
/// starts, so a push's use of it leaves it running (M7 final review, user decision 2026-11-15),
/// and my other devices sync now. Launches in the background never call this: the FCM receiver
/// and the WorkManager jobs run no Dart, and Dart calls it only once the app is resumed.
pub async fn mailbox_foreground() -> Result<(), CoreError> {
    super::chat::network_start().await?;
    handle()?.mailbox_sync_now().await.map_err(Into::into)
}
/// The app goes to the background or exits: hand undelivered messages to my mailboxes.
/// Returns how many were handed over.
pub async fn mailbox_background() -> Result<u32, CoreError> {
    handle()?.mailbox_background().await.map_err(Into::into)
}
