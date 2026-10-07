#[path = "../examples/support/banking.rs"]
mod banking;
#[cfg(unix)]
#[path = "../examples/support/banking_v2.rs"]
mod banking_v2;
use banking::*;
use skrin::catalog::CatalogDatabase;
#[cfg(unix)]
use skrin::catalog::{Catalog, IndexDefinition};
#[cfg(unix)]
use skrin::{Decoder, Encoder, Schema};
use skrin::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

fn account(email: &str, balance: u64) -> Account {
    Account {
        email: email.into(),
        balance,
    }
}
#[test]
fn transfers_unique_failures_and_operation_ids_are_atomic() -> Result<()> {
    let db = CatalogDatabase::<Banking>::in_memory()?;
    seed(&db)?;
    db.write(|tx| transfer(tx, u64::MAX, 1, 2, 30))?;
    let sequence = db.stats()?.commits;
    assert!(matches!(
        db.write(|tx| transfer(tx, u64::MAX, 1, 2, 30)),
        Err(Error::DuplicateKey(_))
    ));
    assert!(matches!(
        db.write(|tx| {
            transfer(tx, 5, 1, 2, 10)?;
            tx.put::<Accounts>(3, account("alice@example.test", 10))
        }),
        Err(Error::UniqueViolation { index_id: 1 })
    ));
    assert_eq!(db.stats()?.commits, sequence);
    let read = db.read()?;
    assert_eq!(read.get::<Accounts>(1)?.unwrap().balance, 70);
    assert_eq!(read.get::<Accounts>(2)?.unwrap().balance, 130);
    assert!(read.get::<Transfers>(5)?.is_none());
    assert_eq!(read.get::<Transfers>(u64::MAX)?.unwrap().amount, 30);
    assert_eq!(read.lookup::<Accounts>(2, &70u64.to_be_bytes())?[0].0, 1);
    Ok(())
}
#[test]
fn final_view_swaps_deletes_reinsertions_and_staged_indexes() -> Result<()> {
    let db = CatalogDatabase::<Banking>::in_memory()?;
    seed(&db)?;
    db.write(|tx| {
        tx.put::<Accounts>(1, account("bob@example.test", 100))?;
        assert_eq!(tx.lookup::<Accounts>(1, b"bob@example.test")?.len(), 2);
        tx.put::<Accounts>(2, account("alice@example.test", 100))?;
        assert!(tx.remove::<Accounts>(1)?);
        tx.insert::<Accounts>(1, account("bob@example.test", 100))?;
        assert_eq!(tx.lookup::<Accounts>(1, b"bob@example.test")?[0].0, 1);
        assert_eq!(
            tx.index_range::<Accounts>(1, b"alice".to_vec()..b"c".to_vec())?
                .iter()
                .map(|(key, _)| *key)
                .collect::<Vec<_>>(),
            [2, 1]
        );
        tx.insert::<Accounts>(u64::MAX, account("max@example.test", 50))?;
        assert!(tx.remove::<Accounts>(u64::MAX)?);
        Ok(())
    })?;
    let read = db.read()?;
    assert_eq!(
        read.lookup::<Accounts>(2, &100u64.to_be_bytes())?
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(
        read.index_range::<Accounts>(1, b"alice".to_vec()..b"c".to_vec())?
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>(),
        [2, 1]
    );
    assert_eq!(read.scan::<Accounts>()?.count(), 2);
    Ok(())
}
#[test]
fn randomized_final_view_matches_independent_rows_and_indexes() -> Result<()> {
    randomized_model(CatalogDatabase::<Banking>::in_memory()?, None)
}
fn randomized_model(
    mut db: CatalogDatabase<Banking>,
    path: Option<&std::path::Path>,
) -> Result<()> {
    let mut model = BTreeMap::<u64, (String, u64)>::new();
    let mut random = 991u64;
    let mut next = || {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        random
    };
    for round in 0..800 {
        let mut expected = model.clone();
        let mut edits = Vec::new();
        for _ in 0..1 + next() % 6 {
            let key = next() % 32;
            let value = if next() % 4 == 0 {
                None
            } else {
                Some((format!("email-{}", next() % 24), next() % 8))
            };
            match &value {
                Some(v) => {
                    expected.insert(key, v.clone());
                }
                None => {
                    expected.remove(&key);
                }
            }
            edits.push((key, value));
        }
        let unique = expected
            .values()
            .map(|(email, _)| email)
            .collect::<BTreeSet<_>>()
            .len()
            == expected.len();
        let outcome = db.write(|tx| {
            for (key, value) in edits {
                match value {
                    Some((email, balance)) => {
                        tx.put::<Accounts>(key, Account { email, balance })?
                    }
                    None => {
                        tx.remove::<Accounts>(key)?;
                    }
                }
            }
            Ok(())
        });
        if unique {
            outcome?;
            model = expected;
        } else {
            assert!(
                matches!(outcome, Err(Error::UniqueViolation { .. })),
                "round {round}"
            );
        }
        if let Some(path) = path
            && round % 53 == 0
        {
            if round % 106 == 0 {
                db.checkpoint()?;
                db.reclaim()?;
            }
            drop(db);
            db = CatalogDatabase::<Banking>::open_dir(path)?;
        }
        let read = db.read()?;
        let actual: BTreeMap<_, _> = read
            .scan::<Accounts>()?
            .map(|(k, r)| (k, (r.email.clone(), r.balance)))
            .collect();
        assert_eq!(actual, model, "round {round}");
        for balance in 0u64..8 {
            let keys: Vec<_> = read
                .lookup::<Accounts>(2, &balance.to_be_bytes())?
                .iter()
                .map(|(k, _)| *k)
                .collect();
            assert_eq!(
                keys,
                model
                    .iter()
                    .filter(|(_, (_, b))| *b == balance)
                    .map(|(&k, _)| k)
                    .collect::<Vec<_>>()
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
mod persistent {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "skrin-catalog-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn checkpoint_backup_and_reopen_rebuild_all_tables_and_indexes() -> Result<()> {
        let path = Temp::new();
        let backup_path = Temp::new();
        let mut db = CatalogDatabase::<Banking>::create_dir(&path.0)?;
        seed(&db)?;
        for round in 0..10 {
            db.write(|tx| transfer(tx, round, 1, 2, 1))?;
            let sequence = db.stats()?.commits;
            assert!(matches!(
                CatalogDatabase::<Banking>::open_dir(&path.0),
                Err(Error::Busy)
            ));
            db.checkpoint()?;
            db.reclaim()?;
            drop(db);
            db = CatalogDatabase::open_dir(&path.0)?;
            assert_eq!(db.stats()?.commits, sequence);
            assert_eq!(db.read()?.get::<Accounts>(1)?.unwrap().balance, 99 - round);
            assert_eq!(
                db.read()?
                    .lookup::<Accounts>(2, &(101 + round).to_be_bytes())?[0]
                    .0,
                2
            );
            assert_eq!(db.read()?.scan::<Transfers>()?.count(), round as usize + 1);
        }
        let backup = db.backup_to(&backup_path.0)?;
        drop(backup);
        db.write(|tx| transfer(tx, 50, 1, 2, 1))?;
        let backup = CatalogDatabase::<Banking>::open_dir(&backup_path.0)?;
        assert_eq!(backup.read()?.get::<Accounts>(1)?.unwrap().balance, 90);
        assert!(backup.read()?.get::<Transfers>(50)?.is_none());
        Ok(())
    }
    #[test]
    fn randomized_durable_model_survives_wal_and_snapshot_recovery() -> Result<()> {
        let path = Temp::new();
        randomized_model(
            CatalogDatabase::<Banking>::create_dir(&path.0)?,
            Some(&path.0),
        )
    }
    #[test]
    fn explicit_v1_import_preserves_source_bytes_sequence_and_full_keys() -> Result<()> {
        let source_path = Temp::new();
        let destination = Temp::new();
        let source = skrin::Database::<Account>::create(&source_path.0)?;
        source.write(|tx| tx.insert(u64::MAX, account("source", 9)))?;
        let before = fs::read(&source_path.0)?;
        let imported = CatalogDatabase::<Banking>::import_table::<Accounts>(
            &source,
            &destination.0,
            |_, r| Ok(account(&r.email, r.balance)),
        )?;
        assert_eq!(fs::read(&source_path.0)?, before);
        assert_eq!(imported.stats()?.commits, source.stats()?.commits);
        assert_eq!(
            imported.read()?.lookup::<Accounts>(1, b"source")?[0].0,
            u64::MAX
        );
        drop(imported);
        assert_eq!(
            CatalogDatabase::<Banking>::open_dir(&destination.0)?
                .read()?
                .get::<Accounts>(u64::MAX)?
                .unwrap()
                .balance,
            9
        );
        Ok(())
    }
    #[test]
    fn catalog_preflight_and_budgets_account_for_descriptor_and_preserve_indexes() -> Result<()> {
        use skrin::MaintenanceOptions;
        let path = Temp::new();
        let db = CatalogDatabase::<Banking>::create_dir(&path.0)?;
        seed(&db)?;
        db.write(|tx| transfer(tx, 7, 1, 2, 25))?;
        let inventory = db.storage_inventory()?;
        let current = fs::read(path.0.join("CURRENT"))?;
        let estimate = db.estimate_checkpoint(MaintenanceOptions::default())?;
        assert_eq!(estimate.rows, 4); // Three application rows plus catalog metadata.
        assert_eq!(db.stats()?.rows, 3);
        assert_eq!(db.storage_inventory()?, inventory);
        let row_budget = MaintenanceOptions {
            max_rows: 3,
            ..MaintenanceOptions::default()
        };
        assert!(matches!(
            db.checkpoint_with_options(row_budget),
            Err(Error::BudgetExceeded {
                resource: "snapshot rows",
                required: 4,
                ..
            })
        ));
        assert_eq!(db.storage_inventory()?, inventory);
        let short = MaintenanceOptions {
            max_new_file_bytes: estimate.new_file_bytes - 1,
            ..MaintenanceOptions::default()
        };
        assert!(matches!(
            db.checkpoint_with_options(short),
            Err(Error::BudgetExceeded { .. })
        ));
        assert_eq!(fs::read(path.0.join("CURRENT"))?, current);
        assert_eq!(
            db.read()?.lookup::<Accounts>(2, &75u64.to_be_bytes())?[0].0,
            1
        );
        assert_eq!(db.read()?.get::<Transfers>(7)?.unwrap().amount, 25);
        assert!(db.reclaim()?.generations_removed >= 1);
        let exact = MaintenanceOptions {
            max_new_file_bytes: estimate.new_file_bytes,
            max_rows: 4,
            max_record_bytes: estimate.largest_record_bytes,
            ..Default::default()
        };
        let checkpoint = db.checkpoint_with_options(exact)?;
        assert_eq!(checkpoint.rows, 3);
        assert_eq!(checkpoint.snapshot_bytes, estimate.snapshot_bytes);
        assert_eq!(db.generation_info()?.unwrap().checkpoint_sequence, 2);
        drop(db);
        let db = CatalogDatabase::<Banking>::open_dir(&path.0)?;
        assert_eq!(
            db.read()?.lookup::<Accounts>(1, b"alice@example.test")?[0].0,
            1
        );
        assert_eq!(db.read()?.get::<Transfers>(7)?.unwrap().amount, 25);
        Ok(())
    }
    #[test]
    fn catalog_policy_runs_only_when_called_and_checks_decoded_projection() -> Result<()> {
        use skrin::{CheckpointPolicy, MaintenanceOptions};
        let path = Temp::new();
        let db = CatalogDatabase::<Banking>::create_dir(&path.0)?;
        let policy = CheckpointPolicy {
            wal_bytes: None,
            commits: Some(2),
        };
        assert!(
            db.checkpoint_if_needed(policy, MaintenanceOptions::default())?
                .is_none()
        );
        seed(&db)?;
        assert!(
            db.checkpoint_if_needed(policy, MaintenanceOptions::default())?
                .is_none()
        );
        db.write(|tx| transfer(tx, 42, 1, 2, 25))?;
        assert_eq!(db.generation_info()?.unwrap().generation, 1);
        let report = db
            .checkpoint_if_needed(policy, MaintenanceOptions::default())?
            .unwrap();
        assert_eq!((report.rows, report.sequence), (3, 2));
        assert_eq!(db.stats()?.wal_bytes, 56);
        assert!(
            db.checkpoint_if_needed(
                CheckpointPolicy {
                    wal_bytes: Some(0),
                    commits: Some(0)
                },
                MaintenanceOptions::default()
            )?
            .is_none()
        );
        assert_eq!(
            db.read()?.lookup::<Accounts>(2, &125u64.to_be_bytes())?[0].0,
            2
        );
        Ok(())
    }
    #[test]
    fn catalog_backup_and_migration_limits_refuse_before_current_publication() -> Result<()> {
        use skrin::MaintenanceOptions;
        let path = Temp::new();
        let backup_path = Temp::new();
        let db = CatalogDatabase::<Banking>::create_dir(&path.0)?;
        seed(&db)?;
        let current = fs::read(path.0.join("CURRENT"))?;
        let too_small = MaintenanceOptions {
            max_rows: 2,
            ..MaintenanceOptions::default()
        };
        assert!(matches!(
            db.backup_to_with_options(&backup_path.0, too_small),
            Err(Error::BudgetExceeded { .. })
        ));
        assert!(!backup_path.0.exists());
        assert!(matches!(
            db.migrate_with_options::<banking_v2::BankingV2>(
                "bounded-v2",
                too_small,
                banking_v2::migrate_row
            ),
            Err(Error::BudgetExceeded { .. })
        ));
        assert_eq!(fs::read(path.0.join("CURRENT"))?, current);
        let db = CatalogDatabase::<Banking>::open_dir(&path.0)?;
        let backup = db.backup_to_with_options(&backup_path.0, MaintenanceOptions::default())?;
        assert_eq!(
            backup
                .read()?
                .lookup::<Accounts>(1, b"alice@example.test")?[0]
                .0,
            1
        );
        drop(backup);
        let next = db.migrate_with_options::<banking_v2::BankingV2>(
            "bounded-v2",
            MaintenanceOptions::default(),
            banking_v2::migrate_row,
        )?;
        assert_eq!(
            next.generation_info()?.unwrap().migrations[0].id,
            "bounded-v2"
        );
        assert_eq!(
            next.read()?
                .lookup::<banking_v2::AccountsV2>(1, b"bob@example.test")?[0]
                .0,
            2
        );
        Ok(())
    }
    struct Changed;
    impl Catalog for Changed {
        const SCHEMA: Schema = Banking::SCHEMA;
        const TABLES: &'static [Schema] = Banking::TABLES;
        const INDEXES: &'static [IndexDefinition] = &[];
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
        fn index_key(_: u64, _: &Row) -> Result<Vec<u8>> {
            unreachable!()
        }
    }
    #[test]
    fn missing_catalog_index_is_refused_without_repairing_a_torn_tail() -> Result<()> {
        use std::io::Write;
        let path = Temp::new();
        let db = CatalogDatabase::<Banking>::create_dir(&path.0)?;
        seed(&db)?;
        drop(db);
        let wal = path.0.join("g0000000000000001/wal");
        fs::OpenOptions::new()
            .append(true)
            .open(&wal)?
            .write_all(b"TX")?;
        let before = fs::read(&wal)?;
        let current = fs::read(path.0.join("CURRENT"))?;
        assert!(matches!(
            CatalogDatabase::<Changed>::open_dir(&path.0),
            Err(Error::Corrupt { .. })
        ));
        assert_eq!(fs::read(&wal)?, before);
        assert_eq!(fs::read(path.0.join("CURRENT"))?, current);
        let db = CatalogDatabase::<Banking>::open_dir(&path.0)?;
        assert_eq!(db.stats()?.recovered_tail_bytes, 2);
        Ok(())
    }
    #[test]
    fn migration_constraints_refuse_before_publication_then_valid_transition_reopens() -> Result<()>
    {
        let path = Temp::new();
        let db = CatalogDatabase::<Banking>::create_dir(&path.0)?;
        seed(&db)?;
        db.write(|tx| transfer(tx, 1, 1, 2, 1))?;
        let current = fs::read(path.0.join("CURRENT"))?;
        let outcome = db.migrate::<banking_v2::BankingV2>("bad", |table, key, r| {
            banking_v2::migrate_row(
                table,
                key,
                match r {
                    Row::Account(r) => Row::Account(account("duplicate", r.balance)),
                    r => r,
                },
            )
        });
        assert!(matches!(outcome, Err(Error::UniqueViolation { .. })));
        assert_eq!(fs::read(path.0.join("CURRENT"))?, current);
        let db = CatalogDatabase::<Banking>::open_dir(&path.0)?;
        let next = db.migrate::<banking_v2::BankingV2>("v2", banking_v2::migrate_row)?;
        next.checkpoint()?;
        drop(next);
        assert!(matches!(
            CatalogDatabase::<Banking>::open_dir(&path.0),
            Err(Error::SchemaMismatch { .. })
        ));
        let next = CatalogDatabase::<banking_v2::BankingV2>::open_dir(&path.0)?;
        assert_eq!(
            next.read()?
                .get::<banking_v2::AccountsV2>(1)?
                .unwrap()
                .balance,
            99
        );
        assert_eq!(
            next.read()?
                .lookup::<banking_v2::AccountsV2>(1, b"alice@example.test")?[0]
                .0,
            1
        );
        assert_eq!(next.read()?.get::<Transfers>(1)?.unwrap().amount, 1);
        Ok(())
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn required_catalog_reservation_keeps_all_rows_and_indexes_through_maintenance() -> Result<()> {
        let path = Temp::new();
        let backup_path = Temp::new();
        let db = CatalogDatabase::<Banking>::create_dir(&path.0)?;
        seed(&db)?;
        db.write(|tx| transfer(tx, 7, 1, 2, 25))?;
        let options = skrin::MaintenanceOptions {
            reserve_file_data: true,
            ..Default::default()
        };
        db.checkpoint_with_options(options)?;
        let backup = db.backup_to_with_options(&backup_path.0, options)?;
        assert_eq!(
            backup
                .read()?
                .lookup::<Accounts>(1, b"alice@example.test")?[0]
                .1
                .balance,
            75
        );
        assert_eq!(backup.read()?.get::<Transfers>(7)?.unwrap().amount, 25);
        let next = db.migrate_with_options::<banking_v2::BankingV2>(
            "reserved-active",
            options,
            banking_v2::migrate_row,
        )?;
        drop(next);
        let next = CatalogDatabase::<banking_v2::BankingV2>::open_dir(&path.0)?;
        assert_eq!(next.read()?.get::<Transfers>(7)?.unwrap().amount, 25);
        assert_eq!(
            next.read()?
                .lookup::<banking_v2::AccountsV2>(2, &75u64.to_be_bytes())?[0]
                .0,
            1
        );
        assert_eq!(next.read()?.sequence(), 2);
        Ok(())
    }
}
