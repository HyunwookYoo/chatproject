//! MLS decrypt for the iOS notification service extension (design 12.3, M5 design 10). MLS only.
//!
//! A thin UniFFI wrapper: the state file is a `chat_core` snapshot, and `chat_core` does the
//! decrypting. Nothing is written: OpenMLS's writes during decrypt (it re-saves the message
//! secrets, spike B) land in an in-memory copy that is dropped on return.

use std::fs::File;
use std::io::Read as _;
use std::time::Instant;

use chat_core::mls::nse;

// UniFFI turns a panic into a Swift error only if panics unwind. With `panic = "abort"` the
// notification extension would die without an error, so the build refuses it.
#[cfg(not(panic = "unwind"))]
compile_error!("chat_nse needs panic = \"unwind\"; do not set panic = \"abort\" in a profile");

uniffi::setup_scaffolding!();

/// A state file larger than this is refused before it is read. The extension is a ~15 MB
/// process; one conversation's rows are far smaller (the 200-leaf probe group is about 1 MB).
const MAX_STATE_FILE: u64 = 64 << 20;

#[derive(Debug, uniffi::Record)]
pub struct ProbeOutcome {
    /// Bytes of the message text.
    pub plaintext_len: u32,
    /// Up to 48 bytes of the text, for the notification body.
    pub preview: String,
    /// Reading the state file.
    pub load_micros: u64,
    /// Decoding the snapshot, loading the group, TLS decode and `process_message`.
    pub decrypt_micros: u64,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum NseError {
    #[error("bad state: {0}")]
    BadState(String),
    #[error("group not in state")]
    GroupNotFound,
    #[error("bad ciphertext: {0}")]
    BadCiphertext(String),
    #[error("decrypt failed: {0}")]
    Decrypt(String),
    #[error("not an application message")]
    NotApplication,
}

impl From<nse::NseError> for NseError {
    fn from(error: nse::NseError) -> Self {
        match error {
            nse::NseError::BadState(why) => Self::BadState(why.to_owned()),
            nse::NseError::GroupNotFound => Self::GroupNotFound,
            nse::NseError::BadCiphertext => Self::BadCiphertext("not an MLS private message".to_owned()),
            nse::NseError::Decrypt(class) => Self::Decrypt(format!("class {class}")),
            // A Commit, a receipt or a payload that is no text: no notification text either way.
            nse::NseError::NotApplication | nse::NseError::NoPreview => Self::NotApplication,
        }
    }
}

fn too_large() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "state file too large")
}

fn read_state(path: &str) -> std::io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    if size > MAX_STATE_FILE {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(size as usize);
    // A file that grows after the check still stops at the limit.
    file.take(MAX_STATE_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE_FILE {
        return Err(too_large());
    }
    Ok(bytes)
}

/// Reads the state file (a `chat_core::mls::nse::export_snapshot` snapshot) and decrypts one
/// `MlsMessageOut` wire encoding of a `PrivateMessage` against it.
#[uniffi::export]
pub fn probe_decrypt(state_path: String, ciphertext: Vec<u8>) -> Result<ProbeOutcome, NseError> {
    let start = Instant::now();
    let snapshot = read_state(&state_path).map_err(|e| NseError::BadState(e.to_string()))?;
    let loaded = Instant::now();
    let preview = nse::decrypt_readonly(&snapshot, &ciphertext)?;
    let done = Instant::now();
    Ok(ProbeOutcome {
        plaintext_len: preview.plaintext_len,
        preview: preview.preview,
        load_micros: (loaded - start).as_micros() as u64,
        decrypt_micros: (done - loaded).as_micros() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &tempfile::TempDir, bytes: &[u8]) -> String {
        let path = dir.path().join("nse_state.bin");
        std::fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// `b"CNS2" | gid_len | gid | n` with no rows.
    fn empty_state(group_id: &[u8]) -> Vec<u8> {
        let mut bytes = b"CNS2".to_vec();
        bytes.extend_from_slice(&(group_id.len() as u32).to_be_bytes());
        bytes.extend_from_slice(group_id);
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes
    }

    #[test]
    fn missing_state_file_is_bad_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.bin").to_string_lossy().into_owned();
        let err = probe_decrypt(path, vec![1, 2, 3]).unwrap_err();
        assert!(matches!(err, NseError::BadState(_)), "{err:?}");
    }

    #[test]
    fn state_with_bad_magic_is_bad_state() {
        let dir = tempfile::tempdir().unwrap();
        let err = probe_decrypt(write(&dir, b"NOPE"), Vec::new()).unwrap_err();
        assert!(matches!(err, NseError::BadState(_)), "{err:?}");
        // The M1b format is not accepted any more.
        let err = probe_decrypt(write(&dir, b"CNS1\0\0\0\0\0\0\0\0"), Vec::new()).unwrap_err();
        assert!(matches!(err, NseError::BadState(_)), "{err:?}");
    }

    #[test]
    fn a_well_formed_empty_state_is_not_bad_state() {
        // The state parses; it is the (empty) ciphertext that is refused.
        let dir = tempfile::tempdir().unwrap();
        let err = probe_decrypt(write(&dir, &empty_state(b"gid")), Vec::new()).unwrap_err();
        assert!(matches!(err, NseError::BadCiphertext(_)), "{err:?}");
    }

    #[test]
    fn huge_length_claims_in_the_file_are_bad_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut group_id = b"CNS2".to_vec();
        group_id.extend_from_slice(&u32::MAX.to_be_bytes());
        let mut entries = empty_state(b"g");
        let at = entries.len() - 4;
        entries[at..].copy_from_slice(&u32::MAX.to_be_bytes());
        let mut field = empty_state(b"g");
        let at = field.len() - 4;
        field[at..].copy_from_slice(&1u32.to_be_bytes());
        field.extend_from_slice(&u32::MAX.to_be_bytes()); // key length
        field.extend_from_slice(&u32::MAX.to_be_bytes()); // value length
        for claim in [group_id, entries, field] {
            // The state is checked before the ciphertext, so this malformed one is never reached.
            let err = probe_decrypt(write(&dir, &claim), vec![1, 2, 3]).unwrap_err();
            assert!(matches!(err, NseError::BadState(_)), "{err:?}");
        }
    }

    #[test]
    fn an_oversized_state_file_is_refused_unread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        let file = File::create(&path).unwrap();
        file.set_len(MAX_STATE_FILE + 1).unwrap();
        drop(file);
        let err = probe_decrypt(path.to_string_lossy().into_owned(), Vec::new()).unwrap_err();
        assert!(matches!(&err, NseError::BadState(why) if why.contains("too large")), "{err:?}");
    }

    /// The build refuses `panic = "abort"` (see the top of this file), and a profile setting it
    /// is caught here as well, because cargo's test profile ignores it.
    #[test]
    fn panic_unwind_is_kept() {
        assert!(std::panic::catch_unwind(|| panic!("caught")).is_err());
        let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml")).unwrap();
        for line in manifest.lines() {
            let line = line.split('#').next().unwrap().replace(' ', "");
            assert!(!line.starts_with("panic="), "the workspace sets {line}");
        }
    }

    #[test]
    fn core_errors_map_to_the_exported_ones() {
        let cases = [
            (nse::NseError::BadState("x"), "BadState"),
            (nse::NseError::GroupNotFound, "GroupNotFound"),
            (nse::NseError::BadCiphertext, "BadCiphertext"),
            (nse::NseError::Decrypt(2), "Decrypt"),
            (nse::NseError::NotApplication, "NotApplication"),
            (nse::NseError::NoPreview, "NotApplication"),
        ];
        for (core, expected) in cases {
            let name = format!("{:?}", NseError::from(core));
            assert!(name.starts_with(expected), "{name} vs {expected}");
        }
    }
}
