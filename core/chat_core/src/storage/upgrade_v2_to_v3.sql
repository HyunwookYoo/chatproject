-- 이 기기의 기록 (spec 5.1). seq는 단조 증가, 재사용하지 않는다.
CREATE TABLE mailbox_store (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    sender_peer BLOB NOT NULL CHECK(length(sender_peer)=32),
    sender_install BLOB NOT NULL CHECK(length(sender_install)=32),  -- 원래 보낸 기기 (MLS leaf)
    kind INTEGER NOT NULL CHECK(kind IN (1,2,3,4)),  -- 1 kind 17 frame inner, 2 Delivered 노트, 3 HandoffStop 노트, 4 HandoffWithdraw 노트
    msg_id BLOB CHECK(msg_id IS NULL OR length(msg_id)=16),  -- App Text와 노트의 대상 메시지
    hash BLOB UNIQUE NOT NULL CHECK(length(hash)=32),  -- SHA-256(body)
    body BLOB NOT NULL CHECK(length(body) BETWEEN 1 AND 61434),
    recorded_at_ms INTEGER NOT NULL CHECK(recorded_at_ms>=0)
) STRICT;
CREATE INDEX mailbox_store_age ON mailbox_store(recorded_at_ms);

-- 내 다른 기기가 이 기기 기록을 어디까지 가져갔나 (보관 판정)
CREATE TABLE mailbox_cursor (
    installation BLOB PRIMARY KEY NOT NULL CHECK(length(installation)=32),
    next_seq INTEGER NOT NULL CHECK(next_seq>=1)
) STRICT;

-- 이 기기가 내 다른 기기 기록을 어디까지 처리했나
CREATE TABLE mailbox_pull (
    installation BLOB PRIMARY KEY NOT NULL CHECK(length(installation)=32),
    since_seq INTEGER NOT NULL CHECK(since_seq>=0)
) STRICT;

-- 메시지보다 먼저 온 상태: 1 내가 읽음, 2 상대 전달, 3 상대 읽음, 4 송신 인계 철회 (Ruling 27)
CREATE TABLE mailbox_status_ahead (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    sender_peer BLOB NOT NULL CHECK(length(sender_peer)=32),
    msg_id BLOB NOT NULL CHECK(length(msg_id)=16),
    kind INTEGER NOT NULL CHECK(kind IN (1,2,3,4)),
    at_ms INTEGER NOT NULL CHECK(at_ms>=0),
    PRIMARY KEY(conversation,sender_peer,msg_id,kind)
) STRICT;

-- 메일박스가 넘겨받은 암호문
CREATE TABLE mailbox_handoff (
    origin_install BLOB NOT NULL CHECK(length(origin_install)=32),
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    msg_id BLOB NOT NULL CHECK(length(msg_id)=16),
    peer BLOB NOT NULL CHECK(length(peer)=32),
    ciphertext BLOB NOT NULL CHECK(length(ciphertext) BETWEEN 1 AND 61434),
    attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 4294967295),
    next_retry_at_ms INTEGER NOT NULL CHECK(next_retry_at_ms>=0),
    received_at_ms INTEGER NOT NULL CHECK(received_at_ms>=0),
    PRIMARY KEY(origin_install,conversation,msg_id)
) STRICT;

-- 이 기기의 메일박스 설정과 내 다른 기기가 알린 상태, 가동 기록
CREATE TABLE mailbox_local (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton=1),
    enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
    first_link_shown INTEGER NOT NULL CHECK(first_link_shown IN (0,1))
) STRICT;
INSERT INTO mailbox_local VALUES(1,0,0);
CREATE TABLE mailbox_device (
    installation BLOB PRIMARY KEY NOT NULL CHECK(length(installation)=32),
    mailbox INTEGER NOT NULL CHECK(mailbox IN (0,1)),
    announced_at_ms INTEGER NOT NULL CHECK(announced_at_ms>=0)
) STRICT;
CREATE TABLE mailbox_uptime (
    started_at_ms INTEGER PRIMARY KEY NOT NULL CHECK(started_at_ms>=0),
    ended_at_ms INTEGER NOT NULL CHECK(ended_at_ms>=started_at_ms)
) STRICT;

-- 다른 내 기기 기록에서 가져와 저장한, 아직 알리지 않은 상대 텍스트 (Ruling 35). 그 가져오기가
-- 끝나면(또는 다시 켜진 뒤) 알리고 지운다.
CREATE TABLE mailbox_notify (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    sender_peer BLOB NOT NULL CHECK(length(sender_peer)=32),
    sender_install BLOB NOT NULL CHECK(length(sender_install)=32),
    msg_id BLOB NOT NULL CHECK(length(msg_id)=16),
    holder BLOB NOT NULL CHECK(length(holder)=32),  -- 가져온 기록의 기기
    PRIMARY KEY(conversation,sender_peer,sender_install,msg_id)
) STRICT;

-- storage_meta: only the schema_version CHECK changes, so the table is rebuilt.
CREATE TABLE storage_meta_v3 (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
    schema_version INTEGER NOT NULL CHECK(schema_version = 3),
    db_id BLOB NOT NULL CHECK(length(db_id) = 16),
    binding BLOB NOT NULL,
    legacy_hash BLOB CHECK(legacy_hash IS NULL OR length(legacy_hash) = 32),
    migration_ready INTEGER NOT NULL CHECK(migration_ready IN (0, 1)),
    message_revision INTEGER NOT NULL CHECK(message_revision BETWEEN 0 AND 9223372036854775807)
) STRICT;
INSERT INTO storage_meta_v3
    SELECT singleton,3,db_id,binding,legacy_hash,migration_ready,message_revision FROM storage_meta;
DROP TABLE storage_meta;
ALTER TABLE storage_meta_v3 RENAME TO storage_meta;
