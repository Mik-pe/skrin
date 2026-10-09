//! A SQL-free game-world save with coherent frame reads and safe retry.
#[path = "support/world.rs"]
mod world;
use skrin::catalog::CatalogDatabase;
use skrin::{Error, Result};
use world::*;

fn main() -> Result<()> {
    let path = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let db = match &path {
        Some(path) => CatalogDatabase::<World>::create_dir(path)?,
        None => CatalogDatabase::<World>::in_memory()?,
    };
    seed(&db, 128)?;
    let db = db.into_snapshots(snapshot_options(), footprint)?;
    let frame = db.snapshot()?;
    let worker = db.clone();
    let request = Save {
        id: 0,
        rows: 128,
        batch: 64,
    };
    // Simulated changes are sent as one bounded transaction. Only join/Ok
    // acknowledges durable completion; spawning a worker is not a saved game.
    let saved = std::thread::spawn(move || worker.write(|tx| save(tx, request)));
    assert_eq!(frame.get::<Entities>(0)?.unwrap().revision, 0);
    assert_eq!(frame.matching(Owner, &0u64)?.next().unwrap().0, 0);
    assert!(saved.join().expect("save worker panicked")?);
    let current = db.snapshot()?;
    assert_eq!(current.get::<Entities>(0)?.unwrap().revision, 1);
    assert_eq!(current.get::<Items>(0)?.unwrap().owner, 1);
    assert!(current.matching(Owner, &0u64)?.next().is_none());
    assert_eq!(current.matching(Owner, &1u64)?.count(), 2);
    assert_eq!(current.matching(Area, &0u64)?.count(), 64);
    // Retry after a lost acknowledgment: the complete request has already saved.
    assert!(!db.write(|tx| save(tx, request))?);
    assert!(matches!(
        db.write(|tx| save(
            tx,
            Save {
                batch: 32,
                ..request
            }
        )),
        Err(Error::InvalidOperation(_))
    ));
    drop(current);
    drop(frame);
    let db = db.into_database()?;
    verify(&db, 128, 1, 64)?;
    if let Some(path) = &path {
        db.checkpoint()?;
        db.reclaim()?;
        let backup_path = path.with_extension("world-backup");
        let backup = db.backup_to(&backup_path)?;
        verify(&backup, 128, 1, 64)?;
        drop(backup);
        drop(db);
        verify(&CatalogDatabase::<World>::open_dir(path)?, 128, 1, 64)?;
        verify(
            &CatalogDatabase::<World>::open_dir(&backup_path)?,
            128,
            1,
            64,
        )?;
        println!(
            "world save, checkpoint, backup and reopen verified at {}",
            path.display()
        );
    } else {
        println!(
            "volatile world save, atomic inventory move, old/current frames and retry verified"
        );
    }
    Ok(())
}
