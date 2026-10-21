use super::schema::FILE_LIMIT;
use crate::{CoreError, ErrorCode};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

pub(super) fn read_bounded(path: &Path) -> Result<Vec<u8>, CoreError> {
    read_with_limit(path, FILE_LIMIT)
}

pub(crate) fn read_with_limit(path: &Path, limit: usize) -> Result<Vec<u8>, CoreError> {
    let file = File::open(path).map_err(|_| {
        CoreError::new(
            ErrorCode::StorageMissing,
            "Account storage file unavailable",
        )
    })?;
    let size = file
        .metadata()
        .map_err(|_| CoreError::new(ErrorCode::StorageCorrupt, "Cannot inspect account file"))?
        .len();
    if size > limit as u64 {
        return Err(CoreError::new(
            ErrorCode::StorageFull,
            "Account file exceeds limit",
        ));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CoreError::new(ErrorCode::StorageCorrupt, "Cannot read account file"))?;
    if bytes.len() > limit {
        return Err(CoreError::new(
            ErrorCode::StorageFull,
            "Account file exceeds limit",
        ));
    }
    Ok(bytes)
}

fn write_error() -> CoreError {
    CoreError::new(
        ErrorCode::StorageFull,
        "Account durability operation failed",
    )
}

fn sync_file(file: &File) -> std::io::Result<()> {
    #[cfg(test)]
    tests::io_failure("sync")?;
    file.sync_all()
}

fn replace_file(temporary: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    tests::io_failure("replace")?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // Both UTF-16 buffers are NUL-terminated and valid for the duration of the call.
        if unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    std::fs::rename(temporary, destination)
}

pub(crate) fn sync_directory(root: &Path) -> Result<(), CoreError> {
    #[cfg(unix)]
    File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| write_error())?;
    // MoveFileExW WRITE_THROUGH is the Windows durability operation.
    #[cfg(not(unix))]
    let _ = root;
    Ok(())
}

pub(crate) fn replace(root: &Path, name: &str, bytes: &[u8]) -> Result<(), CoreError> {
    let temporary = root.join(format!(".{name}.tmp"));
    let destination = root.join(name);
    #[cfg(test)]
    tests::boundary(name, "before_write");
    // A prior process may have left an encrypted candidate. The fixed lock owns it.
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|_| write_error())?;
    file.write_all(bytes).map_err(|_| write_error())?;
    #[cfg(test)]
    tests::boundary(name, "after_write");
    sync_file(&file).map_err(|_| write_error())?;
    #[cfg(test)]
    tests::boundary(name, "after_sync");
    drop(file);
    #[cfg(test)]
    tests::boundary(name, "before_replace");
    replace_file(&temporary, &destination).map_err(|_| write_error())?;
    #[cfg(test)]
    tests::boundary(name, "after_replace");
    #[cfg(test)]
    tests::io_failure("after_replace").map_err(|_| write_error())?;
    sync_directory(root)?;
    #[cfg(test)]
    tests::boundary(name, "after_directory_sync");
    Ok(())
}

#[cfg(test)]
pub(crate) fn with_after_replace_failure<T>(operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<&'static str>);
    impl Drop for Restore {
        fn drop(&mut self) {
            tests::FAIL_AT.with(|failure| failure.set(self.0));
        }
    }
    let previous = tests::FAIL_AT.with(|failure| failure.replace(Some("after_replace")));
    let _restore = Restore(previous);
    operation()
}

#[cfg(test)]
mod tests {
    use super::super::{
        Store,
        test_support::{protector, state},
    };
    use std::{fs, process::Command};

    #[test]
    fn after_replace_scope_restores_previous_failure_on_return_error_and_panic() {
        for panic in [false, true] {
            FAIL_AT.with(|failure| failure.set(Some("sync")));
            let result = std::panic::catch_unwind(|| {
                super::with_after_replace_failure(|| {
                    assert!(io_failure("after_replace").is_err());
                    assert!(io_failure("sync").is_ok());
                    if panic {
                        panic!("test-only unwind");
                    }
                    Err::<(), ()>(())
                })
            });
            assert_eq!(result.is_err(), panic);
            assert!(io_failure("after_replace").is_ok());
            assert!(io_failure("sync").is_err());
            FAIL_AT.with(|failure| failure.set(None));
        }
    }

    pub(super) fn boundary(name: &str, boundary: &str) {
        let target = std::env::var("CHAT_A2_CRASH_FILE").unwrap_or_else(|_| "account.bin".into());
        if name == target && std::env::var("CHAT_A2_CRASH_BOUNDARY").as_deref() == Ok(boundary) {
            std::process::exit(86);
        }
    }

    pub(super) fn io_failure(stage: &str) -> std::io::Result<()> {
        if FAIL_AT.with(|failure| failure.get() == Some(stage)) {
            return Err(std::io::Error::other("private injected IO failure"));
        }
        Ok(())
    }

    #[test]
    fn crash_child() {
        let Some(root) = std::env::var_os("CHAT_A2_CRASH_ROOT") else {
            return;
        };
        let mut store = Store::open(std::path::Path::new(&root), protector()).unwrap();
        store.commit(&state(b"new-complete-snapshot")).unwrap();
        panic!("crash boundary was not reached");
    }

    #[test]
    fn crash_at_each_replace_boundary_keeps_complete_snapshot() {
        for (boundary, expected) in [
            ("before_write", b"old-complete-snapshot".as_slice()),
            ("after_write", b"old-complete-snapshot".as_slice()),
            ("after_sync", b"old-complete-snapshot".as_slice()),
            ("before_replace", b"old-complete-snapshot".as_slice()),
            ("after_replace", b"new-complete-snapshot".as_slice()),
            ("after_directory_sync", b"new-complete-snapshot".as_slice()),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut store = Store::open(dir.path(), protector()).unwrap();
            store.commit(&state(b"old-complete-snapshot")).unwrap();
            drop(store);
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "account::storage::atomic::tests::crash_child",
                    "--nocapture",
                ])
                .env("CHAT_A2_CRASH_ROOT", dir.path())
                .env("CHAT_A2_CRASH_BOUNDARY", boundary)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(86),
                "{boundary}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let mut reopened = Store::open(dir.path(), protector()).unwrap();
            assert_eq!(
                &*reopened.load().unwrap().unwrap().payload,
                expected,
                "{boundary}"
            );
            if let Ok(temp) = fs::read(dir.path().join(".account.bin.tmp")) {
                assert!(!temp.windows(21).any(|w| w == b"new-complete-snapshot"));
            }
            // Orphan ciphertext from the interrupted write cannot prevent future commits.
            reopened.commit(&state(b"subsequent-snapshot")).unwrap();
            assert_eq!(
                &*reopened.load().unwrap().unwrap().payload,
                b"subsequent-snapshot"
            );
        }
    }

    #[test]
    fn flush_and_replace_failures_are_errors_and_preserve_old_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), protector()).unwrap();
        store.commit(&state(b"old")).unwrap();
        // Both failures are injected privately; production has no knobs.
        for stage in ["sync", "replace"] {
            FAIL_AT.with(|failure| failure.set(Some(stage)));
            assert!(
                store.commit(&state(b"new")).is_err(),
                "{stage} failure reported success"
            );
            FAIL_AT.with(|failure| failure.set(None));
            assert_eq!(&*store.load().unwrap().unwrap().payload, b"old");
        }
    }

    #[test]
    fn initialization_crashes_remain_explicit_orphans_until_cleanup() {
        for (file, boundary) in [
            ("store.id", "after_sync"),
            ("store.id", "after_replace"),
            ("key.wrap", "before_write"),
            ("key.wrap", "after_sync"),
            ("key.wrap", "after_replace"),
            ("account.bin", "before_replace"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "account::storage::atomic::tests::crash_child",
                    "--nocapture",
                ])
                .env("CHAT_A2_CRASH_ROOT", dir.path())
                .env("CHAT_A2_CRASH_FILE", file)
                .env("CHAT_A2_CRASH_BOUNDARY", boundary)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(86), "{file}/{boundary}");
            let mut store = Store::open(dir.path(), protector()).unwrap();
            assert!(store.load().is_err(), "{file}/{boundary} became fresh");
            assert!(store.commit(&state(b"fresh")).is_err());
            store.discard_pending().unwrap();
            assert!(store.load().unwrap().is_none());
        }
    }

    thread_local! {
        pub(super) static FAIL_AT: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
    }
}
