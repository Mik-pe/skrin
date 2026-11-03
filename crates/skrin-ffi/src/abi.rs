//! All foreign memory access lives here. C owns its buffers and handle slots.

use crate::store::{Inner, Write};
use crate::{Failure, Result};
use skrin::Database;
use std::ffi::c_char;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::ptr;
use std::sync::Arc;

macro_rules! statuses {
    ($($name:ident = $value:literal: $doc:literal),+ $(,)?) => {$(
        #[doc = $doc]
        pub const $name: i32 = $value;
    )+};
}
statuses! {
    SKRIN_OK = 0: "Successful operation; only commit acknowledges a write.",
    SKRIN_NOT_FOUND = 1: "The key does not exist in this read view.",
    SKRIN_INVALID_ARGUMENT = 2: "Invalid argument or operation for this backend.",
    SKRIN_BUFFER_TOO_SMALL = 3: "Output length reports required capacity; nothing was copied.",
    SKRIN_BUSY = 4: "Another writer, maintenance operation or process owns the database.",
    SKRIN_DUPLICATE_KEY = 5: "Insert refused an existing key.",
    SKRIN_LIMIT_EXCEEDED = 6: "A documented encoded-size or resource limit was exceeded.",
    SKRIN_IO_ERROR = 7: "I/O error outside an uncertain publication.",
    SKRIN_COMMIT_UNCERTAIN = 8: "Commit may have persisted; poisoned handle requires reopen.",
    SKRIN_MAINTENANCE_UNCERTAIN = 9: "Maintenance may have published; close and reopen.",
    SKRIN_POISONED = 10: "The handle is poisoned and must be closed and reopened.",
    SKRIN_CORRUPT = 11: "Complete corruption was refused without repair.",
    SKRIN_UNSUPPORTED_FORMAT = 12: "Unsupported engine storage format.",
    SKRIN_SCHEMA_MISMATCH = 13: "The directory does not have the byte adapter's schema.",
    SKRIN_UNSUPPORTED_PLATFORM = 14: "Persistent storage is unsupported on this platform.",
    SKRIN_PANIC = 15: "An unwinding panic was caught; the affected database is poisoned.",
    SKRIN_ENGINE_ERROR = 16: "Another engine error; consult the diagnostic message.",
}

/// C ABI v1 error storage. Messages are diagnostic, UTF-8 and NUL terminated.
#[repr(C)]
pub struct SkrinError {
    /// Status returned by this call; zero after success.
    pub code: i32,
    /// Up to 255 UTF-8 bytes followed by NUL.
    pub message: [c_char; 256],
}
impl Default for SkrinError {
    fn default() -> Self {
        Self {
            code: 0,
            message: [0; 256],
        }
    }
}

/// Committed state inspection, matching `skrin_stats` in the C header.
#[repr(C)]
#[derive(Default)]
pub struct SkrinStats {
    /// Live row count.
    pub rows: u64,
    /// Last committed sequence.
    pub sequence: u64,
    /// Current WAL byte length.
    pub wal_bytes: u64,
    /// Torn suffix bytes repaired during this open.
    pub recovered_tail_bytes: u64,
    /// One for a persistent database, zero for volatile memory.
    pub persistent: u32,
    /// Reserved; always zero in ABI v1.
    pub reserved: u32,
}

/// Explicit reclamation accounting; unknown contents are preserved.
#[repr(C)]
#[derive(Default)]
pub struct SkrinReclaimReport {
    /// Recognized generations completely removed.
    pub generations_removed: u64,
    /// Valid temporary manifests removed.
    pub temporary_manifests_removed: u64,
    /// Logical file lengths removed, not physical allocation reclaimed.
    pub bytes_removed: u64,
    /// Unknown-content candidates preserved.
    pub skipped: u64,
}

/// Opaque database owner. Only the C functions may inspect or free it.
pub struct SkrinDb {
    inner: Arc<Inner>,
}
/// Opaque owned write transaction. Calls must be externally serialized.
pub struct SkrinWrite {
    inner: Write,
}

macro_rules! export {
    ($(#[$attr:meta])* fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $body:block) => {
        $(#[$attr])*
        ///
        /// # Safety
        /// Follow `include/skrin.h`: handles must be live library-owned handles;
        /// non-NULL pointers must be aligned, valid for the declared sizes and
        /// access, and nonoverlapping. Output handle slots must be initialized.
        /// Do not race close with use or use one write handle concurrently.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name($($arg: $ty),*) -> i32 $body
    };
}

// These helpers only create references for the current foreign call. C must
// keep their memory alive and exclusive/shared as documented for that call.
unsafe fn reference<'a, T>(value: *const T) -> Result<&'a T> {
    // SAFETY: caller guarantees live aligned T when non-NULL; NULL is refused.
    unsafe { value.as_ref() }.ok_or_else(|| Failure::argument("NULL required pointer"))
}
unsafe fn mutable<'a, T>(value: *mut T) -> Result<&'a mut T> {
    // SAFETY: caller guarantees exclusive live aligned T; NULL is refused.
    unsafe { value.as_mut() }.ok_or_else(|| Failure::argument("NULL required output or handle"))
}
// Foreign output objects need not already contain initialized Rust values.
// Initialize through the raw pointer before creating any Rust reference.
unsafe fn initialize<'a, T>(out: *mut T, value: T) -> Result<&'a mut T> {
    if out.is_null() {
        return Err(Failure::argument("NULL required output"));
    }
    // SAFETY: C supplies aligned, exclusive writable storage for one T. Writing
    // does not read its old/uninitialized bytes and establishes a valid T.
    unsafe { out.write(value) };
    // SAFETY: the same storage now contains the valid value written above.
    Ok(unsafe { &mut *out })
}
unsafe fn empty_slot<'a, T>(out: *mut *mut T) -> Result<&'a mut *mut T> {
    // SAFETY: caller supplies an initialized, exclusive handle output slot.
    let out = unsafe { mutable(out)? };
    if !out.is_null() {
        return Err(Failure::argument("output handle must be NULL"));
    }
    Ok(out)
}
unsafe fn take<T>(slot: *mut *mut T) -> Result<Option<Box<T>>> {
    // SAFETY: caller supplies an exclusive initialized slot with a live handle.
    let slot = unsafe { mutable(slot)? };
    let pointer = std::mem::replace(slot, ptr::null_mut());
    if pointer.is_null() {
        return Ok(None);
    }
    // SAFETY: non-NULL handle came from Box::into_raw in this library and the
    // caller surrenders its sole ownership; the cleared slot prevents reuse.
    Ok(Some(unsafe { Box::from_raw(pointer) }))
}
unsafe fn database(db: *const SkrinDb) -> Result<Arc<Inner>> {
    // SAFETY: a live database handle is borrowed for this call, not closed.
    Ok(Arc::clone(&unsafe { reference(db)? }.inner))
}
unsafe fn input<'a>(data: *const u8, len: usize) -> Result<&'a [u8]> {
    if len > isize::MAX as usize {
        return Err(Failure::argument(
            "input length exceeds addressable slice size",
        ));
    }
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() {
        return Err(Failure::argument("NULL nonempty input"));
    }
    // SAFETY: C guarantees len readable initialized bytes in one live allocation.
    Ok(unsafe { std::slice::from_raw_parts(data, len) })
}
unsafe fn decode_path(data: *const u8, len: usize) -> Result<PathBuf> {
    // SAFETY: the caller provides path bytes for this call.
    let bytes = unsafe { input(data, len)? };
    let path = std::str::from_utf8(bytes).map_err(|_| Failure::argument("path must be UTF-8"))?;
    if path.is_empty() || bytes.contains(&0) {
        return Err(Failure::argument("path must be nonempty without NULs"));
    }
    Ok(PathBuf::from(path))
}

struct Output<'a> {
    bytes: *mut u8,
    capacity: usize,
    len: &'a mut usize,
}
impl Output<'_> {
    unsafe fn new<'a>(out: *mut u8, capacity: usize, out_len: *mut usize) -> Result<Output<'a>> {
        // SAFETY: required length output is exclusive, live and nonoverlapping.
        let len = unsafe { initialize(out_len, 0)? };
        if capacity > isize::MAX as usize || (capacity != 0 && out.is_null()) {
            return Err(Failure::argument("invalid output buffer"));
        }
        Ok(Output {
            bytes: out,
            capacity,
            len,
        })
    }
    fn copy(&mut self, value: Option<&[u8]>) -> Result<()> {
        let value = value.ok_or_else(|| Failure::new(SKRIN_NOT_FOUND, "key not found"))?;
        *self.len = value.len();
        if self.capacity < value.len() {
            return Err(Failure::new(
                SKRIN_BUFFER_TOO_SMALL,
                "output buffer is too small",
            ));
        }
        if !value.is_empty() {
            // SAFETY: the constructor's C contract guarantees exclusive writable
            // output storage, disjoint from the live initialized engine value.
            // Capacity was checked above. A C output buffer may be uninitialized:
            // copy through its raw pointer, never form an uninitialized &[u8].
            unsafe { ptr::copy_nonoverlapping(value.as_ptr(), self.bytes, value.len()) };
        }
        Ok(())
    }
}

unsafe fn invoke(error: *mut SkrinError, action: impl FnOnce() -> Result<()>) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(action))
        .unwrap_or_else(|_| Err(Failure::new(SKRIN_PANIC, "Rust panic at the C boundary")));
    let mut output = SkrinError::default();
    if let Err(failure) = result {
        output.code = failure.code;
        let mut end = failure.message.len().min(output.message.len() - 1);
        while !failure.message.is_char_boundary(end) {
            end -= 1;
        }
        for (out, byte) in output
            .message
            .iter_mut()
            .zip(failure.message[..end].bytes())
        {
            *out = if byte == 0 {
                b'?' as c_char
            } else {
                byte as c_char
            };
        }
    }
    let code = output.code;
    if !error.is_null() {
        // SAFETY: caller supplies one exclusive writable error object or NULL.
        unsafe { error.write(output) };
    }
    code
}

/// Return the C ABI version, independent of the engine storage version.
#[unsafe(no_mangle)]
pub extern "C" fn skrin_c_abi_version() -> u32 {
    1
}
/// Maximum byte value length; the record codec uses four bytes for its length.
#[unsafe(no_mangle)]
pub extern "C" fn skrin_max_value_size() -> usize {
    skrin::codec::MAX_RECORD_BYTES - 4
}

export! {
    /// Create an explicitly volatile byte-record database.
    fn skrin_db_in_memory(out_db: *mut *mut SkrinDb, error: *mut SkrinError) {
        // SAFETY: all foreign pointer contracts are inherited from this export.
        unsafe { invoke(error, || {
            let out = empty_slot(out_db)?;
            *out = Box::into_raw(Box::new(SkrinDb { inner: Inner::new(Database::in_memory()) }));
            Ok(())
        }) }
    }
}

unsafe fn open_database(
    data: *const u8,
    len: usize,
    out: *mut *mut SkrinDb,
    error: *mut SkrinError,
    create: bool,
) -> i32 {
    // SAFETY: supplied by the create/open exports with the header's contracts.
    unsafe {
        invoke(error, || {
            let out = empty_slot(out)?;
            let path = decode_path(data, len)?;
            let engine = if create {
                Database::create_dir(path)?
            } else {
                Database::open_dir(path)?
            };
            *out = Box::into_raw(Box::new(SkrinDb {
                inner: Inner::new(engine),
            }));
            Ok(())
        })
    }
}
export! {
    /// Create a new managed directory; never overwrite an existing path.
    fn skrin_db_create(path: *const u8, path_len: usize, out_db: *mut *mut SkrinDb, error: *mut SkrinError) {
        // SAFETY: forwarded unchanged under this function's foreign contract.
        unsafe { open_database(path, path_len, out_db, error, true) }
    }
}
export! {
    /// Open and validate an existing managed byte-record directory.
    fn skrin_db_open(path: *const u8, path_len: usize, out_db: *mut *mut SkrinDb, error: *mut SkrinError) {
        // SAFETY: forwarded unchanged under this function's foreign contract.
        unsafe { open_database(path, path_len, out_db, error, false) }
    }
}
export! {
    /// Consume and clear one database owner, allowing an already NULL handle.
    fn skrin_db_close(db: *mut *mut SkrinDb, error: *mut SkrinError) {
        // SAFETY: caller surrenders the live Box handle through an exclusive slot.
        unsafe { invoke(error, || { drop(take(db)?); Ok(()) }) }
    }
}
export! {
    /// Copy one committed value into a caller-owned buffer.
    fn skrin_db_get(db: *const SkrinDb, key: u64, out: *mut u8, capacity: usize, out_len: *mut usize, error: *mut SkrinError) {
        // SAFETY: caller owns the disjoint output buffers and live database handle.
        unsafe { invoke(error, || {
            let mut out = Output::new(out, capacity, out_len)?;
            let db = database(db)?;
            db.run(|| {
                let read = db.engine.read()?;
                out.copy(read.get(key).map(|row| row.0.as_slice()))
            })
        }) }
    }
}
export! {
    /// Inspect the committed state without retaining a read guard.
    fn skrin_db_stats(db: *const SkrinDb, out: *mut SkrinStats, error: *mut SkrinError) {
        // SAFETY: output is exclusive, live and disjoint from the database handle.
        unsafe { invoke(error, || {
            let out = initialize(out, SkrinStats::default())?;
            let db = database(db)?;
            db.run(|| {
                let stats = db.engine.stats()?;
                *out = SkrinStats { rows: stats.rows as u64, sequence: stats.commits,
                    wal_bytes: stats.wal_bytes, recovered_tail_bytes: stats.recovered_tail_bytes,
                    persistent: u32::from(stats.persistent), reserved: 0 };
                Ok(())
            })
        }) }
    }
}
export! {
    /// Reserve one writer and return owned, initially empty staging.
    fn skrin_write_begin(db: *const SkrinDb, out_write: *mut *mut SkrinWrite, error: *mut SkrinError) {
        // SAFETY: caller supplies a live database and exclusive NULL output slot.
        unsafe { invoke(error, || {
            let out = empty_slot(out_write)?;
            let db = database(db)?;
            db.run(|| {
                let tx = Write::new(db.lease()?);
                *out = Box::into_raw(Box::new(SkrinWrite { inner: tx }));
                Ok(())
            })
        }) }
    }
}
export! {
    /// Copy a value from staging or the transaction's stable committed base.
    fn skrin_write_get(write: *const SkrinWrite, key: u64, out: *mut u8, capacity: usize, out_len: *mut usize, error: *mut SkrinError) {
        // SAFETY: the handle and disjoint output buffers are live for this call.
        unsafe { invoke(error, || {
            let mut out = Output::new(out, capacity, out_len)?;
            let tx = &reference(write)?.inner;
            tx.lease.0.run(|| tx.get(key, |row| out.copy(row)))
        }) }
    }
}
unsafe fn put(
    write: *mut SkrinWrite,
    key: u64,
    value: *const u8,
    len: usize,
    error: *mut SkrinError,
    insert: bool,
) -> i32 {
    // SAFETY: forwarded from insert/put; input bytes and the handle are disjoint.
    unsafe {
        invoke(error, || {
            let tx = &mut mutable(write)?.inner;
            let db = Arc::clone(&tx.lease.0);
            db.run(|| {
                if len > skrin_max_value_size() {
                    return Err(skrin::Error::LimitExceeded {
                        limit: skrin_max_value_size(),
                    }
                    .into());
                }
                tx.put(key, input(value, len)?.to_vec(), insert)
            })
        })
    }
}
export! {
    /// Stage a new key, refusing a duplicate without changing prior staging.
    fn skrin_write_insert(write: *mut SkrinWrite, key: u64, value: *const u8, value_len: usize, error: *mut SkrinError) {
        // SAFETY: arguments retain this export's input/handle contracts.
        unsafe { put(write, key, value, value_len, error, true) }
    }
}
export! {
    /// Explicitly stage insertion or replacement; input bytes are copied.
    fn skrin_write_put(write: *mut SkrinWrite, key: u64, value: *const u8, value_len: usize, error: *mut SkrinError) {
        // SAFETY: arguments retain this export's input/handle contracts.
        unsafe { put(write, key, value, value_len, error, false) }
    }
}
export! {
    /// Stage deletion; report whether the key existed in this transaction view.
    fn skrin_write_remove(write: *mut SkrinWrite, key: u64, out_removed: *mut u32, error: *mut SkrinError) {
        // SAFETY: write is exclusive; out_removed is a live disjoint output.
        unsafe { invoke(error, || {
            let removed = initialize(out_removed, 0)?;
            let tx = &mut mutable(write)?.inner;
            let db = Arc::clone(&tx.lease.0);
            db.run(|| { *removed = u32::from(tx.remove(key)?); Ok(()) })
        }) }
    }
}
export! {
    /// Consume and clear a transaction, then commit through the production engine.
    fn skrin_write_commit(write: *mut *mut SkrinWrite, error: *mut SkrinError) {
        // SAFETY: caller surrenders the sole transaction owner via its slot.
        unsafe { invoke(error, || {
            let tx = take(write)?.ok_or_else(|| Failure::argument("NULL transaction"))?;
            tx.inner.commit()
        }) }
    }
}
export! {
    /// Consume and clear staging without publication; NULL handles are allowed.
    fn skrin_write_abort(write: *mut *mut SkrinWrite, error: *mut SkrinError) {
        // SAFETY: caller surrenders the sole transaction owner via its slot.
        unsafe { invoke(error, || { drop(take(write)?); Ok(()) }) }
    }
}
export! {
    /// Publish a verified checkpoint under exclusive writer admission.
    fn skrin_db_checkpoint(db: *const SkrinDb, error: *mut SkrinError) {
        // SAFETY: the database handle is live and not concurrently closed.
        unsafe { invoke(error, || {
            let db = database(db)?;
            db.run(|| { let _lease = db.lease()?; db.engine.checkpoint()?; Ok(()) })
        }) }
    }
}
export! {
    /// Reclaim recognized generations/manifests and report preserved candidates.
    fn skrin_db_reclaim(db: *const SkrinDb, out: *mut SkrinReclaimReport, error: *mut SkrinError) {
        // SAFETY: out is exclusive live report storage, disjoint from the handle.
        unsafe { invoke(error, || {
            let out = initialize(out, SkrinReclaimReport::default())?;
            let db = database(db)?;
            db.run(|| {
                let _lease = db.lease()?;
                let report = db.engine.reclaim()?;
                *out = SkrinReclaimReport { generations_removed: report.generations_removed,
                    temporary_manifests_removed: report.temporary_manifests_removed,
                    bytes_removed: report.bytes_removed, skipped: report.skipped };
                Ok(())
            })
        }) }
    }
}
export! {
    /// Create and independently verify a new managed backup directory.
    fn skrin_db_backup(db: *const SkrinDb, path: *const u8, path_len: usize, error: *mut SkrinError) {
        // SAFETY: the live handle and readable path obey this export's contracts.
        unsafe { invoke(error, || {
            let path = decode_path(path, path_len)?;
            let db = database(db)?;
            db.run(|| { let _lease = db.lease()?; drop(db.engine.backup_to(path)?); Ok(()) })
        }) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn error_messages_are_bounded_utf8_and_panics_do_not_cross_the_abi() {
        let mut error = SkrinError::default();
        // SAFETY: the optional error output is exclusive live storage.
        let status = unsafe {
            invoke(&mut error, || {
                Err(Failure::new(SKRIN_IO_ERROR, "å".repeat(300)))
            })
        };
        assert_eq!(status, SKRIN_IO_ERROR);
        let bytes: Vec<u8> = error
            .message
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        assert_eq!(bytes.len(), 254);
        assert!(std::str::from_utf8(&bytes).is_ok());
        assert_eq!(error.message[255], 0);
        // SAFETY: unwinding is contained; error points to exclusive live storage.
        let status = unsafe { invoke(&mut error, || panic!("boundary panic")) };
        assert_eq!(status, SKRIN_PANIC);
        assert_eq!(error.code, SKRIN_PANIC);
    }
}
