-- 이 기기의 큐 설정과 판정 (한 행, spec 5). mode 0 자동, 1 항상 서버 사용, 2 서버 사용 안 함
CREATE TABLE queue_local (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    mode INTEGER NOT NULL CHECK(mode IN (0,1,2)),
    qualified INTEGER NOT NULL CHECK(qualified IN (0,1)),  -- 이 기기가 메일박스 자격이 있다고 알렸나
    qualified_at_ms INTEGER CHECK(qualified_at_ms IS NULL OR qualified_at_ms>=0),
    verdict_at_ms INTEGER CHECK(verdict_at_ms IS NULL OR verdict_at_ms>=0)
) STRICT;
INSERT INTO queue_local VALUES(1,0,0,NULL,NULL);
-- 이 기기가 받는 큐 (spec 6.2). state 0 Active, 1 Retiring(delete_at_ms까지 가져온 뒤 DELETE)
CREATE TABLE queue_own (
    queue_id BLOB PRIMARY KEY NOT NULL CHECK(length(queue_id)=32),
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    recipient_secret BLOB NOT NULL CHECK(length(recipient_secret)=32),
    sender_secret BLOB NOT NULL CHECK(length(sender_secret)=32),
    created_at_ms INTEGER NOT NULL CHECK(created_at_ms>=0),
    state INTEGER NOT NULL CHECK(state IN (0,1)),
    delete_at_ms INTEGER CHECK((delete_at_ms IS NULL) = (state=0) AND (delete_at_ms IS NULL OR delete_at_ms>=0)),
    last_seq INTEGER NOT NULL CHECK(last_seq>=0),
    server_confirmed INTEGER NOT NULL CHECK(server_confirmed IN (0,1))  -- CREATE가 성공했나
) STRICT;
-- Active는 대화마다 많아야 하나
CREATE UNIQUE INDEX queue_own_active ON queue_own(conversation) WHERE state=0;
CREATE INDEX queue_own_conversation ON queue_own(conversation,state);
-- 상대 기기가 알린 큐: (대화, 상대 leaf installation)마다 최신 하나 (spec 7.2)
-- queue_id NULL = 철회(묘비). sent_at_ms가 더 큰 frame만 덮어쓴다 (push_peer와 같다)
CREATE TABLE queue_peer (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    installation BLOB NOT NULL CHECK(length(installation)=32),
    server_url TEXT CHECK(server_url IS NULL OR length(CAST(server_url AS BLOB)) BETWEEN 1 AND 2048),
    queue_id BLOB CHECK(queue_id IS NULL OR length(queue_id)=32),
    sender_secret BLOB CHECK(sender_secret IS NULL OR length(sender_secret)=32),
    sent_at_ms INTEGER NOT NULL CHECK(sent_at_ms>=0),
    received_at_ms INTEGER NOT NULL CHECK(received_at_ms>=0),
    dead INTEGER NOT NULL CHECK(dead IN (0,1)),  -- 서버가 gone이라 답했다: 다음 QueueRef까지 쓰지 않는다 (spec 8.3)
    PRIMARY KEY(conversation, installation),
    CHECK((server_url IS NULL) = (queue_id IS NULL) AND (sender_secret IS NULL) = (queue_id IS NULL))
) STRICT;
-- 큐에 넣은 보낸 메시지: 상대 기기마다, 큐 보관이 끝나면(expires_at_ms) 다시 넣는다 (spec 8.2)
CREATE TABLE queue_delivery (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    sender_peer BLOB NOT NULL CHECK(length(sender_peer)=32),
    sender_install BLOB NOT NULL CHECK(length(sender_install)=32),
    msg_id BLOB NOT NULL CHECK(length(msg_id)=16),
    recipient_install BLOB NOT NULL CHECK(length(recipient_install)=32),
    queue_id BLOB NOT NULL CHECK(length(queue_id)=32),
    expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms>=0),
    PRIMARY KEY(conversation,sender_peer,sender_install,msg_id,recipient_install)
) STRICT;
CREATE INDEX queue_delivery_due ON queue_delivery(expires_at_ms);
ALTER TABLE mailbox_device ADD COLUMN qualified INTEGER NOT NULL DEFAULT 0 CHECK(qualified IN (0,1));
ALTER TABLE mailbox_device ADD COLUMN last_seen_ms INTEGER CHECK(last_seen_ms IS NULL OR last_seen_ms>=0);

-- push_delivery: kind joins the primary key, so the table is rebuilt. Every v4 row is a
-- PushHandle frame (kind 1).
CREATE TABLE push_delivery_v5 (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    recipient_install BLOB NOT NULL CHECK(length(recipient_install)=32),
    kind INTEGER NOT NULL DEFAULT 1 CHECK(kind IN (1,2)),  -- 1 PushHandle, 2 QueueRef
    msg_id BLOB NOT NULL CHECK(length(msg_id)=16),
    wire BLOB NOT NULL CHECK(length(wire) BETWEEN 1 AND 61440),
    attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 4294967295),
    next_retry_at_ms INTEGER NOT NULL CHECK(next_retry_at_ms>=0),
    handed_off INTEGER NOT NULL CHECK(handed_off IN (0,1)),
    PRIMARY KEY(conversation, recipient_install, kind)
) STRICT;
INSERT INTO push_delivery_v5(conversation,recipient_install,kind,msg_id,wire,attempts,next_retry_at_ms,handed_off)
    SELECT conversation,recipient_install,1,msg_id,wire,attempts,next_retry_at_ms,handed_off FROM push_delivery;
DROP TABLE push_delivery;
ALTER TABLE push_delivery_v5 RENAME TO push_delivery;

-- storage_meta: only the schema_version CHECK changes, so the table is rebuilt.
CREATE TABLE storage_meta_v5 (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
    schema_version INTEGER NOT NULL CHECK(schema_version = 5),
    db_id BLOB NOT NULL CHECK(length(db_id) = 16),
    binding BLOB NOT NULL,
    legacy_hash BLOB CHECK(legacy_hash IS NULL OR length(legacy_hash) = 32),
    migration_ready INTEGER NOT NULL CHECK(migration_ready IN (0, 1)),
    message_revision INTEGER NOT NULL CHECK(message_revision BETWEEN 0 AND 9223372036854775807)
) STRICT;
INSERT INTO storage_meta_v5
    SELECT singleton,5,db_id,binding,legacy_hash,migration_ready,message_revision FROM storage_meta;
DROP TABLE storage_meta;
ALTER TABLE storage_meta_v5 RENAME TO storage_meta;
