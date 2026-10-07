//! Equivalent synced workloads for plain rows, catalog rows and catalog indexes.
#[path = "../examples/support/banking.rs"]
mod banking;
use banking::*;
use skrin::catalog::{Catalog, CatalogDatabase, IndexDefinition};
use skrin::{Database, Decoder, Encoder, Result, Schema};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

struct Unindexed;
impl Catalog for Unindexed {
    const SCHEMA: Schema = Schema {
        table_id: 8001,
        version: 1,
    };
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
impl skrin::catalog::Table<Unindexed> for Accounts {
    type Record = Account;
    fn into_row(r: Account) -> Row {
        Row::Account(r)
    }
    fn borrow(r: &Row) -> Option<&Account> {
        <Accounts as skrin::catalog::Table<Banking>>::borrow(r)
    }
}
fn account(key: u64) -> Account {
    Account {
        email: format!("account-{key:08}@example.test"),
        balance: 1000,
    }
}
fn summary(name: &str, samples: &[Duration]) {
    let mut samples = samples.to_vec();
    samples.sort();
    let percentile = |p: usize| samples[(samples.len() * p).div_ceil(100).saturating_sub(1)];
    println!(
        "{name}: n={} p50_us={:.3} p95_us={:.3} p99_us={:.3}",
        samples.len(),
        percentile(50).as_secs_f64() * 1e6,
        percentile(95).as_secs_f64() * 1e6,
        percentile(99).as_secs_f64() * 1e6
    );
}
fn measure(name: &str, mut action: impl FnMut(u64) -> Result<()>) -> Result<()> {
    let mut samples = Vec::new();
    for n in 0..1000 {
        let start = Instant::now();
        action(n)?;
        samples.push(start.elapsed());
    }
    summary(name, &samples);
    Ok(())
}
fn plain(path: &Path, rows: u64) -> Result<()> {
    let db = Database::<Account>::create_dir(path)?;
    for begin in (0..rows).step_by(100) {
        db.write(|tx| {
            for key in begin..rows.min(begin + 100) {
                tx.insert(key, account(key))?;
            }
            Ok(())
        })?;
    }
    measure("plain_two_row_synced", |n| {
        db.write(|tx| {
            tx.update(n % rows, |r| {
                Ok(Account {
                    email: r.email.clone(),
                    balance: r.balance + 1,
                })
            })?;
            tx.update((n + 1) % rows, |r| {
                Ok(Account {
                    email: r.email.clone(),
                    balance: r.balance - 1,
                })
            })
        })
    })?;
    assert_eq!(
        db.read()?.iter().map(|(_, r)| r.balance).sum::<u64>(),
        rows * 1000
    );
    let start = Instant::now();
    db.checkpoint()?;
    println!(
        "plain_checkpoint_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    drop(db);
    let start = Instant::now();
    let db = Database::<Account>::open_dir(path)?;
    println!(
        "plain_warm_reopen_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    assert_eq!(
        db.read()?.iter().map(|(_, r)| r.balance).sum::<u64>(),
        rows * 1000
    );
    Ok(())
}
fn unindexed(path: &Path, rows: u64) -> Result<()> {
    let db = CatalogDatabase::<Unindexed>::create_dir(path)?;
    for begin in (0..rows).step_by(100) {
        db.write(|tx| {
            for key in begin..rows.min(begin + 100) {
                tx.insert::<Accounts>(key, account(key))?;
            }
            Ok(())
        })?;
    }
    measure("catalog_two_row_synced_no_indexes", |n| {
        db.write(|tx| {
            tx.update::<Accounts>(n % rows, |r| {
                Ok(Account {
                    email: r.email.clone(),
                    balance: r.balance + 1,
                })
            })?;
            tx.update::<Accounts>((n + 1) % rows, |r| {
                Ok(Account {
                    email: r.email.clone(),
                    balance: r.balance - 1,
                })
            })
        })
    })?;
    assert_eq!(
        db.read()?
            .scan::<Accounts>()?
            .map(|(_, r)| r.balance)
            .sum::<u64>(),
        rows * 1000
    );
    Ok(())
}
fn indexed(path: &Path, rows: u64) -> Result<()> {
    let db = CatalogDatabase::<Banking>::create_dir(path)?;
    for begin in (0..rows).step_by(100) {
        db.write(|tx| {
            for key in begin..rows.min(begin + 100) {
                tx.insert::<Accounts>(key, account(key))?;
            }
            Ok(())
        })?;
    }
    measure("catalog_two_row_synced_two_indexes", |n| {
        db.write(|tx| {
            tx.update::<Accounts>(n % rows, |r| {
                Ok(Account {
                    email: r.email.clone(),
                    balance: r.balance + 1,
                })
            })?;
            tx.update::<Accounts>((n + 1) % rows, |r| {
                Ok(Account {
                    email: r.email.clone(),
                    balance: r.balance - 1,
                })
            })
        })
    })?;
    measure("catalog_transfer_synced_two_indexes", |n| {
        db.write(|tx| transfer(tx, n, n % rows, (n + 1) % rows, 1))
    })?;
    assert_eq!(
        db.read()?
            .scan::<Accounts>()?
            .map(|(_, r)| r.balance)
            .sum::<u64>(),
        rows * 1000
    );
    assert_eq!(db.read()?.scan::<Transfers>()?.count(), 1000);
    assert_eq!(
        db.read()?
            .lookup::<Accounts>(1, account(0).email.as_bytes())?[0]
            .0,
        0
    );
    let start = Instant::now();
    db.checkpoint()?;
    println!(
        "catalog_checkpoint_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    drop(db);
    let start = Instant::now();
    let db = CatalogDatabase::<Banking>::open_dir(path)?;
    println!(
        "catalog_warm_reopen_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    assert_eq!(
        db.read()?
            .scan::<Accounts>()?
            .map(|(_, r)| r.balance)
            .sum::<u64>(),
        rows * 1000
    );
    assert_eq!(db.read()?.scan::<Transfers>()?.count(), 1000);
    Ok(())
}
fn main() -> Result<()> {
    if !cfg!(unix) {
        println!("persistent benchmark requires Unix");
        return Ok(());
    }
    let smoke = CatalogDatabase::<Banking>::in_memory()?;
    seed(&smoke)?;
    drop(smoke);
    let args: Vec<_> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let parent = args
        .first()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let rows: u64 = args
        .get(1)
        .map(|s| s.parse().expect("row count"))
        .unwrap_or(10000);
    let mode = args.get(2).map(String::as_str).unwrap_or("all");
    assert!(rows >= 2);
    let root = parent.as_path().join(format!(
        "skrin-catalog-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root)?;
    println!(
        "rows={rows} samples_per_phase=1000 durability=sync_all root={}",
        root.display()
    );
    let outcome = match mode {
        "plain" => plain(&root.join("plain"), rows),
        "unindexed" => unindexed(&root.join("unindexed"), rows),
        "indexed" => indexed(&root.join("indexed"), rows),
        "all" => plain(&root.join("plain"), rows)
            .and_then(|_| unindexed(&root.join("unindexed"), rows))
            .and_then(|_| indexed(&root.join("indexed"), rows)),
        _ => Err(skrin::Error::InvalidOperation(
            "mode must be plain, unindexed, indexed or all".into(),
        )),
    };
    fs::remove_dir_all(root)?;
    outcome
}
