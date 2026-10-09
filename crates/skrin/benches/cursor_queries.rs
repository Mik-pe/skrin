//! Resident keyset pages over a large equal-key posting. SQL stays in this
//! benchmark-only control. No persistence or whole-application speed claim.
use skrin::catalog::CatalogDatabase;
use skrin::versioned::SnapshotOptions;
use skrin::{Error, Result};
use std::time::{Duration, Instant};

#[derive(skrin::Record)]
#[skrin(table_id = 1, version = 1)]
struct Entry {
    group: u64,
    value: u64,
}
skrin::catalog! {
    Pages, Row {
        schema: (12000, 1), tables: { Entries: Entry },
        indexes: { Group: Entries {
            id: 1, version: 1, unique: false, key: u64 => |row| row.group
        } }
    }
}
type Page = Vec<(u64, u64, u64)>;
fn expected(rows: u64, cursor: u64) -> Page {
    (cursor + 1..rows)
        .filter(|id| (id * 3) % 2 == 0)
        .take(16)
        .map(|id| (id, 0, id * 3))
        .collect()
}
fn measure(
    mode: &str,
    rows: u64,
    samples: usize,
    mut query: impl FnMut(u64) -> Result<Page>,
) -> Result<()> {
    for (depth, cursor) in [("front", 0), ("middle", rows / 2), ("late", rows - 64)] {
        let expected = expected(rows, cursor);
        for _ in 0..8 {
            assert_eq!(query(cursor)?, expected);
        }
        let mut timings = Vec::with_capacity(samples);
        for _ in 0..samples {
            let start = Instant::now();
            let page = query(std::hint::black_box(cursor))?;
            timings.push(start.elapsed());
            assert_eq!(page, expected); // Outside query timing.
        }
        timings.sort_unstable();
        let us = |d: Duration| d.as_secs_f64() * 1e6;
        println!(
            "cursor,mode={mode},rows={rows},depth={depth},after_id={cursor},samples={samples},predicate=even_value,limit=16,p50_us={:.3},p99_us={:.3},max_us={:.3},exact_rows=verified",
            us(timings[(samples - 1) / 2]),
            us(timings[(samples * 99).div_ceil(100) - 1]),
            us(*timings.last().unwrap())
        );
    }
    Ok(())
}
fn sql<T>(result: rusqlite::Result<T>) -> Result<T> {
    result.map_err(|error| Error::InvalidOperation(format!("SQLite benchmark: {error}")))
}
fn main() -> Result<()> {
    let raw: Vec<_> = std::env::args().skip(1).collect();
    let sqlite_first = raw.iter().any(|a| a == "--sqlite-first");
    let args: Vec<_> = raw
        .into_iter()
        .filter(|a| a != "--bench" && a != "--sqlite-first")
        .collect();
    let (rows, samples) = match args.as_slice() {
        [] => (Some(100_000), Some(128)),
        [rows] => (rows.parse().ok(), Some(128)),
        [rows, samples] => (rows.parse().ok(), samples.parse().ok()),
        _ => (None, None),
    };
    let (rows, samples) = match (rows, samples) {
        (Some(rows), Some(samples))
            if (64..=1_000_000).contains(&rows) && (16..=2000).contains(&samples) =>
        {
            (rows, samples)
        }
        _ => {
            return Err(Error::InvalidOperation(
                "usage: cursor_queries [ROWS=64..1000000 [SAMPLES=16..2000]]".into(),
            ));
        }
    };
    println!(
        "workload,resident_in_memory=true,durability=none,one_equal_key_group=true,full_rows_materialized=true,setup_warmup_and_verification_outside_timing=true"
    );
    if sqlite_first {
        sqlite(rows, samples)?;
    }
    let db = CatalogDatabase::<Pages>::in_memory()?;
    db.write(|tx| {
        for id in (0..rows).rev() {
            tx.insert::<Entries>(
                id,
                Entry {
                    group: 0,
                    value: id * 3,
                },
            )?;
        }
        Ok(())
    })?;
    {
        let read = db.read()?;
        measure("native_prefix", rows, samples, |cursor| {
            Ok(read
                .query(Group, 0..=0)?
                .filter(|(id, row)| *id > cursor && row.value % 2 == 0)
                .take(16)
                .map(|(id, row)| (id, row.group, row.value))
                .collect())
        })?;
        measure("native_seek", rows, samples, |cursor| {
            Ok(read
                .query_after(Group, 0..=0, (&0, cursor))?
                .filter(|(_, row)| row.value % 2 == 0)
                .take(16)
                .map(|(id, row)| (id, row.group, row.value))
                .collect())
        })?;
    }
    let db = db.into_snapshots(
        SnapshotOptions {
            max_snapshots: 1,
            max_pinned_bytes: 1024 * 1024 * 1024,
        },
        |_| Ok(std::mem::size_of::<Row>() as u64),
    )?;
    let frame = db.snapshot()?;
    measure("snapshot_prefix", rows, samples, |cursor| {
        Ok(frame
            .query(Group, 0..=0)?
            .filter(|(id, row)| *id > cursor && row.value % 2 == 0)
            .take(16)
            .map(|(id, row)| (id, row.group, row.value))
            .collect())
    })?;
    measure("snapshot_seek", rows, samples, |cursor| {
        Ok(frame
            .query_after(Group, 0..=0, (&0, cursor))?
            .filter(|(_, row)| row.value % 2 == 0)
            .take(16)
            .map(|(id, row)| (id, row.group, row.value))
            .collect())
    })?;
    drop(frame);
    drop(db);
    if !sqlite_first {
        sqlite(rows, samples)?;
    }
    Ok(())
}
fn sqlite(rows: u64, samples: usize) -> Result<()> {
    let mut connection = sql(rusqlite::Connection::open_in_memory())?;
    sql(connection.execute_batch("CREATE TABLE entries(id INTEGER PRIMARY KEY, group_id INTEGER NOT NULL, value INTEGER NOT NULL); CREATE INDEX entries_group ON entries(group_id, id, value);"))?;
    {
        let tx = sql(connection.transaction())?;
        {
            let mut insert = sql(tx.prepare("INSERT INTO entries VALUES (?1, 0, ?2)"))?;
            for id in (0..rows).rev() {
                sql(insert.execute(rusqlite::params![id, id * 3]))?;
            }
        }
        sql(tx.commit())?;
    }
    const QUERY: &str = "SELECT id, group_id, value FROM entries WHERE group_id=0 AND id>?1 AND value%2=0 ORDER BY id LIMIT 16";
    println!(
        "sqlite_control,version={},prepared_statement_reused=true,covering_index=true,transaction_seed_outside_timing=true",
        rusqlite::version()
    );
    {
        let mut plan = sql(connection.prepare(&format!("EXPLAIN QUERY PLAN {QUERY}")))?;
        let steps = sql(
            sql(plan.query_map([rows / 2], |row| row.get::<_, String>(3)))?
                .collect::<rusqlite::Result<Vec<_>>>(),
        )?;
        assert!(
            steps
                .iter()
                .any(|step| step.contains("USING COVERING INDEX entries_group")
                    && step.contains("id>?")),
            "expected covering seek: {steps:?}"
        );
        println!("sqlite_plan={steps:?}");
    }
    let mut query = sql(connection.prepare(QUERY))?;
    measure("sqlite_covering", rows, samples, |cursor| {
        sql(
            sql(query.query_map([cursor], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))))?
                .collect::<rusqlite::Result<Page>>(),
        )
    })
}
