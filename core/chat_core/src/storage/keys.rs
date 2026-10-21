//! Independently random DB material, protected in two separately owned OS slots.
use crate::{
    CoreError, ErrorCode,
    account::{
        Secret32, StoreId, random_secret,
        storage::{KeyProtector, atomic},
    },
};
use std::{fs, path::Path};

type R<T> = Result<T, CoreError>;
const META_LIMIT: usize = 65_536;
const WRAP_LIMIT: usize = 16_384;
pub(crate) struct DbSecrets {
    pub db_id: [u8; 16],
    pub key: Secret32,
    pub salt: Secret32,
}
struct Metadata {
    db: [u8; 16],
    key: StoreId,
    salt: StoreId,
    wrapped_key: Vec<u8>,
    wrapped_salt: Vec<u8>,
}
fn invalid() -> CoreError {
    CoreError::new(ErrorCode::StorageCorrupt, "db_material_invalid")
}
fn exists(root: &Path, name: &str) -> R<bool> {
    root.join(name).try_exists().map_err(|_| invalid())
}
fn id() -> R<[u8; 16]> {
    let mut id = [0; 16];
    getrandom::fill(&mut id)
        .map_err(|_| CoreError::new(ErrorCode::NotAvailable, "secure_random_unavailable"))?;
    Ok(id)
}
impl Metadata {
    fn encode(&self) -> Vec<u8> {
        let mut e = minicbor::Encoder::new(Vec::new());
        e.array(6)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&self.db)
            .unwrap()
            .bytes(&self.key.0)
            .unwrap()
            .bytes(&self.salt.0)
            .unwrap()
            .bytes(&self.wrapped_key)
            .unwrap()
            .bytes(&self.wrapped_salt)
            .unwrap();
        e.into_writer()
    }
    fn decode(bytes: &[u8]) -> R<Self> {
        let mut d = minicbor::Decoder::new(bytes);
        if d.array().map_err(|_| invalid())? != Some(6) || d.u8().map_err(|_| invalid())? != 1 {
            return Err(invalid());
        }
        let db = d
            .bytes()
            .map_err(|_| invalid())?
            .try_into()
            .map_err(|_| invalid())?;
        let key = StoreId(
            d.bytes()
                .map_err(|_| invalid())?
                .try_into()
                .map_err(|_| invalid())?,
        );
        let salt = StoreId(
            d.bytes()
                .map_err(|_| invalid())?
                .try_into()
                .map_err(|_| invalid())?,
        );
        let wrapped_key = d.bytes().map_err(|_| invalid())?;
        let wrapped_salt = d.bytes().map_err(|_| invalid())?;
        if wrapped_key.len() > WRAP_LIMIT
            || wrapped_salt.len() > WRAP_LIMIT
            || key == salt
            || db == key.0
            || db == salt.0
        {
            return Err(invalid());
        }
        let m = Self {
            db,
            key,
            salt,
            wrapped_key: wrapped_key.to_vec(),
            wrapped_salt: wrapped_salt.to_vec(),
        };
        if bytes.len() > META_LIMIT || m.encode() != bytes {
            return Err(invalid());
        }
        Ok(m)
    }
}
pub(crate) fn load_or_prepare(
    root: &Path,
    protector: &dyn KeyProtector,
    legacy: Option<StoreId>,
) -> R<DbSecrets> {
    // Caller has validated durable migration preparation and unchanged source.
    let pending = if exists(root, "db.keys.pending")? {
        let bytes = atomic::read_with_limit(&root.join("db.keys.pending"), 128)?;
        let mut d = minicbor::Decoder::new(&bytes);
        if d.array().map_err(|_| invalid())? != Some(4) || d.u8().map_err(|_| invalid())? != 1 {
            return Err(invalid());
        }
        let ids: [[u8; 16]; 3] = [
            d.bytes()
                .map_err(|_| invalid())?
                .try_into()
                .map_err(|_| invalid())?,
            d.bytes()
                .map_err(|_| invalid())?
                .try_into()
                .map_err(|_| invalid())?,
            d.bytes()
                .map_err(|_| invalid())?
                .try_into()
                .map_err(|_| invalid())?,
        ];
        let mut e = minicbor::Encoder::new(Vec::new());
        e.array(4).unwrap().u8(1).unwrap();
        for id in ids {
            e.bytes(&id).unwrap();
        }
        if e.into_writer() != bytes || ids[0] == ids[1] || ids[0] == ids[2] || ids[1] == ids[2] {
            return Err(invalid());
        }
        if legacy.is_some_and(|old| ids.contains(&old.0)) {
            return Err(invalid());
        }
        Some(ids)
    } else {
        None
    };
    if exists(root, "db.keys")? {
        let m = Metadata::decode(&atomic::read_with_limit(&root.join("db.keys"), META_LIMIT)?)?;
        if legacy.is_some_and(|old| [m.db, m.key.0, m.salt.0].contains(&old.0)) {
            return Err(invalid());
        }
        let key = protector.unprotect(m.key, &m.wrapped_key)?;
        let salt = protector.unprotect(m.salt, &m.wrapped_salt)?;
        if salt.as_bytes()[16..] != [0; 16] {
            return Err(invalid());
        }
        if let Some(ids) = pending {
            if ids != [m.db, m.key.0, m.salt.0] {
                return Err(invalid());
            }
            remove(root, "db.keys.pending")?;
        }
        for name in [".db.keys.pending.tmp", ".db.keys.tmp"] {
            remove(root, name)?;
        }
        return Ok(DbSecrets {
            db_id: m.db,
            key,
            salt,
        });
    }
    for name in [
        "db.sqlite3",
        "db.sqlite3-wal",
        "db.sqlite3-shm",
        "storage.mode",
    ] {
        if exists(root, name)? {
            return Err(invalid());
        }
    }
    if let Some(ids) = pending {
        // Only these recorded pre-DB resources may be reset after partial protection.
        protector.delete(StoreId(ids[1]))?;
        protector.delete(StoreId(ids[2]))?;
        remove(root, ".db.keys.tmp")?;
        remove(root, "db.keys.pending")?;
    } else if exists(root, ".db.keys.tmp")? {
        return Err(invalid());
    }
    remove(root, ".db.keys.pending.tmp")?;
    let mut db = id()?;
    while legacy.is_some_and(|old| old.0 == db) {
        db = id()?;
    }
    let mut key = StoreId(id()?);
    while key.0 == db || legacy == Some(key) {
        key = StoreId(id()?);
    }
    let mut salt = StoreId(id()?);
    while salt.0 == db || salt == key || legacy == Some(salt) {
        salt = StoreId(id()?);
    }
    let mut pending = minicbor::Encoder::new(Vec::new());
    pending
        .array(4)
        .unwrap()
        .u8(1)
        .unwrap()
        .bytes(&db)
        .unwrap()
        .bytes(&key.0)
        .unwrap()
        .bytes(&salt.0)
        .unwrap();
    atomic::replace(root, "db.keys.pending", &pending.into_writer())?;
    let db_key = random_secret()?;
    let mut db_salt = Secret32::from([0; 32]);
    getrandom::fill(&mut db_salt.as_mut_bytes()[..16])
        .map_err(|_| CoreError::new(ErrorCode::NotAvailable, "secure_random_unavailable"))?;
    let wrapped_key = protector.protect(key, &db_key)?;
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    super::migration::checkpoint(root, super::migration::MigrationStage::KeyProtected);
    let wrapped_salt = protector.protect(salt, &db_salt)?;
    if wrapped_key.len() > WRAP_LIMIT || wrapped_salt.len() > WRAP_LIMIT {
        return Err(CoreError::new(
            ErrorCode::StorageFull,
            "db_material_too_large",
        ));
    }
    atomic::replace(
        root,
        "db.keys",
        &Metadata {
            db,
            key,
            salt,
            wrapped_key,
            wrapped_salt,
        }
        .encode(),
    )?;
    fs::remove_file(root.join("db.keys.pending")).map_err(|_| invalid())?;
    atomic::sync_directory(root)?;
    Ok(DbSecrets {
        db_id: db,
        key: db_key,
        salt: db_salt,
    })
}

fn remove(root: &Path, name: &str) -> R<()> {
    match fs::remove_file(root.join(name)) {
        Ok(()) => atomic::sync_directory(root),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(invalid()),
    }
}
