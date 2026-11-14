-- 이 기기의 푸시 설정 (한 행). platform NULL = 푸시 받지 않음(Windows, 권한 없음)
CREATE TABLE push_local (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
    platform INTEGER CHECK(platform IS NULL OR platform IN (1,2)),
    token TEXT CHECK(token IS NULL OR length(CAST(token AS BLOB)) BETWEEN 1 AND 512),
    own_tag BLOB NOT NULL CHECK(length(own_tag)=16),
    own_handle BLOB CHECK(own_handle IS NULL OR length(own_handle) BETWEEN 1 AND 1024),
    own_issued_at_ms INTEGER CHECK(own_issued_at_ms IS NULL OR own_issued_at_ms>=0)
) STRICT;
-- 이 기기가 대화마다 발급받은 handle (spec 5.1)
CREATE TABLE push_tag (
    conversation BLOB PRIMARY KEY NOT NULL CHECK(length(conversation)=32),
    tag BLOB UNIQUE NOT NULL CHECK(length(tag)=16),
    handle BLOB CHECK(handle IS NULL OR length(handle) BETWEEN 1 AND 1024),
    issued_at_ms INTEGER CHECK(issued_at_ms IS NULL OR issued_at_ms>=0),
    announced INTEGER NOT NULL CHECK(announced IN (0,1))   -- 현재 handle(또는 철회)을 봉인했나
) STRICT;
-- 상대 기기가 알린 handle: (대화, 상대 leaf installation)마다 최신 하나 (spec 5.2)
-- handle NULL = 철회(묘비). sent_at_ms(그 frame의 ChatMessage.sent_at_ms)가 더 큰 것만 덮어쓴다 (Ruling R10)
CREATE TABLE push_peer (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    installation BLOB NOT NULL CHECK(length(installation)=32),
    handle BLOB CHECK(handle IS NULL OR length(handle) BETWEEN 1 AND 1024),
    sent_at_ms INTEGER NOT NULL CHECK(sent_at_ms>=0),
    received_at_ms INTEGER NOT NULL CHECK(received_at_ms>=0),
    PRIMARY KEY(conversation, installation)
) STRICT;
-- 내 다른 Android 기기가 Announce로 알린 handle (읽음 정리, spec 7)
CREATE TABLE push_own (
    installation BLOB PRIMARY KEY NOT NULL CHECK(length(installation)=32),
    handle BLOB NOT NULL CHECK(length(handle) BETWEEN 1 AND 1024)
) STRICT;
-- 봉인한 PushHandle frame의 상대 기기별 전달 (mls_delivery와 같은 방식)
CREATE TABLE push_delivery (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    recipient_install BLOB NOT NULL CHECK(length(recipient_install)=32),
    msg_id BLOB NOT NULL CHECK(length(msg_id)=16),
    wire BLOB NOT NULL CHECK(length(wire) BETWEEN 1 AND 61440),
    attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 4294967295),
    next_retry_at_ms INTEGER NOT NULL CHECK(next_retry_at_ms>=0),
    handed_off INTEGER NOT NULL CHECK(handed_off IN (0,1)),
    PRIMARY KEY(conversation, recipient_install)
) STRICT;
ALTER TABLE outbox ADD COLUMN pushed INTEGER NOT NULL DEFAULT 0 CHECK(pushed IN (0,1));

-- storage_meta: only the schema_version CHECK changes, so the table is rebuilt.
CREATE TABLE storage_meta_v4 (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
    schema_version INTEGER NOT NULL CHECK(schema_version = 4),
    db_id BLOB NOT NULL CHECK(length(db_id) = 16),
    binding BLOB NOT NULL,
    legacy_hash BLOB CHECK(legacy_hash IS NULL OR length(legacy_hash) = 32),
    migration_ready INTEGER NOT NULL CHECK(migration_ready IN (0, 1)),
    message_revision INTEGER NOT NULL CHECK(message_revision BETWEEN 0 AND 9223372036854775807)
) STRICT;
INSERT INTO storage_meta_v4
    SELECT singleton,4,db_id,binding,legacy_hash,migration_ready,message_revision FROM storage_meta;
DROP TABLE storage_meta;
ALTER TABLE storage_meta_v4 RENAME TO storage_meta;
