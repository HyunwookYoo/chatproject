//! Debug-only fixtures for the real private worker. No SQL/key controls reach FFI.
pub use crate::account::storage::migration_fixtures::{
    MlsAccount, assert_account_preserved, migration_states, mls_account,
};
pub use crate::message::mls_store::{MlsOutbox, MlsState};
pub use crate::message::store::MessageFacts;
use crate::message::mls_group::{Committed, GroupOp, GroupReply, Outcome};
use crate::message::store::{FixtureOp, MessageDbOp, MessageDbReply, MessageRepo};
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
pub struct RejectedPrepare {
    repo: MessageRepo,
    key: MessageKey,
    token: crate::message::store::PrepareToken,
    pub code: ErrorCode,
}
impl RejectedPrepare {
    pub async fn reset(self)->Result<(),CoreError> {
        self.repo.operation(crate::message::store::MessageDbOp::ResetUnlaunched(self.key,self.token)).await.map(|_|())
    }
}
impl Messages {
    // Finite negative probe: ordinary SQL Prepare authorization/transition path,
    // followed by exact provenance reset. It never launches a network operation.
    pub async fn rejected_prepare(&self,key:MessageKey,receiver:crate::account::InstallationId,now:u64)->Result<RejectedPrepare,CoreError> {
        let (token,transition)=crate::message::store::PrepareToken::new();
        let result=self.0.operation(crate::message::store::MessageDbOp::RejectedPrepare(key,receiver,now,transition)).await;
        let code=result.err().ok_or_else(||CoreError::new(ErrorCode::InvalidMessage,"fixture_expected_rejected_prepare"))?.code;
        Ok(RejectedPrepare{repo:self.0.clone(),key,token,code})
    }

    pub async fn retry_send(&self, key: MessageKey, now_ms: u64) -> Result<MessageView,CoreError> {
        self.0.retry_send(key,now_ms).await
    }
    pub async fn cancel_send(&self, key: MessageKey) -> Result<MessageView,CoreError> {
        self.0.cancel_send(key).await
    }
    async fn mls(&self, op: MessageDbOp) -> Result<MessageDbReply, CoreError> {
        self.0.operation(op).await
    }
    /// A fresh KeyPackage for this installation (kept in the store's global scope).
    pub async fn mls_key_package(&self) -> Result<Vec<u8>, CoreError> {
        let op = MessageDbOp::MlsGroupOp(ConversationId([0; 32]), GroupOp::NewKeyPackage);
        match self.mls(op).await? {
            MessageDbReply::Group(GroupReply::KeyPackage(bytes)) => Ok(bytes),
            _ => unreachable!(),
        }
    }
    pub async fn mls_create_group(
        &self,
        conversation: ConversationId,
        peer: crate::account::PeerId,
        members: Vec<Vec<u8>>,
        now: u64,
    ) -> Result<MlsCreated, CoreError> {
        let op = MessageDbOp::MlsGroupOp(
            conversation,
            GroupOp::Create { peer, members, rejoin: None, now },
        );
        match self.mls(op).await? {
            MessageDbReply::Group(GroupReply::Created { group_id, first_commit_hash, commit, welcome }) => {
                Ok(MlsCreated { group_id, first_commit_hash, commit, welcome })
            }
            _ => unreachable!(),
        }
    }
    pub async fn mls_join_group(
        &self,
        conversation: ConversationId,
        peer: crate::account::PeerId,
        welcome: Vec<u8>,
        first_commit_hash: [u8; 32],
        now: u64,
    ) -> Result<u64, CoreError> {
        let op = MessageDbOp::MlsGroupOp(
            conversation,
            GroupOp::Join {
                peer,
                sender_peer: peer,
                sender_install: crate::account::InstallationId([0; 32]),
                welcome,
                first_commit_hash,
                rejoin: None,
                joined_with: None,
                now,
                wall: now,
            },
        );
        match self.mls(op).await? {
            MessageDbReply::Group(GroupReply::Outcome(Outcome::Joined)) => {}
            MessageDbReply::Group(GroupReply::Outcome(_)) => {
                return Err(CoreError::new(ErrorCode::Busy, "fixture_not_joined"));
            }
            _ => unreachable!(),
        }
        match self.mls(MessageDbOp::MlsGroupOp(conversation, GroupOp::Info)).await? {
            MessageDbReply::Group(GroupReply::Info(Some(info))) => Ok(info.epoch),
            _ => unreachable!(),
        }
    }
    /// One own Commit (the group protocol's `sync_members` step) with the given KeyPackages and
    /// removals; `None` when the group refuses a new Commit (an own one is unacknowledged).
    pub async fn mls_commit(
        &self,
        conversation: ConversationId,
        peer: crate::account::PeerId,
        add: Vec<Vec<u8>>,
        remove: Vec<crate::account::InstallationId>,
        now: u64,
    ) -> Result<Option<u64>, CoreError> {
        let op = MessageDbOp::MlsGroupOp(conversation, GroupOp::Commit { peer, add, remove, now });
        match self.mls(op).await? {
            MessageDbReply::Group(GroupReply::Committed(Committed::Done { epoch })) => Ok(Some(epoch)),
            MessageDbReply::Group(GroupReply::Committed(_)) => Ok(None),
            _ => unreachable!(),
        }
    }
    /// What this device does with a received Commit (the store half of a kind 17 `Commit`),
    /// as the group protocol would answer it.
    pub async fn mls_apply_commit(
        &self,
        conversation: ConversationId,
        peer: crate::account::PeerId,
        sender_peer: crate::account::PeerId,
        epoch: u64,
        commit: Vec<u8>,
        now: u64,
    ) -> Result<MlsCommitOutcome, CoreError> {
        let op = MessageDbOp::MlsGroupOp(
            conversation,
            GroupOp::ApplyCommit { peer, sender_peer, epoch, commit, now },
        );
        match self.mls(op).await? {
            MessageDbReply::Group(GroupReply::Outcome(outcome)) => Ok(MlsCommitOutcome::from(outcome)),
            _ => unreachable!(),
        }
    }
    /// The store half of a kind 17 `JoinRequest` from `sender_peer`'s installation named in the
    /// KeyPackage: `"done"`, `"busy"` or `"held"`.
    pub async fn mls_join_request(
        &self,
        conversation: ConversationId,
        peer: crate::account::PeerId,
        sender_peer: crate::account::PeerId,
        key_package: Vec<u8>,
        now: u64,
    ) -> Result<&'static str, CoreError> {
        let op = MessageDbOp::MlsGroupOp(
            conversation,
            GroupOp::JoinRequest { peer, sender_peer, key_package, now },
        );
        match self.mls(op).await? {
            MessageDbReply::Group(GroupReply::Committed(Committed::Done { .. })) => Ok("done"),
            MessageDbReply::Group(GroupReply::Committed(Committed::Busy)) => Ok("busy"),
            MessageDbReply::Group(GroupReply::Committed(Committed::Held(_))) => Ok("held"),
            _ => unreachable!(),
        }
    }
    pub async fn mls_drop_group(&self, conversation: ConversationId) -> Result<(), CoreError> {
        match self.mls(MessageDbOp::MlsGroupOp(conversation, GroupOp::Drop)).await? {
            MessageDbReply::Group(GroupReply::Done) => Ok(()),
            _ => unreachable!(),
        }
    }
    /// The ciphertext hash, or `None` when the conversation has no group yet.
    pub async fn mls_seal(&self, key: MessageKey, now: u64) -> Result<Option<[u8; 32]>, CoreError> {
        match self.mls(MessageDbOp::Seal(key, now)).await? {
            MessageDbReply::Sealed { ct_hash } => Ok(Some(ct_hash)),
            MessageDbReply::NeedGroup(_) => Ok(None),
            _ => unreachable!(),
        }
    }
    /// How many rows were sealed, or `None` when the conversation has no group yet.
    pub async fn mls_seal_pending(&self, conversation: ConversationId, now: u64) -> Result<Option<u64>, CoreError> {
        match self.mls(MessageDbOp::SealPending(conversation, now)).await? {
            MessageDbReply::Count(n) => Ok(Some(n)),
            MessageDbReply::NeedGroup(_) => Ok(None),
            _ => unreachable!(),
        }
    }
    pub async fn mls_reencrypt(&self, key: MessageKey, now: u64) -> Result<(), CoreError> {
        match self.mls(MessageDbOp::Reencrypt(key, now)).await? {
            MessageDbReply::Done => Ok(()),
            MessageDbReply::NeedGroup(_) => Err(CoreError::new(ErrorCode::NotAvailable, "fixture_no_group")),
            _ => unreachable!(),
        }
    }
    pub async fn mls_open_app(
        &self,
        sender: crate::account::PeerId,
        installation: crate::account::InstallationId,
        conversation: ConversationId,
        ciphertext: Vec<u8>,
        now: u64,
    ) -> Result<crate::mls::wire::MlsReply, CoreError> {
        let op = MessageDbOp::OpenApp(sender, installation, conversation, ciphertext, now);
        match self.mls(op).await? {
            MessageDbReply::Opened(reply) => Ok(reply),
            _ => unreachable!(),
        }
    }
    /// The ordinary M4 `Prepare` of an attempt to `receiver` (marks it in flight).
    pub async fn prepare_attempt(
        &self,
        key: MessageKey,
        receiver: crate::account::InstallationId,
        now: u64,
    ) -> Result<(), CoreError> {
        let (_token, transition) = crate::message::store::PrepareToken::new();
        let op = MessageDbOp::Prepare(key, receiver, false, now, now, transition);
        match self.mls(op).await? {
            MessageDbReply::Prepared(..) => Ok(()),
            _ => unreachable!(),
        }
    }
    /// The recorded outcome of a prepared attempt: bytes written (`SentDirect`) or not.
    pub async fn settle_attempt(&self, key: MessageKey, written: bool) -> Result<(), CoreError> {
        match self.mls(MessageDbOp::AttemptOutcome(key, None, None, written)).await? {
            MessageDbReply::Done => Ok(()),
            _ => unreachable!(),
        }
    }
    /// An M4 `Delivered` receipt from `recipient` for the canonical M4 text hash.
    pub async fn apply_delivered_receipt(
        &self,
        key: MessageKey,
        text_hash: [u8; 32],
        recipient: crate::account::InstallationId,
        peer: crate::account::PeerId,
        now: u64,
    ) -> Result<(), CoreError> {
        let receipt = crate::message::wire::ReceiptFrame {
            kind: crate::message::wire::ReceiptKind::Delivered,
            key,
            text_hash,
            recipient,
        };
        match self.mls(MessageDbOp::ApplyReceipt(receipt, peer, now)).await? {
            MessageDbReply::Receipt(_) => Ok(()),
            _ => unreachable!(),
        }
    }
    /// Keys of the outgoing messages the scheduler's `Due` would hand out now.
    pub async fn due_keys(&self, now: u64) -> Result<Vec<MessageKey>, CoreError> {
        match self.mls(MessageDbOp::Due(now, vec![], [None, None])).await? {
            MessageDbReply::Due(text, _, _) => Ok(text.into_iter().map(|d| d.key).collect()),
            _ => unreachable!(),
        }
    }
    pub async fn mls_mark_waiting(&self, key: MessageKey, waiting: bool) -> Result<(), CoreError> {
        match self.mls(MessageDbOp::MarkWaitingPeerUpdate(key, waiting)).await? {
            MessageDbReply::Done => Ok(()),
            _ => unreachable!(),
        }
    }
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

/// Read-only snapshot of a closed store's SQL, taken WITHOUT upgrading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbProbe {
    pub user_version: i64,
    pub schema_version: i64,
    pub tables: Vec<String>,
    /// (name, whitespace/quote-normalised DDL) of every table and index.
    pub schema: Vec<(String, String)>,
    pub message_count: u64,
    /// Canonical, type-tagged dump of every `message` row (all columns, primary-key order).
    pub message_rows: Vec<u8>,
    pub outbox: Vec<OutboxProbe>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxProbe {
    pub msg_id: Vec<u8>,
    pub codec: i64,
    pub wire: Vec<u8>,
    pub attempts: i64,
    pub retry_delay_ms: i64,
    pub next_retry_at_ms: i64,
    pub dispatch_state: i64,
    /// `None` on a v1 table, which has no such column.
    pub reencrypts: Option<i64>,
    pub waiting_peer_update: Option<i64>,
}
const V1_DOWNGRADE: &str = "
DROP TABLE mls_kv;
DROP TABLE mls_conversation;
DROP TABLE mls_delivery;
DROP TABLE mls_hold;
CREATE TABLE storage_meta_v1 (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
    schema_version INTEGER NOT NULL CHECK(schema_version = 1),
    db_id BLOB NOT NULL CHECK(length(db_id) = 16),
    binding BLOB NOT NULL,
    legacy_hash BLOB CHECK(legacy_hash IS NULL OR length(legacy_hash) = 32),
    migration_ready INTEGER NOT NULL CHECK(migration_ready IN (0, 1)),
    message_revision INTEGER NOT NULL CHECK(message_revision BETWEEN 0 AND 9223372036854775807)
) STRICT;
INSERT INTO storage_meta_v1
    SELECT singleton,1,db_id,binding,legacy_hash,migration_ready,message_revision FROM storage_meta;
DROP TABLE storage_meta;
ALTER TABLE storage_meta_v1 RENAME TO storage_meta;
CREATE TABLE outbox_v1 (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    sender_peer BLOB NOT NULL CHECK(length(sender_peer)=32),
    sender_install BLOB NOT NULL CHECK(length(sender_install)=32),
    msg_id BLOB NOT NULL CHECK(length(msg_id)=16),
    codec INTEGER NOT NULL CHECK(codec=1),
    wire BLOB NOT NULL CHECK(length(wire) BETWEEN 1 AND 61440),
    attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 4294967295),
    retry_delay_ms INTEGER NOT NULL CHECK(retry_delay_ms BETWEEN 0 AND 300000),
    next_retry_at_ms INTEGER NOT NULL CHECK(next_retry_at_ms>=0),
    dispatch_state INTEGER NOT NULL CHECK(dispatch_state IN (0,1,2)),
    PRIMARY KEY(conversation,sender_peer,sender_install,msg_id),
    FOREIGN KEY(conversation,sender_peer,sender_install,msg_id)
        REFERENCES message(conversation,sender_peer,sender_install,msg_id)
) STRICT;
INSERT INTO outbox_v1
    SELECT conversation,sender_peer,sender_install,msg_id,codec,wire,attempts,
           retry_delay_ms,next_retry_at_ms,dispatch_state FROM outbox;
DROP TABLE outbox;
ALTER TABLE outbox_v1 RENAME TO outbox;
CREATE INDEX outbox_due ON outbox(dispatch_state,next_retry_at_ms,conversation);
";
fn probe_error(_: rusqlite::Error) -> CoreError {
    CoreError::new(ErrorCode::StorageCorrupt, "fixture_probe_failed")
}
fn probe(c: &rusqlite::Connection) -> Result<DbProbe, CoreError> {
    use rusqlite::types::ValueRef;
    let user_version = c
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(probe_error)?;
    let schema_version = c
        .query_row("SELECT schema_version FROM storage_meta", [], |r| r.get(0))
        .map_err(probe_error)?;
    let names = |sql: &str| -> Result<Vec<String>, CoreError> {
        c.prepare(sql)
            .map_err(probe_error)?
            .query_map([], |r| r.get(0))
            .map_err(probe_error)?
            .collect::<Result<_, _>>()
            .map_err(probe_error)
    };
    let tables = names("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
    let schema = c
        .prepare("SELECT name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")
        .map_err(probe_error)?
        .query_map([], |r| {
            let sql: String = r.get(1)?;
            let sql = sql.replace('"', "").split_whitespace().collect::<Vec<_>>().join(" ");
            Ok((r.get(0)?, sql))
        })
        .map_err(probe_error)?
        .collect::<Result<_, _>>()
        .map_err(probe_error)?;
    let mut message_rows = Vec::new();
    let mut message_count = 0;
    {
        let mut stmt = c
            .prepare("SELECT * FROM message ORDER BY conversation,sender_peer,sender_install,msg_id")
            .map_err(probe_error)?;
        let columns = stmt.column_count();
        let mut rows = stmt.query([]).map_err(probe_error)?;
        while let Some(row) = rows.next().map_err(probe_error)? {
            message_count += 1;
            for i in 0..columns {
                match row.get_ref(i).map_err(probe_error)? {
                    ValueRef::Null => message_rows.push(0),
                    ValueRef::Integer(n) => {
                        message_rows.push(1);
                        message_rows.extend(n.to_be_bytes());
                    }
                    ValueRef::Real(f) => {
                        message_rows.push(2);
                        message_rows.extend(f.to_be_bytes());
                    }
                    ValueRef::Text(t) | ValueRef::Blob(t) => {
                        message_rows.push(3);
                        message_rows.extend((t.len() as u64).to_be_bytes());
                        message_rows.extend(t);
                    }
                }
            }
        }
    }
    let has_v2_columns = c
        .query_row(
            "SELECT count(*) FROM pragma_table_info('outbox') WHERE name='reencrypts'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(probe_error)?
        == 1;
    let columns = if has_v2_columns {
        "reencrypts,waiting_peer_update"
    } else {
        "NULL,NULL"
    };
    let outbox = c
        .prepare(&format!(
            "SELECT msg_id,codec,wire,attempts,retry_delay_ms,next_retry_at_ms,dispatch_state,{columns} \
             FROM outbox ORDER BY msg_id"
        ))
        .map_err(probe_error)?
        .query_map([], |r| {
            Ok(OutboxProbe {
                msg_id: r.get(0)?,
                codec: r.get(1)?,
                wire: r.get(2)?,
                attempts: r.get(3)?,
                retry_delay_ms: r.get(4)?,
                next_retry_at_ms: r.get(5)?,
                dispatch_state: r.get(6)?,
                reencrypts: r.get(7)?,
                waiting_peer_update: r.get(8)?,
            })
        })
        .map_err(probe_error)?
        .collect::<Result<_, _>>()
        .map_err(probe_error)?;
    Ok(DbProbe {
        user_version,
        schema_version,
        tables,
        schema,
        message_count,
        message_rows,
        outbox,
    })
}

/// What `Messages::mls_create_group` produced for the peer.
pub struct MlsCreated {
    pub group_id: Vec<u8>,
    pub first_commit_hash: [u8; 32],
    pub commit: Vec<u8>,
    pub welcome: Vec<u8>,
}
/// What became of a received Commit, as the group protocol reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MlsCommitOutcome {
    Ack,
    Joined,
    Adopted,
    Held,
    Failed { class: u8, epoch: u64 },
    Rejoin,
    Later,
    Removed,
}
impl From<Outcome> for MlsCommitOutcome {
    fn from(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Ack => Self::Ack,
            Outcome::Joined => Self::Joined,
            Outcome::Adopted => Self::Adopted,
            Outcome::Held(_) => Self::Held,
            Outcome::Failed { class, epoch } => Self::Failed { class, epoch },
            Outcome::Rejoin { .. } => Self::Rejoin,
            Outcome::Later => Self::Later,
            Outcome::Removed => Self::Removed,
        }
    }
}
/// A valid KeyPackage of the installation derived from `device_seed` for account `peer`, whose
/// private keys are thrown away: it can be added to a group but that device never joins.
pub fn mls_detached_key_package(peer: crate::account::PeerId, device_seed: [u8; 32]) -> Vec<u8> {
    let device = crate::account::derive_device(crate::account::Secret32::from(device_seed)).unwrap();
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch(include_str!("schema.sql")).unwrap();
    let provider = crate::mls::MlsProvider::new(
        crate::mls::kv::KvStore::load(&connection, [0; 32]).unwrap(),
    );
    crate::mls::group::new_key_package(
        &provider,
        &crate::mls::identity::mls_signer(&device),
        &crate::mls::identity::MlsCredential::issue(peer, &device),
    )
    .unwrap()
}
/// The conversation, epoch and Commit inside a kind 17 `Commit` frame (the complete envelope).
pub fn mls_commit_from_wire(wire: &[u8]) -> Option<(ConversationId, u64, Vec<u8>)> {
    let inner = crate::account::auth::payload(wire, 17).ok()?;
    match crate::mls::wire::MlsFrame::decode(&inner).ok()? {
        crate::mls::wire::MlsFrame::Commit { conversation, epoch, commit } => Some((conversation, epoch, commit)),
        _ => None,
    }
}
/// The conversation and ciphertext inside a kind 17 `App` frame (the complete envelope).
pub fn mls_app_from_wire(wire: &[u8]) -> Option<(ConversationId, Vec<u8>)> {
    let inner = crate::account::auth::payload(wire, 17).ok()?;
    match crate::mls::wire::MlsFrame::decode(&inner).ok()? {
        crate::mls::wire::MlsFrame::App { conversation, ciphertext } => Some((conversation, ciphertext)),
        _ => None,
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
    pub async fn saturate_message_attempts(&self,key:MessageKey,receipt:bool)->Result<(),CoreError> {
        let (tx,rx)=oneshot::channel();
        self.handle.0.test(TestOp::Message(FixtureOp::SaturateAttempts(key,receipt,tx))).await;
        rx.await.unwrap()
    }
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
    /// Turns an M4 plaintext job (codec 1) into the "awaiting encryption" row (codec 3, no
    /// wire) that the v1 -> v2 upgrade makes of it.
    pub async fn mls_promote_outbox(&self, key: MessageKey) -> Result<(), CoreError> {
        let (tx, rx) = oneshot::channel();
        self.handle.0.test(TestOp::Message(FixtureOp::MlsPromote(key, tx))).await;
        rx.await.unwrap()
    }
    pub async fn mls_outbox(&self, key: MessageKey) -> Result<MlsOutbox, CoreError> {
        let (tx, rx) = oneshot::channel();
        self.handle.0.test(TestOp::Message(FixtureOp::MlsOutbox(key, tx))).await;
        rx.await.unwrap()
    }
    /// Lowers (or with `None` restores) the outbox byte cap that sealing enforces.
    pub async fn mls_outbox_cap(&self, cap: Option<u64>) {
        let (tx, rx) = oneshot::channel();
        self.handle.0.test(TestOp::Message(FixtureOp::MlsOutboxCap(cap, tx))).await;
        rx.await.unwrap();
    }
    /// Lets the next own Commit of the conversation be made before the last one is acknowledged.
    pub async fn mls_clear_commit_flag(&self, conversation: ConversationId) -> Result<(), CoreError> {
        let (tx, rx) = oneshot::channel();
        self.handle.0.test(TestOp::Message(FixtureOp::MlsClearCommitFlag(conversation, tx))).await;
        rx.await.unwrap()
    }
    /// The kind 17 frame of one queued delivery (kind 1 Commit, 2 Welcome), if it is queued.
    pub async fn mls_delivery_frame(
        &self,
        conversation: ConversationId,
        recipient: crate::account::InstallationId,
        kind: u8,
        epoch: u64,
    ) -> Result<Option<Vec<u8>>, CoreError> {
        let (tx, rx) = oneshot::channel();
        self.handle.0.test(TestOp::Message(FixtureOp::MlsDeliveryFrame(conversation, recipient, kind, epoch, tx))).await;
        rx.await.unwrap()
    }
    /// Queues a delivery row as is (`Some((body, next_retry_at_ms))`) or deletes one (`None`).
    pub async fn mls_set_delivery(
        &self,
        conversation: ConversationId,
        recipient: crate::account::InstallationId,
        kind: u8,
        epoch: u64,
        row: Option<(Vec<u8>, u64)>,
    ) -> Result<(), CoreError> {
        let (tx, rx) = oneshot::channel();
        self.handle.0.test(TestOp::Message(FixtureOp::MlsSetDelivery(conversation, recipient, kind, epoch, row, tx))).await;
        rx.await.unwrap()
    }
    pub async fn due_hint(&self, now: u64) -> Result<bool, CoreError> {
        self.handle.0.due_hint(now).await
    }
    pub async fn mls_state(&self, conversation: ConversationId) -> Result<MlsState, CoreError> {
        let (tx, rx) = oneshot::channel();
        self.handle.0.test(TestOp::Message(FixtureOp::MlsState(conversation, tx))).await;
        rx.await.unwrap()
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
    /// Rewrite a closed, migrated store into the exact M4 (schema v1) shape: the four MLS tables
    /// and the v2 outbox columns go away, `user_version` and `schema_version` return to 1.
    /// Only valid while every outbox row is still codec 1.
    pub fn downgrade_to_v1(root: &Path, p: Arc<dyn KeyProtector>) -> Result<(), CoreError> {
        crate::storage::migration::with_raw_connection(root, &*p, |c| {
            let tx = c.transaction().map_err(probe_error)?;
            let other: i64 = tx
                .query_row("SELECT count(*) FROM outbox WHERE codec<>1", [], |r| r.get(0))
                .map_err(probe_error)?;
            if other != 0 {
                return Err(CoreError::new(ErrorCode::InvalidMessage, "fixture_not_downgradable"));
            }
            tx.execute_batch(V1_DOWNGRADE).map_err(probe_error)?;
            tx.pragma_update(None, "user_version", 1).map_err(probe_error)?;
            tx.commit().map_err(probe_error)
        })
    }
    /// Inspect a closed store without upgrading it.
    pub fn probe_database(root: &Path, p: Arc<dyn KeyProtector>) -> Result<DbProbe, CoreError> {
        crate::storage::migration::with_raw_connection(root, &*p, |c| probe(c))
    }
    /// Run the M4 (v1) reader's version check against a closed store.
    pub fn v1_reader_check(root: &Path, p: Arc<dyn KeyProtector>) -> Result<(), CoreError> {
        crate::storage::migration::with_raw_connection(root, &*p, |c| {
            crate::storage::migration::ready_v1(c)
        })
    }
    /// The next schema upgrade of the store under `root` fails after its DDL, before commit.
    pub fn fail_next_upgrade(root: &Path) {
        crate::storage::migration::fail_next_upgrade(root);
    }
    pub async fn set_outbox_retry(
        &self,
        key: MessageKey,
        attempts: u64,
        retry_delay_ms: u64,
        next_retry_at_ms: u64,
    ) -> Result<(), CoreError> {
        let (tx, rx) = oneshot::channel();
        self.handle
            .0
            .test(TestOp::Message(FixtureOp::OutboxRetry(
                key,
                attempts,
                retry_delay_ms,
                next_retry_at_ms,
                tx,
            )))
            .await;
        rx.await.unwrap()
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
pub use crate::mls::wire::{MlsFrame, MlsReply};
