use chat_core::account::Secret32;
use chat_core::{CoreError, ErrorCode};
use std::{fs, path::Path, process::Command};

mod account {
    pub use chat_core::account::Secret32;
}

#[path = "../src/storage/engine.rs"]
mod engine;

const SENTINEL: &[u8] = b"public-sqlcipher-encryption-fixture-2026";

fn assert_no_plaintext(path: &Path) {
    for suffix in ["", "-wal", "-journal"] {
        let file = path.with_file_name(format!("engine.db{suffix}"));
        if file.exists() {
            let bytes = fs::read(file).unwrap();
            assert!(!bytes.windows(SENTINEL.len()).any(|w| w == SENTINEL));
        }
    }
}

#[test]
fn real_cipher_reopens_and_plain_sqlite_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engine.db");
    let key = Secret32::from([0x31; 32]);
    let salt = [0x42; 16];
    let conn = engine::open(&path, &key, &salt).unwrap();
    let info = engine::info(&conn).unwrap();
    assert_eq!(info.sqlite_version, 3_051_003);
    assert!(info.cipher_version.starts_with("4.14.0"));
    assert_eq!(
        info.provider,
        if cfg!(target_vendor = "apple") {
            "commoncrypto"
        } else {
            "openssl"
        }
    );
    let header: String = conn
        .query_row("PRAGMA cipher_plaintext_header_size", [], |r| r.get(0))
        .unwrap();
    assert_eq!(header, if cfg!(target_os = "ios") { "32" } else { "0" });
    for (pragma, expected) in [
        ("synchronous", 2),
        ("foreign_keys", 1),
        ("temp_store", 2),
        ("cache_size", -2048),
    ] {
        let value: i32 = conn
            .query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, expected, "{pragma}");
    }
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
    conn.execute_batch(include_str!("../src/storage/schema.sql"))
        .unwrap();
    conn.execute(
        "INSERT INTO account_state VALUES (1, ?1, ?2)",
        rusqlite::params![[9u8; 32].as_slice(), SENTINEL],
    )
    .unwrap();
    assert_no_plaintext(&path);
    drop(conn);
    let conn = engine::open(&path, &key, &salt).unwrap();
    let payload: Vec<u8> = conn
        .query_row("SELECT payload FROM account_state", [], |r| r.get(0))
        .unwrap();
    assert_eq!(payload, SENTINEL);
    drop(conn);
    assert!(engine::open(&path, &Secret32::from([0x32; 32]), &salt).is_err());
    assert!(engine::open(&path, &key, &[0x43; 16]).is_err());
    assert_no_plaintext(&path);
    let status = Command::new("python").arg("-c").arg(
        "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]);\ntry: c.execute('SELECT * FROM sqlite_master').fetchall()\nexcept sqlite3.DatabaseError: sys.exit(0)\nsys.exit(1)"
    ).arg(&path).status().unwrap();
    assert!(
        status.success(),
        "ordinary SQLite must reject the encrypted database"
    );
}

#[test]
fn plaintext_pragma_noop_is_not_encryption() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain.db");
    let status = Command::new("python").arg("-c").arg(
        "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute(\"PRAGMA key='ignored'\"); assert c.execute('PRAGMA cipher_version').fetchone() is None; c.execute('CREATE TABLE public_fixture(value)'); c.commit()"
    ).arg(&path).status().unwrap();
    assert!(status.success());
    assert!(engine::open(&path, &Secret32::from([0x31; 32]), &[0x42; 16]).is_err());
}

#[test]
fn schema_enforces_version_one_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let conn = engine::open(
        &dir.path().join("engine.db"),
        &Secret32::from([1; 32]),
        &[2; 16],
    )
    .unwrap();
    conn.execute_batch(include_str!("../src/storage/schema.sql"))
        .unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        tables,
        [
            "account_state",
            "conversation",
            "message",
            "outbox",
            "processed_ct",
            "receipt_job",
            "storage_meta"
        ]
    );
    let unconfigured: i64 = conn
        .query_row("SELECT count(*) FROM account_state", [], |r| r.get(0))
        .unwrap();
    assert_eq!(unconfigured, 0);
    conn.execute_batch("INSERT INTO storage_meta VALUES (1,1,zeroblob(16),x'01',NULL,0,0)")
        .unwrap();
    for update in [
        "singleton=2",
        "singleton=NULL",
        "schema_version=NULL",
        "schema_version=2",
        "db_id=NULL",
        "db_id=zeroblob(15)",
        "binding=NULL",
        "legacy_hash=zeroblob(31)",
        "migration_ready=NULL",
        "migration_ready=2",
        "message_revision=NULL",
        "message_revision=-1",
        "message_revision=9223372036854775808",
    ] {
        assert!(
            conn.execute_batch(&format!("UPDATE storage_meta SET {update}"))
                .is_err(),
            "{update}"
        );
    }
    conn.execute_batch(
        "UPDATE storage_meta SET legacy_hash=zeroblob(32), migration_ready=1, \
         message_revision=9223372036854775807",
    )
    .unwrap();
    conn.execute_batch("INSERT INTO account_state VALUES (1,zeroblob(32),x'')")
        .unwrap();
    for update in [
        "singleton=2",
        "singleton=NULL",
        "installation=NULL",
        "installation=zeroblob(31)",
        "payload=NULL",
        "payload=zeroblob(4194305)",
    ] {
        assert!(
            conn.execute_batch(&format!("UPDATE account_state SET {update}"))
                .is_err(),
            "{update}"
        );
    }
    conn.execute_batch("UPDATE account_state SET payload=zeroblob(4194304)")
        .unwrap();
}
