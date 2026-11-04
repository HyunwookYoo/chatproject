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

-- storage_meta: only the schema_version CHECK changes, so the table is rebuilt.
CREATE TABLE storage_meta_v2 (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
    schema_version INTEGER NOT NULL CHECK(schema_version = 2),
    db_id BLOB NOT NULL CHECK(length(db_id) = 16),
    binding BLOB NOT NULL,
    legacy_hash BLOB CHECK(legacy_hash IS NULL OR length(legacy_hash) = 32),
    migration_ready INTEGER NOT NULL CHECK(migration_ready IN (0, 1)),
    message_revision INTEGER NOT NULL CHECK(message_revision BETWEEN 0 AND 9223372036854775807)
) STRICT;
INSERT INTO storage_meta_v2
    SELECT singleton,2,db_id,binding,legacy_hash,migration_ready,message_revision FROM storage_meta;
DROP TABLE storage_meta;
ALTER TABLE storage_meta_v2 RENAME TO storage_meta;

-- outbox: every M4 plaintext (codec 1) job becomes "awaiting encryption" (codec 3, no wire).
-- The plaintext stays in `message`; retry facts and a blocked state (2) are kept, while an
-- in-flight mark (1) cannot survive a restart and returns to 0.
CREATE TABLE outbox_v2 (
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
    PRIMARY KEY(conversation,sender_peer,sender_install,msg_id),
    FOREIGN KEY(conversation,sender_peer,sender_install,msg_id)
        REFERENCES message(conversation,sender_peer,sender_install,msg_id)
) STRICT;
INSERT INTO outbox_v2(conversation,sender_peer,sender_install,msg_id,codec,wire,attempts,
                      retry_delay_ms,next_retry_at_ms,dispatch_state)
    SELECT conversation,sender_peer,sender_install,msg_id,3,x'',attempts,retry_delay_ms,
           next_retry_at_ms,CASE WHEN dispatch_state=1 THEN 0 ELSE dispatch_state END
    FROM outbox;
DROP TABLE outbox;
ALTER TABLE outbox_v2 RENAME TO outbox;
CREATE INDEX outbox_due ON outbox(dispatch_state,next_retry_at_ms,conversation);
