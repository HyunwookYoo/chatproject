use chat_core::ErrorCode;
use chat_core::account::{
    AccountKeys, DeviceKind, FlowId, Generation, InstallationId, PeerId, Secret32, StoreId, TxId,
    decode_recovery, derive_account, derive_device, encode_recovery, random_secret,
};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/m3-key-vectors.json")).unwrap()
}

fn bytes32(hex: &str) -> [u8; 32] {
    assert_eq!(hex.len(), 64);
    std::array::from_fn(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
}

fn account(row: &Value) -> AccountKeys {
    derive_account(&Secret32::from(bytes32(
        row["entropy_hex"].as_str().unwrap(),
    )))
    .unwrap()
}

#[test]
fn entropy_round_trip_24_words() {
    for row in fixture()["recovery"].as_array().unwrap() {
        let entropy = bytes32(row["entropy_hex"].as_str().unwrap());
        let words = row["words"].as_str().unwrap();
        assert_eq!(words.split_whitespace().count(), 24);
        assert_eq!(encode_recovery(&Secret32::from(entropy)).unwrap(), words);
        assert_eq!(decode_recovery(words).unwrap().as_bytes(), &entropy);
        let pasted = format!(" \t{}\n", words.to_uppercase().replace(' ', "  \n"));
        assert_eq!(decode_recovery(&pasted).unwrap().as_bytes(), &entropy);
        let fullwidth = words
            .chars()
            .map(|c| {
                if c.is_ascii_lowercase() {
                    char::from_u32(c as u32 + 0xfee0).unwrap()
                } else {
                    c
                }
            })
            .collect::<String>();
        assert_eq!(decode_recovery(&fullwidth).unwrap().as_bytes(), &entropy);
    }
}

#[test]
fn fixed_hkdf_domains() {
    for row in fixture()["recovery"].as_array().unwrap() {
        assert_eq!(
            account(row).peer_id().0,
            bytes32(row["identity_public_hex"].as_str().unwrap())
        );
        // The independent HKDF seed must produce the same frozen public key.
        let signing = ed25519_dalek::SigningKey::from_bytes(&bytes32(
            row["identity_seed_hex"].as_str().unwrap(),
        ));
        assert_eq!(signing.verifying_key().to_bytes(), account(row).peer_id().0);
    }
    let f = fixture();
    let row = &f["device"];
    let device = derive_device(Secret32::from(bytes32(row["seed_hex"].as_str().unwrap()))).unwrap();
    assert_eq!(
        device.transport_public_key(),
        bytes32(row["transport_public_hex"].as_str().unwrap())
    );
    assert_eq!(
        device.installation_id().0,
        bytes32(row["installation_public_hex"].as_str().unwrap())
    );
}

#[test]
fn ids_are_52_lowercase_base32() {
    let f = fixture();
    for row in f["recovery"].as_array().unwrap() {
        let id = account(row).peer_id().to_string();
        assert_eq!(id, row["peer_id"].as_str().unwrap());
        assert_eq!(id.len(), 52);
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
        );
    }
    let row = &f["device"];
    let device = derive_device(Secret32::from(bytes32(row["seed_hex"].as_str().unwrap()))).unwrap();
    let id = device.installation_id().to_string();
    assert_eq!(id, row["installation_id"].as_str().unwrap());
    assert_eq!(id.len(), 52);
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    );
    assert!(PeerId([0; 32]) < PeerId([1; 32]));
    assert!(InstallationId([0; 32]) < InstallationId([1; 32]));
    assert!(Generation([0; 16]) < Generation([1; 16]));
    assert!(FlowId([0; 16]) < FlowId([1; 16]));
    assert!(TxId([0; 16]) < TxId([1; 16]));
    assert!(StoreId([0; 16]) < StoreId([1; 16]));
    assert_eq!(FlowId([0; 16]).to_string(), "a".repeat(26));
    assert_ne!(DeviceKind::Desktop, DeviceKind::Mobile);
}

#[test]
fn device_keys_are_distinct() {
    let device = derive_device(Secret32::from([0xff; 32])).unwrap();
    let other = derive_device(Secret32::from([0; 32])).unwrap();
    let account = derive_account(&Secret32::from([0xff; 32])).unwrap();
    assert_ne!(device.transport_public_key(), device.installation_id().0);
    assert_ne!(device.transport_public_key(), account.peer_id().0);
    assert_ne!(device.installation_id().0, account.peer_id().0);
    assert_ne!(device.transport_public_key(), other.transport_public_key());
    assert_ne!(device.installation_id(), other.installation_id());
}

fn invalid(input: &str) {
    match decode_recovery(input) {
        Err(error) => {
            assert_eq!(error.code, ErrorCode::InvalidRecovery);
            if input.trim().len() > 1 {
                assert!(!error.detail.contains(input));
            }
        }
        Ok(_) => panic!("invalid recovery accepted"),
    }
}

#[test]
fn invalid_word_checksum_and_23_or_25_rejected() {
    let f = fixture();
    let words = f["recovery"][0]["words"].as_str().unwrap();
    invalid(&words.replacen("abandon", "notaword", 1));
    invalid(&words.replace("art", "abandon"));
    invalid(
        &words
            .split_whitespace()
            .take(23)
            .collect::<Vec<_>>()
            .join(" "),
    );
    invalid(&format!("{words} abandon"));
    invalid(
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
    );
    invalid(" ");
    invalid(&words.replacen("abandon", "abandón", 1));
}

#[test]
fn recovery_input_byte_limit_is_checked_before_normalization() {
    let f = fixture();
    let words = f["recovery"][0]["words"].as_str().unwrap();
    let at_limit = format!("{}{words}", " ".repeat(4096 - words.len()));
    assert_eq!(at_limit.len(), 4096);
    assert_eq!(decode_recovery(&at_limit).unwrap().as_bytes(), &[0; 32]);
    invalid(&format!(" {at_limit}"));
    // U+3000 whitespace is three UTF-8 bytes and must be counted before normalization.
    invalid(&format!("{}{words}", "\u{3000}".repeat(1366)));
}

#[test]
fn random_secret_produces_independent_recoverable_entropy() {
    let first = random_secret().unwrap();
    let second = random_secret().unwrap();
    assert_ne!(first.as_bytes(), second.as_bytes());
    assert_eq!(
        decode_recovery(&encode_recovery(&first).unwrap())
            .unwrap()
            .as_bytes(),
        first.as_bytes()
    );
}

#[cfg(all(feature = "test-hooks", debug_assertions))]
#[test]
fn fake_clock_controls_wall_and_monotonic_time_independently() {
    use chat_core::account::clock::{Clock, FakeClock};
    let clock = FakeClock::new(1_800_000_000, 10);
    clock.set_unix_seconds(1_799_999_000);
    clock.advance_monotonic_ms(25);
    assert_eq!(clock.unix_seconds(), 1_799_999_000);
    assert_eq!(clock.monotonic_ms(), 35);
}
