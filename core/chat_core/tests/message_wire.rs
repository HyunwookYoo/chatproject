#![cfg(all(feature = "test-hooks", debug_assertions))]
use chat_core::{
    ErrorCode,
    storage_fixture::{MessageFrame, decode_message},
};
fn hex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks_exact(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}
fn vector() -> Vec<u8> {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/m4-wire-vectors.json")).unwrap();
    hex(v["vectors"][0]["inner"].as_str().unwrap())
}
#[test]
fn canonical_text_golden_decodes() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/m4-wire-vectors.json")).unwrap();
    for item in v["vectors"].as_array().unwrap() {
        let result = decode_message(&hex(item["inner"].as_str().unwrap()));
        assert!(
            matches!(result, Ok(MessageFrame::Text(_))),
            "canonical text rejected"
        );
        let Ok(MessageFrame::Text(text)) = result else {
            unreachable!()
        };
        assert!(text.body == item["body"].as_str().unwrap());
        assert_eq!(text.lamport, item["lamport"].as_u64().unwrap());
    }
}
#[test]
fn canonical_rejections_and_receipts_decode() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/m4-wire-vectors.json")).unwrap();
    for item in v["reject_vectors"].as_array().unwrap() {
        assert!(matches!(
            decode_message(&hex(item.as_str().unwrap())),
            Ok(MessageFrame::Reject(_))
        ));
    }
    for item in v["receipt_vectors"].as_array().unwrap() {
        let decoded = decode_message(&hex(item["inner"].as_str().unwrap()));
        if item["kind"].as_str().unwrap().starts_with("applied") {
            assert!(matches!(decoded, Ok(MessageFrame::ReceiptApplied(_))));
        } else {
            assert!(matches!(decoded, Ok(MessageFrame::Receipt(_))));
        }
    }
}
#[test]
fn noncanonical_trailing_lengths_version_rejected() {
    let original = vector();
    let mut noncanonical = original.clone();
    noncanonical.splice(1..2, [0x18, 1]);
    let mut trailing = original.clone();
    trailing.push(0);
    let mut fields = original.clone();
    fields[0] = 0x89;
    for bad in [
        noncanonical,
        trailing,
        fields,
        vec![0; 61441],
        vec![0x83, 1, 3, 5],
    ] {
        assert!(decode_message(&bad).is_err());
    }
    let mut version = original;
    version[1] = 2;
    assert!(
        matches!(decode_message(&version),Err(e) if e.code==ErrorCode::UnsupportedMessageVersion)
    );
}

#[test]
fn text_utf8_and_integer_boundaries_are_exact() {
    let original = vector();
    let mut d = minicbor::Decoder::new(&original);
    d.array().unwrap();
    for _ in 0..9 {
        d.skip().unwrap();
    }
    let prefix = original[..d.position()].to_vec();
    for (body, accepted) in [
        ("x".repeat(16384), true),
        ("x".repeat(16385), false),
        (String::new(), false),
        ("한".repeat(5461) + "x", true),
        ("한".repeat(5462), false),
    ] {
        let mut e = minicbor::Encoder::new(prefix.clone());
        e.str(&body).unwrap();
        assert_eq!(decode_message(&e.into_writer()).is_ok(), accepted);
    }
    let mut d = minicbor::Decoder::new(&original);
    d.array().unwrap();
    for _ in 0..6 {
        d.skip().unwrap();
    }
    let before = d.position();
    d.skip().unwrap();
    let after = d.position();
    let mut overflowing = original[..before].to_vec();
    overflowing.push(0x1b);
    overflowing.extend(u64::MAX.to_be_bytes());
    overflowing.extend(&original[after..]);
    assert!(decode_message(&overflowing).is_err());
    let mut oversized = prefix;
    oversized.extend([0x79, 0x40, 0x00]);
    oversized.extend(vec![0xff; 16384]);
    assert!(decode_message(&oversized).is_err());
}
