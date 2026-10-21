//! Locked, durable transition from the protected M3 envelope to SQL authority.
use super::{
    engine,
    keys::{self, DbSecrets},
};
use crate::{
    CoreError, ErrorCode,
    account::{
        StoreId,
        storage::{
            KeyProtector, StoredPlaintext, atomic, load_for_migration, ownership::Ownership,
            schema::AccountState,
        },
    },
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;
type R<T> = Result<T, CoreError>;
const DB: &str = "db.sqlite3";
const PREP: &str = "migration.prep";
pub(crate) struct OpenedStore {
    pub connection: Option<Connection>,
    pub initial: Option<StoredPlaintext>,
    pub secrets: DbSecrets,
    pub ownership: Ownership,
}
fn corrupt() -> CoreError {
    CoreError::new(ErrorCode::StorageCorrupt, "migration_state_invalid")
}
fn exists(root: &Path, name: &str) -> R<bool> {
    root.join(name).try_exists().map_err(|_| corrupt())
}
fn remove(root: &Path, name: &str) -> R<()> {
    match fs::remove_file(root.join(name)) {
        Ok(()) => atomic::sync_directory(root),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(CoreError::new(
            ErrorCode::StorageLocked,
            "legacy_cleanup_incomplete",
        )),
    }
}
// Kept after cleanup: immutable provenance, including the ONLY legacy resource
// identifier cleanup is permitted to delete. No private material is stored here.
struct Preparation {
    source: Option<[u8; 32]>,
    legacy: Option<StoreId>,
    wrapped_hash: Option<[u8; 32]>,
    account_hash: Option<[u8; 32]>,
    started: Option<[u8; 16]>,
    importing: bool,
    cleaned: bool,
}
impl Preparation {
    fn encode(&self) -> Vec<u8> {
        let mut e = minicbor::Encoder::new(Vec::new());
        e.array(8).unwrap().u8(1).unwrap();
        if let Some(h) = self.source {
            e.bytes(&h).unwrap();
        } else {
            e.null().unwrap();
        }
        if let Some(id) = self.legacy {
            e.bytes(&id.0).unwrap();
        } else {
            e.null().unwrap();
        }
        for hash in [self.wrapped_hash, self.account_hash] {
            if let Some(h) = hash {
                e.bytes(&h).unwrap();
            } else {
                e.null().unwrap();
            }
        }
        if let Some(id) = self.started {
            e.bytes(&id).unwrap();
        } else {
            e.null().unwrap();
        }
        e.bool(self.importing).unwrap().bool(self.cleaned).unwrap();
        e.into_writer()
    }
    fn decode(bytes: &[u8]) -> R<Self> {
        let mut d = minicbor::Decoder::new(bytes);
        if d.array().map_err(|_| corrupt())? != Some(8) || d.u8().map_err(|_| corrupt())? != 1 {
            return Err(corrupt());
        }
        let source = if d.datatype().map_err(|_| corrupt())? == minicbor::data::Type::Null {
            d.null().map_err(|_| corrupt())?;
            None
        } else {
            Some(
                d.bytes()
                    .map_err(|_| corrupt())?
                    .try_into()
                    .map_err(|_| corrupt())?,
            )
        };
        let legacy = if d.datatype().map_err(|_| corrupt())? == minicbor::data::Type::Null {
            d.null().map_err(|_| corrupt())?;
            None
        } else {
            Some(StoreId(
                d.bytes()
                    .map_err(|_| corrupt())?
                    .try_into()
                    .map_err(|_| corrupt())?,
            ))
        };
        fn optional<const N: usize>(d: &mut minicbor::Decoder<'_>) -> R<Option<[u8; N]>> {
            if d.datatype().map_err(|_| corrupt())? == minicbor::data::Type::Null {
                d.null().map_err(|_| corrupt())?;
                Ok(None)
            } else {
                Ok(Some(
                    d.bytes()
                        .map_err(|_| corrupt())?
                        .try_into()
                        .map_err(|_| corrupt())?,
                ))
            }
        }
        let wrapped_hash = optional(&mut d)?;
        let account_hash = optional(&mut d)?;
        let started = optional(&mut d)?;
        let importing = d.bool().map_err(|_| corrupt())?;
        let cleaned = d.bool().map_err(|_| corrupt())?;
        let p = Self {
            source,
            legacy,
            wrapped_hash,
            account_hash,
            started,
            importing,
            cleaned,
        };
        if source.is_some() != legacy.is_some()
            || source.is_some() != wrapped_hash.is_some()
            || source.is_some() != account_hash.is_some()
            || ((cleaned || importing) && started.is_none())
            || p.encode() != bytes
        {
            return Err(corrupt());
        }
        Ok(p)
    }
}
fn file_hash(root: &Path, name: &str) -> R<[u8; 32]> {
    Ok(Sha256::digest(atomic::read_with_limit(&root.join(name), 4 * 1024 * 1024)?).into())
}
fn fingerprint(root: &Path) -> R<[u8; 32]> {
    let mut hash = Sha256::new();
    for name in ["store.id", "key.wrap", "account.bin"] {
        let bytes = atomic::read_with_limit(&root.join(name), 4 * 1024 * 1024)?;
        hash.update((name.len() as u64).to_be_bytes());
        hash.update(name.as_bytes());
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    Ok(hash.finalize().into())
}
fn mode(id: &[u8; 16], source: Option<[u8; 32]>) -> Vec<u8> {
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(3).unwrap().u8(1).unwrap().bytes(id).unwrap();
    if let Some(h) = source {
        e.bytes(&h).unwrap();
    } else {
        e.null().unwrap();
    }
    e.into_writer()
}
fn open(root: &Path, secrets: &DbSecrets) -> R<Connection> {
    engine::open(
        &root.join(DB),
        &secrets.key,
        &secrets.salt.as_bytes()[..16]
            .try_into()
            .map_err(|_| corrupt())?,
    )
}
fn ready(conn: &Connection, id: &[u8; 16], p: &Preparation) -> R<Option<StoredPlaintext>> {
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|_| corrupt())?;
    let (hash, complete): (Option<Vec<u8>>, i64) = conn
        .query_row(
            "SELECT legacy_hash,migration_ready FROM storage_meta WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| corrupt())?;
    if version != 1 || complete != 1 || hash.as_deref() != p.source.as_ref().map(|h| h.as_slice()) {
        return Err(corrupt());
    }
    load(conn, id)
}
fn same(a: Option<&StoredPlaintext>, b: Option<&StoredPlaintext>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.installation == b.installation
                && bool::from(a.payload.as_slice().ct_eq(b.payload.as_slice()))
        }
        _ => false,
    }
}
pub(crate) fn open_or_migrate(root: &Path, protector: &dyn KeyProtector) -> R<OpenedStore> {
    let mut ownership = Ownership::open(root)?;
    let (connection, secrets, initial) = ownership.with_lock(|root| {
        let (connection, secrets) = open_locked(root, protector)?;
        let initial = load(&connection, &secrets.db_id)?;
        // iOS returns no live connection after releasing its operation lock.
        let connection = if cfg!(target_os = "ios") {
            drop(connection);
            None
        } else {
            Some(connection)
        };
        Ok((connection, secrets, initial))
    })?;
    Ok(OpenedStore {
        connection,
        secrets,
        ownership,
        initial,
    })
}
// Also used for resume/uncertain-result revalidation under the existing owner.
pub(crate) fn reopen(root: &Path, protector: &dyn KeyProtector, id: &[u8; 16]) -> R<Connection> {
    if !exists(root, PREP)? || !exists(root, DB)? || !exists(root, "storage.mode")? {
        return Err(corrupt());
    }
    let (conn, secrets) = open_locked(root, protector)?;
    if &secrets.db_id != id {
        return Err(corrupt());
    }
    Ok(conn)
}
fn open_locked(root: &Path, protector: &dyn KeyProtector) -> R<(Connection, DbSecrets)> {
    let has_mode = exists(root, "storage.mode")?;
    let mut p = if exists(root, PREP)? {
        Preparation::decode(&atomic::read_with_limit(&root.join(PREP), 256)?)?
    } else {
        // An unrecorded SQL artifact is uncertain, never permission to initialize.
        for name in [
            DB,
            "db.sqlite3-wal",
            "db.sqlite3-shm",
            "db.keys",
            "db.keys.pending",
            ".db.keys.tmp",
            ".db.keys.pending.tmp",
            "storage.mode",
            ".storage.mode.tmp",
        ] {
            if exists(root, name)? {
                return Err(corrupt());
            }
        }
        let legacy = load_for_migration(root, protector)?;
        if let Some(s) = &legacy {
            AccountState::decode(s)?;
        }
        let p = if legacy.is_some() {
            Preparation {
                source: Some(fingerprint(root)?),
                legacy: Some(StoreId(
                    atomic::read_with_limit(&root.join("store.id"), 16)?
                        .try_into()
                        .map_err(|_| corrupt())?,
                )),
                wrapped_hash: Some(file_hash(root, "key.wrap")?),
                account_hash: Some(file_hash(root, "account.bin")?),
                started: None,
                importing: false,
                cleaned: false,
            }
        } else {
            Preparation {
                source: None,
                legacy: None,
                wrapped_hash: None,
                account_hash: None,
                started: None,
                importing: false,
                cleaned: false,
            }
        };
        if exists(root, ".migration.prep.tmp")?
            && Preparation::decode(&atomic::read_with_limit(
                &root.join(".migration.prep.tmp"),
                256,
            )?)?
            .encode()
                != p.encode()
        {
            return Err(corrupt());
        }
        atomic::replace(root, PREP, &p.encode())?;
        p
    };
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    checkpoint(root, MigrationStage::Prepared);
    if p.cleaned && !has_mode {
        return Err(corrupt());
    }
    if has_mode {
        for (name, hash) in [
            ("key.wrap", p.wrapped_hash),
            ("account.bin", p.account_hash),
        ] {
            if exists(root, name)? && (p.cleaned || hash != Some(file_hash(root, name)?)) {
                return Err(corrupt());
            }
        }
    }
    let legacy = if !has_mode {
        if let Some(h) = p.source {
            if fingerprint(root)? != h {
                return Err(corrupt());
            }
            let state = load_for_migration(root, protector)?.ok_or_else(corrupt)?;
            AccountState::decode(&state)?;
            Some(state)
        } else {
            for name in [
                "account.bin",
                "key.wrap",
                ".account.bin.tmp",
                ".key.wrap.tmp",
                ".store.id.tmp",
            ] {
                if exists(root, name)? {
                    return Err(corrupt());
                }
            }
            None
        }
    } else {
        None
    };
    // Once SQL authority exists, missing material/DB must never call a create path.
    if p.started.is_none() {
        for name in [DB, "db.sqlite3-wal", "db.sqlite3-shm"] {
            if exists(root, name)? {
                return Err(corrupt());
            }
        }
    }
    if (has_mode || p.started.is_some()) && (!exists(root, "db.keys")? || !exists(root, DB)?) {
        return Err(corrupt());
    }
    // SQLite treats a zero-length file as a new database. After commit became
    // possible, reject it before engine journal setup can write a fresh header.
    if p.importing && fs::metadata(root.join(DB)).map_err(|_| corrupt())?.len() == 0 {
        return Err(corrupt());
    }
    let secrets = keys::load_or_prepare(root, protector, p.legacy)?;
    if p.started.is_some_and(|id| id != secrets.db_id) {
        return Err(corrupt());
    }
    let expected_mode = mode(&secrets.db_id, p.source);
    if has_mode && atomic::read_with_limit(&root.join("storage.mode"), 128)? != expected_mode {
        return Err(corrupt());
    }
    if exists(root, ".storage.mode.tmp")?
        && atomic::read_with_limit(&root.join(".storage.mode.tmp"), 128)? != expected_mode
    {
        return Err(corrupt());
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    checkpoint(root, MigrationStage::KeysPrepared);
    if p.started.is_none() {
        p.started = Some(secrets.db_id);
        atomic::replace(root, PREP, &p.encode())?;
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    checkpoint(root, MigrationStage::DatabaseStarted);
    let mut conn = open(root, &secrets)?;
    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| corrupt())?;
    if tables == 0 {
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(|_| corrupt())?;
        if has_mode || p.importing || version != 0 {
            return Err(corrupt());
        }
        let tx = conn.transaction().map_err(|_| corrupt())?;
        tx.execute_batch(include_str!("schema.sql"))
            .map_err(|_| corrupt())?;
        tx.pragma_update(None, "user_version", 1)
            .map_err(|_| corrupt())?;
        tx.execute(
            "INSERT INTO storage_meta VALUES(1,1,?1,?2,?3,1,0)",
            params![
                secrets.db_id.as_slice(),
                binding(legacy.as_ref())?,
                p.source.as_ref().map(|h| h.as_slice())
            ],
        )
        .map_err(|_| corrupt())?;
        if let Some(s) = &legacy {
            tx.execute(
                "INSERT INTO account_state VALUES(1,?1,?2)",
                params![s.installation.as_slice(), s.payload.as_slice()],
            )
            .map_err(|_| corrupt())?;
        }
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        checkpoint(root, MigrationStage::BeforeImport);
        // Persist the uncertainty boundary before commit can become durable.
        // An empty DB with this flag is never safe to reimport automatically.
        p.importing = true;
        atomic::replace(root, PREP, &p.encode())?;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        checkpoint(root, MigrationStage::CommitStarted);
        tx.commit().map_err(|_| {
            CoreError::new(ErrorCode::StorageUncertain, "migration_commit_uncertain")
        })?;
    }
    // A completed SQL DB is authoritative even without mode. Never overwrite it.
    if !p.importing {
        return Err(corrupt());
    }
    ready(&conn, &secrets.db_id, &p)?;
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    checkpoint(root, MigrationStage::AfterImport);
    drop(conn);
    let conn = open(root, &secrets)?;
    let loaded = ready(&conn, &secrets.db_id, &p)?;
    if !has_mode && !same(legacy.as_ref(), loaded.as_ref()) {
        return Err(corrupt());
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    checkpoint(root, MigrationStage::Reopened);
    let guard = p.legacy.map(|s| s.0).unwrap_or(secrets.db_id);
    if exists(root, "store.id")? {
        if atomic::read_with_limit(&root.join("store.id"), 16)? != guard {
            return Err(corrupt());
        }
    } else if has_mode || p.legacy.is_some() {
        return Err(corrupt());
    } else {
        atomic::replace(root, "store.id", &guard)?;
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    checkpoint(root, MigrationStage::BeforeMode);
    if !has_mode {
        atomic::replace(root, "storage.mode", &expected_mode)?;
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    checkpoint(root, MigrationStage::AfterMode);
    if let Some(id) = p.legacy.filter(|_| !p.cleaned) {
        // The retained guard must match the recorded original; do not derive an
        // external deletion target from a new/unverified store.id read.
        for name in [
            "account.bin",
            ".account.bin.tmp",
            ".key.wrap.tmp",
            ".store.id.tmp",
        ] {
            remove(root, name)?;
        }
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        checkpoint(root, MigrationStage::BeforeKeyDelete);
        protector.delete(id)?;
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        checkpoint(root, MigrationStage::AfterKeyDelete);
        remove(root, "key.wrap")?;
    }
    if !p.cleaned {
        p.cleaned = true;
        atomic::replace(root, PREP, &p.encode())?;
    }
    remove(root, ".storage.mode.tmp")?;
    remove(root, ".migration.prep.tmp")?;
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    checkpoint(root, MigrationStage::CleanupComplete);
    Ok((conn, secrets))
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
#[derive(Debug, Clone, Copy)]
pub enum MigrationStage {
    Prepared,
    KeyProtected,
    KeysPrepared,
    DatabaseStarted,
    BeforeImport,
    CommitStarted,
    AfterImport,
    Reopened,
    BeforeMode,
    AfterMode,
    BeforeKeyDelete,
    AfterKeyDelete,
    CleanupComplete,
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
pub(crate) fn checkpoint(root: &Path, stage: MigrationStage) {
    let name = format!("{stage:?}");
    if std::env::var("CHAT_M4_MIGRATION_STAGE").as_deref() != Ok(&name) {
        return;
    }
    // Only debug fixture child processes opt into this. The parent really kills
    // the recorded child; normal return/Drop/rollback are intentionally bypassed.
    let marker = root.join("migration.checkpoint");
    use std::io::Write;
    let temporary = root.join(".migration.checkpoint.tmp");
    let mut file = fs::File::create(&temporary).unwrap();
    file.write_all(name.as_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    fs::rename(temporary, marker).unwrap();
    loop {
        std::thread::park();
    }
}

pub(crate) fn binding(state: Option<&StoredPlaintext>) -> R<Vec<u8>> {
    let mut e = minicbor::Encoder::new(Vec::new());
    match state {
        None => {
            e.array(2).unwrap().u8(1).unwrap().u8(0).unwrap();
        }
        Some(stored) => match AccountState::decode(stored)? {
            AccountState::Active(a) => {
                e.array(4)
                    .unwrap()
                    .u8(1)
                    .unwrap()
                    .u8(2)
                    .unwrap()
                    .bytes(&a.keys.peer_id().0)
                    .unwrap()
                    .bytes(&stored.installation)
                    .unwrap();
            }
            _ => {
                e.array(3)
                    .unwrap()
                    .u8(1)
                    .unwrap()
                    .u8(1)
                    .unwrap()
                    .bytes(&stored.installation)
                    .unwrap();
            }
        },
    }
    Ok(e.into_writer())
}
pub(crate) fn load(conn: &Connection, id: &[u8; 16]) -> R<Option<StoredPlaintext>> {
    let (version, db, stored_binding): (i64, Vec<u8>, Vec<u8>) = conn
        .query_row(
            "SELECT schema_version,db_id,binding FROM storage_meta WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|_| corrupt())?;
    if version != 1 || db != id {
        return Err(corrupt());
    }
    let state = conn
        .query_row(
            "SELECT installation,payload FROM account_state WHERE singleton=1",
            [],
            |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    Zeroizing::new(r.get::<_, Vec<u8>>(1)?),
                ))
            },
        )
        .optional()
        .map_err(|_| corrupt())?
        .map(|(installation, payload)| {
            Ok(StoredPlaintext {
                installation: installation.try_into().map_err(|_| corrupt())?,
                payload,
            })
        })
        .transpose()?;
    if binding(state.as_ref())? != stored_binding {
        return Err(corrupt());
    }
    Ok(state)
}
