//! Retained coherent read versions, admission limits, grouped retries and maintenance.
#[path = "support/banking.rs"]
mod banking;
#[cfg(unix)]
#[path = "support/banking_v2.rs"]
mod banking_v2;
use banking::*;
use skrin::catalog::CatalogDatabase;
#[cfg(unix)]
use skrin::group_commit::GroupCommitOptions;
use skrin::versioned::{CatalogSnapshotWrite, SnapshotOptions};
use skrin::{Error, Result};

fn footprint(row: &Row) -> Result<u64> {
    // This enum owns only Account.email's allocation. Assess actual capacity,
    // not codec size; more complicated application rows need nested accounting.
    Ok(std::mem::size_of::<Row>() as u64
        + match row {
            Row::Account(r) => r.email.capacity() as u64,
            Row::Transfer(_) => 0,
        })
}
fn transfer_version(
    tx: &mut CatalogSnapshotWrite<'_, Banking>,
    id: u64,
    amount: u64,
) -> Result<()> {
    tx.update::<Accounts>(1, |r| {
        Ok(Account {
            email: r.email.clone(),
            balance: r
                .balance
                .checked_sub(amount)
                .ok_or_else(|| Error::InvalidOperation("insufficient balance".into()))?,
        })
    })?;
    tx.update::<Accounts>(2, |r| {
        Ok(Account {
            email: r.email.clone(),
            balance: r
                .balance
                .checked_add(amount)
                .ok_or_else(|| Error::InvalidOperation("balance overflow".into()))?,
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
fn main() -> Result<()> {
    let path = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let baseline = match &path {
        Some(path) => CatalogDatabase::<Banking>::create_dir(path)?,
        None => CatalogDatabase::<Banking>::in_memory()?,
    };
    seed(&baseline)?;
    // Exercise the original baseline before the one-time shared-row conversion.
    baseline.write(|tx| transfer(tx, 7, 1, 2, 25))?;
    let db = baseline.into_snapshots(
        SnapshotOptions {
            max_snapshots: 2,
            max_pinned_bytes: 1024 * 1024,
        },
        footprint,
    )?;
    let old = db.snapshot()?;
    let cloned = old.clone();
    db.write(|tx| {
        tx.update::<Accounts>(1, |r| {
            Ok(Account {
                email: r.email.clone(),
                balance: r.balance - 5,
            })
        })?;
        tx.update::<Accounts>(2, |r| {
            Ok(Account {
                email: r.email.clone(),
                balance: r.balance + 5,
            })
        })?;
        tx.insert::<Transfers>(
            8,
            Transfer {
                from: 1,
                to: 2,
                amount: 5,
            },
        )
    })?;
    let current = db.snapshot()?;
    assert_eq!(old.get::<Accounts>(1)?.unwrap().balance, 75);
    assert_eq!(current.get::<Accounts>(1)?.unwrap().balance, 70);
    assert_eq!(old.lookup::<Accounts>(2, &75u64.to_be_bytes())?[0].0, 1);
    assert!(old.get::<Transfers>(8)?.is_none());
    assert!(current.get::<Transfers>(8)?.is_some());
    assert_eq!(db.retention()?.snapshots, 2); // `cloned` shares the first lease.
    assert!(matches!(db.snapshot(), Err(Error::BudgetExceeded { .. })));
    drop(current);
    drop(cloned);
    println!("reader retention: {:?}", db.retention()?);
    if path.is_none() {
        assert!(matches!(
            db.write(|tx| transfer_version(tx, 7, 25)),
            Err(Error::DuplicateKey(7))
        ));
        assert_eq!(db.snapshot()?.get::<Accounts>(1)?.unwrap().balance, 70);
        drop(old);
        assert_eq!(
            db.into_database()?
                .read()?
                .get::<Accounts>(1)?
                .unwrap()
                .balance,
            70
        );
        return Ok(());
    }
    #[cfg(unix)]
    if let Some(path) = path {
        let group = db.into_group_commit(GroupCommitOptions::default())?;
        // Dropping this result does not cancel its staged duplicate operation.
        // A propagated operation-ID conflict rolls back both account changes.
        drop(group.submit(|tx| transfer_version(tx, 7, 25))?);
        let retry = group.submit(|tx| transfer_version(tx, 7, 25))?;
        assert!(matches!(retry.wait(), Err(Error::DuplicateKey(7))));
        let barrier = group.submit(|_| Ok(()))?.wait()?;
        let current = group.snapshot()?;
        assert_eq!(current.sequence()?, barrier.synchronized_sequence);
        assert_eq!(current.get::<Accounts>(1)?.unwrap().balance, 70);
        assert_eq!(current.get::<Accounts>(2)?.unwrap().balance, 130);
        assert_eq!(
            current.lookup::<Accounts>(1, b"alice@example.test")?[0].0,
            1
        );
        assert_eq!(old.get::<Accounts>(1)?.unwrap().balance, 75);
        group.checkpoint()?;
        group.reclaim()?;
        let backup_path = path.with_extension("snapshot-backup");
        let backup = group.backup_to(&backup_path)?;
        assert_eq!(backup.read()?.get::<Accounts>(1)?.unwrap().balance, 70);
        drop(backup);
        let restored = CatalogDatabase::<Banking>::open_dir(&backup_path)?;
        assert_eq!(restored.read()?.scan::<Transfers>()?.count(), 2);
        assert!(matches!(
            CatalogDatabase::<Banking>::open_dir(&path),
            Err(Error::Busy)
        ));
        drop(current);
        drop(old);
        let db = group
            .into_database()?
            .migrate::<banking_v2::BankingV2>("snapshot-active-v2", banking_v2::migrate_row)?;
        assert!(db.read()?.get::<banking_v2::AccountsV2>(1)?.unwrap().active);
        drop(db);
        assert!(matches!(
            CatalogDatabase::<Banking>::open_dir(&path),
            Err(Error::SchemaMismatch { .. })
        ));
        let reopened = CatalogDatabase::<banking_v2::BankingV2>::open_dir(&path)?;
        assert_eq!(
            reopened
                .read()?
                .get::<banking_v2::AccountsV2>(1)?
                .unwrap()
                .balance,
            70
        );
        println!(
            "snapshot/group/checkpoint/backup/migration verified at {}",
            path.display()
        );
    }
    Ok(())
}
