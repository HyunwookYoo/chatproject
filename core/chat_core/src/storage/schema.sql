CREATE TABLE storage_meta (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
    schema_version INTEGER NOT NULL CHECK(schema_version = 5),
    db_id BLOB NOT NULL CHECK(length(db_id) = 16),
    binding BLOB NOT NULL,
    legacy_hash BLOB CHECK(legacy_hash IS NULL OR length(legacy_hash) = 32),
    migration_ready INTEGER NOT NULL CHECK(migration_ready IN (0, 1)),
    message_revision INTEGER NOT NULL CHECK(message_revision BETWEEN 0 AND 9223372036854775807)
) STRICT;

CREATE TABLE conversation (
    id BLOB PRIMARY KEY NOT NULL CHECK(length(id)=32),
    peer BLOB UNIQUE NOT NULL CHECK(length(peer)=32),
    lamport_max INTEGER NOT NULL CHECK(lamport_max>=0),
    local_prev_hash BLOB NOT NULL CHECK(length(local_prev_hash)=32),
    change_revision INTEGER NOT NULL CHECK(change_revision>=0),
    unread INTEGER NOT NULL CHECK(unread>=0)
) STRICT;

CREATE TABLE message (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    sender_peer BLOB NOT NULL CHECK(length(sender_peer)=32),
    sender_install BLOB NOT NULL CHECK(length(sender_install)=32),
    msg_id BLOB NOT NULL CHECK(length(msg_id)=16),
    lamport INTEGER NOT NULL CHECK(lamport>=0),
    prev_hash BLOB NOT NULL CHECK(length(prev_hash)=32),
    payload_hash BLOB NOT NULL CHECK(length(payload_hash)=32),
    sent_at_ms INTEGER NOT NULL CHECK(sent_at_ms>=0),
    recv_at_ms INTEGER NOT NULL CHECK(recv_at_ms>=0),
    body TEXT NOT NULL CHECK(length(CAST(body AS BLOB)) BETWEEN 1 AND 16384),
    outgoing INTEGER NOT NULL CHECK(outgoing IN (0,1)),
    outcome INTEGER NOT NULL CHECK(outcome IN (0,1,2,3)),
    delivered_at_ms INTEGER CHECK(delivered_at_ms IS NULL OR delivered_at_ms>=0),
    read_at_ms INTEGER CHECK(read_at_ms IS NULL OR read_at_ms>=0),
    seen_local INTEGER NOT NULL CHECK(seen_local IN (0,1)),
    cancel_requested INTEGER NOT NULL CHECK(cancel_requested IN (0,1)),
    block_reason INTEGER CHECK(block_reason IS NULL OR block_reason IN (1,2,3)),
    failure_code TEXT CHECK(failure_code IS NULL OR failure_code IN
        ('InvalidConfig','AlreadyStartedDifferentConfig','CoreNotStarted','NetworkNotStarted',
         'InvalidPeerAddress','SelfConnection','PeerNotConnected','MessageTooLarge','Busy',
         'NetworkTimeout','TransportFailed','NotAvailable','InvalidRecovery','StorageLocked',
         'StorageMissing','StorageCorrupt','StorageFull','AccountExists','WriterBusy',
         'InvalidInvite','InviteExpired','DeviceLimit','PairMismatch','PairExpired',
         'RecordConflict','RecordUnavailable','AuthFailed','AccountRestricted','PairHistoryFull',
         'InvalidMessage','MessageConflict','MessageNotFound','ContactRequired',
         'UnsupportedMessageVersion','StorageUncertain')),
    attempted_receivers BLOB NOT NULL CHECK(length(attempted_receivers)<=160 AND length(attempted_receivers)%32=0),
    PRIMARY KEY(conversation,sender_peer,sender_install,msg_id),
    UNIQUE(conversation,lamport,sender_install,msg_id),
    FOREIGN KEY(conversation) REFERENCES conversation(id)
) STRICT;

CREATE TABLE outbox (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    sender_peer BLOB NOT NULL CHECK(length(sender_peer)=32),
    sender_install BLOB NOT NULL CHECK(length(sender_install)=32),
    msg_id BLOB NOT NULL CHECK(length(msg_id)=16),
    codec INTEGER NOT NULL CHECK(codec IN (2,3)),
    wire BLOB NOT NULL CHECK(length(wire)<=61440 AND (codec=3 OR length(wire)>=1)),
    attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 4294967295),
    retry_delay_ms INTEGER NOT NULL CHECK(retry_delay_ms BETWEEN 0 AND 300000),
    next_retry_at_ms INTEGER NOT NULL CHECK(next_retry_at_ms>=0),
    dispatch_state INTEGER NOT NULL CHECK(dispatch_state IN (0,1,2)),
    reencrypts INTEGER NOT NULL DEFAULT 0 CHECK(reencrypts BETWEEN 0 AND 3),
    waiting_peer_update INTEGER NOT NULL DEFAULT 0 CHECK(waiting_peer_update IN (0,1)),
    pushed INTEGER NOT NULL DEFAULT 0 CHECK(pushed IN (0,1)),
    PRIMARY KEY(conversation,sender_peer,sender_install,msg_id),
    FOREIGN KEY(conversation,sender_peer,sender_install,msg_id)
        REFERENCES message(conversation,sender_peer,sender_install,msg_id)
) STRICT;

CREATE TABLE processed_ct (
    codec INTEGER NOT NULL CHECK(codec>=1),
    sender_peer BLOB NOT NULL CHECK(length(sender_peer)=32),
    sender_install BLOB NOT NULL CHECK(length(sender_install)=32),
    hash BLOB NOT NULL CHECK(length(hash)=32),
    seen_at_ms INTEGER NOT NULL CHECK(seen_at_ms>=0),
    PRIMARY KEY(codec,sender_peer,sender_install,hash)
) STRICT;

CREATE TABLE receipt_job (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    sender_peer BLOB NOT NULL CHECK(length(sender_peer)=32),
    sender_install BLOB NOT NULL CHECK(length(sender_install)=32),
    msg_id BLOB NOT NULL CHECK(length(msg_id)=16),
    recipient_install BLOB NOT NULL CHECK(length(recipient_install)=32),
    kind INTEGER NOT NULL CHECK(kind IN (1,2)),
    next_retry_at_ms INTEGER NOT NULL CHECK(next_retry_at_ms>=0),
    attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 4294967295),
    PRIMARY KEY(conversation,sender_peer,sender_install,msg_id,recipient_install,kind),
    FOREIGN KEY(conversation,sender_peer,sender_install,msg_id)
        REFERENCES message(conversation,sender_peer,sender_install,msg_id)
) STRICT;

CREATE INDEX message_order ON message(conversation,lamport DESC,sender_install DESC,msg_id DESC);
CREATE INDEX outbox_due ON outbox(dispatch_state,next_retry_at_ms,conversation);
CREATE INDEX receipt_due ON receipt_job(next_retry_at_ms,conversation);
CREATE INDEX conversation_recency ON conversation(change_revision DESC,id DESC);

CREATE TABLE account_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
    installation BLOB NOT NULL CHECK(length(installation) = 32),
    payload BLOB NOT NULL CHECK(length(payload) <= 4194304)
) STRICT;

CREATE TABLE mls_kv (
    scope BLOB NOT NULL CHECK(length(scope)=32),
    key BLOB PRIMARY KEY NOT NULL CHECK(length(key) BETWEEN 1 AND 4096),
    value BLOB NOT NULL CHECK(length(value)<=4194304)
) STRICT;
CREATE INDEX mls_kv_scope ON mls_kv(scope);

CREATE TABLE mls_conversation (
    conversation BLOB PRIMARY KEY NOT NULL CHECK(length(conversation)=32),
    group_id BLOB NOT NULL CHECK(length(group_id) BETWEEN 1 AND 64),
    first_commit_hash BLOB NOT NULL CHECK(length(first_commit_hash)=32),
    epoch INTEGER NOT NULL CHECK(epoch>=0),
    previous_epoch_snapshot BLOB,
    own_commit_unacked INTEGER NOT NULL CHECK(own_commit_unacked IN (0,1)),
    rejoin_count INTEGER NOT NULL CHECK(rejoin_count>=0),
    rejoin_after_ms INTEGER NOT NULL CHECK(rejoin_after_ms>=0),
    peer_mls INTEGER NOT NULL CHECK(peer_mls IN (0,1)),
    last_commit_hash BLOB CHECK(last_commit_hash IS NULL OR length(last_commit_hash)=32),
    FOREIGN KEY(conversation) REFERENCES conversation(id)
) STRICT;

CREATE TABLE mls_delivery (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    recipient_install BLOB NOT NULL CHECK(length(recipient_install)=32),
    kind INTEGER NOT NULL CHECK(kind IN (1,2)),
    epoch INTEGER NOT NULL CHECK(epoch>=0),
    body BLOB NOT NULL CHECK(length(body) BETWEEN 1 AND 61440),
    attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 4294967295),
    next_retry_at_ms INTEGER NOT NULL CHECK(next_retry_at_ms>=0),
    PRIMARY KEY(conversation,recipient_install,kind,epoch)
) STRICT;

CREATE TABLE mls_hold (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    sender_peer BLOB NOT NULL CHECK(length(sender_peer)=32),
    body_hash BLOB NOT NULL CHECK(length(body_hash)=32),
    body BLOB NOT NULL CHECK(length(body) BETWEEN 1 AND 61440),
    held_at_ms INTEGER NOT NULL CHECK(held_at_ms>=0),
    PRIMARY KEY(conversation,body_hash)
) STRICT;

-- Installations of the conversation's peer account that have spoken MLS to this device (an
-- accepted App, Commit or Welcome). Never cleared, not even when the group is dropped or
-- rejoined: the downgrade guard refuses plaintext texts from exactly these installations.
CREATE TABLE mls_peer (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    installation BLOB NOT NULL CHECK(length(installation)=32),
    PRIMARY KEY(conversation,installation),
    FOREIGN KEY(conversation) REFERENCES conversation(id)
) STRICT;

-- A sealed MLS receipt (`Content::Receipt`) for a received message: its retries resend this
-- same ciphertext, the way a sealed text's retries resend its outbox wire. Dropped when the job
-- settles, when the recipient could not open it, and with the group.
CREATE TABLE mls_receipt (
    conversation BLOB NOT NULL CHECK(length(conversation)=32),
    sender_peer BLOB NOT NULL CHECK(length(sender_peer)=32),
    sender_install BLOB NOT NULL CHECK(length(sender_install)=32),
    msg_id BLOB NOT NULL CHECK(length(msg_id)=16),
    kind INTEGER NOT NULL CHECK(kind IN (1,2)),
    wire BLOB NOT NULL CHECK(length(wire) BETWEEN 1 AND 61440),
    PRIMARY KEY(conversation,sender_peer,sender_install,msg_id,kind)
) STRICT;

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
    announced_at_ms INTEGER NOT NULL CHECK(announced_at_ms>=0),
    qualified INTEGER NOT NULL DEFAULT 0 CHECK(qualified IN (0,1)),
    last_seen_ms INTEGER CHECK(last_seen_ms IS NULL OR last_seen_ms>=0)
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
    announced INTEGER NOT NULL CHECK(announced IN (0,1)),  -- 현재 handle(또는 철회)을 봉인했나
    -- 푸시로 "새 메시지"가 울린 뒤 아직 다 읽지 않았나: 그동안 이 대화의 새 글은 소리 없이 고친다 (Ruling R13)
    generic INTEGER NOT NULL DEFAULT 0 CHECK(generic IN (0,1))
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
-- 봉인한 PushHandle(kind 1)·QueueRef(kind 2) frame의 상대 기기별 전달 (mls_delivery와 같은 방식)
CREATE TABLE push_delivery (
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
