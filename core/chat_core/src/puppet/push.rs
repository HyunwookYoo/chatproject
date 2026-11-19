//! Local Debug actions on this device's push setting (M7): what the app's push model calls
//! (`push_configure`, `push_set_token`, `push_set_enabled`, `push_status`, `push_renew_due`),
//! and counts of the handles this device holds, for the m7 scenario. Counts only: no handle,
//! tag or token leaves the core.
use super::account::SecretJson;
use crate::push::{Platform, PushConfig};
use crate::{AccountHandle, CoreError, ErrorCode};
use serde::Deserialize;
use serde_json::{Value, json};
use zeroize::Zeroize;

#[derive(Deserialize)]
#[serde(
    tag = "op",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Action {
    /// A node sends pushes (`platform: null`, Windows-like); the app's own model configures it.
    Configure {
        proxy_url: Option<String>,
        platform: Option<Platform>,
    },
    /// A fake token for a device without Firebase (the debug app on an emulator).
    SetToken {
        platform: Platform,
        token: String,
    },
    SetEnabled {
        enabled: bool,
    },
    /// Issues the handles due now (a conversation made after the token came has none yet).
    RenewDue {},
    Status {},
    TestCounts {},
}
impl Drop for Action {
    fn drop(&mut self) {
        if let Self::SetToken { token, .. } = self {
            token.zeroize();
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub async fn execute(
    handle: Option<AccountHandle>,
    op: &str,
    mut args: SecretJson,
) -> Result<Value, CoreError> {
    let op = op
        .strip_prefix("push_local_")
        .ok_or_else(super::protocol::invalid)?;
    let command = SecretJson(json!({"op":op,"args":std::mem::take(&mut args.0)}));
    let mut action = Action::deserialize(&command.0).map_err(|_| super::protocol::invalid())?;
    let handle =
        handle.ok_or_else(|| CoreError::new(ErrorCode::NotAvailable, "Account not opened"))?;
    Ok(match &mut action {
        Action::Configure {
            proxy_url,
            platform,
        } => {
            handle.push_configure(PushConfig {
                proxy_url: proxy_url.take(),
                platform: *platform,
                snapshot_dir: None,
            })?;
            Value::Null
        }
        Action::SetToken { platform, token } => {
            handle
                .push_set_token(*platform, std::mem::take(token))
                .await?;
            Value::Null
        }
        Action::SetEnabled { enabled } => {
            handle.push_set_enabled(*enabled).await?;
            Value::Null
        }
        Action::RenewDue {} => {
            let summary = handle.push_renew_due(now_ms()).await?;
            json!({
                "issued": summary.issued,
                "announced": summary.announced,
                "withdrawn": summary.withdrawn,
            })
        }
        Action::Status {} => {
            let status = handle.push_status().await?;
            json!({
                "enabled": status.enabled,
                "platform": status.platform,
                "handles": status.handles,
            })
        }
        Action::TestCounts {} => {
            let fixture = handle.storage_test_fixture();
            let own = fixture.push_state().await?;
            let peers = fixture.push_peer_state().await?;
            let clears = handle.push_test_read_clears().await?;
            let fetches = handle.push_test_fetches();
            let sync = handle.mailbox_test_sync_trace().await?;
            // Only a few fixed failure kinds are named (e2e m7 facts); any other is "other".
            let code = |code: Option<ErrorCode>| match code {
                None => "none",
                Some(ErrorCode::NetworkTimeout) => "NetworkTimeout",
                Some(ErrorCode::TransportFailed) => "TransportFailed",
                Some(ErrorCode::AuthFailed) => "AuthFailed",
                Some(ErrorCode::Busy) => "Busy",
                Some(ErrorCode::StorageLocked) => "StorageLocked",
                Some(ErrorCode::AccountRestricted) => "AccountRestricted",
                Some(ErrorCode::NetworkNotStarted) => "NetworkNotStarted",
                Some(_) => "other",
            };
            let usize_count = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
            json!({
                // Handles of this device: per conversation, and the one for my other devices.
                "handles": usize_count(own.tags.iter().filter(|t| t.handle.is_some()).count()),
                "announced": usize_count(own.tags.iter().filter(|t| t.announced).count()),
                "own_handle": own.own_handle.is_some(),
                // Handles other devices gave this one: contacts' devices, and my other devices.
                "peer_handles": usize_count(peers.peers.iter().filter(|p| p.handle.is_some()).count()),
                "peer_withdrawn": usize_count(peers.peers.iter().filter(|p| p.handle.is_none()).count()),
                "own_handles": usize_count(own.own.len()),
                "deliveries": usize_count(own.deliveries.len()),
                // Read clearing (spec 7): my other devices with a ready session now, read
                // clears decided, and the `read` items they pushed.
                "own_sessions": usize_count(handle.mailbox_test_own_sessions().await?.len()),
                "read_clears": usize_count(clears.len()),
                "read_items": clears.iter().map(|(_, items)| u64::from(*items)).sum::<u64>(),
                // Fetches of received pushes (wake, read clear): all, online in time, done in time.
                "fetches": usize_count(fetches.len()),
                "fetches_online": usize_count(fetches.iter().filter(|(online, _)| *online).count()),
                "fetches_done": usize_count(fetches.iter().filter(|(_, done)| *done).count()),
                // Sessions with my other devices since this core started: dials that came back
                // installed or failed, connections from them accepted or refused (with the last
                // failure's kind), and pulls started and how they ended.
                "own_dials_ok": sync.dials_ok,
                "own_dials_failed": sync.dials_failed,
                "own_dial_error": code(sync.dial_error),
                "own_dial_check": sync.dial_check.unwrap_or("none"),
                "own_accepts_ok": sync.accepts_ok,
                "own_accepts_failed": sync.accepts_failed,
                "own_accept_error": code(sync.accept_error),
                "own_accept_check": sync.accept_check.unwrap_or("none"),
                "pulls": sync.pulls,
                "pulls_done": sync.pulls_done,
                "pulls_failed": sync.pulls_failed,
                "pulls_stopped": sync.pulls_stopped,
                "pull_failure": sync.pull_failure.unwrap_or("none"),
                "own_closes": sync.own_closes,
                "own_close": sync.own_close.unwrap_or("none"),
            })
        }
    })
}
