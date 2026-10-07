//! Required file-data allocation; never substitute sparse length extension.
use std::{fs::File, io};

pub(crate) fn reserve(file: &File, offset: u64, len: u64) -> io::Result<()> {
    crate::directory::boundary()?;
    #[cfg(all(test, target_os = "linux"))]
    faults::hit()?;
    #[cfg(target_os = "linux")]
    rustix::fs::fallocate(file, rustix::fs::FallocateFlags::KEEP_SIZE, offset, len)?;
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (file, offset, len);
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "file-data reservation requires Linux",
        ));
    }
    #[cfg(target_os = "linux")]
    crate::directory::boundary()
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) mod faults {
    use std::{cell::Cell, io};
    thread_local! {
        static FAILURE: Cell<Option<(usize, io::ErrorKind)>> = const { Cell::new(None) };
    }
    pub(crate) fn arm(after: usize, kind: io::ErrorKind) {
        FAILURE.with(|slot| slot.set(Some((after, kind))));
    }
    pub(crate) fn clear() {
        FAILURE.with(|slot| slot.set(None));
    }
    pub(super) fn hit() -> io::Result<()> {
        FAILURE.with(|slot| match slot.get() {
            Some((0, kind)) => {
                slot.set(None);
                Err(io::Error::new(kind, "injected allocation failure"))
            }
            Some((after, kind)) => {
                slot.set(Some((after - 1, kind)));
                Ok(())
            }
            None => Ok(()),
        })
    }
}
