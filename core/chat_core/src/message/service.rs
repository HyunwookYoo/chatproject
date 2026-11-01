use super::types::*;
use crate::{AccountHandle, CoreError, account::PeerId};

/// Message commands for the same owning account actor and storage worker.
#[derive(Clone)]
pub struct MessageHandle(pub(crate) AccountHandle);

pub(crate) enum MessageRequest {
    Open(PeerId),
    Conversations(Option<ConversationCursor>, u32),
    Messages(ConversationId, Option<MessageCursor>, u32),
    Send(PeerId, MessageId, String),
    Cancel(MessageKey),
    Retry(MessageKey),
    Read(ConversationId, Vec<MessageKey>),
}
pub(crate) enum MessageReply {
    Conversation(ConversationView),
    Conversations(ConversationPage),
    Messages(MessagePage),
    Message(MessageView),
    Done,
}
impl AccountHandle {
    pub fn messages(&self) -> MessageHandle {
        MessageHandle(self.clone())
    }
}
impl MessageHandle {
    pub async fn open_dm(&self, peer: PeerId) -> Result<ConversationView, CoreError> {
        match self.0.message(MessageRequest::Open(peer)).await? {
            MessageReply::Conversation(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    pub async fn list_conversations(
        &self,
        before: Option<ConversationCursor>,
        limit: u32,
    ) -> Result<ConversationPage, CoreError> {
        match self
            .0
            .message(MessageRequest::Conversations(before, limit))
            .await?
        {
            MessageReply::Conversations(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    pub async fn load_messages(
        &self,
        conversation: ConversationId,
        before: Option<MessageCursor>,
        limit: u32,
    ) -> Result<MessagePage, CoreError> {
        match self
            .0
            .message(MessageRequest::Messages(conversation, before, limit))
            .await?
        {
            MessageReply::Messages(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    /// Returns only after durable enqueue; `id` is the retained idempotency token.
    pub async fn send_text(
        &self,
        peer: PeerId,
        id: MessageId,
        body: String,
    ) -> Result<MessageView, CoreError> {
        match self.0.message(MessageRequest::Send(peer, id, body)).await? {
            MessageReply::Message(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    pub async fn cancel_send(&self, key: MessageKey) -> Result<MessageView, CoreError> {
        match self.0.message(MessageRequest::Cancel(key)).await? {
            MessageReply::Message(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    pub async fn retry_send(&self, key: MessageKey) -> Result<MessageView, CoreError> {
        match self.0.message(MessageRequest::Retry(key)).await? {
            MessageReply::Message(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    /// Marks at most 50 incoming keys the caller actually displayed, never the whole conversation.
    pub async fn mark_read(
        &self,
        conversation: ConversationId,
        keys: Vec<MessageKey>,
    ) -> Result<(), CoreError> {
        match self
            .0
            .message(MessageRequest::Read(conversation, keys))
            .await?
        {
            MessageReply::Done => Ok(()),
            _ => unreachable!(),
        }
    }
}
