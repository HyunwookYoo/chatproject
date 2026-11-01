use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
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
    let path = write_state(&dir, &fixture.snapshot);
    for (seq, ct) in fixture.ciphertexts.iter().enumerate() {
        let outcome = probe_decrypt(path.clone(), ct.clone()).unwrap();
        assert_eq!(outcome.plaintext_len, 800);
        assert!(outcome.preview.starts_with(&format!("M1b probe message {seq} ")), "{}", outcome.preview);
        assert_eq!(outcome.preview.len(), 48);
    }
}

#[test]
fn decrypting_twice_works_and_leaves_the_state_file_unchanged() {
    let fixture = build(10, 1, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(&dir, &fixture.snapshot);
    probe_decrypt(path.clone(), fixture.ciphertexts[0].clone()).unwrap();
    probe_decrypt(path.clone(), fixture.ciphertexts[0].clone()).unwrap();
    assert_eq!(fs::read(&path).unwrap(), fixture.snapshot);
}

#[test]
fn corrupted_ciphertext_is_an_error_not_a_panic() {
    let fixture = build(10, 1, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(&dir, &fixture.snapshot);
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
    let path = write_state(&dir, &fixture.snapshot);
    let err = probe_decrypt(path, Vec::new()).unwrap_err();
    assert!(matches!(err, NseError::BadCiphertext(_)), "{err:?}");
}

#[test]
fn a_commit_is_not_an_application_message() {
    let fixture = build(10, 1, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(&dir, &fixture.snapshot);
    let err = probe_decrypt(path, fixture.commit.clone()).unwrap_err();
    assert!(matches!(err, NseError::NotApplication), "{err:?}");
}

#[test]
fn ciphertexts_of_other_epochs_are_decrypt_errors() {
    let fixture = build(10, 1, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(&dir, &fixture.snapshot);
    for other in [&fixture.next, &fixture.stale] {
        let err = probe_decrypt(path.clone(), other.clone()).unwrap_err();
        assert!(matches!(err, NseError::Decrypt(_)), "{err:?}");
    }
    // The refusals changed nothing.
    probe_decrypt(path, fixture.ciphertexts[0].clone()).unwrap();
}

#[test]
fn a_changed_group_id_length_is_bad_state() {
    let fixture = build(10, 1, 4);
    let dir = tempfile::tempdir().unwrap();
    let mut state = fixture.snapshot.clone();
    state[7] -= 1; // group id length 48 -> 47: every later field shifts by one byte
    let err = probe_decrypt(write_state(&dir, &state), fixture.ciphertexts[0].clone()).unwrap_err();
    assert!(matches!(err, NseError::BadState(_)), "{err:?}");
}

/// Flips each of `offsets` in turn: an error or a normal result, never a panic.
fn flip_each(state: &[u8], ciphertext: &[u8], offsets: impl Iterator<Item = usize>) -> usize {
    let dir = tempfile::tempdir().unwrap();
    let mut errors = 0;
    for at in offsets {
        let mut flipped = state.to_vec();
        flipped[at] ^= 0xFF;
        let path = write_state(&dir, &flipped);
        match catch_unwind(AssertUnwindSafe(|| probe_decrypt(path, ciphertext.to_vec()))) {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => errors += 1,
            Err(_) => panic!("probe_decrypt panicked with byte {at} of {} flipped", state.len()),
        }
    }
    errors
}

#[test]
fn flipping_bytes_of_a_small_state_file_never_panics() {
    // chat_core flips every byte in memory; through files, every 7th is enough to cover the path.
    let fixture = build(2, 1, 0);
    let errors = flip_each(&fixture.snapshot, &fixture.ciphertexts[0], (0..fixture.snapshot.len()).step_by(7));
    assert!(errors > 0);
}

fn committed() -> (Vec<u8>, Vec<Vec<u8>>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/ios/NotificationService/Fixture");
    let state = fs::read(dir.join("nse_state.bin")).unwrap();
    let lines = fs::read_to_string(dir.join("ciphertexts.txt")).unwrap();
    (state, lines.lines().map(|l| STANDARD.decode(l).unwrap()).collect())
}

#[test]
fn committed_fixture_decrypts() {
    let (state, cts) = committed();
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(&dir, &state);
    assert_eq!(cts.len(), 8);
    for seq in [0, 7] {
        let outcome = probe_decrypt(path.clone(), cts[seq].clone()).unwrap();
        assert_eq!(outcome.plaintext_len, 800);
        assert!(outcome.preview.starts_with(&format!("M1b probe message {seq} ")), "{}", outcome.preview);
    }
}

/// The header and the first rows byte by byte, then every 8,191st byte of the 200-leaf file.
#[test]
fn flipping_bytes_of_the_committed_fixture_never_panics() {
    let (state, cts) = committed();
    let offsets = (0..256).chain((256..state.len()).step_by(8191));
    let errors = flip_each(&state, &cts[0], offsets);
    assert!(errors > 0);
}

#[test]
fn state_without_the_group_is_group_not_found() {
    let fixture = build(10, 1, 4);
    let dir = tempfile::tempdir().unwrap();
    // `CNS2 | gid_len | gid | 0 rows`: a well-formed snapshot of some other group.
    let state = [b"CNS2".as_slice(), &1u32.to_be_bytes(), b"g", &0u32.to_be_bytes()].concat();
    let err = probe_decrypt(write_state(&dir, &state), fixture.ciphertexts[0].clone()).unwrap_err();
    assert!(matches!(err, NseError::GroupNotFound), "{err:?}");
}
