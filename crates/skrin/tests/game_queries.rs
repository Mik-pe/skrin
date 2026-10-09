#![cfg(feature = "derive")]

#[path = "../examples/support/world_queries.rs"]
mod queries;
#[path = "../examples/support/world.rs"]
mod world;
use queries::*;
use skrin::catalog::CatalogDatabase;
use skrin::{Error, Result};
use world::*;

#[test]
fn filtered_pages_and_inventory_joins_keep_old_and_current_frames_coherent() -> Result<()> {
    let db = CatalogDatabase::<World>::in_memory()?;
    seed(&db, 257)?;
    let db = db.into_snapshots(snapshot_options(), footprint)?;
    let old = db.snapshot()?;
    for id in 0..73 {
        let request = Save {
            id,
            rows: 257,
            batch: 13,
        };
        assert!(db.write(|tx| save(tx, request))?);
        assert!(!db.write(|tx| save(tx, request))?);
    }
    verify_queries(&old, 257, 0, 13)?;
    let current = db.snapshot()?;
    verify_queries(&current, 257, 73, 13)?;
    assert_eq!(
        inventory(&old, 1, None, None, 2)?
            .iter()
            .map(|e| e.key)
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        inventory(&current, 73, None, None, 2)?
            .iter()
            .map(|e| e.key)
            .collect::<Vec<_>>(),
        [72, 73]
    );
    assert_eq!(inventory(&current, 73, Some(8), None, 2)?[0].key, 72);
    assert_eq!(inventory(&current, 73, None, Some(72), 1)?[0].key, 73);
    assert!(inventory(&current, 73, None, Some(u64::MAX), 1)?.is_empty());
    assert!(inventory(&current, 73, None, None, 0)?.is_empty());
    drop(current);
    drop(old);
    verify(&db.into_database()?, 257, 73, 13)
}

#[test]
fn cursor_uses_secondary_key_and_primary_tie_breaker_not_primary_order() -> Result<()> {
    let db = CatalogDatabase::<World>::in_memory()?;
    seed(&db, 5)?;
    let db = db.into_snapshots(snapshot_options(), footprint)?;
    db.write(|tx| {
        for (key, area) in [(1, 2), (2, 1), (4, 1)] {
            tx.update::<Entities>(key, |r| Ok(Entity { area, ..*r }))?;
        }
        Ok(())
    })?;
    let frame = db.snapshot()?;
    let query = Visible {
        areas: 0..=2,
        x: 0..=100,
        y: 0..=0,
    };
    let mut after = None;
    for expected in [0, 3, 2, 4, 1] {
        let page = visible_entities(&frame, &query, after, 1)?;
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].0, expected);
        after = Some((page[0].1.area, page[0].0));
    }
    assert!(visible_entities(&frame, &query, after, 1)?.is_empty());
    assert!(visible_entities(&frame, &query, Some((u64::MAX, u64::MAX)), 1)?.is_empty());
    let (lower, upper) = (2, 1);
    assert!(matches!(
        visible_entities(
            &frame,
            &Visible {
                areas: lower..=upper,
                ..query
            },
            None,
            1
        ),
        Err(Error::InvalidOperation(_))
    ));
    Ok(())
}
