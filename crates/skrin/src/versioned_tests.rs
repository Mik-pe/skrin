use super::*;
use crate::log::{Storage, Wal, encode_transaction, file_header};
use crate::test_support::{Item, TestStorage};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::mpsc;
use std::time::Duration;

fn options() -> SnapshotOptions {
    SnapshotOptions {
        max_snapshots: 64,
        max_pinned_bytes: 64 * 1024 * 1024,
    }
}
fn footprint(_: &Item) -> Result<u64> {
    Ok(std::mem::size_of::<Item>() as u64)
}
fn durable(disk: &TestStorage) -> Database<Item> {
    let (wal, recovered) = Wal::recover::<Item>(Box::new(disk.clone())).unwrap();
    Database::from_recovered(wal, recovered)
}
fn versioned(disk: &TestStorage) -> SnapshotDatabase<Item> {
    durable(disk).into_snapshots(options(), footprint).unwrap()
}

#[test]
fn randomized_retained_roots_ranges_and_recovery_match_independent_maps() {
    let disk = TestStorage::new(file_header(Item::SCHEMA));
    let db = versioned(&disk);
    let mut reference = BTreeMap::new();
    let mut retained = Vec::new();
    let mut random = 13u64;
    for n in 0..1000 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let key = random % 100;
        if random & 3 == 0 {
            db.write(|tx| {
                assert_eq!(tx.remove(key), reference.remove(&key).is_some());
                Ok(())
            })
            .unwrap();
        } else {
            db.write(|tx| {
                tx.put(key, Item(random));
                assert_eq!(tx.get(key).unwrap().0, random);
                Ok(())
            })
            .unwrap();
            reference.insert(key, random);
        }
        if n % 43 == 0 {
            retained.push((db.snapshot().unwrap(), reference.clone()));
        }
        let view = db.snapshot().unwrap();
        assert_eq!(
            view.range(20..=70)
                .unwrap()
                .map(|(k, r)| (k, r.0))
                .collect::<BTreeMap<_, _>>(),
            reference.range(20..=70).map(|(&k, &v)| (k, v)).collect()
        );
        assert_eq!(view.len().unwrap(), reference.len());
    }
    for (view, reference) in &retained {
        assert_eq!(
            view.iter()
                .unwrap()
                .map(|(k, r)| (k, r.0))
                .collect::<BTreeMap<_, _>>(),
            *reference
        );
    }
    drop(retained);
    let baseline = db.into_database().unwrap();
    assert_eq!(
        baseline
            .read()
            .unwrap()
            .iter()
            .map(|(k, r)| (k, r.0))
            .collect::<BTreeMap<_, _>>(),
        reference
    );
    drop(baseline);
    assert_eq!(
        durable(&disk)
            .read()
            .unwrap()
            .iter()
            .map(|(k, r)| (k, r.0))
            .collect::<BTreeMap<_, _>>(),
        reference
    );
}
#[test]
fn reader_backpressure_counts_clones_once_and_releases_capacity() {
    let db = Database::<Item>::in_memory()
        .into_snapshots(
            SnapshotOptions {
                max_snapshots: 2,
                max_pinned_bytes: u64::MAX,
            },
            footprint,
        )
        .unwrap();
    db.write(|tx| tx.insert(1, Item(10))).unwrap();
    let bytes = db.retention().unwrap().current_bytes;
    let first = db.snapshot().unwrap();
    let clone = first.clone();
    assert_eq!(db.retention().unwrap().snapshots, 1);
    db.write(|tx| tx.update(1, |_| Ok(Item(20)))).unwrap();
    let second = db.snapshot().unwrap();
    assert!(matches!(
        db.snapshot(),
        Err(Error::BudgetExceeded {
            resource: "snapshot leases",
            ..
        })
    ));
    let stats = db.retention().unwrap();
    assert_eq!(
        (
            stats.snapshots,
            stats.pinned_versions,
            stats.oldest_pinned_sequence,
            stats.pinned_bytes
        ),
        (2, 2, Some(1), bytes * 2)
    );
    for n in 0..100 {
        db.write(|tx| {
            tx.put(1, Item(n));
            Ok(())
        })
        .unwrap();
    }
    assert_eq!(db.retention().unwrap().pinned_bytes, bytes * 2);
    assert_eq!(first.get(1).unwrap().unwrap().0, 10);
    assert_eq!(second.get(1).unwrap().unwrap().0, 20);
    drop(first);
    assert_eq!(db.retention().unwrap().snapshots, 2);
    drop(clone);
    assert_eq!(db.retention().unwrap().snapshots, 1);
    assert!(db.snapshot().is_ok());
    drop(second);
    assert_eq!(db.retention().unwrap().pinned_bytes, 0);
    let baseline = db.into_database().unwrap();
    let db = baseline
        .into_snapshots(
            SnapshotOptions {
                max_snapshots: 4,
                max_pinned_bytes: bytes - 1,
            },
            footprint,
        )
        .unwrap();
    assert!(
        matches!(db.snapshot(), Err(Error::BudgetExceeded { resource: "pinned snapshot bytes", required, .. }) if required == bytes)
    );
    // Backpressure applies to read admission, never silently cancels writes.
    db.write(|tx| {
        tx.remove(1);
        Ok(())
    })
    .unwrap();
    assert!(db.snapshot().unwrap().is_empty().unwrap());
}
#[test]
fn footprint_refusal_and_callback_errors_precede_append_and_do_not_poison() {
    fn assess(row: &Item) -> Result<u64> {
        if row.0 == 99 {
            return Err(Error::InvalidOperation("unaccountable row".into()));
        }
        footprint(row)
    }
    let disk = TestStorage::new(file_header(Item::SCHEMA));
    let db = durable(&disk).into_snapshots(options(), assess).unwrap();
    let before = disk.image();
    assert!(matches!(
        db.write(|tx| tx.insert(1, Item(99))),
        Err(Error::InvalidOperation(_))
    ));
    assert!(matches!(
        db.write(|tx| {
            tx.put(1, Item(10));
            Err::<(), _>(Error::Poisoned)
        }),
        Err(Error::Poisoned)
    ));
    assert_eq!(disk.image(), before);
    assert_eq!(db.snapshot().unwrap().sequence().unwrap(), 0);
    db.write(|tx| tx.insert(1, Item(10))).unwrap();
    assert_eq!(db.snapshot().unwrap().sequence().unwrap(), 1);
    let baseline = db.into_database().unwrap();
    assert!(matches!(
        baseline.into_snapshots(options(), |_| Ok(0)),
        Err(Error::InvalidOperation(_))
    ));
}
#[test]
fn every_append_prefix_and_sync_failure_poison_existing_and_new_snapshots() {
    let frame = encode_transaction(1, &BTreeMap::from([(1, Some(Item(10)))])).unwrap();
    for full in [false, true] {
        for cutoff in 0..frame.len() {
            let disk = TestStorage::new(file_header(Item::SCHEMA));
            let db = versioned(&disk);
            let old = db.snapshot().unwrap();
            if full {
                disk.fail_enospc_after(cutoff);
            } else {
                disk.fail_write_after(cutoff);
            }
            assert!(matches!(
                db.write(|tx| tx.insert(1, Item(10))),
                Err(Error::CommitUncertain(_))
            ));
            assert!(matches!(old.get(1), Err(Error::Poisoned)));
            assert!(matches!(db.snapshot(), Err(Error::Poisoned)));
            drop(old);
            drop(db);
            disk.clear_faults();
            assert_eq!(durable(&disk).read().unwrap().sequence(), 0);
        }
        let disk = TestStorage::new(file_header(Item::SCHEMA));
        let db = versioned(&disk);
        let old = db.snapshot().unwrap();
        if full {
            disk.fail_sync_enospc();
        } else {
            disk.fail_sync();
        }
        assert!(matches!(
            db.write(|tx| tx.insert(1, Item(10))),
            Err(Error::CommitUncertain(_))
        ));
        assert!(matches!(old.sequence(), Err(Error::Poisoned)));
        drop(old);
        drop(db);
        disk.clear_faults();
        assert_eq!(durable(&disk).read().unwrap().sequence(), 1);
    }
}
#[test]
fn group_prefix_failures_recover_independent_transactions_without_root_publication() {
    let frame = encode_transaction(1, &BTreeMap::from([(1, Some(Item(10)))])).unwrap();
    for cutoff in 0..2 * frame.len() {
        let disk = TestStorage::new(file_header(Item::SCHEMA));
        let db = versioned(&disk);
        let old = db.snapshot().unwrap();
        disk.fail_write_after(cutoff);
        {
            let mut batch = db.engine.begin().unwrap();
            let first = SingleEngine::<Item>::execute(&mut batch, |tx| tx.insert(1, Item(10)));
            if first.is_ok() {
                assert!(matches!(
                    SingleEngine::<Item>::execute(&mut batch, |tx| tx.insert(2, Item(20))),
                    Err(Error::CommitUncertain(_))
                ));
            } else {
                assert!(matches!(first, Err(Error::CommitUncertain(_))));
            }
        }
        assert!(matches!(old.get(1), Err(Error::Poisoned)));
        assert!(matches!(db.retention(), Err(Error::Poisoned)));
        drop(old);
        drop(db);
        disk.clear_faults();
        let recovered = durable(&disk);
        let view = recovered.read().unwrap();
        assert_eq!(view.sequence(), u64::from(cutoff >= frame.len()));
        assert_eq!(
            view.get(1).map(|r| r.0),
            if cutoff >= frame.len() {
                Some(10)
            } else {
                None
            }
        );
        assert!(view.get(2).is_none());
    }
}
#[test]
fn writer_panic_poison_reaches_retained_views_and_offline_conversion_refuses_pins() {
    let disk = TestStorage::new(file_header(Item::SCHEMA));
    let db = versioned(&disk);
    let old = db.snapshot().unwrap();
    assert!(matches!(db.clone().into_database(), Err(Error::Busy)));
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = db.write::<()>(|tx| {
                tx.put(1, Item(10));
                panic!("callback panic")
            });
        }))
        .is_err()
    );
    assert!(matches!(old.get(1), Err(Error::Poisoned)));
    assert!(matches!(db.snapshot(), Err(Error::Poisoned)));
    drop(old);
    drop(db);
    assert_eq!(durable(&disk).read().unwrap().sequence(), 0);
    let db = versioned(&disk);
    let old = db.snapshot().unwrap();
    assert!(matches!(db.into_database(), Err(Error::Busy)));
    // Normal controller closure leaves an immutable pin readable, without LOCK.
    assert_eq!(old.sequence().unwrap(), 0);
}

struct Gate {
    disk: TestStorage,
    armed: Arc<AtomicBool>,
    started: mpsc::Sender<()>,
    resume: Mutex<mpsc::Receiver<()>>,
}
impl Read for Gate {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.disk.read(b)
    }
}
impl Write for Gate {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.disk.write(b)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.disk.flush()
    }
}
impl Seek for Gate {
    fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
        self.disk.seek(p)
    }
}
impl Storage for Gate {
    fn size(&self) -> io::Result<u64> {
        self.disk.size()
    }
    fn truncate(&mut self, n: u64) -> io::Result<()> {
        self.disk.truncate(n)
    }
    fn sync(&self) -> io::Result<()> {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.started.send(()).unwrap();
            self.resume.lock().unwrap().recv().unwrap();
        }
        self.disk.sync()
    }
}
pub(super) struct GateControl {
    pub(super) armed: Arc<AtomicBool>,
    pub(super) started: mpsc::Receiver<()>,
    pub(super) resume: mpsc::Sender<()>,
}
impl Drop for GateControl {
    fn drop(&mut self) {
        let _ = self.resume.send(());
    }
}
pub(super) fn gated(disk: TestStorage) -> (Box<dyn Storage>, GateControl) {
    let armed = Arc::new(AtomicBool::new(false));
    let (started, observed) = mpsc::channel();
    let (resume, wait) = mpsc::channel();
    (
        Box::new(Gate {
            disk,
            armed: armed.clone(),
            started,
            resume: Mutex::new(wait),
        }),
        GateControl {
            armed,
            started: observed,
            resume,
        },
    )
}
#[test]
fn snapshots_remain_nonblocking_and_old_until_a_real_shared_sync_completes() {
    let disk = TestStorage::new(file_header(Item::SCHEMA));
    let armed = Arc::new(AtomicBool::new(false));
    let (started, start) = mpsc::channel();
    let (resume, wait) = mpsc::channel();
    let (wal, recovered) = Wal::recover::<Item>(Box::new(Gate {
        disk: disk.clone(),
        armed: armed.clone(),
        started,
        resume: Mutex::new(wait),
    }))
    .unwrap();
    let group = Database::from_recovered(wal, recovered)
        .into_snapshots(options(), footprint)
        .unwrap()
        .into_group_commit(GroupCommitOptions {
            queue_capacity: 3,
            max_transactions: 2,
            max_delay: Duration::ZERO,
        })
        .unwrap();
    let old = group.snapshot().unwrap();
    let (began, begin) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let paused = group
        .submit(move |_| {
            began.send(()).unwrap();
            released.recv().unwrap();
            Ok(())
        })
        .unwrap();
    begin.recv_timeout(Duration::from_secs(10)).unwrap();
    let first = group.submit(|tx| tx.insert(1, Item(10))).unwrap();
    let dropped = group
        .submit(|tx| {
            tx.update(1, |r| Ok(Item(r.0 + 1)))?;
            tx.insert(2, Item(20))
        })
        .unwrap();
    drop(dropped);
    armed.store(true, Ordering::SeqCst);
    release.send(()).unwrap();
    assert_eq!(paused.wait().unwrap().transactions_in_group, 0);
    start.recv_timeout(Duration::from_secs(10)).unwrap();
    let (read, reads) = mpsc::channel();
    let client = group.clone();
    let reader = std::thread::spawn(move || {
        let view = client.snapshot().unwrap();
        read.send((view.sequence().unwrap(), view.get(1).unwrap().is_none()))
            .unwrap();
    });
    let observed = reads.recv_timeout(Duration::from_secs(10));
    // Always release the I/O gate before assertions/join, even on a regression.
    resume.send(()).unwrap();
    assert_eq!(observed.unwrap(), (0, true));
    reader.join().unwrap();
    let receipt = first.wait().unwrap();
    assert_eq!(
        (
            receipt.sequence,
            receipt.synchronized_sequence,
            receipt.transactions_in_group
        ),
        (1, 2, 2)
    );
    let new = group.snapshot().unwrap();
    assert_eq!(new.sequence().unwrap(), 2);
    assert_eq!(new.get(1).unwrap().unwrap().0, 11);
    assert!(old.get(1).unwrap().is_none());
    drop(new);
    drop(old);
    let baseline = group.into_database().unwrap();
    drop(baseline);
    assert_eq!(durable(&disk).read().unwrap().get(1).unwrap().0, 11);
}

#[test]
fn native_alignment_and_accounting_overflow_are_checked_before_append() {
    #[repr(align(256))]
    struct Aligned(u64);
    impl Record for Aligned {
        const SCHEMA: Schema = Item::SCHEMA;
        fn encode(&self, e: &mut Encoder) -> Result<()> {
            e.u64(self.0)
        }
        fn decode(d: &mut Decoder<'_>) -> Result<Self> {
            Ok(Self(d.u64()?))
        }
    }
    let db = Database::in_memory()
        .into_snapshots(options(), |_: &Aligned| {
            Ok(std::mem::size_of::<Aligned>() as u64)
        })
        .unwrap();
    db.write(|tx| tx.insert(1, Aligned(10))).unwrap();
    assert_eq!(
        db.retention().unwrap().current_bytes,
        std::mem::size_of::<Aligned>() as u64
            + arc_prefix::<Aligned>()
            + VersionTree::<u64, Arc<Aligned>>::node_bytes()
    );
    assert_eq!(arc_prefix::<Aligned>(), 256);
    let disk = TestStorage::new(file_header(Item::SCHEMA));
    let db = durable(&disk)
        .into_snapshots(options(), |_| Ok(u64::MAX / 2))
        .unwrap();
    db.write(|tx| tx.insert(1, Item(10))).unwrap();
    let before = disk.image();
    assert!(matches!(
        db.write(|tx| tx.insert(2, Item(20))),
        Err(Error::InvalidOperation(_))
    ));
    assert_eq!(disk.image(), before);
    assert_eq!(db.retention().unwrap().published_sequence, 1);
    drop(db);
    let baseline = durable(&disk);
    assert!(matches!(
        baseline.into_snapshots(options(), |_| Ok(u64::MAX)),
        Err(Error::InvalidOperation(_))
    ));
    assert_eq!(disk.image(), before);
}
#[test]
fn no_op_and_rollback_leave_root_sequence_wal_and_sync_count_unchanged() {
    let disk = TestStorage::new(file_header(Item::SCHEMA));
    let db = versioned(&disk);
    let old = db.snapshot().unwrap();
    let syncs = disk.syncs();
    let before = disk.image();
    db.write(|tx| {
        tx.insert(1, Item(10))?;
        assert!(tx.remove(1));
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        db.write(|tx| {
            tx.insert(1, Item(10))?;
            Err::<(), _>(Error::Codec("rollback".into()))
        }),
        Err(Error::Codec(_))
    ));
    assert_eq!(disk.syncs(), syncs);
    assert_eq!(disk.image(), before);
    assert_eq!(db.retention().unwrap().published_sequence, 0);
    assert!(old.is_empty().unwrap());
}
