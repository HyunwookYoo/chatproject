use std::fs;
use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chat_nse::{NseError, probe_decrypt};
use nse_fixture::build;

fn write_state(dir: &tempfile::TempDir, state: &[u8]) -> String {
    let path = dir.path().join("nse_state.bin");
    fs::write(&path, state).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn every_ciphertext_decrypts_against_the_same_state() {
    let fixture = build(10, 3, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(&dir, &fixture.state);
    for (seq, ct) in fixture.ciphertexts.iter().enumerate() {
        let outcome = probe_decrypt(path.clone(), ct.clone()).unwrap();
        assert_eq!(outcome.plaintext_len, 1024);
        assert!(outcome.preview.starts_with(&format!("M1b probe message {seq} ")), "{}", outcome.preview);
    }
}

#[test]
fn decrypting_twice_works_and_leaves_the_state_file_unchanged() {
    let fixture = build(10, 1, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(&dir, &fixture.state);
    probe_decrypt(path.clone(), fixture.ciphertexts[0].clone()).unwrap();
    probe_decrypt(path.clone(), fixture.ciphertexts[0].clone()).unwrap();
    assert_eq!(fs::read(&path).unwrap(), fixture.state);
}

#[test]
fn corrupted_ciphertext_is_an_error_not_a_panic() {
    let fixture = build(10, 1, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(&dir, &fixture.state);
    let mut ct = fixture.ciphertexts[0].clone();
    let last = ct.len() - 1;
    ct[last] ^= 0xFF; // inside the AEAD tag
    let err = probe_decrypt(path, ct).unwrap_err();
    assert!(matches!(err, NseError::Decrypt(_) | NseError::BadCiphertext(_)), "{err:?}");
}

#[test]
fn empty_ciphertext_is_bad_ciphertext() {
    let fixture = build(10, 1, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(&dir, &fixture.state);
    let err = probe_decrypt(path, Vec::new()).unwrap_err();
    assert!(matches!(err, NseError::BadCiphertext(_)), "{err:?}");
}

#[test]
fn committed_fixture_decrypts() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/ios/NotificationService/Fixture");
    let path = dir.join("nse_state.bin").to_string_lossy().into_owned();
    let lines = fs::read_to_string(dir.join("ciphertexts.txt")).unwrap();
    let cts: Vec<Vec<u8>> = lines.lines().map(|l| STANDARD.decode(l).unwrap()).collect();
    assert_eq!(cts.len(), 8);
    for seq in [0, 7] {
        let outcome = probe_decrypt(path.clone(), cts[seq].clone()).unwrap();
        assert!(outcome.preview.starts_with(&format!("M1b probe message {seq} ")), "{}", outcome.preview);
    }
}
