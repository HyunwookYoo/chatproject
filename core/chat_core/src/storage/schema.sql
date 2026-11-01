CREATE TABLE storage_meta (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton = 1),
    schema_version INTEGER NOT NULL CHECK(schema_version = 1),
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
    codec INTEGER NOT NULL CHECK(codec=1),
    wire BLOB NOT NULL CHECK(length(wire) BETWEEN 1 AND 61440),
    attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 4294967295),
    retry_delay_ms INTEGER NOT NULL CHECK(retry_delay_ms BETWEEN 0 AND 300000),
    next_retry_at_ms INTEGER NOT NULL CHECK(next_retry_at_ms>=0),
    dispatch_state INTEGER NOT NULL CHECK(dispatch_state IN (0,1,2)),
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
