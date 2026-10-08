use super::*;
use crate::log::{Wal, encode_transaction, file_header};
use crate::test_support::TestStorage;
use crate::test_support::banking;
#[cfg(unix)]
use crate::test_support::banking_v2;
use banking::*;

fn reopen(storage: &TestStorage) -> CatalogDatabase<Banking> {
    let (wal, recovered) = Wal::recover::<Stored<Banking>>(Box::new(storage.clone())).unwrap();
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
    let storage = TestStorage::new(bytes);
    let db = reopen(&storage);
    seed(&db).unwrap();
    storage.image()
}
fn verify(db: &CatalogDatabase<Banking>, committed: bool) {
    let read = db.read().unwrap();
    assert_eq!(
        read.get::<Accounts>(1).unwrap().unwrap().balance,
        if committed { 75 } else { 100 }
    );
    assert_eq!(
        read.get::<Accounts>(2).unwrap().unwrap().balance,
        if committed { 125 } else { 100 }
    );
    assert_eq!(read.get::<Transfers>(7).unwrap().is_some(), committed);
    let balance = if committed { 75u64 } else { 100 };
    assert_eq!(
        read.lookup::<Accounts>(2, &balance.to_be_bytes()).unwrap()[0].0,
        1
    );
}
#[test]
fn every_short_write_and_uncertain_sync_keep_rows_and_indexes_atomic() {
    let before = initial();
    let probe = TestStorage::new(before.clone());
    let db = reopen(&probe);
    db.write(|tx| transfer(tx, 7, 1, 2, 25)).unwrap();
    let length = probe.image().len() - before.len();
    for cutoff in 0..length {
        let disk = TestStorage::new(before.clone());
        let db = reopen(&disk);
        disk.fail_write_after(cutoff);
        assert!(matches!(
            db.write(|tx| transfer(tx, 7, 1, 2, 25)),
            Err(Error::CommitUncertain(_))
        ));
        assert!(matches!(db.read(), Err(Error::Poisoned)));
        assert!(matches!(db.write(|_| Ok(())), Err(Error::Poisoned)));
        drop(db);
        disk.clear_faults();
        verify(&reopen(&disk), false);
        assert_eq!(disk.image(), before);
    }
    let disk = TestStorage::new(before);
    let db = reopen(&disk);
    disk.fail_sync();
    assert!(matches!(
        db.write(|tx| transfer(tx, 7, 1, 2, 25)),
        Err(Error::CommitUncertain(_))
    ));
    assert!(matches!(db.read(), Err(Error::Poisoned)));
    drop(db);
    disk.clear_faults();
    verify(&reopen(&disk), true);
}
#[test]
fn catalog_insert_then_remove_is_a_noop_without_append_or_sync() {
    let disk = TestStorage::new(initial());
    let db = reopen(&disk);
    let before = disk.image();
    let syncs = disk.syncs();
    db.write(|tx| {
        tx.insert::<Accounts>(
            3,
            Account {
                email: "temporary".into(),
                balance: 100,
            },
        )?;
        assert!(tx.remove::<Accounts>(3)?);
        assert!(tx.lookup::<Accounts>(1, b"temporary")?.is_empty());
        Ok(())
    })
    .unwrap();
    assert_eq!(disk.image(), before);
    assert_eq!(disk.syncs(), syncs);
    verify(&db, false);
}

#[test]
fn catalog_golden_envelope_is_independently_encoded() {
    let fixture: Vec<u8> = include_str!("../tests/fixtures/catalog-v1.hex")
        .split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect();
    let mut encoder = Encoder::default();
    Stored::<Banking>::metadata().encode(&mut encoder).unwrap();
    let mut bytes = encoder.finish();
    let mut encoder = Encoder::default();
    Stored::<Banking>::row(
        u64::MAX,
        Row::Account(Account {
            email: "a".into(),
            balance: 9,
        }),
    )
    .encode(&mut encoder)
    .unwrap();
    bytes.extend(encoder.finish());
    assert_eq!(bytes, fixture);
}

#[cfg(unix)]
mod managed {
    use super::*;
    use std::fs;
    use std::io::Write;
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            Self(std::env::temp_dir().join(format!(
                    "skrin-catalog-fault-{}-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos(),
                    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                )))
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn corrupt_complete_unique_delta_prevents_torn_tail_repair() {
        let path = Temp::new();
        let db = CatalogDatabase::<Banking>::create_dir(&path.0).unwrap();
        seed(&db).unwrap();
        let slot = db.indexes.read().unwrap().primary[&(1, 2)];
        drop(db);
        let wal = path.0.join("g0000000000000001/wal");
        let corrupt = BTreeMap::from([(
            slot,
            Some(Stored::<Banking>::row(
                2,
                Row::Account(Account {
                    email: "alice@example.test".into(),
                    balance: 100,
                }),
            )),
        )]);
        let mut file = fs::OpenOptions::new().append(true).open(&wal).unwrap();
        file.write_all(&encode_transaction(2, &corrupt).unwrap())
            .unwrap();
        file.write_all(b"TX").unwrap();
        drop(file);
        let before = fs::read(&wal).unwrap();
        assert!(matches!(
            CatalogDatabase::<Banking>::open_dir(&path.0),
            Err(Error::Corrupt { .. })
        ));
        assert_eq!(fs::read(wal).unwrap(), before);
    }
    #[test]
    fn every_checkpoint_failure_keeps_stable_lock_and_coherent_indexes() {
        let mut clean = 0;
        let mut uncertain = 0;
        for cutoff in 0..100 {
            let path = Temp::new();
            let db = CatalogDatabase::<Banking>::create_dir(&path.0).unwrap();
            seed(&db).unwrap();
            db.write(|tx| transfer(tx, 7, 1, 2, 25)).unwrap();
            crate::directory::faults::arm(cutoff);
            let outcome = db.checkpoint();
            crate::directory::faults::clear();
            assert!(matches!(
                CatalogDatabase::<Banking>::open_dir(&path.0),
                Err(Error::Busy)
            ));
            match outcome {
                Err(Error::MaintenanceUncertain(_)) => {
                    uncertain += 1;
                    assert!(matches!(db.read(), Err(Error::Poisoned)));
                }
                Err(_) => {
                    clean += 1;
                    verify(&db, true);
                }
                Ok(_) => {
                    assert!(clean > 20 && uncertain >= 3);
                    return;
                }
            }
            drop(db);
            verify(
                &CatalogDatabase::<Banking>::open_dir(&path.0).unwrap(),
                true,
            );
        }
        panic!("not every boundary exercised");
    }
    #[test]
    fn persistence_images_preserve_acknowledged_multi_table_rows_and_indexes() {
        for migration in [false, true] {
            let path = Temp::new();
            let db = CatalogDatabase::<Banking>::create_dir(&path.0).unwrap();
            seed(&db).unwrap();
            crate::persistence_model::start(&path.0, false);
            db.write(|tx| transfer(tx, 7, 1, 2, 25)).unwrap();
            crate::persistence_model::boundary();
            if migration {
                drop(
                    db.migrate::<banking_v2::BankingV2>("active", banking_v2::migrate_row)
                        .unwrap(),
                );
            } else {
                db.checkpoint().unwrap();
                drop(db);
            }
            let images = crate::persistence_model::finish();
            assert!(images.len() > 20);
            for (cut, image) in images.iter().enumerate() {
                image.restore(&path.0);
                match CatalogDatabase::<Banking>::open_dir(&path.0) {
                    Ok(db) => verify(&db, cut != 0),
                    Err(Error::SchemaMismatch { .. }) if migration => {
                        let db =
                            CatalogDatabase::<banking_v2::BankingV2>::open_dir(&path.0).unwrap();
                        let read = db.read().unwrap();
                        assert_eq!(
                            read.get::<banking_v2::AccountsV2>(1)
                                .unwrap()
                                .unwrap()
                                .balance,
                            75
                        );
                        assert_eq!(
                            read.lookup::<banking_v2::AccountsV2>(2, &75u64.to_be_bytes())
                                .unwrap()[0]
                                .0,
                            1
                        );
                        assert_eq!(read.get::<Transfers>(7).unwrap().unwrap().amount, 25);
                    }
                    Err(error) => panic!("migration={migration} cut={cut}: {error}"),
                }
            }
        }
    }
    #[test]
    fn process_exits_at_catalog_publication_boundaries_recover_coherent_indexes() {
        for migration in [false, true] {
            let mut completed = false;
            for cutoff in 0..100 {
                let path = Temp::new();
                let db = CatalogDatabase::<Banking>::create_dir(&path.0).unwrap();
                seed(&db).unwrap();
                db.write(|tx| transfer(tx, 7, 1, 2, 25)).unwrap();
                drop(db);
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "catalog::tests::managed::child_catalog_publication_exit",
                    ])
                    .env("SKRIN_CATALOG_EXIT_ROOT", &path.0)
                    .env("SKRIN_CATALOG_EXIT_AFTER", cutoff.to_string())
                    .env("SKRIN_CATALOG_EXIT_MIGRATION", migration.to_string())
                    .stdout(std::process::Stdio::null())
                    .status()
                    .unwrap();
                assert!(status.success() || status.code() == Some(73));
                match CatalogDatabase::<Banking>::open_dir(&path.0) {
                    Ok(db) => verify(&db, true),
                    Err(Error::SchemaMismatch { .. }) if migration => {
                        let db =
                            CatalogDatabase::<banking_v2::BankingV2>::open_dir(&path.0).unwrap();
                        let read = db.read().unwrap();
                        assert_eq!(
                            read.get::<banking_v2::AccountsV2>(1)
                                .unwrap()
                                .unwrap()
                                .balance,
                            75
                        );
                        assert_eq!(
                            read.lookup::<banking_v2::AccountsV2>(1, b"alice@example.test")
                                .unwrap()[0]
                                .0,
                            1
                        );
                        assert_eq!(read.get::<Transfers>(7).unwrap().unwrap().amount, 25);
                    }
                    Err(error) => panic!("migration={migration} cutoff={cutoff}: {error}"),
                }
                if status.success() {
                    completed = true;
                    break;
                }
            }
            assert!(completed);
        }
    }
    #[test]
    fn child_catalog_publication_exit() {
        let Some(path) = std::env::var_os("SKRIN_CATALOG_EXIT_ROOT") else {
            return;
        };
        let db = CatalogDatabase::<Banking>::open_dir(path).unwrap();
        let cutoff = std::env::var("SKRIN_CATALOG_EXIT_AFTER")
            .unwrap()
            .parse()
            .unwrap();
        crate::directory::faults::exit_after(cutoff);
        if std::env::var("SKRIN_CATALOG_EXIT_MIGRATION").unwrap() == "true" {
            drop(
                db.migrate::<banking_v2::BankingV2>("active", banking_v2::migrate_row)
                    .unwrap(),
            );
        } else {
            db.checkpoint().unwrap();
        }
    }
    struct Broken;
    impl Catalog for Broken {
        const SCHEMA: Schema = Schema {
            table_id: 8000,
            version: 2,
        };
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
            Ok(match Banking::decode(id, d)? {
                Row::Account(r) => Row::Account(Account {
                    email: "broken-roundtrip".into(),
                    balance: r.balance,
                }),
                r => r,
            })
        }
        fn index_key(id: u64, r: &Row) -> Result<Vec<u8>> {
            Banking::index_key(id, r)
        }
    }
    impl Table<Broken> for Accounts {
        type Record = Account;
        fn into_row(r: Account) -> Row {
            Row::Account(r)
        }
        fn borrow(r: &Row) -> Option<&Account> {
            <Accounts as Table<Banking>>::borrow(r)
        }
    }
    #[test]
    fn decoded_constraints_and_projection_changes_fail_before_publication() {
        let path = Temp::new();
        let backup = Temp::new();
        let broken = CatalogDatabase::<Broken>::create_dir(&path.0).unwrap();
        broken
            .write(|tx| {
                tx.insert::<Accounts>(
                    1,
                    Account {
                        email: "one".into(),
                        balance: 1,
                    },
                )?;
                tx.insert::<Accounts>(
                    2,
                    Account {
                        email: "two".into(),
                        balance: 2,
                    },
                )
            })
            .unwrap();
        let current = fs::read(path.0.join("CURRENT")).unwrap();
        assert!(
            broken
                .checkpoint_if_needed(
                    crate::CheckpointPolicy {
                        wal_bytes: None,
                        commits: Some(1)
                    },
                    MaintenanceOptions::default()
                )
                .is_err()
        );
        assert!(broken.checkpoint().is_err());
        assert_eq!(fs::read(path.0.join("CURRENT")).unwrap(), current);
        assert!(matches!(
            broken.backup_to(&backup.0),
            Err(Error::UniqueViolation { .. })
        ));
        assert!(!backup.0.join("CURRENT").exists());
        assert_eq!(
            broken
                .read()
                .unwrap()
                .get::<Accounts>(1)
                .unwrap()
                .unwrap()
                .email,
            "one"
        );
        let path = Temp::new();
        let db = CatalogDatabase::<Banking>::create_dir(&path.0).unwrap();
        seed(&db).unwrap();
        let current = fs::read(path.0.join("CURRENT")).unwrap();
        assert!(matches!(
            db.migrate::<Broken>("broken-codec", |_, _, r| Ok(r)),
            Err(Error::UniqueViolation { .. })
        ));
        assert_eq!(fs::read(path.0.join("CURRENT")).unwrap(), current);
        verify(
            &CatalogDatabase::<Banking>::open_dir(&path.0).unwrap(),
            false,
        );
    }
    #[test]
    fn full_survival_projection_keeps_multi_table_rows_and_indexes_coherent() {
        use crate::persistence_model::{self as model, Omission};
        for migration in [false, true] {
            let path = Temp::new();
            let db = CatalogDatabase::<Banking>::create_dir(&path.0).unwrap();
            seed(&db).unwrap();
            model::start_full(&path.0, Omission::None, 1);
            db.write(|tx| transfer(tx, 7, 1, 2, 25)).unwrap();
            model::acknowledged(2);
            if migration {
                let next = db
                    .migrate::<banking_v2::BankingV2>("full-active", banking_v2::migrate_row)
                    .unwrap();
                model::published(next.generation_info().unwrap().unwrap().generation);
                drop(next);
            } else {
                let cp = db.checkpoint().unwrap();
                model::published(cp.generation);
                drop(db);
            }
            let images = model::finish();
            assert!(images.iter().any(|image| image.partial_append));
            for (cut, image) in images.iter().enumerate() {
                image.restore(&path.0);
                match CatalogDatabase::<Banking>::open_dir(&path.0) {
                    Ok(db) => {
                        let sequence = db.stats().unwrap().commits;
                        assert!(
                            (image.acknowledged_sequence.unwrap()..=2).contains(&sequence),
                            "cut {cut}"
                        );
                        verify(&db, sequence == 2);
                        if let Some(generation) = image.published_generation {
                            assert!(!migration);
                            assert_eq!(
                                db.generation_info().unwrap().unwrap().generation,
                                generation
                            );
                        }
                    }
                    Err(Error::SchemaMismatch { .. }) if migration => {
                        let db =
                            CatalogDatabase::<banking_v2::BankingV2>::open_dir(&path.0).unwrap();
                        let read = db.read().unwrap();
                        assert_eq!(read.sequence(), 2);
                        assert_eq!(read.get::<Transfers>(7).unwrap().unwrap().amount, 25);
                        assert_eq!(
                            read.get::<banking_v2::AccountsV2>(1)
                                .unwrap()
                                .unwrap()
                                .balance,
                            75
                        );
                        assert_eq!(
                            read.get::<banking_v2::AccountsV2>(2)
                                .unwrap()
                                .unwrap()
                                .balance,
                            125
                        );
                        assert_eq!(
                            read.lookup::<banking_v2::AccountsV2>(2, &75u64.to_be_bytes())
                                .unwrap()[0]
                                .0,
                            1
                        );
                        if let Some(generation) = image.published_generation {
                            assert_eq!(
                                db.generation_info().unwrap().unwrap().generation,
                                generation
                            );
                        }
                    }
                    Err(error) => panic!("migration={migration} cut={cut}: {error}"),
                }
            }
        }
    }
}

#[test]
fn grouped_transactions_keep_final_uniqueness_and_all_indexes_coherent() {
    use crate::group_commit::GroupCommitOptions;
    use std::sync::mpsc;
    let disk = TestStorage::new(initial());
    let group = reopen(&disk)
        .into_group_commit(GroupCommitOptions {
            queue_capacity: 3,
            max_transactions: 3,
            max_delay: std::time::Duration::ZERO,
        })
        .unwrap();
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
    let first = group.submit(|tx| transfer(tx, 7, 1, 2, 25)).unwrap();
    let conflict = group
        .submit(|tx| {
            tx.insert::<Accounts>(
                3,
                Account {
                    email: "alice@example.test".into(),
                    balance: 100,
                },
            )
        })
        .unwrap();
    let last = group
        .submit(|tx| {
            assert_eq!(tx.lookup::<Accounts>(2, &75u64.to_be_bytes())?[0].0, 1);
            transfer(tx, 8, 1, 2, 5)
        })
        .unwrap();
    resume.send(()).unwrap();
    paused.wait().unwrap();
    let first = first.wait().unwrap();
    assert!(matches!(
        conflict.wait(),
        Err(Error::UniqueViolation { index_id: 1 })
    ));
    let last = last.wait().unwrap();
    assert_eq!((first.sequence, last.sequence), (3, 4));
    assert_eq!(
        (first.transactions_in_group, last.transactions_in_group),
        (2, 2)
    );
    let db = group.into_database().unwrap();
    let read = db.read().unwrap();
    assert_eq!(read.get::<Accounts>(1).unwrap().unwrap().balance, 70);
    assert_eq!(
        read.lookup::<Accounts>(2, &130u64.to_be_bytes()).unwrap()[0].0,
        2
    );
    assert!(read.get::<Accounts>(3).unwrap().is_none());
    drop(read);
    drop(db);
    let db = reopen(&disk);
    assert_eq!(db.read().unwrap().sequence(), 4);
    let read = db.read().unwrap();
    assert!(read.get::<Transfers>(7).unwrap().is_some());
    assert!(read.get::<Transfers>(8).unwrap().is_some());
}

#[test]
fn grouped_catalog_short_writes_and_shared_sync_recover_only_coherent_prefixes() {
    use crate::group_commit::GroupCommitOptions;
    use std::sync::mpsc;
    let before = initial();
    let probe = TestStorage::new(before.clone());
    let db = reopen(&probe);
    db.write(|tx| transfer(tx, 7, 1, 2, 25)).unwrap();
    let first = probe.image().len() - before.len();
    db.write(|tx| transfer(tx, 8, 1, 2, 5)).unwrap();
    let length = probe.image().len() - before.len();
    drop(db);
    for cutoff in 0..=length {
        let disk = TestStorage::new(before.clone());
        let db = reopen(&disk);
        let group = db
            .into_group_commit(GroupCommitOptions {
                queue_capacity: 2,
                max_transactions: 2,
                max_delay: std::time::Duration::ZERO,
            })
            .unwrap();
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
        let a = group.submit(|tx| transfer(tx, 7, 1, 2, 25)).unwrap();
        let b = group.submit(|tx| transfer(tx, 8, 1, 2, 5)).unwrap();
        if cutoff == length {
            disk.fail_sync_enospc();
        } else {
            disk.fail_enospc_after(cutoff);
        }
        resume.send(()).unwrap();
        paused.wait().unwrap();
        assert!(matches!(a.wait(), Err(Error::CommitUncertain(_))));
        assert!(matches!(
            b.wait(),
            Err(Error::CommitUncertain(_)) | Err(Error::Poisoned)
        ));
        assert!(matches!(group.read(), Err(Error::Poisoned)));
        drop(group);
        disk.clear_faults();
        let db = reopen(&disk);
        let read = db.read().unwrap();
        let committed = if cutoff < first {
            0
        } else if cutoff < length {
            1
        } else {
            2
        };
        assert_eq!(read.sequence(), 2 + committed);
        let balance = [100u64, 75, 70][committed as usize];
        assert_eq!(read.get::<Accounts>(1).unwrap().unwrap().balance, balance);
        assert_eq!(
            read.lookup::<Accounts>(2, &balance.to_be_bytes()).unwrap()[0].0,
            1
        );
        assert_eq!(read.get::<Transfers>(7).unwrap().is_some(), committed >= 1);
        assert_eq!(read.get::<Transfers>(8).unwrap().is_some(), committed == 2);
    }
}

#[test]
fn replacement_delta_keeps_primary_and_only_changes_affected_postings() {
    let disk = TestStorage::new(initial());
    let db = reopen(&disk);
    let indexes = db.indexes.read().unwrap();
    let read = db.database.read().unwrap();
    let slot = indexes.primary[&(1, 1)];
    let old = &read.state.rows[&slot].data.as_ref().unwrap().1;
    let Row::Account(old) = old else {
        panic!("account")
    };
    let prepare = |email: &str, balance| {
        indexes
            .prepare(
                &read.state.rows,
                &BTreeMap::from([(
                    slot,
                    Some(Stored::row(
                        1,
                        Row::Account(Account {
                            email: email.into(),
                            balance,
                        }),
                    )),
                )]),
            )
            .unwrap()
    };
    let unchanged = prepare(&old.email, old.balance);
    assert!(unchanged.removed[0].retain_address && unchanged.added[0].retain_address);
    assert!(unchanged.removed[0].keys.is_empty() && unchanged.added[0].keys.is_empty());
    let balance = prepare(&old.email, old.balance + 1);
    assert_eq!(
        balance.removed[0].keys,
        vec![(2, old.balance.to_be_bytes().to_vec())]
    );
    assert_eq!(
        balance.added[0].keys,
        vec![(2, (old.balance + 1).to_be_bytes().to_vec())]
    );
    // Filtering must not exempt unchanged unique keys from final-view validation.
    let collision = BTreeMap::from([
        (
            slot,
            Some(Stored::row(
                1,
                Row::Account(Account {
                    email: old.email.clone(),
                    balance: old.balance + 1,
                }),
            )),
        ),
        (
            indexes.next_slot,
            Some(Stored::row(
                3,
                Row::Account(Account {
                    email: old.email.clone(),
                    balance: 9,
                }),
            )),
        ),
    ]);
    assert!(matches!(
        indexes.prepare(&read.state.rows, &collision),
        Err(Error::UniqueViolation { index_id: 1 })
    ));
}
