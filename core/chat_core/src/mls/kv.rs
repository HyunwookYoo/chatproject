//! OpenMLS `StorageProvider` over an in-memory map that is loaded from, and flushed to, the
//! `mls_kv` table inside the caller's SQL transaction.
//!
//! Key layout follows OpenMLS's own memory storage (label, key, version) but every part is
//! MessagePack (`rmp-serde`, structs as positional arrays, byte iterables as `bin`): binary,
//! deterministic and compact. It must be self-describing: OpenMLS 0.9 deserializes some types
//! (`PastEpochDeletionPolicy`, an untagged enum) with `deserialize_any`, which non-self-describing
//! formats such as postcard cannot do. A key is `[label_len] label msgpack(key) version_be`; a
//! value is `[FORMAT] msgpack(value)`. List values (own leaf nodes, queued proposal refs) are
//! `[FORMAT] msgpack(Vec<msgpack(item)>)`. The leading `FORMAT` byte versions the encoding and
//! can never be `[` or `{`, so no stored value is ever mistaken for JSON.
//!
//! A `KvStore` holds the rows of one scope (a conversation id) plus the global rows. Writes
//! only mark keys dirty; `flush` upserts/deletes exactly those keys in the caller's
//! transaction. If that transaction is rolled back, drop the store and `load` again.
use crate::{CoreError, ErrorCode};
use openmls_traits::storage::{CURRENT_VERSION, StorageProvider, traits};
use rusqlite::{Connection, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::RwLock,
};

type R<T> = Result<T, CoreError>;

/// Scope of rows that belong to no conversation (key packages, signature keys).
const GLOBAL: [u8; 32] = [0; 32];
/// First byte of every stored value: encoding version 1 (MessagePack).
const FORMAT: u8 = 1;
/// `mls_kv` column limits (see the schema); `restore` refuses anything larger.
const MAX_KEY_BYTES: usize = 4_096;
const MAX_VALUE_BYTES: usize = 4_194_304;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum KvError {
    #[error("mls storage value encoding failed")]
    Encoding,
    #[error("mls storage state unavailable")]
    Unavailable,
}

#[derive(Clone, Copy)]
struct Label {
    name: &'static [u8],
    /// Rows that do not belong to one conversation are written to the global scope.
    global: bool,
}
const fn scoped(name: &'static [u8]) -> Label {
    Label {
        name,
        global: false,
    }
}
const fn global(name: &'static [u8]) -> Label {
    Label { name, global: true }
}
const KEY_PACKAGE: Label = global(b"KeyPackage");
const PSK: Label = global(b"Psk");
const SIGNATURE_KEY_PAIR: Label = global(b"SignatureKeyPair");
const ENCRYPTION_KEY_PAIR: Label = scoped(b"EncryptionKeyPair");
const EPOCH_KEY_PAIRS: Label = scoped(b"EpochKeyPairs");
const TREE: Label = scoped(b"Tree");
const GROUP_CONTEXT: Label = scoped(b"GroupContext");
const INTERIM_TRANSCRIPT_HASH: Label = scoped(b"InterimTranscriptHash");
const CONFIRMATION_TAG: Label = scoped(b"ConfirmationTag");
const JOIN_CONFIG: Label = scoped(b"MlsGroupJoinConfig");
const OWN_LEAF_NODES: Label = scoped(b"OwnLeafNodes");
const GROUP_STATE: Label = scoped(b"GroupState");
const QUEUED_PROPOSAL: Label = scoped(b"QueuedProposal");
const PROPOSAL_QUEUE_REFS: Label = scoped(b"ProposalQueueRefs");
const OWN_LEAF_NODE_INDEX: Label = scoped(b"OwnLeafNodeIndex");
const EPOCH_SECRETS: Label = scoped(b"EpochSecrets");
const RESUMPTION_PSK: Label = scoped(b"ResumptionPsk");
const MESSAGE_SECRETS: Label = scoped(b"MessageSecrets");
const GLOBAL_LABELS: [&[u8]; 3] = [KEY_PACKAGE.name, PSK.name, SIGNATURE_KEY_PAIR.name];

fn label_key(label: Label, body: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + label.name.len() + body.len() + 2);
    key.push(label.name.len() as u8);
    key.extend_from_slice(label.name);
    key.extend_from_slice(body);
    key.extend_from_slice(&CURRENT_VERSION.to_be_bytes());
    key
}
fn ser<V: Serialize + ?Sized>(value: &V) -> Result<Vec<u8>, KvError> {
    let mut out = Vec::new();
    value
        .serialize(
            &mut rmp_serde::Serializer::new(&mut out)
                .with_bytes(rmp_serde::config::BytesMode::ForceIterables),
        )
        .map_err(|_| KvError::Encoding)?;
    Ok(out)
}
fn de<V: DeserializeOwned>(bytes: &[u8]) -> Result<V, KvError> {
    let mut cursor = std::io::Cursor::new(bytes);
    let value = V::deserialize(&mut rmp_serde::Deserializer::new(&mut cursor))
        .map_err(|_| KvError::Encoding)?;
    if cursor.position() == bytes.len() as u64 {
        Ok(value)
    } else {
        Err(KvError::Encoding)
    }
}
fn make_key<K: Serialize + ?Sized>(label: Label, key: &K) -> Result<Vec<u8>, KvError> {
    Ok(label_key(label, &ser(key)?))
}
pub(super) fn is_global(key: &[u8]) -> bool {
    let Some((len, rest)) = key.split_first() else {
        return false;
    };
    rest.get(..*len as usize)
        .is_some_and(|label| GLOBAL_LABELS.contains(&label))
}
fn encode<V: Serialize + ?Sized>(value: &V) -> Result<Vec<u8>, KvError> {
    let mut out = vec![FORMAT];
    out.extend_from_slice(&ser(value)?);
    Ok(out)
}
fn decode<V: DeserializeOwned>(bytes: &[u8]) -> Result<V, KvError> {
    match bytes.split_first() {
        Some((&FORMAT, rest)) => de(rest),
        _ => Err(KvError::Encoding),
    }
}

#[derive(Default)]
struct State {
    rows: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Keys written or deleted since the last flush.
    dirty: BTreeSet<Vec<u8>>,
}
impl State {
    fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        if self.rows.get(&key) != Some(&value) {
            self.dirty.insert(key.clone());
            self.rows.insert(key, value);
        }
    }
    fn remove(&mut self, key: &[u8]) {
        if self.rows.remove(key).is_some() {
            self.dirty.insert(key.to_vec());
        }
    }
    fn list(&self, key: &[u8]) -> Result<Vec<Vec<u8>>, KvError> {
        self.rows.get(key).map_or(Ok(Vec::new()), |v| decode(v))
    }
    fn put_list(&mut self, key: Vec<u8>, list: &Vec<Vec<u8>>) -> Result<(), KvError> {
        if list.is_empty() {
            self.remove(&key);
        } else {
            self.put(key, encode(list)?);
        }
        Ok(())
    }
}

pub(crate) struct KvStore {
    state: RwLock<State>,
}

pub(super) fn storage_error(e: rusqlite::Error) -> CoreError {
    if let rusqlite::Error::SqliteFailure(code, _) = e
        && code.code == rusqlite::ErrorCode::DiskFull
    {
        return CoreError::new(ErrorCode::StorageFull, "mls_storage_full");
    }
    CoreError::new(ErrorCode::StorageUncertain, "mls_storage_uncertain")
}
fn corrupt() -> CoreError {
    CoreError::new(ErrorCode::StorageCorrupt, "mls_storage_invalid")
}

impl KvStore {
    /// Loads the rows of `scope` plus the global rows. Use `[0; 32]` for global-only work.
    pub(crate) fn load(c: &Connection, scope: [u8; 32]) -> R<KvStore> {
        let mut statement = c
            .prepare("SELECT key,value FROM mls_kv WHERE scope=?1 OR scope=?2")
            .map_err(storage_error)?;
        let mut rows = statement
            .query(params![scope.as_slice(), GLOBAL.as_slice()])
            .map_err(storage_error)?;
        let mut state = State::default();
        while let Some(row) = rows.next().map_err(storage_error)? {
            let value: Vec<u8> = row.get(1).map_err(storage_error)?;
            if value.first() != Some(&FORMAT) {
                return Err(corrupt());
            }
            state.rows.insert(row.get(0).map_err(storage_error)?, value);
        }
        Ok(KvStore {
            state: RwLock::new(state),
        })
    }

    /// A store over rows that were read elsewhere (the notification extension's snapshot), with
    /// nothing marked dirty. Values are checked like `load` checks them.
    pub(crate) fn from_rows<'a>(rows: impl IntoIterator<Item = (&'a [u8], &'a [u8])>) -> R<KvStore> {
        let mut state = State::default();
        for (key, value) in rows {
            if key.is_empty() || key.len() > MAX_KEY_BYTES || value.first() != Some(&FORMAT) {
                return Err(corrupt());
            }
            state.rows.insert(key.to_vec(), value.to_vec());
        }
        Ok(KvStore {
            state: RwLock::new(state),
        })
    }

    /// The rows of one conversation `scope` in key order, and nothing of the global scope: no
    /// row of that scope and no row with a global label (installation signature key pair,
    /// KeyPackage private keys, PSKs), even if one were stored under `scope`. For handing a
    /// conversation's state to a process that must not hold long-term keys.
    pub(crate) fn scope_rows(c: &Connection, scope: [u8; 32]) -> R<Vec<(Vec<u8>, Vec<u8>)>> {
        if scope == GLOBAL {
            return Err(corrupt());
        }
        let mut statement = c
            .prepare("SELECT key,value FROM mls_kv WHERE scope=?1 ORDER BY key")
            .map_err(storage_error)?;
        let mut rows = statement
            .query(params![scope.as_slice()])
            .map_err(storage_error)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(storage_error)? {
            let (key, value): (Vec<u8>, Vec<u8>) = (
                row.get(0).map_err(storage_error)?,
                row.get(1).map_err(storage_error)?,
            );
            if value.first() != Some(&FORMAT) {
                return Err(corrupt());
            }
            if !is_global(&key) {
                out.push((key, value));
            }
        }
        Ok(out)
    }

    /// Writes only the keys changed since `load`/the last flush (upsert or delete) into `tx`
    /// and returns the bytes written. New rows of group-independent labels (key packages,
    /// signature keys, PSKs) go to the global scope, everything else to `scope`.
    pub(crate) fn flush(&self, tx: &Transaction, scope: [u8; 32]) -> R<usize> {
        let mut state = self.state.write().map_err(|_| corrupt())?;
        let mut written = 0;
        {
            let mut upsert = tx
                .prepare_cached(
                    "INSERT INTO mls_kv(scope,key,value) VALUES(?1,?2,?3) \
                     ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                )
                .map_err(storage_error)?;
            let mut delete = tx
                .prepare_cached("DELETE FROM mls_kv WHERE key=?1")
                .map_err(storage_error)?;
            for key in &state.dirty {
                if let Some(value) = state.rows.get(key) {
                    let row_scope = if is_global(key) { GLOBAL } else { scope };
                    upsert
                        .execute(params![row_scope.as_slice(), key, value])
                        .map_err(storage_error)?;
                    written += key.len() + value.len();
                } else if delete.execute([key]).map_err(storage_error)? > 0 {
                    written += key.len();
                }
            }
        }
        state.dirty.clear();
        Ok(written)
    }

    /// Binary snapshot of this store's conversation rows (every row except the global labels),
    /// for `restore`. `[FORMAT]` then, per row in key order, `u32be key_len, key, u32be
    /// value_len, value`. One conversation scope holds one group, so this is that group's state.
    pub(crate) fn snapshot(&self) -> R<Vec<u8>> {
        let state = self.state.read().map_err(|_| corrupt())?;
        let mut out = vec![FORMAT];
        for (key, value) in state.rows.iter().filter(|(key, _)| !is_global(key)) {
            out.extend_from_slice(&(key.len() as u32).to_be_bytes());
            out.extend_from_slice(key);
            out.extend_from_slice(&(value.len() as u32).to_be_bytes());
            out.extend_from_slice(value);
        }
        Ok(out)
    }

    /// Replaces this store's conversation rows with a `snapshot`: rows missing from it are
    /// deleted, differing rows are rewritten, and every change is marked for the next `flush`.
    /// A malformed snapshot is rejected before anything changes.
    pub(crate) fn restore(&self, snapshot: &[u8]) -> R<()> {
        fn take<'a>(input: &mut &'a [u8], limit: usize) -> Option<&'a [u8]> {
            let (len, rest) = input.split_first_chunk::<4>()?;
            let len = u32::from_be_bytes(*len) as usize;
            if len > limit || len > rest.len() {
                return None;
            }
            let (item, rest) = rest.split_at(len);
            *input = rest;
            Some(item)
        }
        let mut input = match snapshot.split_first() {
            Some((&FORMAT, rest)) => rest,
            _ => return Err(corrupt()),
        };
        let mut rows = BTreeMap::new();
        while !input.is_empty() {
            let key = take(&mut input, MAX_KEY_BYTES).ok_or_else(corrupt)?;
            let value = take(&mut input, MAX_VALUE_BYTES).ok_or_else(corrupt)?;
            if key.is_empty()
                || is_global(key)
                || value.first() != Some(&FORMAT)
                || rows.insert(key.to_vec(), value.to_vec()).is_some()
            {
                return Err(corrupt());
            }
        }
        let mut state = self.state.write().map_err(|_| corrupt())?;
        let stale: Vec<Vec<u8>> = state
            .rows
            .keys()
            .filter(|key| !is_global(key) && !rows.contains_key(*key))
            .cloned()
            .collect();
        for key in stale {
            state.remove(&key);
        }
        for (key, value) in rows {
            state.put(key, value);
        }
        Ok(())
    }

    fn read(&self) -> Result<std::sync::RwLockReadGuard<'_, State>, KvError> {
        self.state.read().map_err(|_| KvError::Unavailable)
    }
    fn write(&self) -> Result<std::sync::RwLockWriteGuard<'_, State>, KvError> {
        self.state.write().map_err(|_| KvError::Unavailable)
    }
    fn put<K: Serialize + ?Sized, V: Serialize + ?Sized>(
        &self,
        label: Label,
        key: &K,
        value: &V,
    ) -> Result<(), KvError> {
        let (key, value) = (make_key(label, key)?, encode(value)?);
        self.write()?.put(key, value);
        Ok(())
    }
    fn get<K: Serialize + ?Sized, V: DeserializeOwned>(
        &self,
        label: Label,
        key: &K,
    ) -> Result<Option<V>, KvError> {
        let key = make_key(label, key)?;
        self.read()?.rows.get(&key).map(|v| decode(v)).transpose()
    }
    fn delete<K: Serialize + ?Sized>(&self, label: Label, key: &K) -> Result<(), KvError> {
        let key = make_key(label, key)?;
        self.write()?.remove(&key);
        Ok(())
    }
    fn append<K: Serialize + ?Sized, V: Serialize + ?Sized>(
        &self,
        label: Label,
        key: &K,
        item: &V,
    ) -> Result<(), KvError> {
        let (key, item) = (make_key(label, key)?, ser(item)?);
        let mut state = self.write()?;
        let mut list = state.list(&key)?;
        list.push(item);
        state.put_list(key, &list)
    }
    fn remove_item<K: Serialize + ?Sized, V: Serialize + ?Sized>(
        &self,
        label: Label,
        key: &K,
        item: &V,
    ) -> Result<(), KvError> {
        let (key, item) = (make_key(label, key)?, ser(item)?);
        let mut state = self.write()?;
        let mut list = state.list(&key)?;
        if let Some(position) = list.iter().position(|stored| *stored == item) {
            list.remove(position);
            state.put_list(key, &list)?;
        }
        Ok(())
    }
    fn get_list<K: Serialize + ?Sized, V: DeserializeOwned>(
        &self,
        label: Label,
        key: &K,
    ) -> Result<Vec<V>, KvError> {
        let key = make_key(label, key)?;
        let list = self.read()?.list(&key)?;
        list.iter().map(|item| de(item)).collect()
    }
}

impl StorageProvider<CURRENT_VERSION> for KvStore {
    type Error = KvError;

    fn queue_proposal<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
        QueuedProposal: traits::QueuedProposal<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        proposal_ref: &ProposalRef,
        proposal: &QueuedProposal,
    ) -> Result<(), Self::Error> {
        self.put(QUEUED_PROPOSAL, &(group_id, proposal_ref), proposal)?;
        self.append(PROPOSAL_QUEUE_REFS, group_id, proposal_ref)
    }

    fn write_tree<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        TreeSync: traits::TreeSync<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        tree: &TreeSync,
    ) -> Result<(), Self::Error> {
        self.put(TREE, group_id, tree)
    }

    fn write_interim_transcript_hash<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        InterimTranscriptHash: traits::InterimTranscriptHash<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        interim_transcript_hash: &InterimTranscriptHash,
    ) -> Result<(), Self::Error> {
        self.put(INTERIM_TRANSCRIPT_HASH, group_id, interim_transcript_hash)
    }

    fn write_context<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        GroupContext: traits::GroupContext<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        group_context: &GroupContext,
    ) -> Result<(), Self::Error> {
        self.put(GROUP_CONTEXT, group_id, group_context)
    }

    fn write_confirmation_tag<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ConfirmationTag: traits::ConfirmationTag<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        confirmation_tag: &ConfirmationTag,
    ) -> Result<(), Self::Error> {
        self.put(CONFIRMATION_TAG, group_id, confirmation_tag)
    }

    fn write_signature_key_pair<
        SignaturePublicKey: traits::SignaturePublicKey<CURRENT_VERSION>,
        SignatureKeyPair: traits::SignatureKeyPair<CURRENT_VERSION>,
    >(
        &self,
        public_key: &SignaturePublicKey,
        signature_key_pair: &SignatureKeyPair,
    ) -> Result<(), Self::Error> {
        self.put(SIGNATURE_KEY_PAIR, public_key, signature_key_pair)
    }

    fn queued_proposal_refs<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<ProposalRef>, Self::Error> {
        self.get_list(PROPOSAL_QUEUE_REFS, group_id)
    }

    fn queued_proposals<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
        QueuedProposal: traits::QueuedProposal<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<(ProposalRef, QueuedProposal)>, Self::Error> {
        let refs: Vec<ProposalRef> = self.get_list(PROPOSAL_QUEUE_REFS, group_id)?;
        refs.into_iter()
            .map(|proposal_ref| {
                let proposal = self
                    .get(QUEUED_PROPOSAL, &(group_id, &proposal_ref))?
                    .ok_or(KvError::Encoding)?;
                Ok((proposal_ref, proposal))
            })
            .collect()
    }

    fn tree<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        TreeSync: traits::TreeSync<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<TreeSync>, Self::Error> {
        self.get(TREE, group_id)
    }

    fn group_context<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        GroupContext: traits::GroupContext<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupContext>, Self::Error> {
        self.get(GROUP_CONTEXT, group_id)
    }

    fn interim_transcript_hash<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        InterimTranscriptHash: traits::InterimTranscriptHash<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<InterimTranscriptHash>, Self::Error> {
        self.get(INTERIM_TRANSCRIPT_HASH, group_id)
    }

    fn confirmation_tag<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ConfirmationTag: traits::ConfirmationTag<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<ConfirmationTag>, Self::Error> {
        self.get(CONFIRMATION_TAG, group_id)
    }

    fn signature_key_pair<
        SignaturePublicKey: traits::SignaturePublicKey<CURRENT_VERSION>,
        SignatureKeyPair: traits::SignatureKeyPair<CURRENT_VERSION>,
    >(
        &self,
        public_key: &SignaturePublicKey,
    ) -> Result<Option<SignatureKeyPair>, Self::Error> {
        self.get(SIGNATURE_KEY_PAIR, public_key)
    }

    fn write_key_package<
        HashReference: traits::HashReference<CURRENT_VERSION>,
        KeyPackage: traits::KeyPackage<CURRENT_VERSION>,
    >(
        &self,
        hash_ref: &HashReference,
        key_package: &KeyPackage,
    ) -> Result<(), Self::Error> {
        self.put(KEY_PACKAGE, hash_ref, key_package)
    }

    fn write_psk<
        PskId: traits::PskId<CURRENT_VERSION>,
        PskBundle: traits::PskBundle<CURRENT_VERSION>,
    >(
        &self,
        psk_id: &PskId,
        psk: &PskBundle,
    ) -> Result<(), Self::Error> {
        self.put(PSK, psk_id, psk)
    }

    fn write_encryption_key_pair<
        EncryptionKey: traits::EncryptionKey<CURRENT_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<CURRENT_VERSION>,
    >(
        &self,
        public_key: &EncryptionKey,
        key_pair: &HpkeKeyPair,
    ) -> Result<(), Self::Error> {
        self.put(ENCRYPTION_KEY_PAIR, public_key, key_pair)
    }

    fn key_package<
        KeyPackageRef: traits::HashReference<CURRENT_VERSION>,
        KeyPackage: traits::KeyPackage<CURRENT_VERSION>,
    >(
        &self,
        hash_ref: &KeyPackageRef,
    ) -> Result<Option<KeyPackage>, Self::Error> {
        self.get(KEY_PACKAGE, hash_ref)
    }

    fn psk<PskBundle: traits::PskBundle<CURRENT_VERSION>, PskId: traits::PskId<CURRENT_VERSION>>(
        &self,
        psk_id: &PskId,
    ) -> Result<Option<PskBundle>, Self::Error> {
        self.get(PSK, psk_id)
    }

    fn encryption_key_pair<
        HpkeKeyPair: traits::HpkeKeyPair<CURRENT_VERSION>,
        EncryptionKey: traits::EncryptionKey<CURRENT_VERSION>,
    >(
        &self,
        public_key: &EncryptionKey,
    ) -> Result<Option<HpkeKeyPair>, Self::Error> {
        self.get(ENCRYPTION_KEY_PAIR, public_key)
    }

    fn delete_signature_key_pair<
        SignaturePublicKey: traits::SignaturePublicKey<CURRENT_VERSION>,
    >(
        &self,
        public_key: &SignaturePublicKey,
    ) -> Result<(), Self::Error> {
        self.delete(SIGNATURE_KEY_PAIR, public_key)
    }

    fn delete_encryption_key_pair<EncryptionKey: traits::EncryptionKey<CURRENT_VERSION>>(
        &self,
        public_key: &EncryptionKey,
    ) -> Result<(), Self::Error> {
        self.delete(ENCRYPTION_KEY_PAIR, public_key)
    }

    fn delete_key_package<KeyPackageRef: traits::HashReference<CURRENT_VERSION>>(
        &self,
        hash_ref: &KeyPackageRef,
    ) -> Result<(), Self::Error> {
        self.delete(KEY_PACKAGE, hash_ref)
    }

    fn delete_psk<PskKey: traits::PskId<CURRENT_VERSION>>(
        &self,
        psk_id: &PskKey,
    ) -> Result<(), Self::Error> {
        self.delete(PSK, psk_id)
    }

    fn group_state<
        GroupState: traits::GroupState<CURRENT_VERSION>,
        GroupId: traits::GroupId<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupState>, Self::Error> {
        self.get(GROUP_STATE, group_id)
    }

    fn write_group_state<
        GroupState: traits::GroupState<CURRENT_VERSION>,
        GroupId: traits::GroupId<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        group_state: &GroupState,
    ) -> Result<(), Self::Error> {
        self.put(GROUP_STATE, group_id, group_state)
    }

    fn delete_group_state<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete(GROUP_STATE, group_id)
    }

    fn message_secrets<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        MessageSecrets: traits::MessageSecrets<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<MessageSecrets>, Self::Error> {
        self.get(MESSAGE_SECRETS, group_id)
    }

    fn write_message_secrets<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        MessageSecrets: traits::MessageSecrets<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        message_secrets: &MessageSecrets,
    ) -> Result<(), Self::Error> {
        self.put(MESSAGE_SECRETS, group_id, message_secrets)
    }

    fn delete_message_secrets<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete(MESSAGE_SECRETS, group_id)
    }

    fn resumption_psk_store<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ResumptionPskStore: traits::ResumptionPskStore<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<ResumptionPskStore>, Self::Error> {
        self.get(RESUMPTION_PSK, group_id)
    }

    fn write_resumption_psk_store<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ResumptionPskStore: traits::ResumptionPskStore<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        resumption_psk_store: &ResumptionPskStore,
    ) -> Result<(), Self::Error> {
        self.put(RESUMPTION_PSK, group_id, resumption_psk_store)
    }

    fn delete_all_resumption_psk_secrets<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete(RESUMPTION_PSK, group_id)
    }

    fn own_leaf_index<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        LeafNodeIndex: traits::LeafNodeIndex<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<LeafNodeIndex>, Self::Error> {
        self.get(OWN_LEAF_NODE_INDEX, group_id)
    }

    fn write_own_leaf_index<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        LeafNodeIndex: traits::LeafNodeIndex<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        own_leaf_index: &LeafNodeIndex,
    ) -> Result<(), Self::Error> {
        self.put(OWN_LEAF_NODE_INDEX, group_id, own_leaf_index)
    }

    fn delete_own_leaf_index<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete(OWN_LEAF_NODE_INDEX, group_id)
    }

    fn group_epoch_secrets<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        GroupEpochSecrets: traits::GroupEpochSecrets<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupEpochSecrets>, Self::Error> {
        self.get(EPOCH_SECRETS, group_id)
    }

    fn write_group_epoch_secrets<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        GroupEpochSecrets: traits::GroupEpochSecrets<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        group_epoch_secrets: &GroupEpochSecrets,
    ) -> Result<(), Self::Error> {
        self.put(EPOCH_SECRETS, group_id, group_epoch_secrets)
    }

    fn delete_group_epoch_secrets<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete(EPOCH_SECRETS, group_id)
    }

    fn write_encryption_epoch_key_pairs<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        EpochKey: traits::EpochKey<CURRENT_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
        key_pairs: &[HpkeKeyPair],
    ) -> Result<(), Self::Error> {
        self.put(EPOCH_KEY_PAIRS, &(group_id, epoch, leaf_index), key_pairs)
    }

    fn encryption_epoch_key_pairs<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        EpochKey: traits::EpochKey<CURRENT_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
    ) -> Result<Vec<HpkeKeyPair>, Self::Error> {
        Ok(self
            .get(EPOCH_KEY_PAIRS, &(group_id, epoch, leaf_index))?
            .unwrap_or_default())
    }

    fn delete_encryption_epoch_key_pairs<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        EpochKey: traits::EpochKey<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
    ) -> Result<(), Self::Error> {
        self.delete(EPOCH_KEY_PAIRS, &(group_id, epoch, leaf_index))
    }

    fn clear_proposal_queue<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        let refs_key = make_key(PROPOSAL_QUEUE_REFS, group_id)?;
        let mut state = self.write()?;
        for item in state.list(&refs_key)? {
            let proposal_ref: ProposalRef = de(&item)?;
            state.remove(&make_key(QUEUED_PROPOSAL, &(group_id, &proposal_ref))?);
        }
        state.remove(&refs_key);
        Ok(())
    }

    fn mls_group_join_config<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        MlsGroupJoinConfig: traits::MlsGroupJoinConfig<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<MlsGroupJoinConfig>, Self::Error> {
        self.get(JOIN_CONFIG, group_id)
    }

    fn write_mls_join_config<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        MlsGroupJoinConfig: traits::MlsGroupJoinConfig<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        config: &MlsGroupJoinConfig,
    ) -> Result<(), Self::Error> {
        self.put(JOIN_CONFIG, group_id, config)
    }

    fn own_leaf_nodes<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        LeafNode: traits::LeafNode<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<LeafNode>, Self::Error> {
        self.get_list(OWN_LEAF_NODES, group_id)
    }

    fn append_own_leaf_node<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        LeafNode: traits::LeafNode<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        leaf_node: &LeafNode,
    ) -> Result<(), Self::Error> {
        self.append(OWN_LEAF_NODES, group_id, leaf_node)
    }

    fn delete_own_leaf_nodes<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete(OWN_LEAF_NODES, group_id)
    }

    fn delete_group_config<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete(JOIN_CONFIG, group_id)
    }

    fn delete_tree<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete(TREE, group_id)
    }

    fn delete_confirmation_tag<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete(CONFIRMATION_TAG, group_id)
    }

    fn delete_context<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete(GROUP_CONTEXT, group_id)
    }

    fn delete_interim_transcript_hash<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.delete(INTERIM_TRANSCRIPT_HASH, group_id)
    }

    fn remove_proposal<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        proposal_ref: &ProposalRef,
    ) -> Result<(), Self::Error> {
        self.remove_item(PROPOSAL_QUEUE_REFS, group_id, proposal_ref)?;
        self.delete(QUEUED_PROPOSAL, &(group_id, proposal_ref))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mls::{MAX_FORWARD, MAX_PAST_EPOCHS, MlsProvider, OUT_OF_ORDER};
    use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
    use openmls::prelude::*;
    use openmls_basic_credential::SignatureKeyPair;
    use openmls_rust_crypto::OpenMlsRustCrypto;
    use rusqlite::Connection;
    use std::collections::BTreeMap;

    const CS: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;
    const SCOPE: [u8; 32] = [7; 32];

    fn config() -> MlsGroupCreateConfig {
        MlsGroupCreateConfig::builder()
            .ciphersuite(CS)
            .max_past_epochs(MAX_PAST_EPOCHS)
            .sender_ratchet_configuration(SenderRatchetConfiguration::new(
                OUT_OF_ORDER,
                MAX_FORWARD,
            ))
            .use_ratchet_tree_extension(true)
            .build()
    }
    fn database() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        c.execute_batch(include_str!("../storage/schema.sql"))
            .unwrap();
        c
    }
    fn wire(m: &MlsMessageOut) -> Vec<u8> {
        m.tls_serialize_detached().unwrap()
    }
    fn protocol(bytes: &[u8]) -> ProtocolMessage {
        MlsMessageIn::tls_deserialize_exact(bytes)
            .unwrap()
            .try_into_protocol_message()
            .unwrap()
    }

    /// One MLS client whose OpenMLS state lives in `MlsProvider`/`KvStore` over SQLite.
    struct Party {
        conn: Connection,
        provider: MlsProvider,
        signer: SignatureKeyPair,
        credential: CredentialWithKey,
    }
    impl Party {
        fn new(name: &str) -> Self {
            let conn = database();
            let provider = MlsProvider::new(KvStore::load(&conn, SCOPE).unwrap());
            let signer = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
            signer.store(provider.storage()).unwrap();
            let credential = CredentialWithKey {
                credential: BasicCredential::new(name.as_bytes().to_vec()).into(),
                signature_key: signer.public().into(),
            };
            Self {
                conn,
                provider,
                signer,
                credential,
            }
        }
        fn key_package(&self) -> KeyPackage {
            KeyPackage::builder()
                .build(CS, &self.provider, &self.signer, self.credential.clone())
                .unwrap()
                .key_package()
                .clone()
        }
        /// Commit the in-memory changes in one SQL transaction; returns bytes written.
        fn save(&mut self) -> usize {
            let tx = self.conn.transaction().unwrap();
            let n = self.provider.storage().flush(&tx, SCOPE).unwrap();
            tx.commit().unwrap();
            n
        }
        fn reload(&mut self) {
            self.provider = MlsProvider::new(KvStore::load(&self.conn, SCOPE).unwrap());
        }
        fn load_group(&self, id: &GroupId) -> MlsGroup {
            MlsGroup::load(self.provider.storage(), id)
                .unwrap()
                .unwrap()
        }
        fn rows(&self) -> BTreeMap<Vec<u8>, (Vec<u8>, Vec<u8>)> {
            let mut s = self
                .conn
                .prepare("SELECT key,scope,value FROM mls_kv")
                .unwrap();
            s.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        }
    }

    /// Alice creates the group and adds Bob (both over KvStore); returns the group id.
    fn established() -> (Party, Party, GroupId) {
        let mut alice = Party::new("alice");
        let mut bob = Party::new("bob");
        let mut group = MlsGroup::new(
            &alice.provider,
            &alice.signer,
            &config(),
            alice.credential.clone(),
        )
        .unwrap();
        let (_, welcome, _) = group
            .add_members(&alice.provider, &alice.signer, &[bob.key_package()])
            .unwrap();
        group.merge_pending_commit(&alice.provider).unwrap();
        let MlsMessageBodyIn::Welcome(welcome) =
            MlsMessageIn::tls_deserialize_exact(wire(&welcome))
                .unwrap()
                .extract()
        else {
            panic!("expected a welcome")
        };
        let joined =
            StagedWelcome::new_from_welcome(&bob.provider, config().join_config(), welcome, None)
                .unwrap()
                .into_group(&bob.provider)
                .unwrap();
        assert_eq!(joined.group_id(), group.group_id());
        let id = group.group_id().clone();
        alice.save();
        bob.save();
        (alice, bob, id)
    }

    fn open(party: &Party, group: &mut MlsGroup, bytes: &[u8]) -> Vec<u8> {
        match group
            .process_message(&party.provider, protocol(bytes))
            .unwrap()
            .into_content()
        {
            ProcessedMessageContent::ApplicationMessage(m) => m.into_bytes(),
            _ => panic!("expected an application message"),
        }
    }
    fn apply_commit(party: &Party, group: &mut MlsGroup, bytes: &[u8]) {
        match group
            .process_message(&party.provider, protocol(bytes))
            .unwrap()
            .into_content()
        {
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                group.merge_staged_commit(&party.provider, *staged).unwrap()
            }
            _ => panic!("expected a commit"),
        }
    }

    /// The same group, built in OpenMLS's own JSON memory storage, for a size baseline.
    fn json_value_total() -> usize {
        let alice = OpenMlsRustCrypto::default();
        let bob = OpenMlsRustCrypto::default();
        let signer = |p: &OpenMlsRustCrypto, n: &str| {
            let s = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
            s.store(p.storage()).unwrap();
            let c = CredentialWithKey {
                credential: BasicCredential::new(n.as_bytes().to_vec()).into(),
                signature_key: s.public().into(),
            };
            (s, c)
        };
        let (a_sig, a_cred) = signer(&alice, "alice");
        let (b_sig, b_cred) = signer(&bob, "bob");
        let kp = KeyPackage::builder()
            .build(CS, &bob, &b_sig, b_cred)
            .unwrap()
            .key_package()
            .clone();
        let mut group = MlsGroup::new(&alice, &a_sig, &config(), a_cred).unwrap();
        group.add_members(&alice, &a_sig, &[kp]).unwrap();
        group.merge_pending_commit(&alice).unwrap();
        alice
            .storage()
            .values
            .read()
            .unwrap()
            .values()
            .map(Vec::len)
            .sum()
    }

    #[test]
    fn kv_values_are_binary_not_json() {
        let (alice, ..) = established();
        let rows = alice.rows();
        assert!(rows.len() > 5, "{} rows", rows.len());
        let mut binary_total = 0;
        for (value_scope, value) in rows.values() {
            assert!(!value.is_empty());
            assert!(!matches!(value[0], b'[' | b'{'), "JSON-looking value");
            assert!(
                serde_json::from_slice::<serde_json::Value>(value).is_err(),
                "value parses as JSON"
            );
            assert!(value_scope.as_slice() == SCOPE || value_scope.iter().all(|b| *b == 0));
            binary_total += value.len();
        }
        let json_total = json_value_total();
        assert!(
            binary_total * 3 < json_total,
            "binary {binary_total} B vs json {json_total} B"
        );
        // Signature and key-package keys belong to no group: global scope. Group state is scoped.
        let global = rows
            .values()
            .filter(|(s, _)| s.iter().all(|b| *b == 0))
            .count();
        assert!(global >= 1 && global < rows.len());
    }

    #[test]
    fn kv_flush_writes_only_changed_keys() {
        let (mut alice, _bob, id) = established();
        alice.reload();
        assert_eq!(alice.save(), 0, "an untouched load must write nothing");
        let before = alice.rows();

        let mut group = alice.load_group(&id);
        assert_eq!(alice.save(), 0, "loading a group must write nothing");
        group
            .create_message(&alice.provider, &alice.signer, b"one message")
            .unwrap();
        let written = alice.save();
        let after = alice.rows();

        let mut expected = 0;
        let mut changed = 0;
        for (key, (_, value)) in &after {
            if before.get(key).map(|(_, v)| v) != Some(value) {
                expected += key.len() + value.len();
                changed += 1;
            }
        }
        for key in before.keys().filter(|k| !after.contains_key(*k)) {
            expected += key.len();
            changed += 1;
        }
        assert!(changed >= 1, "sending must advance the sender ratchet");
        assert!(changed < before.len(), "only changed keys are written");
        assert_eq!(written, expected);
        assert_eq!(alice.save(), 0, "second flush has nothing left");
    }

    #[test]
    fn kv_rollback_leaves_previous_state() {
        let (mut alice, mut bob, id) = established();
        let mut a = alice.load_group(&id);
        let first = wire(
            &a.create_message(&alice.provider, &alice.signer, b"epoch one A")
                .unwrap(),
        );
        let second = wire(
            &a.create_message(&alice.provider, &alice.signer, b"epoch one B")
                .unwrap(),
        );
        let commit = a
            .self_update(
                &alice.provider,
                &alice.signer,
                LeafNodeParameters::default(),
            )
            .unwrap();
        a.merge_pending_commit(&alice.provider).unwrap();
        let commit = wire(commit.commit());
        alice.save();

        // Bob opens a message and advances an epoch, flushes, then the transaction rolls back.
        bob.reload();
        let mut b = bob.load_group(&id);
        assert_eq!(open(&bob, &mut b, &first), b"epoch one A");
        apply_commit(&bob, &mut b, &commit);
        assert_eq!(b.epoch().as_u64(), 2);
        let tx = bob.conn.transaction().unwrap();
        assert!(bob.provider.storage().flush(&tx, SCOPE).unwrap() > 0);
        tx.rollback().unwrap();

        // Reloading yields the committed (previous) state: still epoch 1, and the commit and the
        // epoch-1 messages are processed exactly as if nothing had happened.
        bob.reload();
        let mut b = bob.load_group(&id);
        assert_eq!(b.epoch().as_u64(), 1);
        assert_eq!(open(&bob, &mut b, &first), b"epoch one A");
        apply_commit(&bob, &mut b, &commit);
        assert_eq!(b.epoch().as_u64(), 2);
        assert_eq!(open(&bob, &mut b, &second), b"epoch one B");
        bob.save();
        bob.reload();
        assert_eq!(bob.load_group(&id).epoch().as_u64(), 2);
    }

    #[test]
    fn kv_load_reads_only_own_and_global_scope() {
        let (alice, _bob, id) = established();
        let mine = MlsProvider::new(KvStore::load(&alice.conn, SCOPE).unwrap());
        assert!(MlsGroup::load(mine.storage(), &id).unwrap().is_some());
        let other = MlsProvider::new(KvStore::load(&alice.conn, [9; 32]).unwrap());
        assert!(MlsGroup::load(other.storage(), &id).unwrap().is_none());
        // Global rows (the signature key) are visible from every scope.
        assert!(
            SignatureKeyPair::read(
                other.storage(),
                alice.signer.public(),
                CS.signature_algorithm()
            )
            .is_some()
        );
    }

    #[test]
    fn kv_snapshot_restore_reverts_scoped_rows_only() {
        let (alice, _bob, id) = established();
        let store = alice.provider.storage();
        let before = store.snapshot().unwrap();
        assert_eq!(before[0], FORMAT);
        // The snapshot holds no global (signature key) rows.
        assert!(!before.windows(16).any(|w| w == b"SignatureKeyPair"));

        let mut group = alice.load_group(&id);
        assert_eq!(group.epoch().as_u64(), 1);
        group
            .self_update(
                &alice.provider,
                &alice.signer,
                LeafNodeParameters::default(),
            )
            .unwrap();
        group.merge_pending_commit(&alice.provider).unwrap();
        assert_eq!(group.epoch().as_u64(), 2);
        assert_ne!(store.snapshot().unwrap(), before);

        store.restore(&before).unwrap();
        assert_eq!(store.snapshot().unwrap(), before);
        assert_eq!(alice.load_group(&id).epoch().as_u64(), 1);
        assert!(
            SignatureKeyPair::read(store, alice.signer.public(), CS.signature_algorithm())
                .is_some()
        );
    }

    #[test]
    fn kv_restore_rejects_malformed_snapshots() {
        let (alice, _bob, _id) = established();
        let store = alice.provider.storage();
        let good = store.snapshot().unwrap();
        for bad in [
            Vec::new(),
            vec![FORMAT + 1],
            // Truncated length, key longer than the data, zero-length key, wrong value format.
            vec![FORMAT, 0, 0, 0],
            vec![FORMAT, 0, 0, 0, 9, 7],
            vec![FORMAT, 0, 0, 0, 0],
            vec![FORMAT, 0, 0, 0, 1, 7, 0, 0, 0, 1, FORMAT + 1],
        ] {
            assert!(store.restore(&bad).is_err(), "{bad:?}");
            assert_eq!(
                store.snapshot().unwrap(),
                good,
                "a bad snapshot changed state"
            );
        }
        store.restore(&good).unwrap();
    }
}
