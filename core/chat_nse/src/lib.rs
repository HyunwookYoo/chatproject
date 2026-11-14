//! MLS decrypt for the iOS notification service extension (M7 spec 5.6, design 12.3). MLS only.
//!
//! A thin UniFFI wrapper: the app writes one state file per conversation a push can name
//! (`chat_core::push::ios_state`), and `chat_core` does the decrypting. Nothing is written:
//! OpenMLS's writes during decrypt (it re-saves the message secrets, spike B) land in an
//! in-memory copy that is dropped on return. This crate has no HTTP client and no network code
//! (`chat_core` without its `push-http` feature).

use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use chat_core::mls::nse;
use chat_core::push::ios_state::{decode_state, state_file_name};
use push_wire::{BASE64URL_NOPAD, PUSH_CIPHER_MAX, Tag};

// UniFFI turns a panic into a Swift error only if panics unwind. With `panic = "abort"` the
// notification extension would die without an error, so the build refuses it.
#[cfg(not(panic = "unwind"))]
compile_error!("chat_nse needs panic = \"unwind\"; do not set panic = \"abort\" in a profile");

uniffi::setup_scaffolding!();

/// A state file larger than this is refused before it is read. The extension is a ~15 MB
/// process; one conversation's rows are far smaller (the 200-leaf probe group is about
/// 0.4 MB), so 8 MiB is a ceiling for a corrupt file, not a target.
const MAX_STATE_FILE: u64 = 8 << 20;

/// What a notification shows when the text cannot be: the app's `NEW_MSG`.
const GENERIC: &str = "새 메시지";

/// What the notification of one push shows.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NseNotice {
    /// The contact's alias, or "새 메시지" without one. Empty with `body`: show nothing.
    pub title: String,
    /// The start of the text (at most 48 bytes, whole characters), or "새 메시지". Empty: the
    /// tag names no conversation of this device (withdrawn, or the conversation is gone), and
    /// nothing is shown.
    pub body: String,
    /// The body is the text.
    pub decrypted: bool,
}

impl NseNotice {
    fn silent() -> Self {
        Self {
            title: String::new(),
            body: String::new(),
            decrypted: false,
        }
    }

    fn generic(alias: &str) -> Self {
        Self {
            title: title_of(alias),
            body: GENERIC.to_owned(),
            decrypted: false,
        }
    }
}

fn title_of(alias: &str) -> String {
    if alias.is_empty() { GENERIC } else { alias }.to_owned()
}

fn too_large() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "state file too large")
}

fn read_state(path: &Path) -> std::io::Result<Vec<u8>> {
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

/// The notification of one push: `tag_b64` is its `t`, `stripped_b64` its `e` (empty for a
/// wake push), both base64url without padding; `push_dir` is `<App Group>/push`.
///
/// * No state file for the tag: nothing is shown (`title` and `body` empty).
/// * A bad tag, a state file that cannot be read (before the first unlock, for one) or
///   decoded, no `e`, or a ciphertext that does not open against the state (the state is older
///   than the message, or it is not a text): "새 메시지", with the alias as the title if the
///   file has one.
/// * Otherwise the alias and the start of the text.
///
/// Reads one file, writes nothing, and never fails.
#[uniffi::export]
pub fn nse_open(push_dir: String, tag_b64: String, stripped_b64: String) -> NseNotice {
    let Some(tag) = BASE64URL_NOPAD
        .decode(tag_b64.as_bytes())
        .ok()
        .and_then(|tag| Tag::try_from(tag).ok())
    else {
        return NseNotice::generic("");
    };
    let bytes = match read_state(&Path::new(&push_dir).join(state_file_name(&tag))) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return NseNotice::silent(),
        Err(_) => return NseNotice::generic(""),
    };
    let Some(state) = decode_state(&bytes) else {
        return NseNotice::generic("");
    };
    let ciphertext = match BASE64URL_NOPAD.decode(stripped_b64.as_bytes()) {
        Ok(ciphertext) if !ciphertext.is_empty() && ciphertext.len() <= PUSH_CIPHER_MAX => {
            ciphertext
        }
        _ => return NseNotice::generic(state.alias),
    };
    match nse::decrypt_readonly(state.snapshot, &ciphertext) {
        Ok(preview) => NseNotice {
            title: title_of(state.alias),
            body: preview.preview,
            decrypted: true,
        },
        Err(_) => NseNotice::generic(state.alias),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat_core::mls::nse::fixture::{Fixture, build_full, stripped};
    use chat_core::push::ios_state::encode_state;

    const TAG: Tag = [3; 16];

    fn b64(bytes: &[u8]) -> String {
        BASE64URL_NOPAD.encode(bytes)
    }

    /// A push folder holding `bytes` as the state file of `TAG`.
    fn folder(bytes: &[u8]) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(state_file_name(&TAG)), bytes).unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        (dir, path)
    }

    fn fixture() -> Fixture {
        build_full(3, 2, 0)
    }

    #[test]
    fn state_file_opens_with_nse_open() {
        let fixture = fixture();
        let (_dir, push) = folder(&encode_state("앨리스", &fixture.snapshot));
        for (seq, ciphertext) in fixture.ciphertexts.iter().enumerate() {
            let notice = nse_open(push.clone(), b64(&TAG), b64(&stripped(ciphertext)));
            assert!(notice.decrypted, "{notice:?}");
            assert_eq!(notice.title, "앨리스");
            assert!(notice.body.starts_with(&format!("M1b probe message {seq} ")));
            assert_eq!(notice.body.len(), 48);
        }
        // Without an alias the title is the generic one.
        let (_dir, push) = folder(&encode_state("", &fixture.snapshot));
        let notice = nse_open(push, b64(&TAG), b64(&stripped(&fixture.ciphertexts[0])));
        assert!(notice.decrypted);
        assert_eq!(notice.title, GENERIC);
    }

    #[test]
    fn nse_open_without_state_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        let push = dir.path().to_string_lossy().into_owned();
        assert_eq!(nse_open(push.clone(), b64(&TAG), b64(b"anything")), NseNotice::silent());
        assert_eq!(nse_open(push, b64(&TAG), String::new()), NseNotice::silent());
        let missing = dir.path().join("no-such-folder").to_string_lossy().into_owned();
        assert_eq!(nse_open(missing, b64(&TAG), String::new()), NseNotice::silent());
    }

    #[test]
    fn nse_open_stale_state_is_generic() {
        let fixture = fixture();
        let (_dir, push) = folder(&encode_state("앨리스", &fixture.snapshot));
        let generic = NseNotice::generic("앨리스");
        assert_eq!(generic.body, GENERIC);
        assert_eq!(generic.title, "앨리스");
        // A text of an epoch the state has not reached, or of one it no longer holds.
        for other in [&fixture.next, &fixture.stale] {
            assert_eq!(nse_open(push.clone(), b64(&TAG), b64(&stripped(other))), generic);
        }
        // No `e` (a wake push), bad base64, an oversized or a tampered ciphertext.
        let mut tampered = stripped(&fixture.ciphertexts[0]);
        *tampered.last_mut().unwrap() ^= 0xFF;
        for e in [
            String::new(),
            "not base64!".to_owned(),
            b64(&vec![0; PUSH_CIPHER_MAX + 1]),
            b64(&tampered),
        ] {
            assert_eq!(nse_open(push.clone(), b64(&TAG), e), generic);
        }
        // A file that is not a state file, a bare snapshot, a state of no group.
        for bytes in [b"NOPE".to_vec(), fixture.snapshot.clone(), encode_state("x", b"CNS2")] {
            let (_dir, push) = folder(&bytes);
            let notice = nse_open(push, b64(&TAG), b64(&stripped(&fixture.ciphertexts[0])));
            assert!(!notice.decrypted && notice.body == GENERIC, "{notice:?}");
        }
        // A tag that is not 16 bytes of unpadded base64url.
        for tag in [String::new(), b64(&[3; 15]), "AwMDAwMDAwMDAwMDAwMDAw==".to_owned()] {
            assert_eq!(nse_open(push.clone(), tag, String::new()), NseNotice::generic(""));
        }
        // The state is only read: a text still opens after all that.
        let again = nse_open(push, b64(&TAG), b64(&stripped(&fixture.ciphertexts[1])));
        assert!(again.decrypted);
    }

    #[test]
    fn an_oversized_state_file_is_refused_unread() {
        let dir = tempfile::tempdir().unwrap();
        let file = File::create(dir.path().join(state_file_name(&TAG))).unwrap();
        file.set_len(MAX_STATE_FILE + 1).unwrap();
        drop(file);
        let push = dir.path().to_string_lossy().into_owned();
        assert_eq!(nse_open(push, b64(&TAG), String::new()), NseNotice::generic(""));
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
}
