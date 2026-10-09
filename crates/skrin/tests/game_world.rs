#![cfg(feature = "derive")]

#[path = "../examples/support/world.rs"]
mod world;
use skrin::catalog::CatalogDatabase;
use skrin::{Error, Result};
use world::*;

#[test]
fn wrapped_saves_match_an_independent_model_and_keep_old_frames() -> Result<()> {
    const ROWS: u64 = 67;
    const BATCH: u64 = 13;
    const SAVES: u64 = 73;
    let db = CatalogDatabase::<World>::in_memory()?;
    seed(&db, ROWS)?;
    let db = db.into_snapshots(snapshot_options(), footprint)?;
    let old = db.snapshot()?;
    let mut entities: Vec<_> = (0..ROWS).map(entity).collect();
    let mut items: Vec<_> = (0..ROWS).map(item).collect();
    let mut cursor = 0;
    for id in 0..SAVES {
        let request = Save {
            id,
            rows: ROWS,
            batch: BATCH,
        };
        assert!(db.write(|tx| save(tx, request))?);
        for _ in 0..BATCH {
            entities[cursor].x += 1;
            entities[cursor].revision += 1;
            cursor = (cursor + 1) % ROWS as usize;
        }
        let item = &mut items[(id % ROWS) as usize];
        item.owner = (item.owner + 1) % ROWS;
        let view = db.snapshot()?;
        for key in 0..ROWS {
            assert_eq!(view.get::<Entities>(key)?.unwrap(), &entities[key as usize]);
            assert_eq!(view.get::<Items>(key)?.unwrap(), &items[key as usize]);
            assert_eq!(old.get::<Entities>(key)?.unwrap(), &entity(key));
            assert_eq!(old.get::<Items>(key)?.unwrap(), &world::item(key));
        }
        assert!(!db.write(|tx| save(tx, request))?);
    }
    drop(old);
    verify(&db.into_database()?, ROWS, SAVES, BATCH)
}

#[test]
fn refused_inventory_move_rolls_back_already_staged_positions() -> Result<()> {
    let db = CatalogDatabase::<World>::in_memory()?;
    seed(&db, 4)?;
    // Save 4 expects item 0 to have moved once already. Its position changes
    // are staged before the ownership check, so refusal must roll back them.
    let request = Save {
        id: 4,
        rows: 4,
        batch: 2,
    };
    assert!(matches!(
        db.write(|tx| save(tx, request)),
        Err(Error::InvalidOperation(_))
    ));
    verify(&db, 4, 0, 2)?;
    let request = Save { id: 0, ..request };
    assert!(db.write(|tx| save(tx, request))?);
    let sequence = db.read()?.sequence();
    assert!(matches!(
        db.write(|tx| save(
            tx,
            Save {
                batch: 1,
                ..request
            }
        )),
        Err(Error::InvalidOperation(_))
    ));
    assert_eq!(db.read()?.sequence(), sequence);
    verify(&db, 4, 1, 2)
}
