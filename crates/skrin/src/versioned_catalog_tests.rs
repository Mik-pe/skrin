use super::*;
use crate::log::{Wal, encode_transaction, file_header};
use crate::test_support::TestStorage;
use crate::test_support::banking;
#[cfg(unix)]
use crate::test_support::banking_v2;
use banking::*;
use std::time::Duration;
fn options() -> SnapshotOptions {
    SnapshotOptions {
        max_snapshots: 64,
        max_pinned_bytes: 64 * 1024 * 1024,
    }
}
fn footprint(row: &Row) -> Result<u64> {
    Ok(std::mem::size_of::<Row>() as u64
        + match row {
            Row::Account(r) => r.email.capacity() as u64,
            Row::Transfer(_) => 0,
        })
}
fn reopen(disk: &TestStorage) -> CatalogDatabase<Banking> {
    let (wal, recovered) = Wal::recover::<Stored<Banking>>(Box::new(disk.clone())).unwrap();
    CatalogDatabase::wrap(Database::from_recovered(wal, recovered)).unwrap()
}
fn initial() -> Vec<u8> {
    let mut bytes = file_header(Banking::SCHEMA);
    bytes.extend(
        encode_transaction(
            1,
            &BTreeMap::from([(0, Some(Stored::<Banking>::metadata()))]),
        )
        .unwrap(),
    );
    let disk = TestStorage::new(bytes);
    let db = reopen(&disk);
    seed(&db).unwrap();
    disk.image()
}
fn transfer_version(
    tx: &mut CatalogSnapshotWrite<'_, Banking>,
    id: u64,
    amount: u64,
) -> Result<()> {
    tx.update::<Accounts>(1, |r| {
        Ok(Account {
            email: r.email.clone(),
            balance: r.balance - amount,
        })
    })?;
    tx.update::<Accounts>(2, |r| {
        Ok(Account {
            email: r.email.clone(),
            balance: r.balance + amount,
        })
    })?;
    tx.insert::<Transfers>(
        id,
        Transfer {
            from: 1,
            to: 2,
            amount,
        },
    )
}
fn verify(view: &CatalogSnapshot<Banking>, transfers: u64) {
    let balance = 100 - transfers * 10;
    assert_eq!(view.get::<Accounts>(1).unwrap().unwrap().balance, balance);
    assert_eq!(
        view.get::<Accounts>(2).unwrap().unwrap().balance,
        200 - balance
    );
    assert_eq!(
        view.scan::<Transfers>().unwrap().count(),
        transfers as usize
    );
    for id in 7..7 + transfers {
        let row = view.get::<Transfers>(id).unwrap().unwrap();
        assert_eq!((row.from, row.to, row.amount), (1, 2, 10));
    }

    assert_eq!(
        view.lookup::<Accounts>(1, b"alice@example.test").unwrap()[0].0,
        1
    );
    let expected: Vec<_> = if transfers == 0 { vec![1, 2] } else { vec![1] };
    assert_eq!(
        view.lookup::<Accounts>(2, &balance.to_be_bytes())
            .unwrap()
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>(),
        expected
    );
    let scan = view.index_range::<Accounts>(2, ..).unwrap();
    assert_eq!(scan.iter().map(|(k, _)| *k).collect::<Vec<_>>(), [1, 2]);
}
#[test]
fn failed_initial_catalog_assessment_preserves_storage_rows_and_indexes() {
    fn assess(row: &Row) -> Result<u64> {
        if matches!(row, Row::Account(r) if r.email == "bob@example.test") {
            Err(Error::InvalidOperation("unaccountable row".into()))
        } else {
            footprint(row)
        }
    }
    let disk = TestStorage::new(initial());
    let db = reopen(&disk);
    let before = disk.image();
    let syncs = disk.syncs();
    assert!(matches!(
        db.into_snapshots(options(), assess),
        Err(Error::InvalidOperation(_))
    ));
    assert_eq!(disk.image(), before);
    assert_eq!(disk.syncs(), syncs);
    let db = reopen(&disk).into_snapshots(options(), footprint).unwrap();
    verify(&db.snapshot().unwrap(), 0);
}
#[test]
fn retained_catalog_roots_include_all_rows_unique_and_nonunique_postings() {
    let disk = TestStorage::new(initial());
    let db = reopen(&disk).into_snapshots(options(), footprint).unwrap();
    let original = db.snapshot().unwrap();
    verify(&original, 0);
    db.write(|tx| transfer_version(tx, 7, 10)).unwrap();
    let changed = db.snapshot().unwrap();
    verify(&changed, 1);
    verify(&original, 0);
    assert!(matches!(
        db.write(|tx| transfer_version(tx, 7, 10)),
        Err(Error::DuplicateKey(7))
    ));
    assert!(matches!(
        db.write(|tx| tx.put::<Accounts>(
            3,
            Account {
                email: "alice@example.test".into(),
                balance: 200
            }
        )),
        Err(Error::UniqueViolation { index_id: 1 })
    ));
    verify(&db.snapshot().unwrap(), 1);
    // Final-view validation allows swaps; each pinned index keeps its own keys.
    db.write(|tx| {
        tx.update::<Accounts>(1, |r| {
            Ok(Account {
                email: "bob@example.test".into(),
                balance: r.balance,
            })
        })?;
        tx.update::<Accounts>(2, |r| {
            Ok(Account {
                email: "alice@example.test".into(),
                balance: r.balance,
            })
        })?;
        assert_eq!(tx.lookup::<Accounts>(1, b"alice@example.test")?[0].0, 2);
        Ok(())
    })
    .unwrap();
    let swapped = db.snapshot().unwrap();
    assert_eq!(
        swapped
            .lookup::<Accounts>(1, b"alice@example.test")
            .unwrap()[0]
            .0,
        2
    );
    assert_eq!(
        changed
            .lookup::<Accounts>(1, b"alice@example.test")
            .unwrap()[0]
            .0,
        1
    );
    db.write(|tx| {
        assert!(tx.remove::<Accounts>(1)?);
        assert!(tx.remove::<Transfers>(7)?);
        Ok(())
    })
    .unwrap();
    let deleted = db.snapshot().unwrap();
    assert!(deleted.get::<Accounts>(1).unwrap().is_none());
    assert!(
        deleted
            .lookup::<Accounts>(1, b"bob@example.test")
            .unwrap()
            .is_empty()
    );
    assert!(
        deleted
            .lookup::<Accounts>(2, &90u64.to_be_bytes())
            .unwrap()
            .is_empty()
    );
    assert_eq!(swapped.scan::<Transfers>().unwrap().count(), 1);
    drop(deleted);
    drop(swapped);
    drop(changed);
    drop(original);
    let baseline = db.into_database().unwrap();
    assert!(
        baseline
            .read()
            .unwrap()
            .get::<Accounts>(1)
            .unwrap()
            .is_none()
    );
    drop(baseline);
    let baseline = reopen(&disk);
    assert_eq!(
        baseline
            .read()
            .unwrap()
            .lookup::<Accounts>(1, b"alice@example.test")
            .unwrap()[0]
            .0,
        2
    );
}
#[test]
fn randomized_retained_rows_and_index_ranges_match_independent_reference() {
    let db = CatalogDatabase::<Banking>::in_memory()
        .unwrap()
        .into_snapshots(options(), footprint)
        .unwrap();
    let mut reference = BTreeMap::new();
    let mut retained = Vec::new();
    let mut random = 57u64;
    for n in 0..700 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let key = random % 70;
        let balance = (random >> 8) % 7;
        if random & 3 == 0 {
            db.write(|tx| {
                assert_eq!(
                    tx.remove::<Accounts>(key)?,
                    reference.remove(&key).is_some()
                );
                Ok(())
            })
            .unwrap();
        } else {
            let email = format!("owner-{key:02}");
            db.write(|tx| {
                tx.put::<Accounts>(
                    key,
                    Account {
                        email: email.clone(),
                        balance,
                    },
                )
            })
            .unwrap();
            reference.insert(key, (email, balance));
        }
        if n % 19 == 0 {
            retained.push((db.snapshot().unwrap(), reference.clone()));
        }
    }
    for (view, expected) in retained {
        assert_eq!(
            view.scan::<Accounts>()
                .unwrap()
                .map(|(k, r)| (k, (r.email.clone(), r.balance)))
                .collect::<BTreeMap<_, _>>(),
            expected
        );
        for balance in 0..7 {
            let ids: Vec<_> = expected
                .iter()
                .filter(|(_, (_, b))| *b == balance)
                .map(|(&k, _)| k)
                .collect();
            assert_eq!(
                view.lookup::<Accounts>(2, &balance.to_be_bytes())
                    .unwrap()
                    .iter()
                    .map(|(k, _)| *k)
                    .collect::<Vec<_>>(),
                ids
            );
        }
        let mut sorted: Vec<_> = expected
            .iter()
            .filter(|(_, (_, b))| (2..5).contains(b))
            .map(|(&k, (_, b))| (*b, k))
            .collect();
        sorted.sort();
        assert_eq!(
            view.index_range::<Accounts>(
                2,
                2u64.to_be_bytes().to_vec()..5u64.to_be_bytes().to_vec()
            )
            .unwrap()
            .iter()
            .map(|(k, r)| (r.balance, *k))
            .collect::<Vec<_>>(),
            sorted
        );
        for (&key, (email, _)) in &expected {
            assert_eq!(
                view.lookup::<Accounts>(1, email.as_bytes()).unwrap()[0].0,
                key
            );
        }
    }
}
#[test]
fn every_catalog_append_prefix_and_uncertain_sync_refuse_all_snapshot_reads() {
    let before = initial();
    let probe = TestStorage::new(before.clone());
    let db = reopen(&probe).into_snapshots(options(), footprint).unwrap();
    db.write(|tx| transfer_version(tx, 7, 10)).unwrap();
    let length = probe.image().len() - before.len();
    for full in [false, true] {
        for cutoff in 0..length {
            let disk = TestStorage::new(before.clone());
            let db = reopen(&disk).into_snapshots(options(), footprint).unwrap();
            let old = db.snapshot().unwrap();
            if full {
                disk.fail_enospc_after(cutoff);
            } else {
                disk.fail_write_after(cutoff);
            }
            assert!(matches!(
                db.write(|tx| transfer_version(tx, 7, 10)),
                Err(Error::CommitUncertain(_))
            ));
            assert!(matches!(
                old.lookup::<Accounts>(1, b"alice@example.test"),
                Err(Error::Poisoned)
            ));
            assert!(matches!(
                old.index_scan::<Accounts>(2, ..),
                Err(Error::Poisoned)
            ));
            assert!(matches!(db.snapshot(), Err(Error::Poisoned)));
            drop(old);
            drop(db);
            disk.clear_faults();
            verify(
                &reopen(&disk)
                    .into_snapshots(options(), footprint)
                    .unwrap()
                    .snapshot()
                    .unwrap(),
                0,
            );
        }
        let disk = TestStorage::new(before.clone());
        let db = reopen(&disk).into_snapshots(options(), footprint).unwrap();
        let old = db.snapshot().unwrap();
        if full {
            disk.fail_sync_enospc();
        } else {
            disk.fail_sync();
        }
        assert!(matches!(
            db.write(|tx| transfer_version(tx, 7, 10)),
            Err(Error::CommitUncertain(_))
        ));
        assert!(matches!(old.scan::<Transfers>(), Err(Error::Poisoned)));
        assert!(matches!(
            old.index_scan::<Accounts>(2, ..),
            Err(Error::Poisoned)
        ));
        drop(old);
        drop(db);
        disk.clear_faults();
        verify(
            &reopen(&disk)
                .into_snapshots(options(), footprint)
                .unwrap()
                .snapshot()
                .unwrap(),
            1,
        );
    }
}
#[test]
fn independent_group_conflict_dropped_response_and_shared_sync_uncertainty() {
    for fail in [false, true] {
        let disk = TestStorage::new(initial());
        let group = reopen(&disk)
            .into_snapshots(options(), footprint)
            .unwrap()
            .into_group_commit(GroupCommitOptions {
                queue_capacity: 3,
                max_transactions: 3,
                max_delay: Duration::ZERO,
            })
            .unwrap();
        let old = group.snapshot().unwrap();
        let (start, began) = std::sync::mpsc::channel();
        let (resume, wait) = std::sync::mpsc::channel();
        let paused = group
            .submit(move |_| {
                start.send(()).unwrap();
                wait.recv().unwrap();
                Ok(())
            })
            .unwrap();
        began.recv_timeout(Duration::from_secs(10)).unwrap();
        let first = group.submit(|tx| transfer_version(tx, 7, 10)).unwrap();
        let conflict = group
            .submit(|tx| {
                tx.put::<Accounts>(
                    3,
                    Account {
                        email: "alice@example.test".into(),
                        balance: 100,
                    },
                )
            })
            .unwrap();
        let second = group
            .submit(|tx| {
                assert_eq!(tx.get::<Accounts>(1)?.unwrap().balance, 90);
                transfer_version(tx, 8, 10)
            })
            .unwrap();
        let second = if fail {
            disk.fail_sync_enospc();
            Some(second)
        } else {
            drop(second);
            None
        };
        resume.send(()).unwrap();
        paused.wait().unwrap();
        if fail {
            assert!(matches!(first.wait(), Err(Error::CommitUncertain(_))));
            assert!(matches!(
                second.unwrap().wait(),
                Err(Error::CommitUncertain(_))
            ));
            assert!(matches!(old.get::<Accounts>(1), Err(Error::Poisoned)));
            assert!(matches!(group.snapshot(), Err(Error::Poisoned)));
        } else {
            let receipt = first.wait().unwrap();
            assert_eq!(
                (
                    receipt.sequence,
                    receipt.synchronized_sequence,
                    receipt.transactions_in_group
                ),
                (3, 4, 2)
            );
            verify(&group.snapshot().unwrap(), 2);
            verify(&old, 0);
        }
        assert!(matches!(
            conflict.wait(),
            Err(Error::UniqueViolation { index_id: 1 })
        ));
        drop(old);
        drop(group);
        disk.clear_faults();
        verify(
            &reopen(&disk)
                .into_snapshots(options(), footprint)
                .unwrap()
                .snapshot()
                .unwrap(),
            2,
        );
    }
}
#[test]
fn every_grouped_catalog_append_prefix_keeps_recovered_rows_and_indexes_coherent() {
    let before = initial();
    let probe = TestStorage::new(before.clone());
    let db = reopen(&probe).into_snapshots(options(), footprint).unwrap();
    db.write(|tx| transfer_version(tx, 7, 10)).unwrap();
    let first = probe.image().len() - before.len();
    db.write(|tx| transfer_version(tx, 8, 10)).unwrap();
    let total = probe.image().len() - before.len();
    for cutoff in 0..total {
        let disk = TestStorage::new(before.clone());
        let db = reopen(&disk).into_snapshots(options(), footprint).unwrap();
        let old = db.snapshot().unwrap();
        disk.fail_enospc_after(cutoff);
        {
            let mut batch = db.engine.begin().unwrap();
            let a = CatalogEngine::<Banking>::execute(&mut batch, |tx| transfer_version(tx, 7, 10));
            if a.is_ok() {
                assert!(matches!(
                    CatalogEngine::<Banking>::execute(&mut batch, |tx| transfer_version(tx, 8, 10)),
                    Err(Error::CommitUncertain(_))
                ));
            } else {
                assert!(matches!(a, Err(Error::CommitUncertain(_))));
            }
        }
        assert!(matches!(
            old.lookup::<Accounts>(2, &100u64.to_be_bytes()),
            Err(Error::Poisoned)
        ));
        assert!(matches!(db.snapshot(), Err(Error::Poisoned)));
        drop(old);
        drop(db);
        disk.clear_faults();
        verify(
            &reopen(&disk)
                .into_snapshots(options(), footprint)
                .unwrap()
                .snapshot()
                .unwrap(),
            u64::from(cutoff >= first),
        );
    }
}
#[test]
fn blocked_production_sync_exposes_only_old_coherent_catalog_roots() {
    let disk = TestStorage::new(initial());
    let (storage, gate) = crate::versioned::tests::gated(disk.clone());
    let (wal, recovered) = Wal::recover::<Stored<Banking>>(storage).unwrap();
    let group = CatalogDatabase::wrap(Database::from_recovered(wal, recovered))
        .unwrap()
        .into_snapshots(options(), footprint)
        .unwrap()
        .into_group_commit(GroupCommitOptions {
            queue_capacity: 3,
            max_transactions: 2,
            max_delay: Duration::ZERO,
        })
        .unwrap();
    let old = group.snapshot().unwrap();
    let (start, began) = std::sync::mpsc::channel();
    let (resume, wait) = std::sync::mpsc::channel();
    let paused = group
        .submit(move |_| {
            start.send(()).unwrap();
            wait.recv().unwrap();
            Ok(())
        })
        .unwrap();
    began.recv_timeout(Duration::from_secs(10)).unwrap();
    let a = group.submit(|tx| transfer_version(tx, 7, 10)).unwrap();
    let b = group.submit(|tx| transfer_version(tx, 8, 10)).unwrap();
    gate.armed.store(true, Ordering::SeqCst);
    resume.send(()).unwrap();
    paused.wait().unwrap();
    gate.started.recv_timeout(Duration::from_secs(10)).unwrap();
    let client = group.clone();
    let (result, observed) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let view = client.snapshot().unwrap();
        verify(&view, 0);
        result.send(view.sequence().unwrap()).unwrap();
    });
    let sequence = observed.recv_timeout(Duration::from_secs(10));
    gate.resume.send(()).unwrap();
    assert_eq!(sequence.unwrap(), 2);
    reader.join().unwrap();
    assert_eq!(a.wait().unwrap().synchronized_sequence, 4);
    assert_eq!(b.wait().unwrap().sequence, 4);
    verify(&group.snapshot().unwrap(), 2);
    verify(&old, 0);
    drop(old);
    drop(group);
    disk.clear_faults();
    verify(
        &reopen(&disk)
            .into_snapshots(options(), footprint)
            .unwrap()
            .snapshot()
            .unwrap(),
        2,
    );
}
#[cfg(unix)]
mod managed {
    use super::*;
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            Self(std::env::temp_dir().join(format!(
                    "skrin-versioned-{}-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                )))
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn snapshots_read_coherent_indexes_while_checkpoint_verification_blocks_writers() {
        use std::cell::RefCell;
        use std::sync::mpsc::{self, Receiver, Sender};
        thread_local! {
            static PAUSE: RefCell<Option<(Sender<()>, Receiver<()>)>> = const { RefCell::new(None) };
        }
        struct Gate;
        impl Catalog for Gate {
            const SCHEMA: Schema = Banking::SCHEMA;
            const TABLES: &'static [Schema] = Banking::TABLES;
            const INDEXES: &'static [IndexDefinition] = Banking::INDEXES;
            type Row = Row;
            fn table_id(r: &Row) -> u64 {
                Banking::table_id(r)
            }
            fn encode(r: &Row, e: &mut Encoder) -> Result<()> {
                Banking::encode(r, e)
            }
            fn decode(id: u64, d: &mut Decoder<'_>) -> Result<Row> {
                // Pause the real production snapshot verifier on its first row,
                // only on the checkpoint thread. All other codecs are unchanged.
                PAUSE.with(|pause| {
                    if let Some((entered, release)) = pause.borrow_mut().take() {
                        entered.send(()).unwrap();
                        release.recv().unwrap();
                    }
                });
                Banking::decode(id, d)
            }
            fn index_key(id: u64, r: &Row) -> Result<Vec<u8>> {
                Banking::index_key(id, r)
            }
        }
        impl Table<Gate> for Accounts {
            type Record = Account;
            fn into_row(r: Account) -> Row {
                Row::Account(r)
            }
            fn borrow(r: &Row) -> Option<&Account> {
                <Accounts as Table<Banking>>::borrow(r)
            }
        }
        impl Table<Gate> for Transfers {
            type Record = Transfer;
            fn into_row(r: Transfer) -> Row {
                Row::Transfer(r)
            }
            fn borrow(r: &Row) -> Option<&Transfer> {
                <Transfers as Table<Banking>>::borrow(r)
            }
        }
        fn verify_gate(view: &CatalogSnapshot<Gate>, sequence: u64, email: &str, balance: u64) {
            assert_eq!(view.sequence().unwrap(), sequence);
            assert_eq!(view.get::<Accounts>(1).unwrap().unwrap().email, email);
            assert_eq!(view.get::<Accounts>(1).unwrap().unwrap().balance, balance);
            assert_eq!(
                view.lookup::<Accounts>(1, email.as_bytes()).unwrap()[0].0,
                1
            );
            assert_eq!(
                view.lookup::<Accounts>(2, &balance.to_be_bytes()).unwrap()[0].0,
                1
            );
            assert_eq!(
                view.scan::<Transfers>().unwrap().count(),
                usize::from(sequence > 1)
            );
            assert_eq!(view.scan::<Accounts>().unwrap().count(), 1);
            for index in [1, 2] {
                assert_eq!(
                    view.index_range::<Accounts>(index, ..)
                        .unwrap()
                        .iter()
                        .map(|(key, _)| *key)
                        .collect::<Vec<_>>(),
                    [1]
                );
            }
            if sequence > 1 {
                let transfer = view.get::<Transfers>(7).unwrap().unwrap();
                assert_eq!((transfer.from, transfer.to, transfer.amount), (1, 2, 10));
            }
        }
        struct Release(Option<Sender<()>>);
        impl Drop for Release {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }
        let path = Temp::new();
        let db = CatalogDatabase::<Gate>::create_dir(&path.0).unwrap();
        db.write(|tx| {
            tx.insert::<Accounts>(
                1,
                Account {
                    email: "old".into(),
                    balance: 100,
                },
            )
        })
        .unwrap();
        let db = db.into_snapshots(options(), footprint).unwrap();
        let old = db.snapshot().unwrap();
        db.write(|tx| {
            tx.put::<Accounts>(
                1,
                Account {
                    email: "new".into(),
                    balance: 90,
                },
            )?;
            tx.insert::<Transfers>(
                7,
                Transfer {
                    from: 1,
                    to: 2,
                    amount: 10,
                },
            )
        })
        .unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release = Release(Some(release_tx));
        let checkpointer = db.clone();
        let checkpoint = std::thread::spawn(move || {
            PAUSE.with(|pause| *pause.borrow_mut() = Some((entered_tx, release_rx)));
            checkpointer.checkpoint().unwrap()
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        verify_gate(&old, 1, "old", 100);
        let reader = db.clone();
        let (read_tx, read_rx) = mpsc::channel();
        let reading = std::thread::spawn(move || {
            let current = reader.snapshot().unwrap();
            verify_gate(&current, 2, "new", 90);
            assert!(current.lookup::<Accounts>(1, b"old").unwrap().is_empty());
            read_tx.send(current).unwrap();
        });
        // Completing before release proves capture, rows and every index avoid
        // the source writer/maintenance guards, rather than merely surviving CP.
        let during = read_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        reading.join().unwrap();
        assert!(matches!(
            CatalogDatabase::<Gate>::open_dir(&path.0),
            Err(Error::Busy)
        ));
        let writer = db.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (written_tx, written_rx) = mpsc::channel();
        let writing = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            writer
                .write(|tx| {
                    tx.put::<Accounts>(
                        1,
                        Account {
                            email: "latest".into(),
                            balance: 80,
                        },
                    )
                })
                .unwrap();
            written_tx.send(()).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(matches!(
            written_rx.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(release);
        assert_eq!(checkpoint.join().unwrap().sequence, 2);
        written_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        writing.join().unwrap();
        verify_gate(&old, 1, "old", 100);
        verify_gate(&during, 2, "new", 90);
        verify_gate(&db.snapshot().unwrap(), 3, "latest", 80);
        db.reclaim().unwrap();
        drop(old);
        drop(during);
        drop(db);
        let restored = CatalogDatabase::<Gate>::open_dir(&path.0).unwrap();
        assert_eq!(restored.read().unwrap().sequence(), 3);
        assert_eq!(
            restored
                .read()
                .unwrap()
                .lookup::<Accounts>(1, b"latest")
                .unwrap()[0]
                .0,
            1
        );
        assert_eq!(
            restored
                .read()
                .unwrap()
                .get::<Transfers>(7)
                .unwrap()
                .unwrap()
                .amount,
            10
        );
    }
    #[test]
    fn checkpoint_backup_reclaim_and_offline_migration_preserve_old_memory_pins() {
        let path = Temp::new();
        let backup = Temp::new();
        let baseline = CatalogDatabase::<Banking>::create_dir(&path.0).unwrap();
        seed(&baseline).unwrap();
        let db = baseline.into_snapshots(options(), footprint).unwrap();
        let old = db.snapshot().unwrap();
        for id in 7..=9 {
            db.write(|tx| transfer_version(tx, id, 10)).unwrap();
            db.checkpoint().unwrap();
            db.reclaim().unwrap();
            verify(&old, 0);
            assert!(matches!(
                CatalogDatabase::<Banking>::open_dir(&path.0),
                Err(Error::Busy)
            ));
        }
        let restored = db.backup_to(&backup.0).unwrap();
        assert_eq!(
            restored
                .read()
                .unwrap()
                .get::<Accounts>(1)
                .unwrap()
                .unwrap()
                .balance,
            70
        );
        drop(restored);
        assert_eq!(
            CatalogDatabase::<Banking>::open_dir(&backup.0)
                .unwrap()
                .read()
                .unwrap()
                .scan::<Transfers>()
                .unwrap()
                .count(),
            3
        );
        let current = db.snapshot().unwrap();
        verify(&current, 3);
        assert_eq!(db.retention().unwrap().oldest_pinned_sequence, Some(1));
        assert!(matches!(db.clone().into_database(), Err(Error::Busy)));
        drop(current);
        drop(old);
        let db = db
            .into_database()
            .unwrap()
            .migrate::<banking_v2::BankingV2>("snapshot-offline", banking_v2::migrate_row)
            .unwrap();
        assert!(
            db.read()
                .unwrap()
                .get::<banking_v2::AccountsV2>(1)
                .unwrap()
                .unwrap()
                .active
        );
        drop(db);
        assert!(matches!(
            CatalogDatabase::<Banking>::open_dir(&path.0),
            Err(Error::SchemaMismatch { .. })
        ));
        assert_eq!(
            CatalogDatabase::<banking_v2::BankingV2>::open_dir(&path.0)
                .unwrap()
                .read()
                .unwrap()
                .get::<banking_v2::AccountsV2>(1)
                .unwrap()
                .unwrap()
                .balance,
            70
        );
    }
    #[test]
    fn uncertain_backup_publication_never_poisons_the_unchanged_source() {
        let mut clean = 0;
        let mut uncertain = 0;
        for cutoff in 0..120 {
            let source = Temp::new();
            let target = Temp::new();
            let baseline = CatalogDatabase::<Banking>::create_dir(&source.0).unwrap();
            seed(&baseline).unwrap();
            let db = baseline.into_snapshots(options(), footprint).unwrap();
            let old = db.snapshot().unwrap();
            crate::directory::faults::arm(cutoff);
            let outcome = db.backup_to(&target.0);
            crate::directory::faults::clear();
            match outcome {
                Err(Error::MaintenanceUncertain(_)) => {
                    uncertain += 1;
                }
                Err(_) => {
                    clean += 1;
                }
                Ok(backup) => {
                    assert!(clean > 20 && uncertain >= 3);
                    drop(backup);
                    return;
                }
            }
            verify(&old, 0);
            verify(&db.snapshot().unwrap(), 0);
            db.write(|tx| transfer_version(tx, 7, 10)).unwrap();
            verify(&db.snapshot().unwrap(), 1);
            assert!(matches!(
                CatalogDatabase::<Banking>::open_dir(&source.0),
                Err(Error::Busy)
            ));
        }
        panic!("all backup boundaries were not exercised");
    }
    #[test]
    fn production_survival_images_preserve_acknowledged_versions_during_checkpoint_and_reclaim() {
        use crate::persistence_model::{self as model, Omission};
        let path = Temp::new();
        let baseline = CatalogDatabase::<Banking>::create_dir(&path.0).unwrap();
        seed(&baseline).unwrap();
        let db = baseline.into_snapshots(options(), footprint).unwrap();
        let old = db.snapshot().unwrap();
        model::start_full(&path.0, Omission::None, 1);
        {
            let mut batch = db.engine.begin().unwrap();
            CatalogEngine::<Banking>::execute(&mut batch, |tx| transfer_version(tx, 7, 10))
                .unwrap();
            CatalogEngine::<Banking>::execute(&mut batch, |tx| transfer_version(tx, 8, 10))
                .unwrap();
            CatalogEngine::<Banking>::finish(&mut batch).unwrap();
        }
        verify(&db.snapshot().unwrap(), 2);
        verify(&old, 0);
        model::acknowledged(3);
        let checkpoint = db.checkpoint().unwrap();
        model::published(checkpoint.generation);
        db.checkpoint().unwrap();
        db.reclaim().unwrap();
        verify(&old, 0);
        let images = model::finish();
        assert!(images.iter().any(|image| image.partial_append));
        drop(old);
        drop(db);
        for image in images {
            image.restore(&path.0);
            let baseline = CatalogDatabase::<Banking>::open_dir(&path.0).unwrap();
            let sequence = baseline.read().unwrap().sequence();
            assert!((image.acknowledged_sequence.unwrap()..=3).contains(&sequence));
            let view = baseline
                .into_snapshots(options(), footprint)
                .unwrap()
                .snapshot()
                .unwrap();
            verify(&view, sequence - 1);
        }
    }
    #[test]
    fn every_checkpoint_failure_poison_boundary_is_propagated_to_pinned_roots() {
        let mut clean = 0;
        let mut uncertain = 0;
        for cutoff in 0..100 {
            let path = Temp::new();
            let baseline = CatalogDatabase::<Banking>::create_dir(&path.0).unwrap();
            seed(&baseline).unwrap();
            let db = baseline.into_snapshots(options(), footprint).unwrap();
            let old = db.snapshot().unwrap();
            crate::directory::faults::arm(cutoff);
            let result = db.checkpoint();
            crate::directory::faults::clear();
            assert!(matches!(
                CatalogDatabase::<Banking>::open_dir(&path.0),
                Err(Error::Busy)
            ));
            match result {
                Err(Error::MaintenanceUncertain(_)) => {
                    uncertain += 1;
                    assert!(matches!(old.get::<Accounts>(1), Err(Error::Poisoned)));
                    assert!(matches!(db.snapshot(), Err(Error::Poisoned)));
                }
                Err(_) => {
                    clean += 1;
                    verify(&old, 0);
                    verify(&db.snapshot().unwrap(), 0);
                }
                Ok(_) => {
                    assert!(clean > 20 && uncertain >= 3);
                    return;
                }
            }
            drop(old);
            drop(db);
            verify(
                &CatalogDatabase::<Banking>::open_dir(&path.0)
                    .unwrap()
                    .into_snapshots(options(), footprint)
                    .unwrap()
                    .snapshot()
                    .unwrap(),
                0,
            );
        }
        panic!("all checkpoint boundaries were not covered");
    }
}

#[test]
fn versioned_and_native_catalog_transactions_encode_identical_wal_bytes() {
    let initial = initial();
    let native_disk = TestStorage::new(initial.clone());
    let snapshot_disk = TestStorage::new(initial);
    let native = reopen(&native_disk);
    let db = reopen(&snapshot_disk)
        .into_snapshots(options(), footprint)
        .unwrap();
    native.write(|tx| transfer(tx, 7, 1, 2, 10)).unwrap();
    db.write(|tx| transfer_version(tx, 7, 10)).unwrap();
    assert_eq!(native_disk.image(), snapshot_disk.image());
    let before = snapshot_disk.image();
    let syncs = snapshot_disk.syncs();
    db.write(|tx| {
        tx.insert::<Accounts>(
            3,
            Account {
                email: "temporary".into(),
                balance: 10,
            },
        )?;
        assert!(tx.remove::<Accounts>(3)?);
        Ok(())
    })
    .unwrap();
    assert_eq!(snapshot_disk.image(), before);
    assert_eq!(snapshot_disk.syncs(), syncs);
    verify(&db.snapshot().unwrap(), 1);
}

#[test]
fn empty_index_keys_full_primary_ids_and_maximum_index_id_have_exact_ranges() {
    struct Edge;
    impl Catalog for Edge {
        const SCHEMA: Schema = Banking::SCHEMA;
        const TABLES: &'static [Schema] = Banking::TABLES;
        const INDEXES: &'static [IndexDefinition] = &[
            IndexDefinition {
                id: 1,
                table_id: 1,
                version: 1,
                unique: true,
            },
            IndexDefinition {
                id: u64::MAX,
                table_id: 1,
                version: 1,
                unique: false,
            },
        ];
        type Row = Row;
        fn table_id(r: &Row) -> u64 {
            Banking::table_id(r)
        }
        fn encode(r: &Row, e: &mut Encoder) -> Result<()> {
            Banking::encode(r, e)
        }
        fn decode(id: u64, d: &mut Decoder<'_>) -> Result<Row> {
            Banking::decode(id, d)
        }
        fn index_key(id: u64, r: &Row) -> Result<Vec<u8>> {
            Banking::index_key(if id == u64::MAX { 2 } else { id }, r)
        }
    }
    impl Table<Edge> for Accounts {
        type Record = Account;
        fn into_row(r: Account) -> Row {
            Row::Account(r)
        }
        fn borrow(r: &Row) -> Option<&Account> {
            <Accounts as Table<Banking>>::borrow(r)
        }
    }
    let db = CatalogDatabase::<Edge>::in_memory()
        .unwrap()
        .into_snapshots(options(), footprint)
        .unwrap();
    db.write(|tx| {
        for (key, email) in [(0, ""), (1, "a"), (u64::MAX, "z")] {
            tx.insert::<Accounts>(
                key,
                Account {
                    email: email.into(),
                    balance: key,
                },
            )?;
        }
        Ok(())
    })
    .unwrap();
    let view = db.snapshot().unwrap();
    assert_eq!(
        view.scan::<Accounts>()
            .unwrap()
            .map(|(k, _)| k)
            .collect::<Vec<_>>(),
        [0, 1, u64::MAX]
    );
    assert_eq!(view.lookup::<Accounts>(1, b"").unwrap()[0].0, 0);
    assert_eq!(
        view.index_range::<Accounts>(u64::MAX, ..)
            .unwrap()
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>(),
        [0, 1, u64::MAX]
    );
    assert_eq!(
        view.index_range::<Accounts>(
            u64::MAX,
            (
                Bound::Excluded(0u64.to_be_bytes().to_vec()),
                Bound::Included(1u64.to_be_bytes().to_vec())
            )
        )
        .unwrap()[0]
            .0,
        1
    );
    assert!(
        view.index_range::<Accounts>(1, "a".as_bytes().to_vec().."a".as_bytes().to_vec())
            .unwrap()
            .is_empty()
    );
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = view.index_range::<Accounts>(
                1,
                (
                    Bound::Excluded(b"a".to_vec()),
                    Bound::Excluded(b"a".to_vec()),
                ),
            );
        }))
        .is_err()
    );
    db.write(|tx| {
        tx.remove::<Accounts>(u64::MAX)?;
        Ok(())
    })
    .unwrap();
    assert!(
        db.snapshot()
            .unwrap()
            .get::<Accounts>(u64::MAX)
            .unwrap()
            .is_none()
    );
    assert!(view.get::<Accounts>(u64::MAX).unwrap().is_some());
}
