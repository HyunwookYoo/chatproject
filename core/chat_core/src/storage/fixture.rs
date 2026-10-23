//! Debug-only fixtures for the real private worker. No SQL/key controls reach FFI.
pub use crate::account::storage::migration_fixtures::{assert_account_preserved, migration_states};
pub use crate::message::store::MessageFacts;
use crate::message::store::{FixtureOp, MessageRepo};
pub use crate::message::types::*;
pub use crate::storage::migration::MigrationStage;
pub use crate::storage::worker::CommitFault;
use crate::{
    CoreError, ErrorCode,
    account::storage::{KeyProtector, StoredPlaintext},
    storage::worker::{StorageHandle, TestOp},
};
use std::{
    path::Path,
    sync::{Arc, atomic::Ordering},
};
use tokio::sync::oneshot;
#[derive(Clone)]
pub struct Messages(MessageRepo);
impl Messages {
    pub fn local_peer(&self) -> crate::account::PeerId {
        self.0.local_peer()
    }
    pub async fn open_dm(
        &self,
        local: crate::account::PeerId,
        peer: crate::account::PeerId,
    ) -> Result<ConversationView, CoreError> {
        self.0.open_dm(local, peer).await
    }
    pub async fn enqueue(
        &self,
        peer: crate::account::PeerId,
        id: MessageId,
        body: String,
        now: u64,
    ) -> Result<MessageView, CoreError> {
        self.0.enqueue(peer, id, body, now).await
    }
    pub async fn load_messages(
        &self,
        id: ConversationId,
        before: Option<MessageCursor>,
        limit: u32,
    ) -> Result<MessagePage, CoreError> {
        self.0.load_messages(id, before, limit).await
    }
    pub async fn list_conversations(
        &self,
        before: Option<ConversationCursor>,
        limit: u32,
    ) -> Result<ConversationPage, CoreError> {
        self.0.list_conversations(before, limit).await
    }
}
pub struct Fixture {
    pub handle: Handle,
}
#[derive(Clone)]
pub struct Handle(StorageHandle);
impl Handle {
    pub async fn discard_pending(&self) -> Result<(), CoreError> {
        self.0.discard_pending().await
    }
    pub async fn load_account(&self) -> Result<Option<StoredPlaintext>, CoreError> {
        self.0.load_account().await
    }
    pub async fn commit_account(&self, state: StoredPlaintext) -> Result<(), CoreError> {
        self.0.commit_account(state).await
    }
    pub async fn suspend(&self) -> Result<(), CoreError> {
        self.0.suspend().await
    }
    pub async fn resume(&self) -> Result<(), CoreError> {
        self.0.resume().await
    }
    pub fn accepted_ticket(&self) -> u64 {
        self.0.accepted_ticket()
    }
    pub fn invalidate(&self, ticket: u64) {
        self.0.invalidate(ticket);
    }
}
pub struct Pause {
    entered: tokio::sync::Mutex<Option<oneshot::Receiver<()>>>,
    release: Option<std::sync::mpsc::Sender<()>>,
}
impl Pause {
    pub async fn wait_entered(&self) {
        self.entered.lock().await.take().unwrap().await.unwrap();
    }
    pub fn release(mut self) {
        let _ = self.release.take().unwrap().send(());
    }
}
impl Drop for Pause {
    fn drop(&mut self) {
        if let Some(tx) = self.release.take() {
            let _ = tx.send(());
        }
    }
}
impl Fixture {
    pub async fn pause_message_reply(&self)->Pause {
        let (entered,rx)=oneshot::channel();let (release,wait)=std::sync::mpsc::channel();let (tx,done)=oneshot::channel();
        self.handle.0.test(TestOp::MessageReplyPause(entered,wait,tx)).await;done.await.unwrap();
        Pause {entered:tokio::sync::Mutex::new(Some(rx)),release:Some(release)}
    }

    pub async fn message_counts(&self)->Result<(u64,u64,u64,u64),CoreError> {
        let (tx,rx)=oneshot::channel();self.handle.0.test(TestOp::Message(FixtureOp::Counts(tx))).await;rx.await.unwrap()
    }
    pub async fn message_clock_after_operation(&self,clock:Arc<crate::account::clock::FakeClock>,seconds:u64) {
        let (tx,rx)=oneshot::channel();self.handle.0.test(TestOp::MessageClock(clock,seconds,tx)).await;rx.await.unwrap()
    }

    pub async fn message_fault(&self, fault: CommitFault) {
        let (tx, rx) = oneshot::channel();
        self.handle.0.test(TestOp::MessageFault(fault, tx)).await;
        rx.await.unwrap();
    }
    pub async fn messages(&self) -> Result<Messages, CoreError> {
        use crate::account::storage::schema::AccountState;
        let stored = self.handle.load_account().await?.ok_or_else(|| {
            CoreError::new(ErrorCode::AccountRestricted, "active_account_required")
        })?;
        let AccountState::Active(local) = AccountState::decode(&stored)? else {
            return Err(CoreError::new(
                ErrorCode::AccountRestricted,
                "active_account_required",
            ));
        };
        Ok(Messages(MessageRepo::new(self.handle.0.clone(), &local)))
    }
    pub async fn message_facts(&self, key: MessageKey) -> Result<MessageFacts, CoreError> {
        let (tx, rx) = oneshot::channel();
        self.handle
            .0
            .test(TestOp::Message(FixtureOp::Facts(key, tx)))
            .await;
        rx.await.unwrap()
    }
    pub async fn fill_message_quota(
        &self,
        key: MessageKey,
        count: u64,
        bytes: Option<u64>,
    ) -> Result<(), CoreError> {
        let (tx, rx) = oneshot::channel();
        self.handle
            .0
            .test(TestOp::Message(FixtureOp::Fill(key, count, bytes, tx)))
            .await;
        rx.await.unwrap()
    }
    pub async fn deliver_message(&self, key: MessageKey) -> Result<(), CoreError> {
        let (tx, rx) = oneshot::channel();
        self.handle
            .0
            .test(TestOp::Message(FixtureOp::Delivered(key, tx)))
            .await;
        rx.await.unwrap()
    }
    pub async fn set_message_lamport(
        &self,
        id: ConversationId,
        value: u64,
    ) -> Result<(), CoreError> {
        let (tx, rx) = oneshot::channel();
        self.handle
            .0
            .test(TestOp::Message(FixtureOp::Lamport(id, value, tx)))
            .await;
        rx.await.unwrap()
    }
    pub(crate) fn from_handle(handle: StorageHandle) -> Self {
        Self {
            handle: Handle(handle),
        }
    }
    /// Narrow corruption fixture, never an arbitrary SQL/key interface.
    pub fn set_unknown_schema(root: &Path, p: Arc<dyn KeyProtector>) -> Result<(), CoreError> {
        let mut opened = crate::storage::migration::open_or_migrate(root, &*p)?;
        drop(opened.connection.take());
        opened.ownership.with_lock(|root| {
            let c = crate::storage::migration::reopen(root, &*p, &opened.secrets.db_id)?;
            c.pragma_update(None, "user_version", 99).map_err(|_| {
                CoreError::new(ErrorCode::StorageCorrupt, "fixture_schema_write_failed")
            })
        })
    }
    pub async fn open(root: &Path, p: Arc<dyn KeyProtector>) -> Result<Self, CoreError> {
        let root = root.to_owned();
        tokio::task::spawn_blocking(move || {
            StorageHandle::open_blocking(root, p).map(|(h, _)| Self { handle: Handle(h) })
        })
        .await
        .unwrap()
    }
    pub fn open_blocking_error(root: &Path, p: Arc<dyn KeyProtector>) -> ErrorCode {
        StorageHandle::open_blocking(root.to_owned(), p)
            .err()
            .expect("fixture unexpectedly opened")
            .code
    }
    pub async fn close(self) {
        let (tx, rx) = oneshot::channel();
        self.handle.0.test(TestOp::Stop(tx)).await;
        rx.await.unwrap();
    }
    pub fn live_connections(&self) -> usize {
        self.handle.0.counts.live.load(Ordering::SeqCst)
    }
    pub fn opens(&self) -> usize {
        self.handle.0.counts.opens.load(Ordering::SeqCst)
    }
    pub fn remaining_capacity(&self) -> usize {
        self.handle.0.capacity()
    }
    pub async fn pause(&self) -> Pause {
        let (entered, rx) = oneshot::channel();
        let (tx, release) = std::sync::mpsc::channel();
        self.handle.0.test(TestOp::Pause(entered, release)).await;
        Pause {
            entered: tokio::sync::Mutex::new(Some(rx)),
            release: Some(tx),
        }
    }
    pub async fn fault(&self, fault: CommitFault) {
        let (tx, rx) = oneshot::channel();
        self.handle.0.test(TestOp::Fault(fault, tx)).await;
        rx.await.unwrap();
    }
    pub async fn fault_after_commits(&self, after: usize, fault: CommitFault) {
        let (tx, rx) = oneshot::channel();
        self.handle
            .0
            .test(TestOp::CommitAction(
                after,
                crate::storage::worker::CommitAction::Fault(fault),
                tx,
            ))
            .await;
        rx.await.unwrap();
    }
    pub async fn clock_after_commits(
        &self,
        after: usize,
        clock: Arc<crate::account::clock::FakeClock>,
        seconds: u64,
    ) {
        let (tx, rx) = oneshot::channel();
        self.handle
            .0
            .test(TestOp::CommitAction(
                after,
                crate::storage::worker::CommitAction::Clock(clock, seconds),
                tx,
            ))
            .await;
        rx.await.unwrap();
    }
    pub async fn account_rows(&self) -> usize {
        let (tx, rx) = oneshot::channel();
        self.handle.0.test(TestOp::Rows(tx)).await;
        rx.await.unwrap().unwrap()
    }
}

// Codec fixtures expose the real decoder only in debug test-hook builds.
pub use crate::message::wire::{decode as decode_message, Frame as MessageFrame};
pub use crate::account::MessageMutation;
