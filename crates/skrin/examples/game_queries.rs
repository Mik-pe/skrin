//! Indexed Rust queries, filtering, projection, joins and stable frame pagination.
#[path = "support/world_queries.rs"]
mod queries;
#[path = "support/world.rs"]
mod world;
use queries::*;
use skrin::Result;
use skrin::catalog::CatalogDatabase;
use world::*;

fn main() -> Result<()> {
    let path = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    let db = match &path {
        Some(path) => CatalogDatabase::<World>::create_dir(path)?,
        None => CatalogDatabase::<World>::in_memory()?,
    };
    seed(&db, 257)?;
    let db = db.into_snapshots(snapshot_options(), footprint)?;
    let old = db.snapshot()?;
    verify_queries(&old, 257, 0, 64)?;
    let query = Visible {
        areas: 0..=3,
        x: 30..=100,
        y: 0..=0,
    };
    let first = visible_entities(&old, &query, None, 7)?;
    let (key, row) = first.last().unwrap();
    let next = visible_entities(&old, &query, Some((row.area, *key)), 7)?;
    println!(
        "visible page 1 IDs: {:?}",
        first.iter().map(|(k, _)| *k).collect::<Vec<_>>()
    );
    println!(
        "visible page 2 IDs: {:?}",
        next.iter().map(|(k, _)| *k).collect::<Vec<_>>()
    );
    let request = Save {
        id: 0,
        rows: 257,
        batch: 64,
    };
    assert!(db.write(|tx| save(tx, request))?);
    let current = db.snapshot()?;
    verify_queries(&old, 257, 0, 64)?;
    verify_queries(&current, 257, 1, 64)?;
    let found = inventory(&current, 1, Some(0), None, 10)?;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].key, 0);
    assert!(inventory(&old, 1, Some(0), None, 10)?.is_empty());
    println!("owner 1, kind 0, item plus owner from one frame: {found:?}");
    assert!(!db.write(|tx| save(tx, request))?);
    drop(old);
    drop(current);
    let db = db.into_database()?;
    verify(&db, 257, 1, 64)?;
    if let Some(path) = path {
        drop(db);
        let db = CatalogDatabase::<World>::open_dir(&path)?
            .into_snapshots(snapshot_options(), footprint)?;
        verify_queries(&db.snapshot()?, 257, 1, 64)?;
        db.checkpoint()?;
        db.reclaim()?;
        let backup_path = path.with_extension("query-backup");
        let backup = db
            .backup_to(&backup_path)?
            .into_snapshots(snapshot_options(), footprint)?;
        verify_queries(&backup.snapshot()?, 257, 1, 64)?;
        drop(backup);
        drop(db);
        let db = CatalogDatabase::<World>::open_dir(path)?
            .into_snapshots(snapshot_options(), footprint)?;
        verify_queries(&db.snapshot()?, 257, 1, 64)?;
        println!(
            "queries verified after WAL reopen, checkpoint, independent backup and snapshot reopen"
        );
    } else {
        println!(
            "volatile indexed queries, filtered pages, coherent inventory joins and old/current frames verified"
        );
    }
    Ok(())
}
