//! Run with no arguments for volatile mode or a NEW managed directory path.
#[path = "support/banking.rs"]
mod banking;
#[path = "support/banking_v2.rs"]
mod banking_v2;
use banking::*;
use skrin::catalog::CatalogDatabase;
use skrin::{CheckpointPolicy, Error, MaintenanceOptions, Result};

fn main() -> Result<()> {
    let path = std::env::args_os().nth(1);
    let db = match &path {
        Some(path) => CatalogDatabase::<Banking>::create_dir(path)?,
        None => CatalogDatabase::in_memory()?,
    };
    seed(&db)?;
    db.write(|tx| transfer(tx, 42, 1, 2, 25))?;
    assert!(matches!(
        db.write(|tx| transfer(tx, 42, 1, 2, 25)),
        Err(Error::DuplicateKey(42))
    ));
    {
        let read = db.read()?;
        assert_eq!(read.get::<Accounts>(1)?.unwrap().balance, 75);
        assert_eq!(read.get::<Accounts>(2)?.unwrap().balance, 125);
        assert_eq!(read.lookup::<Accounts>(1, b"alice@example.test")?[0].0, 1);
        println!(
            "sequence {}: {:?}",
            read.sequence(),
            read.scan::<Accounts>()?.collect::<Vec<_>>()
        );
    }
    if let Some(path) = path {
        let options = MaintenanceOptions {
            max_new_file_bytes: 1024 * 1024,
            max_record_bytes: 4096,
            max_rows: 1000,
            reserve_file_data: cfg!(target_os = "linux"),
        };
        let estimate = db.estimate_checkpoint(options)?;
        println!(
            "catalog snapshot: {} stored rows including descriptor, {} new encoded bytes",
            estimate.rows, estimate.new_file_bytes
        );
        assert!(
            db.checkpoint_if_needed(
                CheckpointPolicy {
                    wal_bytes: None,
                    commits: Some(2)
                },
                options
            )?
            .is_some()
        );
        let inventory = db.storage_inventory()?;
        println!(
            "catalog storage: {} observed bytes",
            inventory.observed_file_bytes
        );
        db.reclaim()?;
        drop(db);
        let reopened = CatalogDatabase::<Banking>::open_dir(&path)?;
        assert_eq!(reopened.read()?.get::<Transfers>(42)?.unwrap().amount, 25);
        assert_eq!(
            reopened
                .read()?
                .lookup::<Accounts>(2, &125u64.to_be_bytes())?[0]
                .0,
            2
        );
        let migrated = reopened.migrate_with_options::<banking_v2::BankingV2>(
            "add-active",
            options,
            banking_v2::migrate_row,
        )?;
        assert!(
            migrated
                .read()?
                .get::<banking_v2::AccountsV2>(1)?
                .unwrap()
                .active
        );
        drop(migrated);
        assert!(matches!(
            CatalogDatabase::<Banking>::open_dir(&path),
            Err(Error::SchemaMismatch { .. })
        ));
        let migrated = CatalogDatabase::<banking_v2::BankingV2>::open_dir(&path)?;
        assert_eq!(migrated.read()?.get::<Transfers>(42)?.unwrap().amount, 25);
    }
    Ok(())
}
