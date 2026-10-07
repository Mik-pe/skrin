use super::*;
use crate::log::{Storage, Wal, encode_transaction, file_header};
use crate::test_support::{Item, TestStorage};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};

fn durable(disk: &TestStorage) -> Database<Item> {
    let (wal, recovered) = Wal::recover::<Item>(Box::new(disk.clone())).unwrap();
    Database::from_recovered(wal, recovered)
}
fn options() -> GroupCommitOptions {
    GroupCommitOptions {
        queue_capacity: 3,
        max_transactions: 3,
        max_delay: Duration::ZERO,
    }
}
type ItemJobs = Vec<Box<dyn Job<Database<Item>>>>;
fn jobs() -> (ItemJobs, Vec<PendingCommit<u64>>) {
    let mut jobs: Vec<Box<dyn Job<Database<Item>>>> = Vec::new();
    let mut responses = Vec::new();
    for key in 1..=2 {
        let (response, receiver) = mpsc::channel();
        jobs.push(Box::new(Request {
            admitted: Instant::now(),
            response,
            operation: move |tx: &mut WriteTransaction<'_, Item>| {
                tx.insert(key, Item(key * 10))?;
                Ok(key)
            },
        }));
        responses.push(PendingCommit { receiver });
    }
    (jobs, responses)
}

#[test]
fn bounded_admission_shared_sync_order_and_dropped_response() {
    let disk = TestStorage::new(file_header(Item::SCHEMA));
    let group = durable(&disk).into_group_commit(options()).unwrap();
    let (started, start) = mpsc::channel();
    let (resume, wait) = mpsc::channel();
    let paused = group
        .submit(move |_| {
            started.send(()).unwrap();
            wait.recv().unwrap();
            Ok(())
        })
        .unwrap();
    start.recv().unwrap();
    let first = group
        .submit(|tx| {
            tx.insert(1, Item(10))?;
            Ok(1)
        })
        .unwrap();
    let dropped = group
        .submit(|tx| {
            tx.update(1, |row| Ok(Item(row.0 + 1)))?;
            Ok(2)
        })
        .unwrap();
    let third = group
        .submit(|tx| {
            assert_eq!(tx.get(1).unwrap().0, 11);
            tx.insert(2, Item(20))?;
            Ok(3)
        })
        .unwrap();
    assert!(matches!(
        group.submit::<()>(|_| panic!("unadmitted callback ran")),
        Err(Error::QueueFull)
    ));
    drop(dropped);
    resume.send(()).unwrap();
    assert_eq!(paused.wait().unwrap().transactions_in_group, 0);
    let first = first.wait().unwrap();
    let third = third.wait().unwrap();
    assert_eq!((first.sequence, third.sequence), (1, 3));
    assert_eq!(
        (first.transactions_in_group, third.transactions_in_group),
        (3, 3)
    );
    assert_eq!(disk.syncs(), 2); // Recovery and one sync for three independent frames.
    let db = group.into_database().unwrap();
    assert_eq!(db.read().unwrap().get(1).unwrap().0, 11);
    drop(db);
    let db = durable(&disk);
    assert_eq!(db.read().unwrap().sequence(), 3);
    assert_eq!(db.read().unwrap().get(2).unwrap().0, 20);
}

#[test]
fn every_partial_group_append_and_sync_failure_recover_an_independent_prefix() {
    let first_length = encode_transaction(1, &BTreeMap::from([(1, Some(Item(10)))]))
        .unwrap()
        .len();
    let second_length = encode_transaction(2, &BTreeMap::from([(2, Some(Item(20)))]))
        .unwrap()
        .len();
    for enospc in [false, true] {
        for cutoff in 0..first_length + second_length {
            let disk = TestStorage::new(file_header(Item::SCHEMA));
            let db = durable(&disk);
            if enospc {
                disk.fail_enospc_after(cutoff);
            } else {
                disk.fail_write_after(cutoff);
            }
            let (requests, responses) = jobs();
            assert!(!process(&db, requests));
            let mut responses = responses.into_iter();
            match responses.next().unwrap().wait() {
                Err(Error::CommitUncertain(error)) => assert_eq!(
                    error.kind(),
                    if enospc {
                        io::ErrorKind::StorageFull
                    } else {
                        io::ErrorKind::Other
                    }
                ),
                other => panic!("prefix outcome: {other:?}"),
            }
            assert!(matches!(
                responses.next().unwrap().wait(),
                Err(Error::CommitUncertain(_)) | Err(Error::Poisoned)
            ));
            assert!(matches!(db.read(), Err(Error::Poisoned)));
            drop(db);
            disk.clear_faults();
            let db = durable(&disk);
            let read = db.read().unwrap();
            let sequence = u64::from(cutoff >= first_length);
            assert_eq!(read.sequence(), sequence);
            assert_eq!(
                read.get(1).map(|row| row.0),
                if sequence == 1 { Some(10) } else { None }
            );
            assert!(read.get(2).is_none());
        }
        let disk = TestStorage::new(file_header(Item::SCHEMA));
        let db = durable(&disk);
        if enospc {
            disk.fail_sync_enospc();
        } else {
            disk.fail_sync();
        }
        let (requests, responses) = jobs();
        assert!(!process(&db, requests));
        for response in responses {
            assert!(matches!(response.wait(), Err(Error::CommitUncertain(_))));
        }
        assert!(matches!(db.read(), Err(Error::Poisoned)));
        drop(db);
        disk.clear_faults();
        assert_eq!(durable(&disk).read().unwrap().sequence(), 2);
    }
}

struct SyncGate {
    disk: TestStorage,
    armed: Arc<AtomicBool>,
    started: mpsc::Sender<()>,
    resume: Mutex<mpsc::Receiver<()>>,
}
impl Read for SyncGate {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.disk.read(bytes)
    }
}
impl Write for SyncGate {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.disk.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.disk.flush()
    }
}
impl Seek for SyncGate {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.disk.seek(position)
    }
}
impl Storage for SyncGate {
    fn size(&self) -> io::Result<u64> {
        self.disk.size()
    }
    fn truncate(&mut self, len: u64) -> io::Result<()> {
        self.disk.truncate(len)
    }
    fn sync(&self) -> io::Result<()> {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.started.send(()).unwrap();
            self.resume.lock().unwrap().recv().unwrap();
        }
        self.disk.sync()
    }
}
#[test]
fn no_reader_or_success_response_escapes_a_blocked_shared_sync() {
    let disk = TestStorage::new(file_header(Item::SCHEMA));
    let armed = Arc::new(AtomicBool::new(false));
    let (started, start) = mpsc::channel();
    let (resume, wait) = mpsc::channel();
    let (wal, recovered) = Wal::recover::<Item>(Box::new(SyncGate {
        disk,
        armed: armed.clone(),
        started,
        resume: Mutex::new(wait),
    }))
    .unwrap();
    let group = Database::from_recovered(wal, recovered)
        .into_group_commit(options())
        .unwrap();
    armed.store(true, Ordering::SeqCst);
    let response = group.submit(|tx| tx.insert(1, Item(10))).unwrap();
    start.recv().unwrap();
    assert!(matches!(
        response.receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let reader = group.clone();
    let (entered, enter) = mpsc::channel();
    let (observed, observe) = mpsc::channel();
    let reader = thread::spawn(move || {
        entered.send(()).unwrap();
        observed
            .send(reader.read().unwrap().get(1).unwrap().0)
            .unwrap();
    });
    enter.recv().unwrap();
    assert!(matches!(observe.try_recv(), Err(mpsc::TryRecvError::Empty)));
    resume.send(()).unwrap();
    response.wait().unwrap();
    assert_eq!(observe.recv().unwrap(), 10);
    reader.join().unwrap();
}

#[test]
fn callback_errors_are_independent_and_panics_leave_an_uncertain_prefix() {
    let disk = TestStorage::new(file_header(Item::SCHEMA));
    let db = durable(&disk);
    let (requests, responses) = jobs();
    let (response, receiver) = mpsc::channel();
    let mut requests = requests;
    requests.insert(
        1,
        Box::new(Request {
            admitted: Instant::now(),
            response,
            operation: |tx: &mut WriteTransaction<'_, Item>| {
                tx.put(99, Item(99));
                Err::<(), _>(Error::Poisoned)
            },
        }),
    );
    assert!(process(&db, requests));
    assert!(matches!(
        PendingCommit { receiver }.wait(),
        Err(Error::Poisoned)
    ));
    for response in responses {
        assert_eq!(response.wait().unwrap().transactions_in_group, 2);
    }
    assert!(db.read().unwrap().get(99).is_none());
    assert_eq!(disk.syncs(), 2);
    drop(db);
    let db = durable(&disk);
    let (mut requests, responses) = jobs();
    // Existing rows make the first inserts clean duplicate failures; replace the
    // first job with a new append, then panic during the second callback.
    let (response, receiver) = mpsc::channel();
    requests[0] = Box::new(Request {
        admitted: Instant::now(),
        response,
        operation: |tx: &mut WriteTransaction<'_, Item>| {
            tx.put(3, Item(30));
            Ok(())
        },
    });
    let (response, _) = mpsc::channel::<Result<CommitReceipt<()>>>();
    requests[1] = Box::new(Request {
        admitted: Instant::now(),
        response,
        operation: |_: &mut WriteTransaction<'_, Item>| -> Result<()> {
            panic!("application panic")
        },
    });
    drop(responses);
    assert!(catch_unwind(AssertUnwindSafe(|| process(&db, requests))).is_err());
    assert!(matches!(
        PendingCommit { receiver }.wait(),
        Err(Error::CommitUncertain(_))
    ));
    assert!(matches!(db.read(), Err(Error::Poisoned)));
    drop(db);
    assert_eq!(durable(&disk).read().unwrap().get(3).unwrap().0, 30);
}

#[test]
fn collection_deadline_and_shutdown_do_not_require_a_full_batch() {
    let disk = TestStorage::new(file_header(Item::SCHEMA));
    let group = durable(&disk)
        .into_group_commit(GroupCommitOptions {
            max_delay: Duration::from_millis(1),
            max_transactions: 1024,
            ..options()
        })
        .unwrap();
    let pending = group.submit(|tx| tx.insert(1, Item(10))).unwrap();
    // A generous diagnostic timeout detects indefinite wait-for-full behavior,
    // not an assertion about scheduler or physical-sync latency.
    let receipt = pending
        .receiver
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert_eq!(receipt.sequence, 1);
    let pending = group.submit(|tx| tx.insert(2, Item(20))).unwrap();
    let db = group.into_database().unwrap(); // Drains this last admitted request.
    assert_eq!(pending.wait().unwrap().sequence, 2);
    assert_eq!(db.read().unwrap().len(), 2);
    assert!(matches!(
        Database::<Item>::in_memory().into_group_commit(options()),
        Err(Error::InvalidOperation(_))
    ));
}

#[cfg(unix)]
#[test]
fn grouped_acknowledgments_survive_production_persistence_and_checkpoint_images() {
    use crate::persistence_model::{self as model, Omission};
    let root = std::env::temp_dir().join(format!(
        "skrin-group-model-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = Database::<Item>::create_dir(&root).unwrap();
    model::start_full(&root, Omission::None, 0);
    let (requests, responses) = jobs();
    assert!(process(&db, requests));
    for response in responses {
        model::acknowledged(response.wait().unwrap().sequence);
    }
    let cp = db.checkpoint().unwrap();
    model::published(cp.generation);
    drop(db);
    for image in model::finish() {
        image.restore(&root);
        let db = Database::<Item>::open_dir(&root).unwrap();
        let read = db.read().unwrap();
        assert!((image.acknowledged_sequence.unwrap()..=2).contains(&read.sequence()));
        assert_eq!(
            read.get(1).map(|row| row.0),
            if read.sequence() >= 1 { Some(10) } else { None }
        );
        assert_eq!(
            read.get(2).map(|row| row.0),
            if read.sequence() == 2 { Some(20) } else { None }
        );
        if let Some(generation) = image.published_generation {
            assert_eq!(
                db.generation_info().unwrap().unwrap().generation,
                generation
            );
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn deferred_append_refuses_a_transaction_without_an_enclosing_guard() {
    let disk = TestStorage::new(file_header(Item::SCHEMA));
    let db = durable(&disk);
    let before = disk.image();
    let mut tx = db.begin_write().unwrap();
    tx.insert(1, Item(10)).unwrap();
    assert!(matches!(
        tx.commit_unsynced(),
        Err(Error::InvalidOperation(_))
    ));
    assert_eq!(disk.image(), before);
    assert_eq!(db.stats().unwrap().commits, 0);
}

#[test]
fn worker_panic_refuses_queued_callbacks_and_closes_admission() {
    let disk = TestStorage::new(file_header(Item::SCHEMA));
    let group = durable(&disk).into_group_commit(options()).unwrap();
    let (started, start) = mpsc::channel();
    let (resume, wait) = mpsc::channel();
    let panic = group
        .submit::<()>(move |_| {
            started.send(()).unwrap();
            wait.recv().unwrap();
            panic!("worker callback panic")
        })
        .unwrap();
    start.recv().unwrap();
    let invoked = Arc::new(AtomicBool::new(false));
    let observe = invoked.clone();
    let queued = group
        .submit(move |_| {
            observe.store(true, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
    resume.send(()).unwrap();
    assert!(matches!(panic.wait(), Err(Error::CommitUncertain(_))));
    assert!(matches!(queued.wait(), Err(Error::Poisoned)));
    assert!(!invoked.load(Ordering::SeqCst));
    assert!(matches!(group.submit(|_| Ok(())), Err(Error::Poisoned)));
    assert!(matches!(group.read(), Err(Error::Poisoned)));
    drop(group);
    assert_eq!(durable(&disk).read().unwrap().sequence(), 0);
}
