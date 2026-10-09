//! Functional resident-query smoke with per-query timings; not durable evidence.
#[path = "../examples/support/world.rs"]
mod world;
use skrin::catalog::CatalogDatabase;
use skrin::{Error, Result};
use std::time::{Duration, Instant};
use world::*;

fn expected(rows: u64, area: u64) -> Vec<(u64, Entity)> {
    (area * AREA_SIZE..rows.min((area + 2) * AREA_SIZE))
        .map(|key| (key, expected_entity(key, rows, 1, 64)))
        .filter(|(_, row)| row.x % 2 == 0)
        .take(16)
        .collect()
}
fn measure(
    mode: &str,
    rows: u64,
    mut query: impl FnMut(u64) -> Result<Vec<(u64, Entity)>>,
) -> Result<()> {
    let mut samples = Vec::new();
    for i in 0..2000 {
        let area = (i * 7919) % rows.div_ceil(AREA_SIZE);
        let start = Instant::now();
        let result = query(area)?;
        samples.push(start.elapsed());
        assert_eq!(result, expected(rows, area)); // Outside query timing.
    }
    samples.sort_unstable();
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    println!(
        "query,mode={mode},rows={rows},samples=2000,indexed_areas=2,predicate=even_x,limit=16,p50_us={:.3},p99_us={:.3},max_us={:.3},exact_rows=verified",
        us(samples[999]),
        us(samples[1979]),
        us(*samples.last().unwrap())
    );
    Ok(())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let rows = match args.as_slice() {
        [] => 10_000,
        [n] => n
            .parse::<u64>()
            .map_err(|_| Error::InvalidOperation("invalid row count".into()))?,
        _ => return Err(Error::InvalidOperation("usage: game_queries [ROWS]".into())),
    };
    if !(64..=1_000_000).contains(&rows) {
        return Err(Error::InvalidOperation("rows must be 64..=1000000".into()));
    }
    println!(
        "workload,resident_in_memory=true,durability=none,seed_and_verification_outside_query_timing=true"
    );
    let db = CatalogDatabase::<World>::in_memory()?;
    seed(&db, rows)?;
    assert!(db.write(|tx| save(
        tx,
        Save {
            id: 0,
            rows,
            batch: 64
        }
    ))?);
    verify(&db, rows, 1, 64)?;
    {
        let read = db.read()?;
        measure("native", rows, |area| {
            Ok(read
                .index_scan::<Entities>(
                    AREA_INDEX,
                    area.to_be_bytes().to_vec()..=(area + 1).to_be_bytes().to_vec(),
                )?
                .filter(|(_, row)| row.x % 2 == 0)
                .take(16)
                .map(|(key, row)| (key, *row))
                .collect())
        })?;
    }
    let options = skrin::versioned::SnapshotOptions {
        max_pinned_bytes: 1024 * 1024 * 1024,
        ..snapshot_options()
    };
    let db = db.into_snapshots(options, footprint)?;
    let frame = db.snapshot()?;
    measure("snapshot", rows, |area| {
        Ok(frame
            .index_scan::<Entities>(
                AREA_INDEX,
                area.to_be_bytes().to_vec()..=(area + 1).to_be_bytes().to_vec(),
            )?
            .filter(|(_, row)| row.x % 2 == 0)
            .take(16)
            .map(|(key, row)| (key, *row))
            .collect())
    })?;
    assert_eq!(frame.sequence()?, rows.div_ceil(256) + 1);
    Ok(())
}
