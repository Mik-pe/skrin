#[path = "support/tasks.rs"]
mod tasks;
use skrin::Result;
use skrin::catalog::CatalogDatabase;
use std::path::PathBuf;
use tasks::*;

fn main() -> Result<()> {
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    let db = match &path {
        Some(path) => CatalogDatabase::<TaskStore>::create_dir(path)?,
        None => CatalogDatabase::<TaskStore>::in_memory()?,
    };
    seed(&db)?;
    let sequence = db.stats()?.commits;
    seed(&db)?; // The request ID makes this a no-op inside the write boundary.
    assert_eq!(db.stats()?.commits, sequence);
    let db = db.into_snapshots(options(), footprint)?;
    let old = db.snapshot()?;
    assert!(db.write(complete)?);
    assert!(!db.write(complete)?); // An acknowledged retry changes nothing.
    assert_eq!(old.matching(ByStatus, "open")?.count(), 1);
    assert_eq!(old.matching(ByTask, &42)?.count(), 0);
    let current = db.snapshot()?;
    assert_eq!(current.matching(ByStatus, "done")?.count(), 1);
    assert_eq!(current.get::<Tasks>(42)?.unwrap().spec_md, SPEC);
    assert_eq!(current.prefix(ByTitle, "Lagring/")?.take(10).count(), 1);
    drop(old);
    drop(current);
    let db = db.into_database()?;
    verify(&db.read()?)?;
    drop(db);

    if let Some(path) = path {
        let db = CatalogDatabase::<TaskStore>::open_dir(&path)?;
        verify(&db.read()?)?; // Decode the WAL before checkpointing.
        db.checkpoint()?;
        let backup = path.with_extension("backup");
        drop(db.backup_to(&backup)?);
        db.reclaim()?;
        drop(db);
        for path in [&path, &backup] {
            let db = CatalogDatabase::<TaskStore>::open_dir(path)?;
            verify(&db.read()?)?;
        }
        println!("Verified tasks, comments and exact text after WAL/snapshot/backup reopen");
    } else {
        println!("Verified atomic tasks/comments and exact text in memory");
    }
    Ok(())
}
