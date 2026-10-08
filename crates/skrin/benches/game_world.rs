//! Typed game workload. Run each mode in a separate process; never compare a
//! volatile or tmpfs run to physical-device durable timings.
#[path = "support/game_sqlite.rs"]
mod sqlite;
#[path = "../examples/support/world.rs"]
mod world;
use skrin::catalog::{CatalogDatabase, CatalogRead};
use skrin::group_commit::GroupCommitOptions;
use skrin::versioned::{CatalogSnapshot, GroupSnapshotCatalog, SnapshotCatalog};
use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Barrier,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use world::*;

type BenchResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
fn invalid(message: &str) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::other(message).into()
}

enum Store {
    Native(Arc<CatalogDatabase<World>>),
    Snapshot(SnapshotCatalog<World>),
    Group(GroupSnapshotCatalog<World>),
    Sqlite(rusqlite::Connection, bool, bool),
}
enum View<'a> {
    Native(CatalogRead<'a, World>),
    Snapshot(CatalogSnapshot<World>),
    Sqlite(SqliteRead<'a>),
}
enum SqliteRead<'a> {
    Statement(&'a rusqlite::Connection),
    Frame(rusqlite::Transaction<'a>),
}
impl std::ops::Deref for SqliteRead<'_> {
    type Target = rusqlite::Connection;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Statement(c) => c,
            Self::Frame(tx) => tx,
        }
    }
}
impl Store {
    fn open(mode: &str, path: &Path) -> BenchResult<Self> {
        if mode.starts_with("sqlite") {
            return Ok(Self::Sqlite(
                sqlite::open(path)?,
                mode != "sqlite",
                mode == "sqlite_batch",
            ));
        }
        let db = CatalogDatabase::<World>::open_dir(path)?;
        if mode == "snapshot" {
            Ok(Self::Snapshot(
                db.into_snapshots(snapshot_options(), footprint)?,
            ))
        } else if mode == "group_snapshot" {
            Ok(Self::Group(
                db.into_snapshots(snapshot_options(), footprint)?
                    .into_group_commit(GroupCommitOptions {
                        queue_capacity: 64,
                        max_transactions: 8,
                        max_delay: Duration::from_millis(1),
                    })?,
            ))
        } else {
            Ok(Self::Native(Arc::new(db)))
        }
    }
    fn create(mode: &str, path: &Path, rows: u64) -> BenchResult<Self> {
        if mode.starts_with("sqlite") {
            return Ok(Self::Sqlite(
                sqlite::create(path, rows)?,
                mode != "sqlite",
                mode == "sqlite_batch",
            ));
        }
        let db = CatalogDatabase::<World>::create_dir(path)?;
        seed(&db, rows)?;
        if mode == "snapshot" {
            Ok(Self::Snapshot(
                db.into_snapshots(snapshot_options(), footprint)?,
            ))
        } else if mode == "group_snapshot" {
            Ok(Self::Group(
                db.into_snapshots(snapshot_options(), footprint)?
                    .into_group_commit(GroupCommitOptions {
                        queue_capacity: 64,
                        max_transactions: 8,
                        max_delay: Duration::from_millis(1),
                    })?,
            ))
        } else {
            Ok(Self::Native(Arc::new(db)))
        }
    }
    fn reader(&self, path: &Path) -> BenchResult<Self> {
        match self {
            Self::Native(db) => Ok(Self::Native(db.clone())),
            Self::Snapshot(db) => Ok(Self::Snapshot(db.clone())),
            Self::Group(db) => Ok(Self::Group(db.clone())),
            Self::Sqlite(_, bulk, batch) => Ok(Self::Sqlite(sqlite::open(path)?, *bulk, *batch)),
        }
    }
    fn view(&self) -> BenchResult<View<'_>> {
        match self {
            Self::Native(db) => Ok(View::Native(db.read()?)),
            Self::Snapshot(db) => Ok(View::Snapshot(db.snapshot()?)),
            Self::Group(db) => Ok(View::Snapshot(db.snapshot()?)),
            Self::Sqlite(db, _, _) => Ok(View::Sqlite(SqliteRead::Frame(sqlite::read(db)?))),
        }
    }
    fn query_view(&self) -> BenchResult<View<'_>> {
        match self {
            // Each isolated SELECT already has a coherent implicit transaction.
            // Avoid adding unnecessary BEGIN/ROLLBACK costs to the comparator.
            Self::Sqlite(db, _, _) => Ok(View::Sqlite(SqliteRead::Statement(db))),
            _ => self.view(),
        }
    }
    fn save(&mut self, r: Save) -> BenchResult<bool> {
        Ok(match self {
            Self::Native(db) => db.write(|tx| save(tx, r))?,
            Self::Snapshot(db) => db.write(|tx| save(tx, r))?,
            Self::Group(db) => db.submit(move |tx| save(tx, r))?.wait()?.value,
            Self::Sqlite(db, bulk, _) => sqlite::save(db, r, *bulk)?,
        })
    }
    fn save_window(
        &mut self,
        requests: &[Save],
    ) -> BenchResult<(Vec<Duration>, BTreeMap<u64, usize>)> {
        let start = Instant::now();
        let mut samples = Vec::with_capacity(requests.len());
        let mut groups = BTreeMap::new();
        match self {
            Self::Group(db) => {
                let mut pending = Vec::with_capacity(requests.len());
                for &r in requests {
                    pending.push(db.submit(move |tx| save(tx, r))?);
                }
                for p in pending {
                    let receipt = p.wait()?;
                    assert!(receipt.value);
                    groups.insert(receipt.synchronized_sequence, receipt.transactions_in_group);
                    samples.push(start.elapsed());
                }
            }
            Self::Sqlite(db, _, true) => {
                sqlite::save_batch(db, requests)?;
                samples.resize(requests.len(), start.elapsed());
                groups.insert(requests[0].id, requests.len());
            }
            _ => {
                for &r in requests {
                    assert!(self.save(r)?);
                    groups.insert(r.id, 1);
                    samples.push(start.elapsed());
                }
            }
        }
        Ok((samples, groups))
    }
    fn checkpoint(&self) -> BenchResult<()> {
        match self {
            Self::Native(db) => {
                db.checkpoint()?;
            }
            Self::Snapshot(db) => {
                db.checkpoint()?;
            }
            Self::Group(db) => {
                db.checkpoint()?;
            }
            Self::Sqlite(db, _, _) => sqlite::checkpoint(db)?,
        }
        Ok(())
    }
    fn reclaim(&self) -> BenchResult<()> {
        match self {
            Self::Native(db) => {
                db.reclaim()?;
            }
            Self::Snapshot(db) => {
                db.reclaim()?;
            }
            Self::Group(db) => {
                db.reclaim()?;
            }
            Self::Sqlite(_, _, _) => {} // checkpoint(TRUNCATE) already reclaims WAL.
        }
        Ok(())
    }
}
impl View<'_> {
    fn entity(&self, key: u64) -> BenchResult<Option<Entity>> {
        Ok(match self {
            Self::Native(r) => r.get::<Entities>(key)?.copied(),
            Self::Snapshot(r) => r.get::<Entities>(key)?.copied(),
            Self::Sqlite(r) => sqlite::get_entity(r, key)?,
        })
    }
    fn item(&self, key: u64) -> BenchResult<Option<Item>> {
        Ok(match self {
            Self::Native(r) => r.get::<Items>(key)?.copied(),
            Self::Snapshot(r) => r.get::<Items>(key)?.copied(),
            Self::Sqlite(r) => sqlite::get_item(r, key)?,
        })
    }
    fn saved(&self, key: u64) -> BenchResult<Option<Saved>> {
        Ok(match self {
            Self::Native(r) => r.get::<Saves>(key)?.copied(),
            Self::Snapshot(r) => r.get::<Saves>(key)?.copied(),
            Self::Sqlite(r) => sqlite::get_saved(r, key)?,
        })
    }
    fn saves(&self, rows: u64) -> BenchResult<u64> {
        Ok(match self {
            Self::Native(r) => r.sequence() - rows.div_ceil(256),
            Self::Snapshot(r) => r.sequence()? - rows.div_ceil(256),
            Self::Sqlite(r) => sqlite::saved_count(r)?,
        })
    }
    fn counts(&self) -> BenchResult<(u64, u64, u64)> {
        Ok(match self {
            Self::Native(r) => (
                r.scan::<Entities>()?.count() as u64,
                r.scan::<Items>()?.count() as u64,
                r.scan::<Saves>()?.count() as u64,
            ),
            Self::Snapshot(r) => (
                r.scan::<Entities>()?.count() as u64,
                r.scan::<Items>()?.count() as u64,
                r.scan::<Saves>()?.count() as u64,
            ),
            Self::Sqlite(r) => sqlite::counts(r)?,
        })
    }
    fn area(&self, key: u64) -> BenchResult<Vec<(u64, Entity)>> {
        Ok(match self {
            Self::Native(r) => r
                .lookup::<Entities>(AREA_INDEX, &key.to_be_bytes())?
                .into_iter()
                .map(|(k, r)| (k, *r))
                .collect(),
            Self::Snapshot(r) => r
                .lookup::<Entities>(AREA_INDEX, &key.to_be_bytes())?
                .into_iter()
                .map(|(k, r)| (k, *r))
                .collect(),
            Self::Sqlite(r) => sqlite::area(r, key)?,
        })
    }
    fn inventory(&self, key: u64) -> BenchResult<Vec<(u64, Item)>> {
        Ok(match self {
            Self::Native(r) => r
                .lookup::<Items>(OWNER_INDEX, &key.to_be_bytes())?
                .into_iter()
                .map(|(k, r)| (k, *r))
                .collect(),
            Self::Snapshot(r) => r
                .lookup::<Items>(OWNER_INDEX, &key.to_be_bytes())?
                .into_iter()
                .map(|(k, r)| (k, *r))
                .collect(),
            Self::Sqlite(r) => sqlite::inventory(r, key)?,
        })
    }
}
fn verify_store(db: &Store, rows: u64, saves: u64, batch: u64) -> BenchResult<()> {
    let r = db.view()?;
    assert_eq!(r.counts()?, (rows, rows, saves));
    assert_eq!(r.saves(rows)?, saves);
    for key in 0..rows {
        assert_eq!(
            r.entity(key)?.unwrap(),
            expected_entity(key, rows, saves, batch)
        );
        assert_eq!(r.item(key)?.unwrap(), expected_item(key, rows, saves));
    }
    for id in 0..saves {
        assert_eq!(r.saved(id)?.unwrap(), Save { id, rows, batch }.saved());
    }
    for area in 0..rows.div_ceil(AREA_SIZE) {
        let found = r.area(area)?;
        let expected: Vec<_> = (area * AREA_SIZE..rows.min((area + 1) * AREA_SIZE))
            .map(|k| (k, expected_entity(k, rows, saves, batch)))
            .collect();
        assert_eq!(found, expected);
    }
    let mut count = 0;
    for owner in 0..rows {
        for (key, item) in r.inventory(owner)? {
            assert_eq!(item, expected_item(key, rows, saves));
            assert_eq!(item.owner, owner);
            count += 1;
        }
    }
    assert_eq!(count, rows);
    // Exercise the example's native verification too, outside timing.
    if let Store::Native(db) = db {
        verify(db, rows, saves, batch)?;
    }
    Ok(())
}
fn report(name: &str, samples: &[Duration]) {
    if samples.is_empty() {
        println!("{name},samples=0");
        return;
    }
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let percentile = |p: usize| ordered[(ordered.len() * p).div_ceil(100) - 1].as_secs_f64() * 1e6;
    println!(
        "{name},samples={},p50_us={:.3},p95_us={:.3},p99_us={:.3},max_us={:.3}",
        samples.len(),
        percentile(50),
        percentile(95),
        percentile(99),
        ordered.last().unwrap().as_secs_f64() * 1e6
    );
}
fn read_phase(db: &Store, rows: u64) -> BenchResult<()> {
    // Warm actual queries and cached statements before sampling both engines.
    for n in 0..128 {
        let r = db.query_view()?;
        black_box(r.entity(n % rows)?);
        black_box(r.area(n % rows.div_ceil(AREA_SIZE))?);
        black_box(r.inventory(n % rows)?);
    }
    for name in ["point", "area", "inventory"] {
        let mut samples = Vec::with_capacity(4000);
        let phase = Instant::now();
        for n in 0..4000u64 {
            let start = Instant::now();
            {
                let r = db.query_view()?;
                match name {
                    "point" => {
                        black_box(r.entity((n * 7919) % rows)?.unwrap());
                    }
                    "area" => {
                        black_box(r.area(n % rows.div_ceil(AREA_SIZE))?);
                    }
                    _ => {
                        black_box(r.inventory((n * 7919) % rows)?);
                    }
                }
            } // Include acquiring and dropping one coherent view per request.
            samples.push(start.elapsed());
        }
        report(name, &samples);
        println!(
            "{name}_rate,operations_per_s={:.3}",
            samples.len() as f64 / phase.elapsed().as_secs_f64()
        );
    }
    Ok(())
}
fn frame(db: &Store, rows: u64, batch: u64, index: u64) -> BenchResult<u64> {
    let r = db.view()?;
    let saves = r.saves(rows)?;
    for offset in 0..64 {
        let key = (index * 64 + offset) % rows;
        assert_eq!(
            black_box(r.entity(key)?.unwrap()),
            expected_entity(key, rows, saves, batch)
        );
    }
    let area = index % rows.div_ceil(AREA_SIZE);
    let found = r.area(area)?;
    assert_eq!(
        found.len() as u64,
        rows.min((area + 1) * AREA_SIZE) - area * AREA_SIZE
    );
    for (key, e) in found {
        assert_eq!(black_box(e), expected_entity(key, rows, saves, batch));
    }
    for (key, i) in r.inventory(index % rows)? {
        assert_eq!(black_box(i), expected_item(key, rows, saves));
        assert_eq!(i.owner, index % rows);
    }
    Ok(saves)
}
fn concurrent(
    mut writer: Store,
    reader: &Store,
    rows: u64,
    saves: u64,
    batch: u64,
    window: u64,
) -> BenchResult<Store> {
    let ready = Arc::new(Barrier::new(2));
    let done = Arc::new(AtomicBool::new(false));
    let barrier = ready.clone();
    let finished = done.clone();
    let handle = std::thread::spawn(move || -> BenchResult<(Store, Vec<Duration>, Duration)> {
        struct Completion(Arc<AtomicBool>);
        impl Drop for Completion {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let _completion = Completion(finished);
        barrier.wait();
        let phase = Instant::now();
        let mut samples = Vec::new();
        let mut histogram = BTreeMap::new();
        for first in (0..saves).step_by(window as usize) {
            let requests: Vec<_> = (first..saves.min(first + window))
                .map(|id| Save { id, rows, batch })
                .collect();
            let (times, groups) = writer.save_window(&requests)?;
            samples.extend(times);
            for size in groups.into_values() {
                *histogram.entry(size).or_insert(0usize) += 1;
            }
        }
        let elapsed = phase.elapsed();
        for (frames, count) in histogram {
            println!("sync_groups,requests={frames},groups={count}");
        }
        Ok((writer, samples, elapsed))
    });
    ready.wait();
    let period = Duration::from_nanos(16_666_667);
    let mut deadline = Instant::now();
    let mut samples = Vec::new();
    let mut lateness = Vec::new();
    let mut previous = 0;
    let mut overlapping = 0;
    let result = (|| -> BenchResult<()> {
        while !done.load(Ordering::Acquire) || samples.is_empty() {
            let active_at_start = !done.load(Ordering::Acquire);
            let start = Instant::now();
            lateness.push(start.saturating_duration_since(deadline));
            let observed = frame(reader, rows, batch, samples.len() as u64)?;
            assert!(observed >= previous && observed <= saves);
            previous = observed;
            let elapsed = start.elapsed();
            overlapping += usize::from(active_at_start && !done.load(Ordering::Acquire));
            samples.push(elapsed);
            deadline += period;
            // Missed periods are skipped, not executed as a burst of catch-up
            // frames. Read duration and scheduler lateness are separate metrics.
            while deadline < Instant::now() {
                deadline += period;
            }
            if !done.load(Ordering::Acquire) {
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            }
        }
        Ok(())
    })();
    let (mut writer, writes, elapsed) = handle
        .join()
        .map_err(|_| invalid("save worker panicked"))??;
    result?;
    report("durable_save", &writes);
    println!(
        "durable_save_rate,saves_per_s={:.3},entity_updates_per_s={:.3}",
        saves as f64 / elapsed.as_secs_f64(),
        (saves * batch) as f64 / elapsed.as_secs_f64()
    );
    report("frame_work_60hz", &samples);
    report("frame_start_lateness", &lateness);
    println!(
        "frame_overlap,completed_while_writer_active={overlapping},over_budget={},last_observed_save={previous}",
        samples.iter().filter(|d| **d > period).count()
    );
    // An already acknowledged request and a mismatched retry must not change
    // rows, indexes or save count. These are not timed commits.
    let request = Save {
        id: saves - 1,
        rows,
        batch,
    };
    assert!(!writer.save(request)?);
    assert!(
        writer
            .save(Save {
                batch: if batch == 1 { 2 } else { 1 },
                ..request
            })
            .is_err()
    );
    Ok(writer)
}
fn memory(phase: &str) -> BenchResult<()> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status")?;
        for key in ["VmRSS:", "VmHWM:"] {
            let line = status
                .lines()
                .find(|l| l.starts_with(key))
                .ok_or_else(|| invalid("missing RSS counter"))?;
            println!(
                "memory,phase={phase},{}",
                line.split_whitespace().collect::<Vec<_>>().join(" ")
            );
        }
    }
    #[cfg(not(target_os = "linux"))]
    println!("memory,phase={phase},unavailable=not_linux");
    Ok(())
}
fn disk(path: &Path, phase: &str) -> BenchResult<()> {
    fn size(path: &Path) -> std::io::Result<u64> {
        let m = path.symlink_metadata()?;
        if m.is_file() {
            return Ok(m.len());
        }
        let mut total = 0;
        if m.is_dir() {
            for entry in std::fs::read_dir(path)? {
                total += size(&entry?.path())?;
            }
        }
        Ok(total)
    }
    println!(
        "disk,phase={phase},logical_regular_file_bytes={}",
        size(path)?
    );
    Ok(())
}
fn reopen(mode: &str, path: &Path, rows: u64, saves: u64, batch: u64) -> BenchResult<()> {
    let start = Instant::now();
    let db = if mode.starts_with("sqlite") {
        Store::Sqlite(
            sqlite::open(path)?,
            mode != "sqlite",
            mode == "sqlite_batch",
        )
    } else {
        Store::Native(Arc::new(CatalogDatabase::<World>::open_dir(path)?))
    };
    println!(
        "fresh_process_open,elapsed_ms={:.3},cache_policy=no_advice",
        start.elapsed().as_secs_f64() * 1e3
    );
    memory("reopen_before_verify")?;
    verify_store(&db, rows, saves, batch)?;
    println!("fresh_process_exact_rows_indexes_and_operations=verified");
    Ok(())
}
fn main() -> BenchResult<()> {
    let args: Vec<_> = std::env::args_os()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let verify_only = args.first().is_some_and(|a| a == "--verify");
    let offset = usize::from(verify_only);
    if !(5 + offset..=6 + offset).contains(&args.len()) {
        return Err(invalid(
            "usage: game_world [--verify] PARENT_OR_DB native|snapshot|group_snapshot|sqlite|sqlite_bulk|sqlite_batch ROWS SAVES BATCH [WINDOW]",
        ));
    }
    let path = PathBuf::from(&args[offset]);
    let mode = args[offset + 1]
        .to_str()
        .ok_or_else(|| invalid("invalid mode"))?;
    if ![
        "native",
        "snapshot",
        "group_snapshot",
        "sqlite",
        "sqlite_bulk",
        "sqlite_batch",
    ]
    .contains(&mode)
    {
        return Err(invalid("unknown mode"));
    }
    let number = |n: usize| -> BenchResult<u64> {
        Ok(args[offset + n]
            .to_str()
            .ok_or_else(|| invalid("invalid number"))?
            .parse()?)
    };
    let rows = number(2)?;
    let saves = number(3)?;
    let batch = number(4)?;
    let window = if args.len() == 6 + offset {
        number(5)?
    } else {
        1
    };
    if !(1..=8).contains(&window) {
        return Err(invalid("window must be 1..=8"));
    }
    if !(2..=1_000_000).contains(&rows)
        || !(1..=1_000_000).contains(&saves)
        || batch == 0
        || batch > rows.min(1024)
    {
        return Err(invalid(
            "require 2..1M rows, 1..1M saves, 1..min(rows,1024) batch",
        ));
    }
    if verify_only {
        return reopen(mode, &path, rows, saves, batch);
    }
    if !path.is_dir() {
        return Err(invalid("scratch parent must already exist"));
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let scratch = path.join(format!("skrin-world-{mode}-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&scratch)?;
    #[cfg(unix)]
    {
        // Both engines receive a durably named scratch parent before setup.
        std::fs::File::open(&scratch)?.sync_all()?;
        std::fs::File::open(&path)?.sync_all()?;
    }
    // Kept even on success so raw output identifies reviewable/reopenable data.
    let database = scratch.join(if mode.starts_with("sqlite") {
        "world.sqlite"
    } else {
        "world"
    });
    println!(
        "workload,mode={mode},rows={rows},items={rows},saves={saves},batch={batch},window={window},durability=sync_before_ack,maintenance=explicit,scratch={}",
        scratch.display()
    );
    println!("sqlite_comparator_version={}", rusqlite::version());
    let db = Store::create(mode, &database, rows)?;
    verify_store(&db, rows, 0, batch)?;
    memory("seeded")?;
    read_phase(&db, rows)?;
    let reader = db.reader(&database)?;
    frame(&reader, rows, batch, 0)?; // Prepare reader queries before the barrier.
    let db = concurrent(db, &reader, rows, saves, batch, window)?;
    drop(reader);
    verify_store(&db, rows, saves, batch)?;
    memory("saved")?;
    disk(&scratch, "before_checkpoint")?;
    drop(db);
    println!("recovery,state=before_checkpoint_wal");
    verify_child(mode, &database, rows, saves, batch)?;
    let db = Store::open(mode, &database)?;
    let start = Instant::now();
    db.checkpoint()?;
    println!(
        "checkpoint,elapsed_ms={:.3}",
        start.elapsed().as_secs_f64() * 1e3
    );
    disk(&scratch, "published_overlap")?;
    let start = Instant::now();
    db.reclaim()?;
    println!(
        "reclaim,elapsed_ms={:.3}",
        start.elapsed().as_secs_f64() * 1e3
    );
    disk(&scratch, "after_reclaim")?;
    memory("after_maintenance")?;
    drop(db);
    println!("recovery,state=after_checkpoint");
    verify_child(mode, &database, rows, saves, batch)?;
    println!("completed,scratch_retained={}", scratch.display());
    Ok(())
}
fn verify_child(mode: &str, database: &Path, rows: u64, saves: u64, batch: u64) -> BenchResult<()> {
    let status = std::process::Command::new(std::env::current_exe()?)
        .arg("--verify")
        .arg(database)
        .arg(mode)
        .arg(rows.to_string())
        .arg(saves.to_string())
        .arg(batch.to_string())
        .status()?;
    if !status.success() {
        return Err(invalid(
            "fresh-process verification failed; scratch retained",
        ));
    }
    Ok(())
}
