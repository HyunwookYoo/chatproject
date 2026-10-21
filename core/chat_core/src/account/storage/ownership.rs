use crate::{CoreError, ErrorCode};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

pub(crate) struct Ownership {
    pub(crate) root: PathBuf,
    held: Option<File>,
    short_lived: bool,
    suspended: bool,
}

fn acquire(root: &Path) -> Result<File, CoreError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".account.lock"))
        .map_err(|_| CoreError::new(ErrorCode::StorageLocked, "Cannot open account lock"))?;
    #[cfg(target_os = "android")]
    {
        use std::os::fd::AsRawFd;
        // Rust 1.92's File::try_lock is unsupported on Android. Bionic's flock
        // locks this borrowed descriptor; retaining File retains ownership.
        // SAFETY: file owns a valid open descriptor throughout this call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let code = if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                ErrorCode::WriterBusy
            } else {
                ErrorCode::StorageLocked
            };
            return Err(CoreError::new(
                code,
                "Cannot acquire account writer ownership",
            ));
        }
    }
    #[cfg(not(target_os = "android"))]
    file.try_lock().map_err(|e| {
        let code = match e {
            std::fs::TryLockError::WouldBlock => ErrorCode::WriterBusy,
            std::fs::TryLockError::Error(_) => ErrorCode::StorageLocked,
        };
        CoreError::new(code, "Cannot acquire account writer ownership")
    })?;
    Ok(file)
}

impl Ownership {
    pub(crate) fn open(root: &Path) -> Result<Self, CoreError> {
        fs::create_dir_all(root).map_err(|_| {
            CoreError::new(ErrorCode::StorageMissing, "Cannot create account folder")
        })?;
        let root = fs::canonicalize(root).map_err(|_| {
            CoreError::new(ErrorCode::StorageMissing, "Cannot resolve account folder")
        })?;
        let lock = acquire(&root)?;
        let short_lived = cfg!(target_os = "ios");
        Ok(Self {
            root,
            held: if short_lived { None } else { Some(lock) },
            short_lived,
            suspended: false,
        })
    }

    pub(crate) fn with_lock<T>(
        &mut self,
        action: impl FnOnce(&Path) -> Result<T, CoreError>,
    ) -> Result<T, CoreError> {
        if self.suspended {
            return Err(CoreError::new(
                ErrorCode::StorageLocked,
                "Account storage suspended",
            ));
        }
        // Transient lock is held across all reads/writes and dropped on every return path.
        let _transient = if self.short_lived {
            Some(acquire(&self.root)?)
        } else {
            None
        };
        action(&self.root)
    }

    pub(crate) fn suspend(&mut self) {
        self.suspended = true;
        if self.short_lived {
            self.held.take();
        }
    }

    pub(crate) fn resume(&mut self) -> Result<(), CoreError> {
        if !self.suspended {
            return Ok(());
        }
        if self.short_lived {
            // Persistent owners keep their original lock through suspension and
            // failed validation; only the iOS policy needs to reacquire here.
            let _lock = acquire(&self.root)?;
        }
        self.suspended = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        Store,
        test_support::{protector, state},
    };
    use super::*;

    #[test]
    fn ios_policy_simulation_releases_between_operations_and_during_suspend() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path(), protector()).unwrap();
        // Exercise the iOS policy on the host's real File::try_lock, not native iOS evidence.
        store.ownership.short_lived = true;
        store.ownership.held.take();
        store.commit(&state(b"old")).unwrap();
        let other = acquire(dir.path()).unwrap();
        assert_eq!(store.load().err().unwrap().code, ErrorCode::WriterBusy);
        assert_eq!(
            store.commit(&state(b"blocked")).unwrap_err().code,
            ErrorCode::WriterBusy
        );
        store.suspend().unwrap();
        assert_eq!(store.load().err().unwrap().code, ErrorCode::StorageLocked);
        assert_eq!(store.resume().unwrap_err().code, ErrorCode::WriterBusy);
        drop(other);
        let lock_while_suspended = acquire(dir.path()).unwrap();
        assert_eq!(store.resume().unwrap_err().code, ErrorCode::WriterBusy);
        drop(lock_while_suspended);
        store.resume().unwrap();
        let lock_after_resume = acquire(dir.path()).unwrap();
        drop(lock_after_resume);
        assert_eq!(&*store.load().unwrap().unwrap().payload, b"old");
        let lock_after_read = acquire(dir.path()).unwrap();
        drop(lock_after_read);
        fs::write(dir.path().join("account.bin"), b"corrupt").unwrap();
        store.suspend().unwrap();
        assert_eq!(store.resume().unwrap_err().code, ErrorCode::StorageCorrupt);
        assert_eq!(store.load().err().unwrap().code, ErrorCode::StorageLocked);
        let lock_after_failed_validation = acquire(dir.path()).unwrap();
        drop(lock_after_failed_validation);
    }
}
