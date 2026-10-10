//! Production in-memory saves: measure the read-index optimization's write and
//! retained-memory cost separately. No SQLite or durable-write speed claim.
#[path = "../examples/support/world.rs"]
mod world;
use skrin::catalog::CatalogDatabase;
use skrin::{Error, Result};
use std::time::{Duration, Instant};
use world::*;

fn stats(mode: &str, rows: u64, batch: u64, times: &mut [Duration]) {
    times.sort_unstable();
    let us = |duration: Duration| duration.as_secs_f64() * 1e6;
    println!(
        "index_update,mode={mode},rows={rows},batch={batch},samples={},p50_us={:.3},p99_us={:.3},max_us={:.3},full_rows_indexes_and_retry=verified,durability=none",
        times.len(),
        us(times[(times.len() - 1) / 2]),
        us(times[(times.len() * 99).div_ceil(100) - 1]),
        us(*times.last().unwrap())
    );
}
fn run(rows: u64, samples: u64, batch: u64, snapshot: bool) -> Result<()> {
    let db = CatalogDatabase::<World>::in_memory()?;
    seed(&db, rows)?;
    let mut times = Vec::with_capacity(samples as usize);
    if snapshot {
        let start = Instant::now();
        let db = db.into_snapshots(snapshot_options(), footprint)?;
        let conversion = start.elapsed();
        let before = db.retention()?.current_bytes;
        let old = db.snapshot()?;
        for id in 0..samples {
            let request = std::hint::black_box(Save { id, rows, batch });
            let start = Instant::now();
            assert!(db.write(|tx| save(tx, request))?);
            times.push(start.elapsed());
        }
        for id in 0..rows {
            assert_eq!(*old.get::<Entities>(id)?.unwrap(), entity(id));
            assert_eq!(*old.get::<Items>(id)?.unwrap(), item(id));
        }
        assert!(!db.write(|tx| save(
            tx,
            Save {
                id: samples - 1,
                rows,
                batch
            }
        ))?);
        let current = db.snapshot()?;
        assert_eq!(current.sequence()?, rows.div_ceil(256) + samples);
        let mut entity_count = 0;
        for (id, row) in current.query(Area, ..)? {
            assert_eq!(*row, expected_entity(id, rows, samples, batch));
            entity_count += 1;
        }
        let mut item_count = 0;
        for (id, row) in current.query(Owner, ..)? {
            assert_eq!(*row, expected_item(id, rows, samples));
            item_count += 1;
        }
        assert_eq!(entity_count, rows);
        assert_eq!(item_count, rows);
        drop(current);
        let after = db.retention()?.current_bytes;
        assert_eq!(db.retention()?.pinned_bytes, before);
        drop(old);
        verify(&db.into_database()?, rows, samples, batch)?;
        println!(
            "index_roots,rows={rows},batch={batch},conversion_ms={:.3},initial_accounted_bytes={before},after_accounted_bytes={after},old_full_frame=verified",
            conversion.as_secs_f64() * 1e3
        );
        stats("snapshot", rows, batch, &mut times);
    } else {
        for id in 0..samples {
            let request = std::hint::black_box(Save { id, rows, batch });
            let start = Instant::now();
            assert!(db.write(|tx| save(tx, request))?);
            times.push(start.elapsed());
        }
        assert!(!db.write(|tx| save(
            tx,
            Save {
                id: samples - 1,
                rows,
                batch
            }
        ))?);
        verify(&db, rows, samples, batch)?;
        stats("native", rows, batch, &mut times);
    }
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let (rows, samples) = match args.as_slice() {
        [] => (Some(10000), Some(128)),
        [rows, samples] => (rows.parse().ok(), samples.parse().ok()),
        _ => (None, None),
    };
    let (rows, samples) = match (rows, samples) {
        (Some(rows), Some(samples))
            if (64..=1000000).contains(&rows) && (16..=2000).contains(&samples) =>
        {
            (rows, samples)
        }
        _ => {
            return Err(Error::InvalidOperation(
                "usage: index_updates [ROWS=64..1000000 SAMPLES=16..2000]".into(),
            ));
        }
    };
    for batch in [1, 16, 64] {
        run(rows, samples, batch, false)?;
        run(rows, samples, batch, true)?;
    }
    Ok(())
}
