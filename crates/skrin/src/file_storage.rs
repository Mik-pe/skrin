use super::Storage;
use crate::{Error, Result};
use std::fs::{File, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};

/// The file and its lock have one owner. Closing a descriptor alone is not
/// sufficient: an unrelated concurrent fork can temporarily inherit it and
/// keep the same open-file-description lock alive until exec.
pub(super) struct LockedFile {
    file: File,
    owner_process: u32,
}

impl LockedFile {
    pub(super) fn acquire(file: File) -> Result<Self> {
        match file.try_lock() {
            Ok(()) => Ok(Self {
                file,
                owner_process: std::process::id(),
            }),
            Err(TryLockError::WouldBlock) => Err(Error::Busy),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }
}

impl Drop for LockedFile {
    fn drop(&mut self) {
        // A forked child's cleanup must not unlock the parent's database.
        // Using an inherited database in a child before exec is unsupported.
        if self.owner_process == std::process::id() {
            // Drop cannot report errors. Closing the owned descriptor remains
            // the fallback, but explicit unlock avoids inherited-FD retention.
            let _ = self.file.unlock();
        }
    }
}

impl Read for LockedFile {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.file.read(bytes)
    }
}

impl Write for LockedFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for LockedFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.file.seek(position)
    }
}

impl Storage for LockedFile {
    fn size(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    fn truncate(&mut self, len: u64) -> io::Result<()> {
        self.file.set_len(len)
    }

    fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::log::Wal;
    use crate::test_support::Item;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestFile(PathBuf);

    impl TestFile {
        fn create() -> (Self, File) {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            for _ in 0..100 {
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir()
                    .join(format!("skrin-lock-{}-{stamp}-{id}", std::process::id()));
                match File::create_new(&path) {
                    Ok(file) => return (Self(path), file),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create lock test file: {error}"),
                }
            }
            panic!("test file collision limit")
        }

        fn another_handle(&self) -> File {
            File::options()
                .read(true)
                .write(true)
                .open(&self.0)
                .unwrap()
        }
    }

    impl Drop for TestFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn owner_drop_unlocks_even_while_a_duplicate_descriptor_survives() {
        let (path, file) = TestFile::create();
        let locked = LockedFile::acquire(file).unwrap();
        // A duplicate deterministically models the shared open-file description
        // inherited between fork and exec, without unsafe fork code in tests.
        let inherited = locked.file.try_clone().unwrap();
        let next = path.another_handle();
        assert!(matches!(next.try_lock(), Err(TryLockError::WouldBlock)));
        drop(locked);
        next.try_lock().expect("the former owner must release its lock");
        drop(inherited);
        let third = path.another_handle();
        assert!(matches!(third.try_lock(), Err(TryLockError::WouldBlock)));
        next.unlock().unwrap();
    }

    #[test]
    fn failed_recovery_also_releases_the_owned_lock() {
        let (path, file) = TestFile::create();
        let locked = LockedFile::acquire(file).unwrap();
        let inherited = locked.file.try_clone().unwrap();
        // An empty file is deliberately invalid. The error path must drop the
        // lock owner even though it never constructs a successfully opened WAL.
        assert!(matches!(
            Wal::recover::<Item>(Box::new(locked)),
            Err(Error::Corrupt { .. })
        ));
        let next = path.another_handle();
        next.try_lock().expect("failed open must release its lock");
        drop(inherited);
        next.unlock().unwrap();
    }

    #[test]
    fn inherited_child_cleanup_does_not_unlock_the_parent_owner() {
        let (path, file) = TestFile::create();
        let mut locked = LockedFile::acquire(file).unwrap();
        let parent = locked.file.try_clone().unwrap();
        // Model cleanup in a different process; PID 0 cannot be this process.
        locked.owner_process = 0;
        drop(locked);
        let next = path.another_handle();
        assert!(matches!(next.try_lock(), Err(TryLockError::WouldBlock)));
        parent.unlock().unwrap();
        next.try_lock().unwrap();
        next.unlock().unwrap();
    }
}
