#![cfg(all(feature = "test-hooks", debug_assertions))]
use chat_core::{
    ErrorCode,
    account::InstallationId,
    storage_fixture::{ConversationId, MlsFrame, MlsReply},
};

fn hex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks_exact(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}
fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/m5-wire-vectors.json")).unwrap()
}
fn array32(v: &serde_json::Value) -> [u8; 32] {
    hex(v.as_str().unwrap()).try_into().unwrap()
}
fn field(v: &serde_json::Value, name: &str) -> Vec<u8> {
    hex(v["fields"][name].as_str().unwrap())
}
fn frame_of(root: &serde_json::Value, v: &serde_json::Value) -> MlsFrame {
    let conversation = ConversationId(array32(&root["conversation"]));
    match v["op"].as_u64().unwrap() {
        0 => MlsFrame::App {
            conversation,
            ciphertext: field(v, "ciphertext"),
        },
        1 => MlsFrame::GroupQuery { conversation },
        2 => MlsFrame::KeyPackageRequest,
        3 => MlsFrame::JoinRequest {
            conversation,
            key_package: field(v, "key_package"),
        },
        4 => MlsFrame::Commit {
            conversation,
            epoch: v["fields"]["epoch"].as_u64().unwrap(),
            commit: field(v, "commit"),
        },
        5 => MlsFrame::Welcome {
            conversation,
            welcome: field(v, "welcome"),
            sender_install: InstallationId(array32(&v["fields"]["sender_install"])),
        },
        op => panic!("unexpected frame op {op}"),
    }
}
fn reply_of(v: &serde_json::Value) -> MlsReply {
    let f = &v["fields"];
    match v["op"].as_u64().unwrap() {
        8 => MlsReply::Ack {
            ct_hash: array32(&f["ct_hash"]),
        },
        9 => MlsReply::DecryptFailed {
            class: f["class"].as_u64().unwrap() as u8,
            epoch: f["epoch"].as_u64().unwrap(),
        },
        10 if f["exists"].as_bool().unwrap() => MlsReply::GroupInfo {
            exists: true,
            epoch: f["epoch"].as_u64().unwrap(),
            first_commit_hash: array32(&f["first_commit_hash"]),
        },
        10 => MlsReply::GroupInfo {
            exists: false,
            epoch: 0,
            first_commit_hash: [0; 32],
        },
        11 => MlsReply::KeyPackage(field(v, "key_package")),
        12 => MlsReply::Accepted,
        13 => MlsReply::Busy,
        14 => MlsReply::ContactRequired,
        op => panic!("unexpected reply op {op}"),
    }
}

// Independent CBOR pieces for hand-built malformed input.
fn head(major: u8, n: u64) -> Vec<u8> {
    let m = major << 5;
    match n {
        0..=23 => vec![m | n as u8],
        24..=255 => vec![m | 24, n as u8],
        256..=65535 => [vec![m | 25], (n as u16).to_be_bytes().to_vec()].concat(),
        _ if n <= u32::MAX as u64 => [vec![m | 26], (n as u32).to_be_bytes().to_vec()].concat(),
        _ => [vec![m | 27], n.to_be_bytes().to_vec()].concat(),
    }
}
fn bstr(b: &[u8]) -> Vec<u8> {
    [head(2, b.len() as u64), b.to_vec()].concat()
}
fn arr(items: &[Vec<u8>]) -> Vec<u8> {
    [head(4, items.len() as u64), items.concat()].concat()
}
fn uint(n: u64) -> Vec<u8> {
    head(0, n)
}
const CONV: [u8; 32] = [0x11; 32];

fn app(ciphertext: &[u8]) -> Vec<u8> {
    arr(&[uint(1), uint(0), bstr(&CONV), bstr(ciphertext)])
}
fn is_invalid<T>(r: Result<T, chat_core::CoreError>) -> bool {
    matches!(r, Err(e) if e.code == ErrorCode::InvalidMessage)
}

#[test]
fn golden_frames_and_replies_match_independent_vectors() {
    let root = fixture();
    let mut seen = 0;
    for v in root["vectors"].as_array().unwrap() {
        let inner = hex(v["inner"].as_str().unwrap());
        let wire = hex(v["wire"].as_str().unwrap());
        let name = v["name"].as_str().unwrap();
        if v["kind"] == "frame" {
            let frame = frame_of(&root, v);
            assert!(*frame.inner() == inner, "inner mismatch: {name}");
            assert!(*frame.encode() == wire, "wire mismatch: {name}");
            assert!(MlsFrame::decode(&inner).unwrap() == frame, "decode: {name}");
        } else {
            let reply = reply_of(v);
            assert!(*reply.inner() == inner, "inner mismatch: {name}");
            assert!(*reply.encode() == wire, "wire mismatch: {name}");
            assert!(MlsReply::decode(&inner).unwrap() == reply, "decode: {name}");
        }
        seen += 1;
    }
    assert_eq!(seen, 29);
}

#[test]
fn oversized_mls_frames_rejected_before_parse() {
    let root = fixture();
    // The inner limit is 61,434 B so the whole envelope stays within 61,440 B.
    let max_ct = root["max_app_ciphertext_for_61434_inner"].as_u64().unwrap() as usize;
    let at_limit = app(&vec![7; max_ct]);
    assert_eq!(at_limit.len(), 61_434);
    assert!(MlsFrame::decode(&at_limit).is_ok());
    let over = app(&vec![7; max_ct + 1]);
    assert!(is_invalid(MlsFrame::decode(&over)));
    assert!(is_invalid(MlsFrame::decode(&vec![0; 61_435])));
    assert!(is_invalid(MlsReply::decode(&vec![0; 61_435])));

    // Per-field limits: KeyPackage 4,096 B in a JoinRequest and in the KeyPackage reply.
    for len in [4_096usize, 4_097] {
        let join = arr(&[uint(1), uint(3), bstr(&CONV), bstr(&vec![1; len])]);
        assert_eq!(MlsFrame::decode(&join).is_ok(), len == 4_096);
        let reply = arr(&[uint(1), uint(11), bstr(&vec![1; len])]);
        assert_eq!(MlsReply::decode(&reply).is_ok(), len == 4_096);
    }
    // Empty bodies are never valid.
    for empty in [
        app(&[]),
        arr(&[uint(1), uint(3), bstr(&CONV), bstr(&[])]),
        arr(&[uint(1), uint(4), bstr(&CONV), uint(1), bstr(&[])]),
        arr(&[uint(1), uint(5), bstr(&CONV), bstr(&[]), bstr(&[2; 32])]),
    ] {
        assert!(is_invalid(MlsFrame::decode(&empty)));
    }
    assert!(is_invalid(MlsReply::decode(&arr(&[
        uint(1),
        uint(11),
        bstr(&[])
    ]))));
    // Identifier lengths are exact.
    for len in [0usize, 31, 33] {
        let bad = arr(&[uint(1), uint(1), bstr(&vec![1; len])]);
        assert!(is_invalid(MlsFrame::decode(&bad)));
        let welcome = arr(&[
            uint(1),
            uint(5),
            bstr(&CONV),
            bstr(&[1]),
            bstr(&vec![2; len]),
        ]);
        assert!(is_invalid(MlsFrame::decode(&welcome)));
        assert!(is_invalid(MlsReply::decode(&arr(&[
            uint(1),
            uint(8),
            bstr(&vec![3; len])
        ]))));
    }
    // Huge CBOR length claims with no data behind them fail without being trusted.
    let claims: [Vec<u8>; 6] = [
        [
            vec![0x84, 1, 0],
            bstr(&CONV),
            vec![0x5b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        ]
        .concat(),
        [
            vec![0x84, 1, 0],
            bstr(&CONV),
            vec![0x5a, 0xff, 0xff, 0xff, 0xff],
        ]
        .concat(),
        [vec![0x84, 1, 0], bstr(&CONV), vec![0x59, 0xff, 0xff, 0x01]].concat(),
        vec![0x9b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        [vec![0x9a, 0xff, 0xff, 0xff, 0xff], uint(1), uint(0)].concat(),
        vec![0x5b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
    ];
    for claim in &claims {
        assert!(is_invalid(MlsFrame::decode(claim)));
        assert!(is_invalid(MlsReply::decode(claim)));
    }
    // Integers outside SQL INTEGER range are malformed.
    let big = i64::MAX as u64 + 1;
    assert!(is_invalid(MlsFrame::decode(&arr(&[
        uint(1),
        uint(4),
        bstr(&CONV),
        uint(big),
        bstr(&[1])
    ]))));
    assert!(is_invalid(MlsReply::decode(&arr(&[
        uint(1),
        uint(9),
        uint(1),
        uint(big)
    ]))));
}

#[test]
fn non_canonical_rejected() {
    let root = fixture();
    for v in root["vectors"].as_array().unwrap() {
        let inner = hex(v["inner"].as_str().unwrap());
        let name = v["name"].as_str().unwrap();
        let decode_ok = |bytes: &[u8]| {
            if v["kind"] == "frame" {
                MlsFrame::decode(bytes).is_ok()
            } else {
                MlsReply::decode(bytes).is_ok()
            }
        };
        assert!(decode_ok(&inner), "{name}");
        let mut trailing = inner.clone();
        trailing.push(0);
        assert!(!decode_ok(&trailing), "trailing: {name}");
        assert!(!decode_ok(&inner[..inner.len() - 1]), "truncated: {name}");
        // Non-minimal op head: 0x18 followed by the op instead of the one-byte form.
        let mut padded_op = inner.clone();
        padded_op.splice(2..3, [0x18, v["op"].as_u64().unwrap() as u8]);
        assert!(!decode_ok(&padded_op), "non-minimal op: {name}");
        // Indefinite-length array.
        let mut indefinite = vec![0x9f];
        indefinite.extend_from_slice(&inner[1..]);
        indefinite.push(0xff);
        assert!(!decode_ok(&indefinite), "indefinite array: {name}");
    }
    // Non-minimal integers and byte-string heads, and indefinite byte strings, in field positions.
    let noncanonical: [Vec<u8>; 4] = [
        arr(&[uint(1), uint(4), bstr(&CONV), vec![0x18, 5], bstr(&[1])]),
        arr(&[uint(1), uint(4), bstr(&CONV), vec![0x19, 0, 5], bstr(&[1])]),
        arr(&[uint(1), uint(0), bstr(&CONV), vec![0x5f, 0x41, 1, 0xff]]),
        [
            vec![0x84, 1, 0],
            vec![0x59, 0, 0x20],
            CONV.to_vec(),
            bstr(&[1]),
        ]
        .concat(),
    ];
    for bad in &noncanonical {
        assert!(is_invalid(MlsFrame::decode(bad)));
    }
    assert!(is_invalid(MlsReply::decode(&arr(&[
        uint(1),
        uint(9),
        vec![0x18, 1],
        uint(2)
    ]))));
    // Wrong field counts for a known op.
    assert!(is_invalid(MlsFrame::decode(&arr(&[
        uint(1),
        uint(2),
        uint(0)
    ]))));
    assert!(is_invalid(MlsFrame::decode(&arr(&[uint(1), uint(1)]))));
    assert!(is_invalid(MlsReply::decode(&arr(&[
        uint(1),
        uint(12),
        uint(0)
    ]))));
    // Reply values that have no canonical form.
    for (class, epoch) in [(0u64, 1u64), (6, 1)] {
        assert!(is_invalid(MlsReply::decode(&arr(&[
            uint(1),
            uint(9),
            uint(class),
            uint(epoch)
        ]))));
    }
    assert!(is_invalid(MlsReply::decode(&arr(&[
        uint(1),
        uint(10),
        uint(1)
    ]))));
    assert!(is_invalid(MlsReply::decode(&arr(&[
        uint(1),
        uint(10),
        vec![0xf4],
        uint(0),
        bstr(&[0; 32])
    ]))));
    assert!(is_invalid(MlsReply::decode(&arr(&[
        uint(1),
        uint(10),
        vec![0xf5]
    ]))));
}

#[test]
fn unknown_op_rejected() {
    for op in [6u64, 7, 15, 16, 17, 255, 256] {
        assert!(
            is_invalid(MlsFrame::decode(&arr(&[uint(1), uint(op)]))),
            "frame op {op}"
        );
        assert!(
            is_invalid(MlsReply::decode(&arr(&[uint(1), uint(op)]))),
            "reply op {op}"
        );
    }
    // A request is not a reply and a reply is not a request.
    assert!(is_invalid(MlsFrame::decode(&arr(&[uint(1), uint(12)]))));
    assert!(is_invalid(MlsReply::decode(&arr(&[uint(1), uint(2)]))));
}

#[test]
fn version_other_than_one_is_unsupported() {
    for version in [0u64, 2, 255] {
        let frame = arr(&[uint(version), uint(0), bstr(&CONV), bstr(&[1])]);
        assert!(matches!(
            MlsFrame::decode(&frame),
            Err(e) if e.code == ErrorCode::UnsupportedMessageVersion
        ));
        let reply = arr(&[uint(version), uint(12)]);
        assert!(matches!(
            MlsReply::decode(&reply),
            Err(e) if e.code == ErrorCode::UnsupportedMessageVersion
        ));
    }
}
