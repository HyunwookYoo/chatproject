//! The iOS notification extension's state files (M7 spec 5.6, Task 11).
//!
//! The extension never writes group state. The app keeps one file per conversation that a push
//! of this device can name, in `<snapshot_dir>/push/<hex(tag)>.state`, formatted
//! `b"CNP1" | alias_len u16be | alias UTF-8 | export_snapshot bytes`. The extension reads it
//! (`decode_state`) and decrypts one pushed ciphertext against the snapshot.
//!
//! * A file exists exactly for the conversations a received push's tag names (the current
//!   handle is set and the group exists, as `PushDbOp::Tagged` looks them up), and only while
//!   the configured platform is APNs with a snapshot directory.
//! * The storage writer records which conversations an operation changed: `KvStore::flush`
//!   (`changed`) for the MLS state, the group being dropped, a handle being stored, and
//!   `changed_all` when every handle went at once (push off, a token or platform change). After
//!   the operation it rewrites or removes those files (`refresh`). No file is written for an
//!   operation that changed no conversation's state.
//! * At every start (`push_configure` with a snapshot directory) files of tags that are not
//!   current are removed, and a current tag without a file gets one (`sync_all`).
//! * A file is replaced atomically (a temporary file, then a rename), so the extension reads the
//!   old or the new state, never a mix. It is not synced to disk: it is derived from the
//!   database and written again with the conversation's next change; a file torn by a power
//!   loss decodes as a bad state, and the extension shows "새 메시지".
//! * Files are created with the app's default data protection class
//!   (`NSFileProtectionCompleteUntilFirstUserAuthentication`): the extension can read them on a
//!   locked phone after the first unlock, never `NSFileProtectionNone`.
//! * The folder is excluded from backups, and with it every file in it: the app (Swift
//!   `PushHost.pushDir`) makes and marks it at launch and before Dart gets the snapshot
//!   directory, which Dart does not pass when that fails (no file is written then).
//! * A failed write keeps the old file (or none); it never fails the database operation, which
//!   is committed already.
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

use data_encoding::HEXLOWER;
use push_wire::Tag;

/// First bytes of a state file.
pub const STATE_MAGIC: [u8; 4] = *b"CNP1";
/// The folder under the snapshot directory (the App Group container) with the state files and
/// the extension's inbox (`inbox.jsonl`).
pub const PUSH_DIR: &str = "push";
const STATE_SUFFIX: &str = ".state";
const TEMP_SUFFIX: &str = ".tmp";

/// The file name of a tag's state: `<32 lower-case hex digits>.state`.
pub fn state_file_name(tag: &Tag) -> String {
    format!("{}{STATE_SUFFIX}", HEXLOWER.encode(tag))
}

/// One state file's contents.
#[derive(Debug, PartialEq, Eq)]
pub struct StateFile<'a> {
    /// The contact's alias; empty without one.
    pub alias: &'a str,
    /// `mls::nse::export_snapshot` of the conversation.
    pub snapshot: &'a [u8],
}

/// The state file bytes. An alias longer than `u16::MAX` bytes is cut at a character boundary.
pub fn encode_state(alias: &str, snapshot: &[u8]) -> Vec<u8> {
    let mut end = alias.len().min(usize::from(u16::MAX));
    while !alias.is_char_boundary(end) {
        end -= 1;
    }
    let alias = &alias[..end];
    let mut out = Vec::with_capacity(6 + alias.len() + snapshot.len());
    out.extend_from_slice(&STATE_MAGIC);
    out.extend_from_slice(&(alias.len() as u16).to_be_bytes());
    out.extend_from_slice(alias.as_bytes());
    out.extend_from_slice(snapshot);
    out
}

/// The parts of a state file; `None` for anything that is not one.
pub fn decode_state(bytes: &[u8]) -> Option<StateFile<'_>> {
    let rest = bytes.strip_prefix(STATE_MAGIC.as_slice())?;
    let (length, rest) = rest.split_first_chunk::<2>()?;
    let length = usize::from(u16::from_be_bytes(*length));
    let alias = std::str::from_utf8(rest.get(..length)?).ok()?;
    Some(StateFile {
        alias,
        snapshot: rest.get(length..)?,
    })
}

/// The conversations one storage operation changed, as far as the state files go.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    pub conversations: BTreeSet<[u8; 32]>,
    /// Every handle may have changed.
    pub all: bool,
}

thread_local! {
    /// Set while the storage writer records one operation (`recording`).
    static CHANGES: RefCell<Option<Changes>> = const { RefCell::new(None) };
}

/// The conversation's MLS state, group or handle changed. Recorded only inside `recording`.
pub(crate) fn changed(conversation: [u8; 32]) {
    CHANGES.with(|changes| {
        if let Some(changes) = changes.borrow_mut().as_mut() {
            changes.conversations.insert(conversation);
        }
    });
}

/// Every handle changed at once. Recorded only inside `recording`.
pub(crate) fn changed_all() {
    CHANGES.with(|changes| {
        if let Some(changes) = changes.borrow_mut().as_mut() {
            changes.all = true;
        }
    });
}

/// Runs `operation` and returns what it changed (`changed`, `changed_all` on this thread).
pub(crate) fn recording<T>(operation: impl FnOnce() -> T) -> (T, Changes) {
    struct Stop;
    impl Drop for Stop {
        fn drop(&mut self) {
            CHANGES.with(|changes| *changes.borrow_mut() = None);
        }
    }
    CHANGES.with(|changes| *changes.borrow_mut() = Some(Changes::default()));
    let stop = Stop;
    let result = operation();
    let changes = CHANGES
        .with(|changes| changes.borrow_mut().take())
        .unwrap_or_default();
    drop(stop);
    (result, changes)
}

/// What a state file needs from the database and the account (the SQL is in `push_store`).
pub(crate) trait StateSource {
    /// The conversation's tag, and whether a push of it names the conversation now (`None`: the
    /// conversation has no tag).
    fn target(&self, conversation: [u8; 32]) -> Option<(Tag, bool)>;
    /// Every tag a push can name now, with its conversation.
    fn live(&self) -> Option<Vec<([u8; 32], Tag)>>;
    /// The state file bytes of the conversation (alias and snapshot).
    fn contents(&self, conversation: [u8; 32]) -> Option<Vec<u8>>;
}

/// Brings the files of what changed up to date in `dir` (`<snapshot_dir>/push`).
pub(crate) fn refresh(source: &dyn StateSource, dir: &Path, changes: &Changes) {
    if changes.all {
        sync_all(source, dir);
    }
    for conversation in &changes.conversations {
        match source.target(*conversation) {
            Some((tag, true)) => {
                if let Some(bytes) = source.contents(*conversation) {
                    let _ = write_state(dir, &tag, &bytes);
                }
            }
            Some((tag, false)) => remove_state(dir, &tag),
            None => {}
        }
    }
}

/// Removes every file of a tag that is not current (and leftover temporary files), and writes
/// the file of a current tag that has none.
pub(crate) fn sync_all(source: &dyn StateSource, dir: &Path) {
    let Some(live) = source.live() else { return };
    let names: BTreeSet<String> = live.iter().map(|(_, tag)| state_file_name(tag)).collect();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let stale = name.ends_with(TEMP_SUFFIX) && name.contains(STATE_SUFFIX)
                || name.ends_with(STATE_SUFFIX) && !names.contains(name);
            if stale {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    for (conversation, tag) in &live {
        if !dir.join(state_file_name(tag)).exists()
            && let Some(bytes) = source.contents(*conversation)
        {
            let _ = write_state(dir, tag, &bytes);
        }
    }
}

/// Replaces the tag's file: written to a temporary file next to it, then renamed over it.
fn write_state(dir: &Path, tag: &Tag, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let name = state_file_name(tag);
    let temporary = dir.join(format!("{name}{TEMP_SUFFIX}"));
    let written = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options.open(&temporary)?.write_all(bytes)?;
        fs::rename(&temporary, dir.join(&name))
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

fn remove_state(dir: &Path, tag: &Tag) {
    let _ = fs::remove_file(dir.join(state_file_name(tag)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn state_bytes_round_trip_and_bad_ones_are_refused() {
        let bytes = encode_state("앨리스", b"snapshot");
        assert_eq!(&bytes[..4], b"CNP1");
        assert_eq!(&bytes[4..6], &9u16.to_be_bytes());
        assert_eq!(
            decode_state(&bytes),
            Some(StateFile {
                alias: "앨리스",
                snapshot: b"snapshot"
            })
        );
        assert_eq!(decode_state(&encode_state("", b"")).unwrap().alias, "");
        for bad in [
            &b""[..],
            b"CNP",
            b"CNS2\0\0",
            b"CNP1\0",
            b"CNP1\0\x05abc",
            b"CNP1\0\x01\xff",
        ] {
            assert_eq!(decode_state(bad), None, "{bad:?}");
        }
        let long = "가".repeat(30_000); // 90,000 bytes
        let cut = encode_state(&long, b"s");
        let decoded = decode_state(&cut).unwrap();
        assert_eq!(decoded.alias, "가".repeat(21_845));
        assert_eq!(decoded.snapshot, b"s");
    }

    #[test]
    fn file_name_is_the_hex_tag() {
        let mut tag = [0u8; 16];
        tag[0] = 0xab;
        tag[15] = 0x01;
        assert_eq!(
            state_file_name(&tag),
            "ab000000000000000000000000000001.state"
        );
    }

    #[test]
    fn changes_are_recorded_only_while_recording() {
        changed([1; 32]);
        changed_all();
        let ((), changes) = recording(|| {
            changed([2; 32]);
            changed([2; 32]);
        });
        assert_eq!(changes.conversations, BTreeSet::from([[2; 32]]));
        assert!(!changes.all);
        let ((), changes) = recording(changed_all);
        assert!(changes.all && changes.conversations.is_empty());
        changed([3; 32]);
        let ((), changes) = recording(|| {});
        assert_eq!(changes, Changes::default());
    }

    struct Source {
        tags: BTreeMap<[u8; 32], (Tag, bool)>,
    }
    impl StateSource for Source {
        fn target(&self, conversation: [u8; 32]) -> Option<(Tag, bool)> {
            self.tags.get(&conversation).copied()
        }
        fn live(&self) -> Option<Vec<([u8; 32], Tag)>> {
            Some(
                self.tags
                    .iter()
                    .filter(|(_, (_, live))| *live)
                    .map(|(c, (tag, _))| (*c, *tag))
                    .collect(),
            )
        }
        fn contents(&self, conversation: [u8; 32]) -> Option<Vec<u8>> {
            Some(encode_state("a", &conversation))
        }
    }

    #[test]
    fn refresh_writes_live_files_and_removes_others() {
        let dir = tempfile::tempdir().unwrap();
        let push = dir.path().join(PUSH_DIR);
        let mut source = Source {
            tags: BTreeMap::from([([1; 32], ([1; 16], true)), ([2; 32], ([2; 16], false))]),
        };
        let changes = Changes {
            conversations: BTreeSet::from([[1; 32], [2; 32], [3; 32]]),
            all: false,
        };
        refresh(&source, &push, &changes);
        let one = push.join(state_file_name(&[1; 16]));
        assert_eq!(fs::read(&one).unwrap(), encode_state("a", &[1; 32]));
        assert!(!push.join(state_file_name(&[2; 16])).exists());
        source.tags.insert([1; 32], ([1; 16], false));
        refresh(&source, &push, &changes);
        assert!(!one.exists());
    }

    #[test]
    fn sync_all_prunes_stale_files_and_writes_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let push = dir.path().join(PUSH_DIR);
        fs::create_dir_all(&push).unwrap();
        let stale = push.join(state_file_name(&[9; 16]));
        let temporary = push.join(format!("{}{TEMP_SUFFIX}", state_file_name(&[1; 16])));
        let inbox = push.join("inbox.jsonl");
        for path in [&stale, &temporary, &inbox] {
            fs::write(path, b"x").unwrap();
        }
        let kept = push.join(state_file_name(&[3; 16]));
        fs::write(&kept, b"old").unwrap();
        let source = Source {
            tags: BTreeMap::from([([1; 32], ([1; 16], true)), ([3; 32], ([3; 16], true))]),
        };
        sync_all(&source, &push);
        assert!(!stale.exists() && !temporary.exists());
        assert!(inbox.exists(), "the extension's inbox is not a state file");
        assert_eq!(
            fs::read(push.join(state_file_name(&[1; 16]))).unwrap(),
            encode_state("a", &[1; 32])
        );
        assert_eq!(fs::read(&kept).unwrap(), b"old", "an existing file is left");
    }
}
