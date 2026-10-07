//! Independent operations, dropped responses and explicit managed maintenance.
#[cfg(unix)]
#[path = "support/banking.rs"]
mod banking;
#[cfg(unix)]
#[path = "support/banking_v2.rs"]
mod banking_v2;

#[cfg(unix)]
fn main() -> skrin::Result<()> {
    use banking::*;
    use skrin::catalog::CatalogDatabase;
    use skrin::group_commit::GroupCommitOptions;
    use skrin::{Error, MaintenanceOptions};
    let path = std::env::args_os()
        .nth(1)
        .ok_or_else(|| Error::InvalidOperation("usage: group_commit NEW_DIRECTORY".into()))?;
    let path = std::path::PathBuf::from(path);
    let db = CatalogDatabase::<Banking>::create_dir(&path)?;
    seed(&db)?;
    let group = db.into_group_commit(GroupCommitOptions::default())?;
    // Dropping a response does not cancel the admitted operation. Retry uses
    // the same persisted operation ID; uniqueness rolls back staged balances.
    drop(group.submit(|tx| transfer(tx, 7, 1, 2, 25))?);
    let duplicate = group.submit(|tx| transfer(tx, 7, 1, 2, 25))?;
    let last = group.submit(|tx| transfer(tx, 8, 1, 2, 5))?;
    assert!(matches!(duplicate.wait(), Err(Error::DuplicateKey(7))));
    let receipt = last.wait()?;
    println!(
        "transaction {}, synchronized through {}, group frames {}, queue {:?}",
        receipt.sequence,
        receipt.synchronized_sequence,
        receipt.transactions_in_group,
        receipt.queue_time
    );
    {
        let read = group.read()?;
        assert_eq!(read.get::<Accounts>(1)?.unwrap().balance, 70);
        assert_eq!(read.lookup::<Accounts>(2, &130u64.to_be_bytes())?[0].0, 2);
        let transfer = read.get::<Transfers>(7)?.unwrap();
        assert_eq!((transfer.from, transfer.to, transfer.amount), (1, 2, 25));
    }
    assert!(matches!(
        CatalogDatabase::<Banking>::open_dir(&path),
        Err(Error::Busy)
    ));
    group.checkpoint()?;
    let backup_path = path.with_extension("backup");
    let backup = group.backup_to(&backup_path)?;
    assert_eq!(backup.read()?.sequence(), 3);
    drop(backup);
    group.reclaim()?;
    println!(
        "{} observed storage bytes",
        group.storage_inventory()?.observed_file_bytes
    );
    // No other clients: drain the queue and stop the worker before migration.
    let db = group.into_database()?;
    let migrated = db.migrate_with_options::<banking_v2::BankingV2>(
        "group-add-active",
        MaintenanceOptions {
            reserve_file_data: cfg!(target_os = "linux"),
            ..Default::default()
        },
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
    let reopened = CatalogDatabase::<banking_v2::BankingV2>::open_dir(&path)?;
    assert_eq!(reopened.read()?.sequence(), 3);
    assert_eq!(
        reopened
            .read()?
            .get::<banking_v2::AccountsV2>(1)?
            .unwrap()
            .balance,
        70
    );
    let backup = CatalogDatabase::<Banking>::open_dir(backup_path)?;
    assert_eq!(backup.read()?.get::<Transfers>(8)?.unwrap().amount, 5);
    Ok(())
}
#[cfg(not(unix))]
fn main() {
    eprintln!("group commit requires Unix persistent storage");
}
