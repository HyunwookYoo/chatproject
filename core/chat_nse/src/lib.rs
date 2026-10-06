//! MLS decrypt for the iOS notification service extension (design 12.3). MLS only.
//!
//! The persisted state is never written: OpenMLS's writes during decrypt (it re-saves
//! the message secrets, spike B) land in an in-memory copy that is dropped on return.

use std::fs::File;
use std::io::BufReader;
use std::time::Instant;

use openmls::prelude::tls_codec::Deserialize as _;
use openmls::prelude::*;
use openmls_rust_crypto::OpenMlsRustCrypto;

pub mod state;

uniffi::setup_scaffolding!();

#[derive(Debug, uniffi::Record)]
pub struct ProbeOutcome {
    pub plaintext_len: u32,
    /// Up to 48 bytes of the plaintext, lossy UTF-8, for the notification body.
    pub preview: String,
    /// Read + decode the state file + `MlsGroup::load`.
    pub load_micros: u64,
    /// TLS decode + `process_message`.
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

/// Reads the state file (see [`state`]) into a fresh provider, loads its group and
/// decrypts one `MlsMessageOut` wire encoding of a `PrivateMessage`. Rust streams the
/// file itself, so Swift never holds the state or copies it across the FFI.
#[uniffi::export]
pub fn probe_decrypt(state_path: String, ciphertext: Vec<u8>) -> Result<ProbeOutcome, NseError> {
    let start = Instant::now();
    let (group_id, map) = File::open(&state_path)
        .and_then(|f| state::read(&mut BufReader::new(f)))
        .map_err(|e| NseError::BadState(e.to_string()))?;
    let provider = OpenMlsRustCrypto::default();
    *provider.storage().values.write().unwrap() = map;
    let mut group = MlsGroup::load(provider.storage(), &GroupId::from_slice(&group_id))
        .map_err(|e| NseError::BadState(e.to_string()))?
        .ok_or(NseError::GroupNotFound)?;
    let loaded = Instant::now();

    let message = MlsMessageIn::tls_deserialize_exact(&ciphertext)
        .map_err(|e| NseError::BadCiphertext(e.to_string()))?
        .try_into_protocol_message()
        .map_err(|e| NseError::BadCiphertext(e.to_string()))?;
    let processed = group
        .process_message(&provider, message)
        .map_err(|e| NseError::Decrypt(e.to_string()))?;
    let ProcessedMessageContent::ApplicationMessage(app) = processed.into_content() else {
        return Err(NseError::NotApplication);
    };
    let plaintext = app.into_bytes();
    let done = Instant::now();

    Ok(ProbeOutcome {
        plaintext_len: plaintext.len() as u32,
        preview: String::from_utf8_lossy(&plaintext[..plaintext.len().min(48)]).into_owned(),
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
    }

    #[test]
    fn state_without_the_group_is_group_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, &state::encode(b"gid", &state::Map::new()));
        let err = probe_decrypt(path, Vec::new()).unwrap_err();
        assert!(matches!(err, NseError::GroupNotFound), "{err:?}");
    }
}
