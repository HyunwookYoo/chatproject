//! Typed message boundary for the existing owning account actor.
use super::chat::{CoreError, DeliveryState, ErrorCode, MessageKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    Account,
    ContactRequired,
    Storage,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageCursor {
    pub lamport: u64,
    pub installation: Vec<u8>,
    pub id: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationCursor {
    pub change_revision: u64,
    pub id: Vec<u8>,
}
// Bodies are display data; intentionally omit Debug to keep them out of diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub struct MessageView {
    pub key: MessageKey,
    pub lamport: u64,
    pub sent_at_ms: u64,
    pub body: String,
    pub outgoing: bool,
    pub state: DeliveryState,
    pub cancel_requested: bool,
    pub block: Option<BlockReason>,
}
#[derive(Clone, PartialEq, Eq)]
pub struct MessagePage {
    pub revision: u64,
    pub items: Vec<MessageView>,
    pub next: Option<MessageCursor>,
}
#[derive(Clone, PartialEq, Eq)]
pub struct ConversationView {
    pub id: Vec<u8>,
    pub peer: Vec<u8>,
    pub last: Option<MessageView>,
    pub unread: u64,
    pub change_revision: u64,
}
#[derive(Clone, PartialEq, Eq)]
pub struct ConversationPage {
    pub revision: u64,
    pub items: Vec<ConversationView>,
    pub next: Option<ConversationCursor>,
}

fn invalid() -> CoreError {
    CoreError {
        code: ErrorCode::InvalidMessage,
        detail: "Invalid message argument".into(),
    }
}
pub(crate) fn id<const N: usize>(bytes: Vec<u8>) -> Result<[u8; N], CoreError> {
    bytes.try_into().map_err(|_| invalid())
}
pub(crate) fn integer(value: u64) -> Result<u64, CoreError> {
    i64::try_from(value).map(|_| value).map_err(|_| invalid())
}
fn page_limit(limit: u32) -> Result<(), CoreError> {
    if (1..=50).contains(&limit) {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn handle() -> Result<chat_core::MessageHandle, CoreError> {
    crate::global::CORE
        .account()
        .map(|account| account.messages())
        .map_err(Into::into)
}
pub async fn open_dm(peer: Vec<u8>) -> Result<ConversationView, CoreError> {
    let peer = chat_core::account::PeerId(id(peer)?);
    handle()?
        .open_dm(peer)
        .await
        .map(Into::into)
        .map_err(Into::into)
}
pub async fn list_conversations(
    before: Option<ConversationCursor>,
    limit: u32,
) -> Result<ConversationPage, CoreError> {
    page_limit(limit)?;
    let before = before.map(TryInto::try_into).transpose()?;
    handle()?
        .list_conversations(before, limit)
        .await
        .map(Into::into)
        .map_err(Into::into)
}
pub async fn load_messages(
    conversation: Vec<u8>,
    before: Option<MessageCursor>,
    limit: u32,
) -> Result<MessagePage, CoreError> {
    page_limit(limit)?;
    let conversation = chat_core::ConversationId(id(conversation)?);
    let before = before.map(TryInto::try_into).transpose()?;
    handle()?
        .load_messages(conversation, before, limit)
        .await
        .map(Into::into)
        .map_err(Into::into)
}
pub async fn send_text(peer: Vec<u8>, id: Vec<u8>, body: String) -> Result<MessageView, CoreError> {
    let peer = chat_core::account::PeerId(self::id(peer)?);
    let id = chat_core::MessageId(self::id(id)?);
    handle()?
        .send_text(peer, id, body)
        .await
        .map(Into::into)
        .map_err(Into::into)
}
pub async fn cancel_send(key: MessageKey) -> Result<MessageView, CoreError> {
    let key = key.try_into()?;
    handle()?
        .cancel_send(key)
        .await
        .map(Into::into)
        .map_err(Into::into)
}
pub async fn retry_send(key: MessageKey) -> Result<MessageView, CoreError> {
    let key = key.try_into()?;
    handle()?
        .retry_send(key)
        .await
        .map(Into::into)
        .map_err(Into::into)
}
pub async fn mark_read(conversation: Vec<u8>, keys: Vec<MessageKey>) -> Result<(), CoreError> {
    if keys.len() > 50 {
        return Err(invalid());
    }
    let conversation = chat_core::ConversationId(id(conversation)?);
    let keys = keys
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<_, _>>()?;
    handle()?
        .mark_read(conversation, keys)
        .await
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn page_bounds_are_rejected_before_account_lookup() {
        for limit in [0, 51, u32::MAX] {
            let error = list_conversations(None, limit)
                .await
                .err()
                .expect("invalid page accepted");
            assert_eq!(error.code, ErrorCode::InvalidMessage);
        }
    }
    #[test]
    fn cursor_integer_bounds_are_checked_without_truncation() {
        for value in [i64::MAX as u64 + 1, u64::MAX] {
            let cursor = MessageCursor {
                lamport: value,
                installation: vec![255; 32],
                id: vec![255; 16],
            };
            assert_eq!(
                chat_core::MessageCursor::try_from(cursor)
                    .expect_err("oversized cursor accepted")
                    .code,
                ErrorCode::InvalidMessage
            );
            let cursor = ConversationCursor {
                change_revision: value,
                id: vec![255; 32],
            };
            assert_eq!(
                chat_core::ConversationCursor::try_from(cursor)
                    .expect_err("oversized revision accepted")
                    .code,
                ErrorCode::InvalidMessage
            );
        }
    }
    #[test]
    fn cursor_and_key_roundtrips_keep_maximum_bytes_and_integer() {
        let cursor = chat_core::MessageCursor {
            lamport: i64::MAX as u64,
            installation: chat_core::account::InstallationId([255; 32]),
            id: chat_core::MessageId([255; 16]),
        };
        assert_eq!(
            chat_core::MessageCursor::try_from(MessageCursor::from(cursor)).unwrap(),
            cursor
        );
        let cursor = chat_core::ConversationCursor {
            change_revision: i64::MAX as u64,
            id: chat_core::ConversationId([255; 32]),
        };
        assert_eq!(
            chat_core::ConversationCursor::try_from(ConversationCursor::from(cursor)).unwrap(),
            cursor
        );
        let key = chat_core::MessageKey {
            conversation: cursor.id,
            sender: chat_core::account::PeerId([255; 32]),
            installation: chat_core::account::InstallationId([255; 32]),
            id: chat_core::MessageId([255; 16]),
        };
        assert_eq!(
            chat_core::MessageKey::try_from(MessageKey::from(key)).unwrap(),
            key
        );
    }
}
