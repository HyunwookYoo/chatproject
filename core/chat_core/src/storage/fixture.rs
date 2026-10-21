//! Debug-only fixtures for the real private worker. No SQL/key controls reach FFI.
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
pub struct Fixture {
    pub handle: Handle,
}
#[derive(Clone)]
pub struct Handle(StorageHandle);
impl Handle {
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
    pub async fn account_rows(&self) -> usize {
        let (tx, rx) = oneshot::channel();
        self.handle.0.test(TestOp::Rows(tx)).await;
        rx.await.unwrap().unwrap()
    }
}
