use skrin_ffi::*;
use std::ptr;

struct Db(*mut SkrinDb);
impl Db {
    fn memory() -> Self {
        let mut db = ptr::null_mut();
        // SAFETY: the initialized output slot is exclusive and error is optional.
        assert_eq!(
            unsafe { skrin_db_in_memory(&mut db, ptr::null_mut()) },
            SKRIN_OK
        );
        Self(db)
    }
    fn begin(&self) -> Tx {
        let mut tx = ptr::null_mut();
        // SAFETY: database is live and the initialized output slot is exclusive.
        assert_eq!(
            unsafe { skrin_write_begin(self.0, &mut tx, ptr::null_mut()) },
            SKRIN_OK
        );
        Tx(tx)
    }
    fn stats(&self) -> SkrinStats {
        let mut out = SkrinStats::default();
        // SAFETY: live handle, exclusive disjoint output, no concurrent close.
        assert_eq!(
            unsafe { skrin_db_stats(self.0, &mut out, ptr::null_mut()) },
            SKRIN_OK
        );
        out
    }
    fn read(&self, key: u64) -> (i32, Vec<u8>) {
        let mut bytes = vec![0; 64];
        let mut len = usize::MAX;
        // SAFETY: live handle and disjoint writable output/length allocations.
        let status = unsafe {
            skrin_db_get(
                self.0,
                key,
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut len,
                ptr::null_mut(),
            )
        };
        bytes.truncate(len);
        (status, bytes)
    }
}
impl Drop for Db {
    fn drop(&mut self) {
        // SAFETY: this owner releases its sole handle; NULL after prior close is valid.
        unsafe {
            skrin_db_close(&mut self.0, ptr::null_mut());
        }
    }
}
struct Tx(*mut SkrinWrite);
impl Tx {
    fn put(&mut self, key: u64, value: &[u8]) -> i32 {
        // SAFETY: write handle is exclusively borrowed; bytes are live for the call.
        unsafe { skrin_write_put(self.0, key, value.as_ptr(), value.len(), ptr::null_mut()) }
    }
    fn insert(&mut self, key: u64, value: &[u8]) -> i32 {
        // SAFETY: write handle is exclusively borrowed; input is disjoint and live.
        unsafe { skrin_write_insert(self.0, key, value.as_ptr(), value.len(), ptr::null_mut()) }
    }
    fn commit(&mut self) -> i32 {
        // SAFETY: exclusive initialized slot surrenders its handle, clearing it.
        unsafe { skrin_write_commit(&mut self.0, ptr::null_mut()) }
    }
}
impl Drop for Tx {
    fn drop(&mut self) {
        // SAFETY: sole transaction owner; NULL after a commit is allowed.
        unsafe {
            skrin_write_abort(&mut self.0, ptr::null_mut());
        }
    }
}

#[test]
fn staging_committed_reads_duplicate_errors_and_empty_values() {
    let db = Db::memory();
    let mut tx = db.begin();
    let value = b"text\0\xc3\xa5\xf0\x9f\xa6\x80\xff";
    assert_eq!(tx.insert(42, value), SKRIN_OK);
    assert_eq!(tx.insert(42, b"duplicate"), SKRIN_DUPLICATE_KEY);
    assert_eq!(tx.insert(7, b""), SKRIN_OK);
    assert_eq!(db.read(42), (SKRIN_NOT_FOUND, vec![]));
    let mut out = [0; 64];
    let mut len = 0;
    // SAFETY: live write handle and disjoint outputs; no operation overlaps it.
    assert_eq!(
        unsafe {
            skrin_write_get(
                tx.0,
                42,
                out.as_mut_ptr(),
                out.len(),
                &mut len,
                ptr::null_mut(),
            )
        },
        SKRIN_OK
    );
    assert_eq!(&out[..len], value);
    let mut second = ptr::null_mut();
    // SAFETY: database remains live and second is an empty exclusive slot.
    assert_eq!(
        unsafe { skrin_write_begin(db.0, &mut second, ptr::null_mut()) },
        SKRIN_BUSY
    );
    assert!(second.is_null());
    // SAFETY: database is live; maintenance cannot enter the active writer.
    assert_eq!(
        unsafe { skrin_db_checkpoint(db.0, ptr::null_mut()) },
        SKRIN_BUSY
    );
    assert_eq!(tx.commit(), SKRIN_OK);
    assert!(tx.0.is_null());
    assert_eq!(db.read(42), (SKRIN_OK, value.to_vec()));
    assert_eq!(db.read(7), (SKRIN_OK, vec![]));
    assert_eq!((db.stats().rows, db.stats().sequence), (2, 1));
    let mut tx = db.begin();
    assert_eq!(tx.put(42, b"abort"), SKRIN_OK);
    drop(tx);
    assert_eq!(db.read(42), (SKRIN_OK, value.to_vec()));
}

#[test]
fn sizing_null_validation_and_error_outputs_are_unambiguous() {
    let db = Db::memory();
    let mut tx = db.begin();
    assert_eq!(tx.put(1, b"abc"), SKRIN_OK);
    assert_eq!(tx.commit(), SKRIN_OK);
    let mut len = 9;
    let mut error = SkrinError::default();
    // SAFETY: NULL/0 is a valid size probe with exclusive length/error outputs.
    assert_eq!(
        unsafe { skrin_db_get(db.0, 1, ptr::null_mut(), 0, &mut len, &mut error) },
        SKRIN_BUFFER_TOO_SMALL
    );
    assert_eq!((len, error.code), (3, SKRIN_BUFFER_TOO_SMALL));
    assert_eq!(error.message[255], 0);
    let mut small = [99; 2];
    // SAFETY: initialized live output memory is disjoint from the database/error.
    assert_eq!(
        unsafe { skrin_db_get(db.0, 1, small.as_mut_ptr(), 2, &mut len, &mut error) },
        SKRIN_BUFFER_TOO_SMALL
    );
    assert_eq!(small, [99; 2]);
    // SAFETY: invalid NULL nonempty output is diagnosed without dereferencing it.
    assert_eq!(
        unsafe { skrin_db_get(db.0, 1, ptr::null_mut(), 1, &mut len, &mut error) },
        SKRIN_INVALID_ARGUMENT
    );
    assert_eq!(len, 0);
    // SAFETY: a missing key and NULL/0 output are valid; error is an owned output.
    assert_eq!(
        unsafe { skrin_db_get(db.0, 2, ptr::null_mut(), 0, &mut len, &mut error) },
        SKRIN_NOT_FOUND
    );
    let mut full = [0; 3];
    // SAFETY: the live disjoint output has enough space for the complete value.
    assert_eq!(
        unsafe { skrin_db_get(db.0, 1, full.as_mut_ptr(), 3, &mut len, &mut error) },
        SKRIN_OK
    );
    assert_eq!(full, *b"abc");
    assert_eq!(error.code, 0);
    assert!(error.message.iter().all(|&c| c == 0));
    let mut another = db.0;
    // SAFETY: this is a borrowed non-NULL alias used only to verify slot refusal;
    // ownership stays with db, and no destructor is called on the alias.
    assert_eq!(
        unsafe { skrin_db_in_memory(&mut another, &mut error) },
        SKRIN_INVALID_ARGUMENT
    );
    assert_eq!(another, db.0);
    assert_eq!(skrin_c_abi_version(), 1);
    assert_eq!(skrin_max_value_size(), 8 * 1024 * 1024 - 4);
}

#[test]
fn c_output_objects_and_byte_buffers_may_start_uninitialized() {
    use std::mem::MaybeUninit;
    let db = Db::memory();
    let mut tx = db.begin();
    assert_eq!(tx.put(1, b"abc"), SKRIN_OK);
    assert_eq!(tx.commit(), SKRIN_OK);
    let mut stats = MaybeUninit::<SkrinStats>::uninit();
    let mut error = MaybeUninit::<SkrinError>::uninit();
    // SAFETY: the outputs are writable/aligned, but deliberately uninitialized,
    // matching ordinary C stack outputs. The API initializes them through raw writes.
    assert_eq!(
        unsafe { skrin_db_stats(db.0, stats.as_mut_ptr(), error.as_mut_ptr()) },
        SKRIN_OK
    );
    // SAFETY: successful calls fully initialized both POD output objects.
    let (stats, error) = unsafe { (stats.assume_init(), error.assume_init()) };
    assert_eq!((stats.rows, stats.sequence, error.code), (1, 1, 0));
    let mut bytes = [MaybeUninit::<u8>::uninit(); 16];
    let mut len = MaybeUninit::<usize>::uninit();
    // SAFETY: disjoint aligned writable outputs; initialized contents are not
    // required and the library may only initialize the returned byte prefix.
    assert_eq!(
        unsafe {
            skrin_db_get(
                db.0,
                1,
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                len.as_mut_ptr(),
                ptr::null_mut(),
            )
        },
        SKRIN_OK
    );
    // SAFETY: the returned length was initialized and exactly its byte prefix
    // was copied. The remaining output capacity is intentionally never read.
    unsafe {
        assert_eq!(len.assume_init(), 3);
        assert_eq!(
            bytes[..3]
                .iter()
                .map(|b| b.assume_init())
                .collect::<Vec<_>>(),
            b"abc"
        );
    }
}

#[test]
fn transaction_retains_owner_and_insert_delete_coalesces_to_no_commit() {
    let db = Db::memory();
    let mut tx = db.begin();
    assert_eq!(tx.put(u64::MAX, b"max"), SKRIN_OK);
    let mut removed = 0;
    // SAFETY: write is exclusively used and the scalar output is disjoint.
    assert_eq!(
        unsafe { skrin_write_remove(tx.0, u64::MAX, &mut removed, ptr::null_mut()) },
        SKRIN_OK
    );
    assert_eq!(removed, 1);
    assert_eq!(tx.commit(), SKRIN_OK);
    assert_eq!(db.stats().sequence, 0);
    let mut tx = db.begin();
    assert_eq!(tx.put(u64::MAX, b"owned"), SKRIN_OK);
    drop(db); // The transaction's Arc keeps its engine alive.
    assert_eq!(tx.commit(), SKRIN_OK);
    assert!(tx.0.is_null());
    assert_eq!(tx.commit(), SKRIN_INVALID_ARGUMENT);
}

#[test]
fn concurrent_database_calls_share_one_writer_admission() {
    let db = Db::memory();
    // SAFETY: db stays alive until all scoped threads join; it is not closed or
    // mutably borrowed while this shared handle reference is used.
    let handle = unsafe { &*db.0 };
    std::thread::scope(|scope| {
        for key in 0..4 {
            scope.spawn(move || {
                for _ in 0..50 {
                    let mut raw = ptr::null_mut();
                    loop {
                        // SAFETY: shared live database, per-thread exclusive slot.
                        let status =
                            unsafe { skrin_write_begin(handle, &mut raw, ptr::null_mut()) };
                        if status == SKRIN_OK {
                            break;
                        }
                        assert_eq!(status, SKRIN_BUSY);
                        std::thread::yield_now();
                    }
                    let mut tx = Tx(raw);
                    assert_eq!(tx.put(key, &[key as u8]), SKRIN_OK);
                    assert_eq!(tx.commit(), SKRIN_OK);
                }
            });
        }
    });
    assert_eq!((db.stats().rows, db.stats().sequence), (4, 200));
}

#[cfg(unix)]
mod persistent {
    use super::*;
    use skrin::{Decoder, Encoder, Record, Schema};
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "skrin-ffi-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn image(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn collect(base: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    collect(base, &path, files);
                } else {
                    files.insert(
                        path.strip_prefix(base).unwrap().to_owned(),
                        std::fs::read(path).unwrap(),
                    );
                }
            }
        }
        let mut files = BTreeMap::new();
        collect(path, path, &mut files);
        files
    }
    fn wal(path: &Path) -> PathBuf {
        std::fs::read_dir(path)
            .unwrap()
            .map(|e| e.unwrap().path().join("wal"))
            .find(|p| p.is_file())
            .unwrap()
    }
    fn open(path: &Path, create: bool) -> (i32, *mut SkrinDb) {
        let path = path.to_str().unwrap();
        let mut db = ptr::null_mut();
        // SAFETY: live UTF-8 bytes and an exclusive initialized NULL output slot.
        let code = unsafe {
            if create {
                skrin_db_create(path.as_ptr(), path.len(), &mut db, ptr::null_mut())
            } else {
                skrin_db_open(path.as_ptr(), path.len(), &mut db, ptr::null_mut())
            }
        };
        (code, db)
    }

    struct Other;
    impl Record for Other {
        const SCHEMA: Schema = Schema {
            table_id: 7,
            version: 1,
        };
        fn encode(&self, e: &mut Encoder) -> skrin::Result<()> {
            e.u64(42)
        }
        fn decode(d: &mut Decoder<'_>) -> skrin::Result<Self> {
            d.u64()?;
            Ok(Self)
        }
    }
    #[test]
    fn wrong_schema_and_complete_corruption_refuse_without_mutation() {
        let scratch = Scratch::new();
        let foreign = scratch.0.join("foreign");
        let db = skrin::Database::<Other>::create_dir(&foreign).unwrap();
        db.write(|tx| tx.insert(1, Other)).unwrap();
        drop(db);
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(wal(&foreign))
            .unwrap();
        log.write_all(b"TXN1").unwrap();
        log.sync_all().unwrap();
        drop(log);
        let before = image(&foreign);
        let (status, db) = open(&foreign, false);
        assert_eq!(status, SKRIN_SCHEMA_MISMATCH);
        assert!(db.is_null());
        assert_eq!(image(&foreign), before); // The otherwise repairable tail is untouched.

        let corrupt = scratch.0.join("corrupt");
        let (status, pointer) = open(&corrupt, true);
        assert_eq!(status, SKRIN_OK);
        let db = Db(pointer);
        let mut tx = db.begin();
        assert_eq!(tx.put(42, b"complete"), SKRIN_OK);
        assert_eq!(tx.commit(), SKRIN_OK);
        drop(tx);
        drop(db);
        let log = wal(&corrupt);
        let mut bytes = std::fs::read(&log).unwrap();
        *bytes.last_mut().unwrap() ^= 1; // A complete trailer is corrupt, not torn.
        std::fs::write(&log, bytes).unwrap();
        let before = image(&corrupt);
        let (status, db) = open(&corrupt, false);
        assert_eq!(status, SKRIN_CORRUPT);
        assert!(db.is_null());
        assert_eq!(image(&corrupt), before);
    }

    #[test]
    fn invalid_outputs_and_paths_never_create_storage() {
        let scratch = Scratch::new();
        let path = scratch.0.join("must-not-exist");
        let text = path.to_str().unwrap();
        // SAFETY: NULL required output is refused before any filesystem action.
        assert_eq!(
            unsafe { skrin_db_create(text.as_ptr(), text.len(), ptr::null_mut(), ptr::null_mut()) },
            SKRIN_INVALID_ARGUMENT
        );
        assert!(!path.exists());
        let mut db = ptr::null_mut();
        for invalid in [&b""[..], &b"\xff"[..], &b"bad\0path"[..]] {
            // SAFETY: live input bytes and exclusive initialized NULL output slot.
            assert_eq!(
                unsafe {
                    skrin_db_create(invalid.as_ptr(), invalid.len(), &mut db, ptr::null_mut())
                },
                SKRIN_INVALID_ARGUMENT
            );
            assert!(db.is_null());
        }
        assert_eq!(std::fs::read_dir(&scratch.0).unwrap().count(), 0);
    }
}
