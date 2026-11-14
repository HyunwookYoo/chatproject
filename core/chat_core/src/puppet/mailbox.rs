//! Local Debug actions on the existing account's own-device sync (M6): what the app's mailbox
//! setting and lifecycle hooks call, and the sync counters for the multi-device scenario.
use super::account::SecretJson;
use crate::{AccountHandle, CoreError, ErrorCode};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(
    tag = "op",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Action {
    Set { enabled: bool },
    SyncNow {},
    Background {},
    State {},
    TestCounters {},
}

pub async fn execute(
    handle: Option<AccountHandle>,
    op: &str,
    mut args: SecretJson,
) -> Result<Value, CoreError> {
    let op = op
        .strip_prefix("mailbox_local_")
        .ok_or_else(super::protocol::invalid)?;
    let command = SecretJson(json!({"op":op,"args":std::mem::take(&mut args.0)}));
    let action = Action::deserialize(&command.0).map_err(|_| super::protocol::invalid())?;
    let handle =
        handle.ok_or_else(|| CoreError::new(ErrorCode::NotAvailable, "Account not opened"))?;
    Ok(match action {
        Action::Set { enabled } => {
            handle.set_mailbox(enabled).await?;
            Value::Null
        }
        Action::SyncNow {} => {
            handle.mailbox_sync_now().await?;
            Value::Null
        }
        Action::Background {} => json!({"handed_over": handle.mailbox_background().await?}),
        Action::State {} => {
            let state = handle.mailbox_state().await?;
            json!({
                "enabled": state.enabled,
                "first_link_shown": state.first_link_shown,
                "uptime_7d_permille": state.uptime_7d_permille,
            })
        }
        Action::TestCounters {} => {
            let counters = handle.mailbox_test_counters().await?;
            json!({
                "missing": counters.missing,
                "notifications": counters.notifications,
                "notified": counters.notified,
            })
        }
    })
}
