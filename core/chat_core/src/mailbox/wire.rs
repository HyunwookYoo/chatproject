//! Frame kind 18: own-device sync requests and replies.
//!
//! Every inner body is canonical CBOR `[1, op, ...]` and at most `MAX_INNER` (61,434 B), so the
//! complete account envelope `auth::envelope(18, inner)` stays within the 61,440 B frame limit.
//! Requests use ops 0-4 and replies ops 8-13 so a request can never be read as a reply. Decoding
//! follows `mls::wire`: the size is checked before anything is parsed, byte strings and text are
//! borrowed (never allocated from a length claim) until their length is checked, array lengths
//! are bounded before any element is read, and the result must re-encode to exactly the input.
use super::PAGE_MAX_ENTRIES;
use crate::account::{InstallationId, PeerId};
use crate::message::types::{ConversationId, MessageId};
use crate::mls::wire::MAX_INNER;
use crate::{CoreError, ErrorCode};
use minicbor::{Decoder, Encoder};
use zeroize::Zeroizing;

type R<T> = Result<T, CoreError>;

const VERSION: u64 = 1;

/// Largest `PlainRecord.body`.
pub(crate) const MAX_PLAIN_BODY: usize = 16_384;
/// `PlainRecord.kind` of a text message.
const PLAIN_TEXT: u64 = 1;

/// Bytes of a `SyncResp` carrying one entry that are not the entry itself, at worst:
/// `[1, 8, [entry], next_seq, more]` is 3 (array, version, op) + 1 (entries head) + 9 (`next_seq`
/// up to `i64::MAX`) + 1 (`more`).
const SYNC_RESP_FIXED: usize = 3 + 1 + 9 + 1;
/// Bytes of a `Frame` entry that are not its body, at worst: `[seq, conversation, peer, install,
/// recorded_at, [1, body]]` is 1 + 9 + 34 + 34 + 34 + 9 + 1 (body array) + 1 (tag) + 3 (bstr head
/// for 256..=65,535 B).
const ENTRY_FIXED: usize = 1 + 9 + 34 + 34 + 34 + 9 + 1 + 1 + 3;
/// Largest `EntryBody::Frame` body for which one `LogEntry` still encodes inside one `SyncResp`
/// page within `MAX_INNER`. `record` refuses larger bodies.
pub(crate) const MAX_ENTRY_BODY: usize = MAX_INNER - SYNC_RESP_FIXED - ENTRY_FIXED;

const OP_SYNC_REQ: u64 = 0;
const OP_NOTIFY: u64 = 1;
const OP_PLAIN_REQ: u64 = 2;
const OP_HANDOFF: u64 = 3;
const OP_ANNOUNCE: u64 = 4;
const OP_SYNC_RESP: u64 = 8;
const OP_OK: u64 = 9;
const OP_PLAIN: u64 = 10;
const OP_NO_PLAIN: u64 = 11;
const OP_BUSY: u64 = 12;
const OP_REFUSED: u64 = 13;

const BODY_FRAME: u64 = 1;
const BODY_DELIVERED: u64 = 2;
const BODY_HANDOFF_STOP: u64 = 3;

/// One log record of a mailbox.
#[derive(Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub seq: u64,
    pub conversation: ConversationId,
    pub sender_peer: PeerId,
    pub sender_install: InstallationId,
    pub recorded_at_ms: u64,
    pub body: EntryBody,
}

#[derive(Clone, PartialEq, Eq)]
pub enum EntryBody {
    /// A kind 17 inner body.
    Frame(Vec<u8>),
    Delivered {
        msg_id: MessageId,
        receiver: InstallationId,
        at_ms: u64,
    },
    HandoffStop {
        msg_id: MessageId,
        class: u8,
        epoch: u64,
    },
}

/// A decrypted message a mailbox hands back to an own device.
#[derive(Clone, PartialEq, Eq)]
pub struct PlainRecord {
    pub conversation: ConversationId,
    pub sender_peer: PeerId,
    pub sender_install: InstallationId,
    pub msg_id: MessageId,
    pub lamport: u64,
    pub prev_hash: [u8; 32],
    pub sent_at_ms: u64,
    /// 1 is Text.
    pub kind: u8,
    /// At most `MAX_PLAIN_BODY` bytes.
    pub body: String,
}

/// A ciphertext an own device passes to a mailbox for delivery.
#[derive(Clone, PartialEq, Eq)]
pub struct HandoffEntry {
    pub conversation: ConversationId,
    pub peer: PeerId,
    pub msg_id: MessageId,
    pub ciphertext: Vec<u8>,
}

/// A request sent to another own device.
#[derive(Clone, PartialEq, Eq)]
pub enum MailboxFrame {
    SyncReq { since_seq: u64 },
    Notify { head_seq: u64 },
    PlainReq { seq: u64 },
    /// One to `PAGE_MAX_ENTRIES` entries.
    Handoff { entries: Vec<HandoffEntry> },
    Announce { mailbox: bool },
}

/// The answer to a [`MailboxFrame`].
#[derive(Clone, PartialEq, Eq)]
pub enum MailboxReply {
    SyncResp {
        entries: Vec<LogEntry>,
        next_seq: u64,
        more: bool,
    },
    Ok,
    Plain(PlainRecord),
    NoPlain,
    Busy,
    /// The peer is not (or no longer) one of the account's devices.
    Refused,
}

fn invalid() -> CoreError {
    CoreError::new(ErrorCode::InvalidMessage, "invalid_mailbox_frame")
}
fn unsupported() -> CoreError {
    CoreError::new(
        ErrorCode::UnsupportedMessageVersion,
        "unsupported_message_version",
    )
}
/// An integer that fits an SQL INTEGER column.
fn int(d: &mut Decoder<'_>) -> R<u64> {
    let n = d.u64().map_err(|_| invalid())?;
    if n > i64::MAX as u64 {
        return Err(invalid());
    }
    Ok(n)
}
fn fixed<const N: usize>(d: &mut Decoder<'_>) -> R<[u8; N]> {
    d.bytes()
        .map_err(|_| invalid())?
        .try_into()
        .map_err(|_| invalid())
}
/// A non-empty byte string of at most `max` bytes, checked on the borrowed slice.
fn blob(d: &mut Decoder<'_>, max: usize) -> R<Vec<u8>> {
    let b = d.bytes().map_err(|_| invalid())?;
    if b.is_empty() || b.len() > max {
        return Err(invalid());
    }
    Ok(b.to_vec())
}
/// A definite array head of at most `max` items.
fn array(d: &mut Decoder<'_>, max: usize) -> R<usize> {
    let n = d.array().map_err(|_| invalid())?.ok_or_else(invalid)?;
    if n > max as u64 {
        return Err(invalid());
    }
    Ok(n as usize)
}
fn conversation(d: &mut Decoder<'_>) -> R<ConversationId> {
    Ok(ConversationId(fixed(d)?))
}
fn peer(d: &mut Decoder<'_>) -> R<PeerId> {
    Ok(PeerId(fixed(d)?))
}
fn install(d: &mut Decoder<'_>) -> R<InstallationId> {
    Ok(InstallationId(fixed(d)?))
}
fn msg_id(d: &mut Decoder<'_>) -> R<MessageId> {
    Ok(MessageId(fixed(d)?))
}

/// Starts an encoder at `[1, op, ...]` with `fields` array items in total.
fn start(fields: u64, op: u64) -> Encoder<Vec<u8>> {
    let mut e = Encoder::new(Vec::new());
    e.array(fields)
        .unwrap()
        .u64(VERSION)
        .unwrap()
        .u64(op)
        .unwrap();
    e
}
fn finish(e: Encoder<Vec<u8>>) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(e.into_writer())
}
/// Checks size, array head, version and op before any field is read.
fn open(inner: &[u8]) -> R<(Decoder<'_>, u64, u64)> {
    if inner.len() > MAX_INNER {
        return Err(invalid());
    }
    let mut d = Decoder::new(inner);
    let fields = d.array().map_err(|_| invalid())?.ok_or_else(invalid)?;
    if d.u64().map_err(|_| invalid())? != VERSION {
        return Err(unsupported());
    }
    let op = d.u64().map_err(|_| invalid())?;
    Ok((d, fields, op))
}

fn encode_entry(e: &mut Encoder<Vec<u8>>, entry: &LogEntry) {
    e.array(6)
        .unwrap()
        .u64(entry.seq)
        .unwrap()
        .bytes(&entry.conversation.0)
        .unwrap()
        .bytes(&entry.sender_peer.0)
        .unwrap()
        .bytes(&entry.sender_install.0)
        .unwrap()
        .u64(entry.recorded_at_ms)
        .unwrap();
    encode_body(e, &entry.body);
}
fn encode_body(e: &mut Encoder<Vec<u8>>, body: &EntryBody) {
    match body {
        EntryBody::Frame(inner) => {
            e.array(2)
                .unwrap()
                .u64(BODY_FRAME)
                .unwrap()
                .bytes(inner)
                .unwrap();
        }
        EntryBody::Delivered {
            msg_id,
            receiver,
            at_ms,
        } => {
            e.array(4)
                .unwrap()
                .u64(BODY_DELIVERED)
                .unwrap()
                .bytes(&msg_id.0)
                .unwrap()
                .bytes(&receiver.0)
                .unwrap()
                .u64(*at_ms)
                .unwrap();
        }
        EntryBody::HandoffStop {
            msg_id,
            class,
            epoch,
        } => {
            e.array(4)
                .unwrap()
                .u64(BODY_HANDOFF_STOP)
                .unwrap()
                .bytes(&msg_id.0)
                .unwrap()
                .u8(*class)
                .unwrap()
                .u64(*epoch)
                .unwrap();
        }
    }
}
fn decode_entry(d: &mut Decoder<'_>) -> R<LogEntry> {
    if array(d, 6)? != 6 {
        return Err(invalid());
    }
    let seq = int(d)?;
    let conversation = conversation(d)?;
    let sender_peer = peer(d)?;
    let sender_install = install(d)?;
    let recorded_at_ms = int(d)?;
    let body = decode_body(d)?;
    Ok(LogEntry {
        seq,
        conversation,
        sender_peer,
        sender_install,
        recorded_at_ms,
        body,
    })
}
fn decode_body(d: &mut Decoder<'_>) -> R<EntryBody> {
    let items = array(d, 4)?;
    Ok(match (d.u64().map_err(|_| invalid())?, items) {
        (BODY_FRAME, 2) => EntryBody::Frame(blob(d, MAX_ENTRY_BODY)?),
        (BODY_DELIVERED, 4) => EntryBody::Delivered {
            msg_id: msg_id(d)?,
            receiver: install(d)?,
            at_ms: int(d)?,
        },
        (BODY_HANDOFF_STOP, 4) => EntryBody::HandoffStop {
            msg_id: msg_id(d)?,
            class: u8::try_from(d.u64().map_err(|_| invalid())?).map_err(|_| invalid())?,
            epoch: int(d)?,
        },
        _ => return Err(invalid()),
    })
}

/// The `mailbox_store.body` of a note (kind 2 `Delivered`, kind 3 `HandoffStop`): the canonical
/// CBOR of its `EntryBody` exactly as a `SyncResp` entry carries it, `[tag, ...]` with the tag
/// equal to the row's kind. `None` for a `Frame`, whose row body is the kind 17 inner itself.
pub(crate) fn note_body(body: &EntryBody) -> Option<Vec<u8>> {
    if matches!(body, EntryBody::Frame(_)) {
        return None;
    }
    let mut e = Encoder::new(Vec::new());
    encode_body(&mut e, body);
    Some(e.into_writer())
}

/// Reads back a note row: `kind` must be its tag and `bytes` exactly its canonical encoding.
pub(crate) fn decode_note(kind: u64, bytes: &[u8]) -> R<EntryBody> {
    if kind == BODY_FRAME {
        return Err(invalid());
    }
    let mut d = Decoder::new(bytes);
    let body = decode_body(&mut d)?;
    let tag = match body {
        EntryBody::Frame(_) => BODY_FRAME,
        EntryBody::Delivered { .. } => BODY_DELIVERED,
        EntryBody::HandoffStop { .. } => BODY_HANDOFF_STOP,
    };
    if tag != kind || d.position() != bytes.len() || note_body(&body).as_deref() != Some(bytes) {
        return Err(invalid());
    }
    Ok(body)
}

impl MailboxFrame {
    /// The canonical CBOR body, without the account envelope.
    pub fn inner(&self) -> Zeroizing<Vec<u8>> {
        let mut e;
        match self {
            Self::SyncReq { since_seq } => {
                e = start(3, OP_SYNC_REQ);
                e.u64(*since_seq).unwrap();
            }
            Self::Notify { head_seq } => {
                e = start(3, OP_NOTIFY);
                e.u64(*head_seq).unwrap();
            }
            Self::PlainReq { seq } => {
                e = start(3, OP_PLAIN_REQ);
                e.u64(*seq).unwrap();
            }
            Self::Handoff { entries } => {
                e = start(3, OP_HANDOFF);
                e.array(entries.len() as u64).unwrap();
                for entry in entries {
                    e.array(4)
                        .unwrap()
                        .bytes(&entry.conversation.0)
                        .unwrap()
                        .bytes(&entry.peer.0)
                        .unwrap()
                        .bytes(&entry.msg_id.0)
                        .unwrap()
                        .bytes(&entry.ciphertext)
                        .unwrap();
                }
            }
            Self::Announce { mailbox } => {
                e = start(3, OP_ANNOUNCE);
                e.bool(*mailbox).unwrap();
            }
        }
        finish(e)
    }
    /// The complete kind 18 account envelope.
    pub fn encode(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(crate::account::auth::envelope(super::KIND, &self.inner()))
    }
    pub fn decode(inner: &[u8]) -> R<Self> {
        let (mut d, fields, op) = open(inner)?;
        if fields != 3 {
            return Err(invalid());
        }
        let frame = match op {
            OP_SYNC_REQ => Self::SyncReq {
                since_seq: int(&mut d)?,
            },
            OP_NOTIFY => Self::Notify {
                head_seq: int(&mut d)?,
            },
            OP_PLAIN_REQ => Self::PlainReq { seq: int(&mut d)? },
            OP_HANDOFF => {
                let n = array(&mut d, PAGE_MAX_ENTRIES)?;
                if n == 0 {
                    return Err(invalid());
                }
                let mut entries = Vec::with_capacity(n);
                for _ in 0..n {
                    if array(&mut d, 4)? != 4 {
                        return Err(invalid());
                    }
                    entries.push(HandoffEntry {
                        conversation: conversation(&mut d)?,
                        peer: peer(&mut d)?,
                        msg_id: msg_id(&mut d)?,
                        ciphertext: blob(&mut d, MAX_INNER)?,
                    });
                }
                Self::Handoff { entries }
            }
            OP_ANNOUNCE => Self::Announce {
                mailbox: d.bool().map_err(|_| invalid())?,
            },
            _ => return Err(invalid()),
        };
        if d.position() != inner.len() || *frame.inner() != *inner {
            return Err(invalid());
        }
        Ok(frame)
    }
}

impl MailboxReply {
    /// The canonical CBOR body, without the account envelope.
    pub fn inner(&self) -> Zeroizing<Vec<u8>> {
        let mut e;
        match self {
            Self::SyncResp {
                entries,
                next_seq,
                more,
            } => {
                e = start(5, OP_SYNC_RESP);
                e.array(entries.len() as u64).unwrap();
                for entry in entries {
                    encode_entry(&mut e, entry);
                }
                e.u64(*next_seq).unwrap().bool(*more).unwrap();
            }
            Self::Ok => e = start(2, OP_OK),
            Self::Plain(record) => {
                e = start(11, OP_PLAIN);
                e.bytes(&record.conversation.0)
                    .unwrap()
                    .bytes(&record.sender_peer.0)
                    .unwrap()
                    .bytes(&record.sender_install.0)
                    .unwrap()
                    .bytes(&record.msg_id.0)
                    .unwrap()
                    .u64(record.lamport)
                    .unwrap()
                    .bytes(&record.prev_hash)
                    .unwrap()
                    .u64(record.sent_at_ms)
                    .unwrap()
                    .u8(record.kind)
                    .unwrap()
                    .str(&record.body)
                    .unwrap();
            }
            Self::NoPlain => e = start(2, OP_NO_PLAIN),
            Self::Busy => e = start(2, OP_BUSY),
            Self::Refused => e = start(2, OP_REFUSED),
        }
        finish(e)
    }
    /// The complete kind 18 account envelope.
    pub fn encode(&self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(crate::account::auth::envelope(super::KIND, &self.inner()))
    }
    pub fn decode(inner: &[u8]) -> R<Self> {
        let (mut d, fields, op) = open(inner)?;
        let reply = match (op, fields) {
            (OP_SYNC_RESP, 5) => {
                let n = array(&mut d, PAGE_MAX_ENTRIES)?;
                let mut entries = Vec::with_capacity(n);
                for _ in 0..n {
                    entries.push(decode_entry(&mut d)?);
                }
                Self::SyncResp {
                    entries,
                    next_seq: int(&mut d)?,
                    more: d.bool().map_err(|_| invalid())?,
                }
            }
            (OP_OK, 2) => Self::Ok,
            (OP_PLAIN, 11) => {
                let conversation = conversation(&mut d)?;
                let sender_peer = peer(&mut d)?;
                let sender_install = install(&mut d)?;
                let msg_id = msg_id(&mut d)?;
                let lamport = int(&mut d)?;
                let prev_hash = fixed(&mut d)?;
                let sent_at_ms = int(&mut d)?;
                if d.u64().map_err(|_| invalid())? != PLAIN_TEXT {
                    return Err(invalid());
                }
                let body = d.str().map_err(|_| invalid())?;
                if body.len() > MAX_PLAIN_BODY {
                    return Err(invalid());
                }
                Self::Plain(PlainRecord {
                    conversation,
                    sender_peer,
                    sender_install,
                    msg_id,
                    lamport,
                    prev_hash,
                    sent_at_ms,
                    kind: PLAIN_TEXT as u8,
                    body: body.to_owned(),
                })
            }
            (OP_NO_PLAIN, 2) => Self::NoPlain,
            (OP_BUSY, 2) => Self::Busy,
            (OP_REFUSED, 2) => Self::Refused,
            _ => return Err(invalid()),
        };
        if d.position() != inner.len() || *reply.inner() != *inner {
            return Err(invalid());
        }
        Ok(reply)
    }
}

/// Encoded size of `entry` inside a `SyncResp`.
fn entry_len(entry: &LogEntry) -> usize {
    let mut e = Encoder::new(Vec::new());
    encode_entry(&mut e, entry);
    e.into_writer().len()
}

/// Takes entries (in `seq` order) while the `SyncResp` that carries them stays within
/// `MAX_INNER` and `PAGE_MAX_ENTRIES`, always at least one. The flag says whether the iterator
/// still had an entry that did not fit. Sizes assume the worst-case `next_seq` and `more`, so the
/// real reply is never larger than measured here.
pub(crate) fn page(entries: impl Iterator<Item = LogEntry>) -> (Vec<LogEntry>, bool) {
    let mut entries = entries.peekable();
    let mut page = Vec::new();
    // `[1, 8, entries, next_seq, more]` without the entries array (head included).
    let fixed = SYNC_RESP_FIXED - 1;
    let mut body = 0;
    while let Some(next) = entries.peek() {
        let size = entry_len(next);
        let count = page.len() + 1;
        let head = if count < 24 { 1 } else { 2 };
        if !page.is_empty() && (count > PAGE_MAX_ENTRIES || fixed + head + body + size > MAX_INNER)
        {
            return (page, true);
        }
        body += size;
        page.extend(entries.next());
    }
    (page, false)
}

/// Splits `entries` (in order) into the entry lists of `Handoff` frames, each within
/// `MAX_INNER` and `PAGE_MAX_ENTRIES`. An entry too large for a frame of its own is left out.
pub(crate) fn handoff_batches(entries: Vec<HandoffEntry>) -> Vec<Vec<HandoffEntry>> {
    // `[1, 3, entries]` without the entries array head.
    const FIXED: usize = 3;
    let mut batches = Vec::new();
    let (mut batch, mut body) = (Vec::new(), 0);
    for entry in entries {
        let size = {
            let mut e = Encoder::new(Vec::new());
            e.array(4)
                .unwrap()
                .bytes(&entry.conversation.0)
                .unwrap()
                .bytes(&entry.peer.0)
                .unwrap()
                .bytes(&entry.msg_id.0)
                .unwrap()
                .bytes(&entry.ciphertext)
                .unwrap();
            e.into_writer().len()
        };
        let fits = |count: usize, body: usize| {
            let head = if count < 24 { 1 } else { 2 };
            count <= PAGE_MAX_ENTRIES && FIXED + head + body <= MAX_INNER
        };
        if !fits(1, size) {
            continue;
        }
        if !fits(batch.len() + 1, body + size) {
            batches.push(std::mem::take(&mut batch));
            body = 0;
        }
        body += size;
        batch.push(entry);
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_batches_fit_one_frame_each_and_keep_order() {
        let entry = |n: u8, len: usize| HandoffEntry {
            conversation: ConversationId([n; 32]),
            peer: PeerId([2; 32]),
            msg_id: MessageId([n; 16]),
            ciphertext: vec![n; len],
        };
        // Two large ciphertexts cannot share a frame, one too large for any frame is left out,
        // and no frame carries more than 64 entries.
        let mut entries = vec![entry(1, 40_000), entry(2, 40_000), entry(3, MAX_INNER)];
        entries.extend((10..80).map(|n| entry(n, 100)));
        let batches = handoff_batches(entries);
        let sizes: Vec<usize> = batches.iter().map(Vec::len).collect();
        assert_eq!(sizes, [1, 64, 7]);
        let order: Vec<u8> = batches.iter().flatten().map(|e| e.msg_id.0[0]).collect();
        assert_eq!(order, [1, 2].into_iter().chain(10..80).collect::<Vec<_>>());
        for entries in batches {
            let frame = MailboxFrame::Handoff { entries };
            let inner = frame.inner();
            assert!(inner.len() <= MAX_INNER);
            assert!(MailboxFrame::decode(&inner).unwrap() == frame);
        }
    }

    #[test]
    fn note_bodies_round_trip_under_their_kind_only() {
        let delivered = EntryBody::Delivered {
            msg_id: MessageId([7; 16]),
            receiver: InstallationId([9; 32]),
            at_ms: 1_800_000_000_000,
        };
        let bytes = note_body(&delivered).unwrap();
        assert_eq!(bytes[..2], [0x84, 0x02], "[2, msg_id, receiver, at_ms]");
        assert!(decode_note(2, &bytes).unwrap() == delivered);
        // The tag is the row's kind: the same bytes are no other kind.
        assert!(decode_note(3, &bytes).is_err());
        assert!(decode_note(1, &bytes).is_err());
        // Trailing bytes, or anything not canonical, are refused.
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(decode_note(2, &longer).is_err());
        assert!(decode_note(2, &bytes[..bytes.len() - 1]).is_err());
        let stop = EntryBody::HandoffStop {
            msg_id: MessageId([7; 16]),
            class: 2,
            epoch: 5,
        };
        assert!(decode_note(3, &note_body(&stop).unwrap()).unwrap() == stop);
        assert!(note_body(&EntryBody::Frame(vec![1])).is_none());
    }
}
