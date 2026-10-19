//! Debug local-UI actions. These call the owning production account actor; none
//! manufactures approval, exports a private seed, or resets an active account.
use crate::{CoreError, ErrorCode, account::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroize;

// App-owned JSON copies are erased on admission rejection, cancellation and
// result disposal. Opaque WebSocket/serde internals have no erasure guarantee.
#[derive(Serialize)]
#[serde(transparent)]
pub struct SecretJson(pub Value);
impl Drop for SecretJson {
    fn drop(&mut self) {
        fn erase(value: &mut Value) {
            match value {
                Value::String(text) => text.zeroize(),
                Value::Array(values) => values.iter_mut().for_each(erase),
                Value::Object(values) => values.values_mut().for_each(erase),
                _ => {}
            }
        }
        erase(&mut self.0);
    }
}

#[derive(Deserialize)]
#[serde(
    tag = "op",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Action {
    Snapshot {},
    PrepareCandidate {},
    BeginCreate {},
    Contacts {},
    PairView {},
    ConfirmCreate {
        flow: FlowId,
        answers: [String; 3],
    },
    Recover {
        words: String,
        confirm_replacement: bool,
    },
    CreateInvite {
        name: Option<String>,
    },
    AcceptInvite {
        input: String,
        alias: String,
    },
    Safety {
        local: PeerId,
        peer: PeerId,
    },
    ConfirmSafety {
        peer: PeerId,
        digest: [u8; 32],
    },
    ConnectAccount {
        peer: PeerId,
    },
    RefreshRecord {
        peer: PeerId,
    },
    RevokeDevice {
        target: InstallationId,
    },
    PairBegin {
        direction: PairDirection,
        resume: Option<TxId>,
    },
    PairScan {
        qr: String,
    },
    Route {
        ticket: String,
    },
    PairConfirmCode {
        flow: FlowId,
        matches: bool,
    },
    PairApprove {
        flow: FlowId,
    },
    PairCancel {
        flow: FlowId,
    },
}
impl Drop for Action {
    fn drop(&mut self) {
        match self {
            Self::ConfirmCreate { answers, .. } => answers.zeroize(),
            Self::Recover { words, .. } => words.zeroize(),
            Self::PairScan { qr } => qr.zeroize(),
            Self::AcceptInvite { input, .. } => input.zeroize(),
            _ => {}
        }
    }
}
pub async fn execute(
    handle: Option<AccountHandle>,
    network: crate::NetworkHandle,
    op: &str,
    mut args: SecretJson,
) -> Result<Value, CoreError> {
    let handle =
        handle.ok_or_else(|| CoreError::new(ErrorCode::NotAvailable, "Account not opened"))?;
    let op = op
        .strip_prefix("account_local_")
        .ok_or_else(super::protocol::invalid)?;
    let mut fields = serde_json::Map::new();
    fields.insert("op".into(), Value::String(op.into()));
    fields.insert("args".into(), std::mem::take(&mut args.0));
    let command = SecretJson(Value::Object(fields));
    let mut action: Action =
        Action::deserialize(&command.0).map_err(|_| super::protocol::invalid())?;
    Ok(match &mut action {
        Action::Snapshot {} => json!(handle.snapshot().await?),
        Action::PrepareCandidate {} => {
            let v = handle.prepare_candidate().await?;
            json!({"installation_id":v.installation_id,"endpoint_id":v.endpoint_id})
        }
        Action::BeginCreate {} => {
            let v = handle.begin_create().await?;
            json!({"flow":v.flow,"words":v.words,"positions":v.positions})
        }
        Action::ConfirmCreate { flow, answers } => {
            handle
                .confirm_create(*flow, std::mem::take(answers))
                .await?;
            Value::Null
        }
        Action::Recover {
            words,
            confirm_replacement,
        } => {
            handle
                .recover(std::mem::take(words), *confirm_replacement)
                .await?;
            Value::Null
        }
        Action::CreateInvite { name } => json!(handle.create_invite(name.take()).await?),
        Action::AcceptInvite { input, alias } => json!(
            handle
                .accept_invite(std::mem::take(input), std::mem::take(alias))
                .await?
        ),
        Action::Contacts {} => json!(handle.contacts().await?),
        Action::Safety { local, peer } => {
            let v = safety_display(*local, *peer);
            json!({"digest":v.digest,"groups":v.groups})
        }
        Action::ConfirmSafety { peer, digest } => {
            handle.confirm_safety(*peer, *digest).await?;
            Value::Null
        }
        Action::ConnectAccount { peer } => {
            handle.connect_account(*peer).await?;
            Value::Null
        }
        Action::RefreshRecord { peer } => {
            handle.refresh_record(*peer).await?;
            Value::Null
        }
        Action::RevokeDevice { target } => {
            handle.revoke_device(*target).await?;
            Value::Null
        }
        Action::PairBegin { direction, resume } => {
            let v = handle.pair_begin(*direction, *resume).await?;
            json!({"flow":v.flow,"qr":v.qr,"expires_at":v.expires_at})
        }
        Action::Route { ticket } => {
            network
                .account_target_for_test(std::mem::take(ticket))
                .await?;
            Value::Null
        }
        Action::PairScan { qr } => json!(handle.pair_scan(std::mem::take(qr)).await?),
        Action::PairConfirmCode { flow, matches } => {
            handle.pair_confirm_code(*flow, *matches).await?;
            Value::Null
        }
        Action::PairApprove { flow } => {
            handle.pair_approve(*flow).await?;
            Value::Null
        }
        Action::PairCancel { flow } => {
            handle.pair_cancel(*flow).await?;
            Value::Null
        }
        Action::PairView {} => json!(handle.pair_view().await?),
    })
}
