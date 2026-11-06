#![cfg(all(feature = "test-hooks", debug_assertions))]
use chat_core::{
    ErrorCode,
    account::{InstallationId, PeerId},
    storage_fixture::{
        ConversationId, EntryBody, HandoffEntry, LogEntry, MAILBOX_MAX_ENTRY_BODY, MailboxFrame,
        MailboxReply, MessageId, PlainRecord, mailbox_page,
    },
};

fn hex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks_exact(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}
fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/m6-wire-vectors.json")).unwrap()
}
fn arr<const N: usize>(v: &serde_json::Value) -> [u8; N] {
    hex(v.as_str().unwrap()).try_into().unwrap()
}

const CONV: ConversationId = ConversationId([0x11; 32]);
const INSTALL: InstallationId = InstallationId([0x22; 32]);
const PEER: PeerId = PeerId([0x33; 32]);
const MSG: MessageId = MessageId([0x44; 16]);
const RECV: InstallationId = InstallationId([0x55; 32]);

fn entry(seq: u64, recorded_at_ms: u64, body: EntryBody) -> LogEntry {
    LogEntry {
        seq,
        conversation: CONV,
        sender_peer: PEER,
        sender_install: INSTALL,
        recorded_at_ms,
        body,
    }
}
fn frame_entry(seq: u64, len: usize) -> LogEntry {
    entry(seq, 1, EntryBody::Frame(vec![0xa5; len]))
}
fn handoff(ciphertext: Vec<u8>) -> HandoffEntry {
    HandoffEntry {
        conversation: CONV,
        peer: PEER,
        msg_id: MSG,
        ciphertext,
    }
}
fn plain(body: String) -> PlainRecord {
    PlainRecord {
        conversation: CONV,
        sender_peer: PEER,
        sender_install: INSTALL,
        msg_id: MSG,
        lamport: 4,
        prev_hash: std::array::from_fn(|i| i as u8),
        sent_at_ms: 1_700_000_004_000,
        kind: 1,
        body,
    }
}
#[test]
fn independent_vectors_for_requests_and_replies() {
    let root = fixture();
    assert_eq!(arr::<32>(&root["conversation"]), CONV.0);
    assert_eq!(arr::<32>(&root["installation"]), INSTALL.0);
    assert_eq!(arr::<32>(&root["peer"]), PEER.0);
    assert_eq!(arr::<16>(&root["msg_id"]), MSG.0);
    assert_eq!(arr::<32>(&root["receiver"]), RECV.0);
    let hash: [u8; 32] = arr(&root["hash"]);
    let ct = hex(root["ciphertext"].as_str().unwrap());
    let frame_inner = hex(root["frame_inner"].as_str().unwrap());
    let mut seen = 0;
    for v in root["vectors"].as_array().unwrap() {
        let name = v["name"].as_str().unwrap();
        let inner = hex(v["inner"].as_str().unwrap());
        let wire = hex(v["wire"].as_str().unwrap());
        if v["kind"] == "frame" {
            let frame = match name {
                "sync_req" => MailboxFrame::SyncReq { since_seq: 5 },
                "sync_req_large" => MailboxFrame::SyncReq { since_seq: 1 << 40 },
                "notify" => MailboxFrame::Notify { head_seq: 12 },
                "plain_req" => MailboxFrame::PlainReq { seq: 9 },
                "handoff" => MailboxFrame::Handoff {
                    entries: vec![handoff(ct.clone()), handoff(ct.clone())],
                },
                "announce_true" => MailboxFrame::Announce { mailbox: true },
                "announce_false" => MailboxFrame::Announce { mailbox: false },
                other => panic!("{other}"),
            };
            assert!(*frame.inner() == inner, "inner: {name}");
            assert!(*frame.encode() == wire, "wire: {name}");
            assert!(MailboxFrame::decode(&inner).unwrap() == frame, "decode: {name}");
        } else {
            let reply = match name {
                "sync_resp" => MailboxReply::SyncResp {
                    entries: vec![
                        entry(7, 1_700_000_000_000, EntryBody::Frame(frame_inner.clone())),
                        entry(
                            8,
                            1_700_000_001_000,
                            EntryBody::Delivered {
                                msg_id: MSG,
                                receiver: RECV,
                                at_ms: 1_700_000_002_000,
                            },
                        ),
                        entry(
                            9,
                            1_700_000_003_000,
                            EntryBody::HandoffStop {
                                msg_id: MSG,
                                class: 2,
                                epoch: 3,
                            },
                        ),
                    ],
                    next_seq: 10,
                    more: true,
                },
                "sync_resp_empty" => MailboxReply::SyncResp {
                    entries: vec![],
                    next_seq: 7,
                    more: false,
                },
                "ok" => MailboxReply::Ok,
                "plain" => {
                    let mut record = plain("hello \u{e9}".into());
                    record.prev_hash = hash;
                    MailboxReply::Plain(record)
                }
                "no_plain" => MailboxReply::NoPlain,
                "busy" => MailboxReply::Busy,
                "refused" => MailboxReply::Refused,
                other => panic!("{other}"),
            };
            assert!(*reply.inner() == inner, "inner: {name}");
            assert!(*reply.encode() == wire, "wire: {name}");
            assert!(MailboxReply::decode(&inner).unwrap() == reply, "decode: {name}");
        }
        seen += 1;
    }
    assert_eq!(seen, 14);
}

#[test]
fn requests_and_replies_do_not_cross_decode() {
    for v in fixture()["vectors"].as_array().unwrap() {
        let inner = hex(v["inner"].as_str().unwrap());
        if v["kind"] == "frame" {
            assert!(MailboxReply::decode(&inner).is_err(), "{}", v["name"]);
        } else {
            assert!(MailboxFrame::decode(&inner).is_err(), "{}", v["name"]);
        }
    }
}

#[test]
fn non_canonical_rejected() {
    // Non-minimal integer head, indefinite array, trailing byte.
    for bad in [
        &[0x83, 0x01, 0x18, 0x00, 0x05][..],
        &[0x9f, 0x01, 0x00, 0x05, 0xff][..],
        &[0x83, 0x01, 0x00, 0x05, 0x00][..],
        &[0x83, 0x01, 0x00, 0x18, 0x05][..],
    ] {
        assert!(MailboxFrame::decode(bad).is_err(), "{bad:02x?}");
    }
    // Wrong field count for a known op, and a seq above i64::MAX.
    assert!(MailboxFrame::decode(&[0x84, 1, 0, 5, 5]).is_err());
    let mut over = vec![0x83, 1, 0, 0x1b];
    over.extend_from_slice(&u64::MAX.to_be_bytes());
    assert!(MailboxFrame::decode(&over).is_err());
    // An empty Handoff.
    assert!(MailboxFrame::decode(&[0x83, 1, 3, 0x80]).is_err());
    // Non-text kind in a PlainRecord.
    let mut record = plain("x".into());
    record.kind = 2;
    assert!(MailboxReply::decode(&MailboxReply::Plain(record).inner()).is_err());
}

#[test]
fn unknown_op_rejected() {
    for op in [5u8, 6, 7, 14, 15, 23] {
        assert!(MailboxFrame::decode(&[0x82, 1, op]).is_err(), "{op}");
        assert!(MailboxReply::decode(&[0x82, 1, op]).is_err(), "{op}");
    }
}

#[test]
fn version_2_is_unsupported() {
    let e = MailboxFrame::decode(&[0x83, 2, 0, 5]).err().unwrap();
    assert_eq!(e.code, ErrorCode::UnsupportedMessageVersion);
    let e = MailboxReply::decode(&[0x82, 2, 9]).err().unwrap();
    assert_eq!(e.code, ErrorCode::UnsupportedMessageVersion);
}

#[test]
fn oversized_fields_rejected_before_parse() {
    // PlainRecord body 16,385 B (16,384 B is accepted).
    let ok = MailboxReply::Plain(plain("a".repeat(16_384))).inner();
    assert!(MailboxReply::decode(&ok).is_ok());
    let big = MailboxReply::Plain(plain("a".repeat(16_385))).inner();
    assert!(big.len() <= 61_434);
    assert!(MailboxReply::decode(&big).is_err());
    // Handoff ciphertext 61,435 B.
    let big = MailboxFrame::Handoff {
        entries: vec![handoff(vec![7; 61_435])],
    }
    .inner();
    assert!(big.len() > 61_434);
    assert!(MailboxFrame::decode(&big).is_err());
    // Frame body above MAX_ENTRY_BODY, though the whole inner is small enough.
    let big = MailboxReply::SyncResp {
        entries: vec![frame_entry(1, MAILBOX_MAX_ENTRY_BODY + 1)],
        next_seq: 2,
        more: false,
    }
    .inner();
    assert!(big.len() <= 61_434);
    assert!(MailboxReply::decode(&big).is_err());
    // Claimed array lengths far beyond the input, and beyond a page.
    for len_head in [
        &[0x9b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff][..],
        &[0x9a, 0xff, 0xff, 0xff, 0xff][..],
        &[0x98, 65][..],
    ] {
        let mut handoff = vec![0x83, 1, 3];
        handoff.extend_from_slice(len_head);
        assert!(MailboxFrame::decode(&handoff).is_err());
        let mut resp = vec![0x85, 1, 8];
        resp.extend_from_slice(len_head);
        assert!(MailboxReply::decode(&resp).is_err());
    }
    // A byte string claiming more than the input holds.
    let claim = [0x83, 1, 3, 0x81, 0x84, 0x58, 0x20, 0x11];
    assert!(MailboxFrame::decode(&claim).is_err());
    // A whole frame is never read past MAX_INNER.
    assert!(MailboxFrame::decode(&vec![0x83; 61_435]).is_err());
}

#[test]
fn max_entry_body_is_the_exact_page_limit() {
    // The worst case for every header size: seq, next_seq and times at i64::MAX.
    let mut worst = frame_entry(i64::MAX as u64, MAILBOX_MAX_ENTRY_BODY);
    worst.recorded_at_ms = i64::MAX as u64;
    let inner = MailboxReply::SyncResp {
        entries: vec![worst],
        next_seq: i64::MAX as u64,
        more: true,
    }
    .inner();
    assert_eq!(inner.len(), 61_434);
    assert!(MailboxReply::decode(&inner).is_ok());
}

#[test]
fn page_never_exceeds_max_inner_and_holds_at_least_one() {
    // A single entry that does not fit anything else is still returned.
    let (page, more) = mailbox_page(vec![frame_entry(1, MAILBOX_MAX_ENTRY_BODY)].into_iter());
    assert_eq!((page.len(), more), (1, false));

    // Two large entries: the second does not fit.
    let (page, more) = mailbox_page(
        vec![
            frame_entry(1, 40_000),
            frame_entry(2, 40_000),
            frame_entry(3, 1),
        ]
        .into_iter(),
    );
    assert_eq!((page.len(), more), (1, true));

    // Many small entries stop at the entry cap, with `more` set only when one is left.
    let small = |n: u64| (1..=n).map(|seq| frame_entry(seq, 10));
    let (page, more) = mailbox_page(small(64));
    assert_eq!((page.len(), more), (64, false));
    let (page, more) = mailbox_page(small(65));
    assert_eq!((page.len(), more), (64, true));
    let (page, more) = mailbox_page(small(0));
    assert_eq!((page.len(), more), (0, false));

    // Sweep entry sizes: a page always encodes within MAX_INNER and takes the maximum.
    for len in [1, 200, 255, 256, 900, 1_000, 5_000, 20_000, 30_000, 30_700] {
        let all: Vec<_> = (1..=70).map(|seq| frame_entry(i64::MAX as u64 - seq, len)).collect();
        let (page, more) = mailbox_page(all.clone().into_iter());
        assert!(!page.is_empty() && page.len() <= 64, "{len}");
        let inner = MailboxReply::SyncResp {
            entries: page.clone(),
            next_seq: i64::MAX as u64,
            more,
        }
        .inner();
        assert!(inner.len() <= 61_434, "{len}: {} B", inner.len());
        assert!(page == all[..page.len()], "{len}");
        assert_eq!(more, page.len() < 70, "{len}");
        // One more entry would not have fit (or the cap was hit).
        if page.len() < all.len() && page.len() < 64 {
            let mut longer = page.clone();
            longer.push(all[page.len()].clone());
            let size = MailboxReply::SyncResp {
                entries: longer,
                next_seq: i64::MAX as u64,
                more,
            }
            .inner()
            .len();
            assert!(size > 61_434, "{len}: {size} B");
        }
    }
}
