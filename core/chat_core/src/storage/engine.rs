use crate::{CoreError, ErrorCode, account::Secret32};
use rusqlite::{Connection, OptionalExtension};
use std::{fmt::Write, path::Path};
use zeroize::Zeroizing;

type R<T> = Result<T, CoreError>;

// Interim until the T2 worker consumes the private engine.
#[allow(dead_code)]
pub(crate) struct EngineInfo {
    pub sqlite_version: i32,
    pub cipher_version: String,
    pub provider: String,
}

fn failure() -> CoreError {
    // Never expose SQLite error text: it can contain SQL, paths, or values.
    CoreError::new(ErrorCode::StorageCorrupt, "storage_engine_failed")
}

fn cipher_version(value: Option<String>) -> R<String> {
    value
        .filter(|version| version.starts_with("4.14.0"))
        .ok_or_else(failure)
}

// Interim until the T2 worker consumes the private engine.
#[allow(dead_code)]
pub(crate) fn info(conn: &Connection) -> R<EngineInfo> {
    let version = conn
        .query_row("PRAGMA cipher_version", [], |row| row.get(0))
        .optional()
        .map_err(|_| failure())?;
    let cipher_version = cipher_version(version)?;
    let provider: String = conn
        .query_row("PRAGMA cipher_provider", [], |row| row.get(0))
        .map_err(|_| failure())?;
    let sqlite_version = rusqlite::version_number();
    let expected = if cfg!(target_vendor = "apple") {
        "commoncrypto"
    } else {
        "openssl"
    };
    if sqlite_version != 3_051_003 || provider != expected {
        return Err(failure());
    }
    Ok(EngineInfo {
        sqlite_version,
        cipher_version,
        provider,
    })
}

// This synchronous entry point must run off the async network/UI executors.
// Interim until the T2 worker consumes the private engine.
#[allow(dead_code)]
pub(crate) fn open(path: &Path, key: &Secret32, salt: &[u8; 16]) -> R<Connection> {
    let conn = Connection::open(path).map_err(|_| failure())?;
    // Raw SQLCipher key material is 32 key bytes followed by 16 salt bytes.
    // Build it directly in the erased allocation; never trace this statement.
    let mut key_sql = Zeroizing::new(String::with_capacity(116));
    key_sql.push_str("PRAGMA key = \"x'");
    for byte in key.as_bytes().iter().chain(salt.iter()) {
        write!(&mut *key_sql, "{byte:02x}").map_err(|_| failure())?;
    }
    key_sql.push_str("'\";");
    conn.execute_batch(&key_sql).map_err(|_| failure())?;
    drop(key_sql);
    conn.pragma_update(
        None,
        "cipher_plaintext_header_size",
        if cfg!(target_os = "ios") { 32 } else { 0 },
    )
    .map_err(|_| failure())?;
    info(&conn)?;
    // A successful PRAGMA key alone is not key validation. Read an encrypted
    // page before returning or changing journal settings on an existing file.
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    })
    .map_err(|_| failure())?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; \
         PRAGMA foreign_keys=ON; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-2048;",
    )
    .map_err(|_| failure())?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_empty_cipher_version_is_rejected() {
        for version in [None, Some(String::new()), Some("4.13.0 community".into())] {
            assert!(cipher_version(version).is_err());
        }
    }
}
