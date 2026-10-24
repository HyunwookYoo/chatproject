//! Local Debug actions borrow the existing account's message facade.
use super::account::SecretJson;
use crate::{AccountHandle, CoreError, ErrorCode, account::PeerId, message::types::*};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroize;

/// A single bounded fault owner per control connection; it never changes facts.
#[derive(Default)]
pub(super) struct AckLossControl {
    used: AtomicBool,
    holds: tokio::sync::Mutex<Option<(crate::account::MessagePause, crate::account::MessagePause)>>,
    cancel: CancellationToken,
}
impl AckLossControl {
    async fn arm(
        self: &Arc<Self>,
        handle: &AccountHandle,
        key: MessageKey,
    ) -> Result<(), CoreError> {
        if self.used.swap(true, Ordering::SeqCst) {
            return Err(CoreError::new(ErrorCode::Busy, "Ack loss fixture busy"));
        }
        let mut holds = self.holds.lock().await;
        let pair = handle.message_ack_pause(key).await?;
        *holds = Some(pair);
        let weak = Arc::downgrade(self);
        let cancelled = self.cancel.clone();
        tokio::spawn(async move {
            tokio::select! { _=tokio::time::sleep(Duration::from_secs(60))=>{}, _=cancelled.cancelled()=>{} }
            if let Some(owner) = weak.upgrade() {
                owner.release().await;
            }
        });
        Ok(())
    }
    async fn wait(&self) -> Result<Value, CoreError> {
        let mut holds = self.holds.lock().await;
        let (response, receipt) = holds.as_mut().ok_or_else(super::protocol::invalid)?;
        let result = tokio::select! {
            _=self.cancel.cancelled()=>Err(CoreError::new(ErrorCode::NotAvailable,"Ack loss fixture closed")),
            result=tokio::time::timeout(Duration::from_secs(10),async {
                tokio::try_join!(response.wait_entered_checked(),receipt.wait_entered_checked())?;
                Ok::<_,CoreError>(json!({"response_checkpoint":true,"receipt_checkpoint":true}))
            })=>result.unwrap_or_else(|_|Err(CoreError::new(ErrorCode::NetworkTimeout,"Ack loss checkpoint deadline"))),
        };
        if result.is_err() {
            holds.take();
            self.cancel.cancel();
        }
        result
    }
    pub(super) async fn release(&self) {
        self.cancel.cancel();
        self.holds.lock().await.take();
    }
}
impl Drop for AckLossControl {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Deserialize)]
#[serde(
    tag = "op",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum TestAction {
    AckLossArm { key: MessageKey },
    AckLossWait {},
    AckLossRelease {},
    WriteCounts {},
}

pub(super) async fn execute_test(
    handle: Option<AccountHandle>,
    control: Arc<AckLossControl>,
    op: &str,
    mut args: SecretJson,
) -> Result<Value, CoreError> {
    let op = op
        .strip_prefix("message_local_test_")
        .ok_or_else(super::protocol::invalid)?;
    let command = SecretJson(json!({"op":op,"args":std::mem::take(&mut args.0)}));
    let action = TestAction::deserialize(&command.0).map_err(|_| super::protocol::invalid())?;
    let handle =
        handle.ok_or_else(|| CoreError::new(ErrorCode::NotAvailable, "Account not opened"))?;
    match action {
        TestAction::AckLossArm { key } => {
            control.arm(&handle, key).await?;
            Ok(Value::Null)
        }
        TestAction::AckLossWait {} => control.wait().await,
        TestAction::AckLossRelease {} => {
            control.release().await;
            Ok(Value::Null)
        }
        TestAction::WriteCounts {} => {
            let (payload, response) = handle.message_test_write_starts().await?;
            Ok(json!({"payload_writes":payload,"response_writes":response}))
        }
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
    OpenDm {
        peer: PeerId,
    },
    ListConversations {
        before: Option<ConversationCursor>,
        limit: u32,
    },
    LoadMessages {
        conversation: ConversationId,
        before: Option<MessageCursor>,
        limit: u32,
    },
    SendText {
        peer: PeerId,
        id: MessageId,
        body: String,
    },
    CancelSend {
        key: MessageKey,
    },
    RetrySend {
        key: MessageKey,
    },
    MarkRead {
        conversation: ConversationId,
        keys: Vec<MessageKey>,
    },
}
impl Drop for Action {
    fn drop(&mut self) {
        if let Self::SendText { body, .. } = self {
            body.zeroize();
        }
    }
}

pub async fn execute(
    handle: Option<AccountHandle>,
    op: &str,
    mut args: SecretJson,
) -> Result<Value, CoreError> {
    let op = op
        .strip_prefix("message_local_")
        .ok_or_else(super::protocol::invalid)?;
    let command = SecretJson(json!({"op":op,"args":std::mem::take(&mut args.0)}));
    let mut action = Action::deserialize(&command.0).map_err(|_| super::protocol::invalid())?;
    let handle = handle
        .ok_or_else(|| CoreError::new(ErrorCode::NotAvailable, "Account not opened"))?
        .messages();
    Ok(match &mut action {
        Action::OpenDm { peer } => json!(handle.open_dm(*peer).await?),
        Action::ListConversations { before, limit } => {
            json!(handle.list_conversations(*before, *limit).await?)
        }
        Action::LoadMessages {
            conversation,
            before,
            limit,
        } => json!(handle.load_messages(*conversation, *before, *limit).await?),
        Action::SendText { peer, id, body } => {
            json!(handle.send_text(*peer, *id, std::mem::take(body)).await?)
        }
        Action::CancelSend { key } => json!(handle.cancel_send(*key).await?),
        Action::RetrySend { key } => json!(handle.retry_send(*key).await?),
        Action::MarkRead { conversation, keys } => {
            handle
                .mark_read(*conversation, std::mem::take(keys))
                .await?;
            Value::Null
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Advance {
    seconds: u64,
}

pub fn advance(slot: &crate::runtime::AccountSlot, args: SecretJson) -> Result<Value, CoreError> {
    let Advance { seconds } =
        Advance::deserialize(&args.0).map_err(|_| super::protocol::invalid())?;
    if seconds > 31_536_000 {
        return Err(super::protocol::invalid());
    }
    let clock = slot
        .controlled_clock
        .as_ref()
        .filter(|_| slot.handle.is_some())
        .ok_or_else(|| CoreError::new(ErrorCode::NotAvailable, "Controlled clock unavailable"))?;
    let (unix_seconds, monotonic_ms) = clock
        .advance_seconds(seconds)
        .ok_or_else(super::protocol::invalid)?;
    Ok(json!({"unix_seconds":unix_seconds,"monotonic_ms":monotonic_ms}))
}
