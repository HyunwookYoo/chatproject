//! Local Debug actions on this device's server queue setting (M8): what the app's mailbox
//! settings call (`queue_set_mode`, `queue_state`), and counts of its queues and of what the
//! queue server answered, for the m8 scenario. Values only: no queue id, key or ciphertext
//! leaves the core.
use super::account::SecretJson;
use crate::account::{InstallationId, QueueMode, Verdict};
use crate::{AccountHandle, CoreError, ErrorCode};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    Auto,
    Always,
    Never,
}

#[derive(Deserialize)]
#[serde(
    tag = "op",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Action {
    SetMode {
        mode: Mode,
    },
    State {},
    /// With `peer`, the `peer_*` counts are of that device's `QueueRef`s only, and
    /// `refs_pending` of the frames to it.
    TestCounts {
        #[serde(default)]
        peer: Option<InstallationId>,
    },
}

fn mode_name(mode: QueueMode) -> &'static str {
    match mode {
        QueueMode::Auto => "auto",
        QueueMode::Always => "always",
        QueueMode::Never => "never",
    }
}

pub async fn execute(
    handle: Option<AccountHandle>,
    op: &str,
    mut args: SecretJson,
) -> Result<Value, CoreError> {
    let op = op
        .strip_prefix("queue_local_")
        .ok_or_else(super::protocol::invalid)?;
    let command = SecretJson(json!({"op":op,"args":std::mem::take(&mut args.0)}));
    let action = Action::deserialize(&command.0).map_err(|_| super::protocol::invalid())?;
    let handle =
        handle.ok_or_else(|| CoreError::new(ErrorCode::NotAvailable, "Account not opened"))?;
    Ok(match action {
        Action::SetMode { mode } => {
            let mode = match mode {
                Mode::Auto => QueueMode::Auto,
                Mode::Always => QueueMode::Always,
                Mode::Never => QueueMode::Never,
            };
            handle.set_queue_mode(mode).await?;
            Value::Null
        }
        Action::State {} => {
            let state = handle.queue_state().await?;
            json!({
                "mode": mode_name(state.mode),
                "has_mailbox": matches!(state.verdict, Verdict::HasMailbox { .. }),
                "using_queue": state.using_queue,
                "uptime_7d_permille": state.uptime_7d_permille,
            })
        }
        Action::TestCounts { peer } => {
            let fixture = handle.storage_test_fixture();
            let tables = fixture.queue_tables().await?;
            let deliveries = fixture.push_state().await?.deliveries;
            let counts = handle.queue_test_counts();
            let n = |count: usize| u64::try_from(count).unwrap_or(u64::MAX);
            let peers: Vec<_> = tables
                .peers
                .iter()
                .filter(|p| peer.is_none_or(|id| p.installation == id))
                .collect();
            json!({
                // This device's queues: `Active` ones the server took, `Retiring` ones.
                "own_active": n(tables
                    .own
                    .iter()
                    .filter(|q| q.server_confirmed && q.delete_at_ms.is_none())
                    .count()),
                "own_retiring": n(tables.own.iter().filter(|q| q.delete_at_ms.is_some()).count()),
                // Other devices' `QueueRef`s: live, withdrawn (a tombstone), dead (`gone`).
                "peer_live": n(peers.iter().filter(|p| p.queue.is_some() && !p.dead).count()),
                "peer_withdrawn": n(peers.iter().filter(|p| p.queue.is_none()).count()),
                "peer_dead": n(peers.iter().filter(|p| p.queue.is_some() && p.dead).count()),
                // This device's `QueueRef` frames still on their way (to `peer`).
                "refs_pending": n(deliveries
                    .iter()
                    .filter(|d| d.kind == 2 && peer.is_none_or(|id| d.recipient == id))
                    .count()),
                // Since this core started.
                "created": counts.created,
                "sends_message": counts.sends_message,
                "sends_standing": counts.sends_standing,
                "fetched": counts.fetched,
                "acked": counts.acked,
            })
        }
    })
}
