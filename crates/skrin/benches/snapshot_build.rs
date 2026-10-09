//! Resident snapshot conversion only; no persistence or recovery speed claim.
#[path = "../examples/support/world.rs"]
mod world;
use skrin::catalog::CatalogDatabase;
use skrin::versioned::SnapshotOptions;
use skrin::{Database, Decoder, Encoder, Error, Record, Result, Schema};
use std::time::Instant;
use world::*;

struct Value(u64);
impl Record for Value {
    const SCHEMA: Schema = Schema {
        table_id: 1,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.u64(self.0)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(d.u64()?))
    }
}
fn value_bytes(_: &Value) -> Result<u64> {
    Ok(std::mem::size_of::<Value>() as u64)
}
fn options() -> SnapshotOptions {
    SnapshotOptions {
        max_pinned_bytes: 4 * 1024 * 1024 * 1024,
        ..snapshot_options()
    }
}

fn single(rows: u64) -> Result<()> {
    let db = Database::<Value>::in_memory();
    db.write(|tx| {
        for key in 0..rows {
            tx.insert(key, Value(key * 3))?;
        }
        Ok(())
    })?;
    let start = Instant::now();
    let db = db.into_snapshots(options(), value_bytes)?;
    let elapsed = start.elapsed();
    let bytes = db.retention()?.current_bytes;
    let old = db.snapshot()?;
    assert_eq!(old.sequence()?, 1);
    assert_eq!(old.len()? as u64, rows);
    for (expected, (key, row)) in old.iter()?.enumerate() {
        assert_eq!(key, expected as u64);
        assert_eq!(row.0, key * 3);
        assert_eq!(old.get(key)?.unwrap().0, row.0);
    }
    db.write(|tx| {
        assert!(tx.remove(0));
        tx.update(rows - 1, |_| Ok(Value(17)))?;
        tx.insert(u64::MAX, Value(42))
    })?;
    let current = db.snapshot()?;
    assert_eq!(current.sequence()?, 2);
    assert_eq!(current.len()? as u64, rows);
    assert!(current.get(0)?.is_none());
    assert_eq!(current.get(rows - 1)?.unwrap().0, 17);
    assert_eq!(current.get(u64::MAX)?.unwrap().0, 42);
    assert_eq!(old.get(0)?.unwrap().0, 0);
    assert_eq!(old.get(rows - 1)?.unwrap().0, (rows - 1) * 3);
    assert!(old.get(u64::MAX)?.is_none());
    println!(
        "conversion,mode=single,rows={rows},elapsed_ms={:.6},accounted_bytes={bytes},exact_state=verified",
        elapsed.as_secs_f64() * 1e3
    );
    Ok(())
}

fn catalog(rows: u64) -> Result<()> {
    let db = CatalogDatabase::<World>::in_memory()?;
    seed(&db, rows)?;
    let start = Instant::now();
    let db = db.into_snapshots(options(), footprint)?;
    let elapsed = start.elapsed();
    let bytes = db.retention()?.current_bytes;
    let old = db.snapshot()?;
    assert_eq!(old.sequence()?, rows.div_ceil(256));
    assert_eq!(old.scan::<Entities>()?.count() as u64, rows);
    assert_eq!(old.scan::<Items>()?.count() as u64, rows);
    assert_eq!(old.scan::<Saves>()?.count(), 0);
    for key in 0..rows {
        assert_eq!(*old.get::<Entities>(key)?.unwrap(), entity(key));
        assert_eq!(*old.get::<Items>(key)?.unwrap(), item(key));
        let found = old.lookup::<Items>(OWNER_INDEX, &key.to_be_bytes())?;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], (key, &item(key)));
    }
    for area in 0..rows.div_ceil(AREA_SIZE) {
        let found = old.lookup::<Entities>(AREA_INDEX, &area.to_be_bytes())?;
        assert_eq!(
            found.len() as u64,
            rows.min((area + 1) * AREA_SIZE) - area * AREA_SIZE
        );
        for (offset, (key, row)) in found.into_iter().enumerate() {
            assert_eq!(key, area * AREA_SIZE + offset as u64);
            assert_eq!(*row, entity(key));
        }
    }
    let request = Save {
        id: 0,
        rows,
        batch: rows.min(64),
    };
    assert!(db.write(|tx| save(tx, request))?);
    assert!(!db.write(|tx| save(tx, request))?);
    assert_eq!(*old.get::<Entities>(0)?.unwrap(), entity(0));
    assert_eq!(
        old.lookup::<Items>(OWNER_INDEX, &0u64.to_be_bytes())?[0].0,
        0
    );
    drop(old);
    let view = db.snapshot()?;
    assert!(
        view.lookup::<Items>(OWNER_INDEX, &0u64.to_be_bytes())?
            .is_empty()
    );
    assert_eq!(
        view.lookup::<Items>(OWNER_INDEX, &1u64.to_be_bytes())?
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    drop(view);
    verify(&db.into_database()?, rows, 1, request.batch)?;
    println!(
        "conversion,mode=catalog,entities={rows},items={rows},postings={},elapsed_ms={:.6},accounted_bytes={bytes},exact_rows_indexes_and_retry=verified",
        rows * 2,
        elapsed.as_secs_f64() * 1e3
    );
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let rows = match args.as_slice() {
        [] => 100_000,
        [n] => n
            .parse::<u64>()
            .map_err(|_| Error::InvalidOperation("invalid row count".into()))?,
        _ => {
            return Err(Error::InvalidOperation(
                "usage: snapshot_build [ROWS]".into(),
            ));
        }
    };
    if !(2..=1_000_000).contains(&rows) {
        return Err(Error::InvalidOperation("rows must be 2..=1000000".into()));
    }
    println!(
        "workload,resident_in_memory=true,seed_and_verification_outside_timing=true,conversion_includes_native_arc_mapping=true"
    );
    single(rows)?;
    catalog(rows)
}
