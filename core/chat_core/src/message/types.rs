use crate::account::{InstallationId, PeerId};
use std::fmt;

macro_rules! id {
    ($name:ident, $n:expr) => {
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            serde::Serialize,
            serde::Deserialize,
        )]
        pub struct $name(pub [u8; $n]);
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    &data_encoding::BASE32_NOPAD
                        .encode(&self.0)
                        .to_ascii_lowercase(),
                )
            }
        }
    };
}
id!(ConversationId, 32);
id!(MessageId, 16);
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct MessageKey {
    pub conversation: ConversationId,
    pub sender: PeerId,
    pub installation: InstallationId,
    pub id: MessageId,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct MessageCursor {
    pub lamport: u64,
    pub installation: InstallationId,
    pub id: MessageId,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeliveryState {
    Queued,
    SentDirect,
    Delivered,
    Read,
    Blocked,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BlockReason {
    Account,
    ContactRequired,
    Storage,
}
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
impl fmt::Debug for MessageView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessageView")
            .field("key", &self.key)
            .field("lamport", &self.lamport)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MessagePage {
    pub revision: u64,
    pub items: Vec<MessageView>,
    pub next: Option<MessageCursor>,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConversationView {
    pub id: ConversationId,
    pub peer: PeerId,
    pub last: Option<MessageView>,
    pub unread: u64,
    pub change_revision: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConversationCursor {
    pub change_revision: u64,
    pub id: ConversationId,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConversationPage {
    pub revision: u64,
    pub items: Vec<ConversationView>,
    pub next: Option<ConversationCursor>,
}
