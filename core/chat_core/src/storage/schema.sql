CREATE TABLE storage_meta (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
    schema_version INTEGER NOT NULL CHECK(schema_version = 1),
    db_id BLOB NOT NULL CHECK(length(db_id) = 16),
    binding BLOB NOT NULL,
    legacy_hash BLOB CHECK(legacy_hash IS NULL OR length(legacy_hash) = 32),
    migration_ready INTEGER NOT NULL CHECK(migration_ready IN (0, 1)),
    message_revision INTEGER NOT NULL CHECK(message_revision BETWEEN 0 AND 9223372036854775807)
) STRICT;

CREATE TABLE account_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
    installation BLOB NOT NULL CHECK(length(installation) = 32),
    payload BLOB NOT NULL CHECK(length(payload) <= 4194304)
) STRICT;
