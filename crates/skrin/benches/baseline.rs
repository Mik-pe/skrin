use skrin::{Database, Decoder, Encoder, Record, Result, Schema};
use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Row(u64);

impl Record for Row {
    const SCHEMA: Schema = Schema {
        table_id: 0x0042_454e_4348,
        version: 1,
    };

    fn encode(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.u64(self.0)
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(decoder.u64()?))
    }
}

fn report(label: &str, operations: u64, elapsed: Duration) {
    println!(
        "{label}: {:.0} ops/s, {:.1} ns/op ({operations} operations)",
        operations as f64 / elapsed.as_secs_f64(),
        elapsed.as_nanos() as f64 / operations as f64
    );
}

fn memory() -> Result<()> {
    const ROWS: u64 = 10_000;
    const READS: u64 = 1_000_000;
    let mut map = BTreeMap::new();
    let db = Database::<Row>::in_memory();
    db.write(|tx| {
        for key in 0..ROWS {
            map.insert(key, Row(key));
            tx.insert(key, Row(key))?;
        }
        Ok(())
    })?;
    let key = |index: u64| black_box((index * 7919) % ROWS);

    let start = Instant::now();
    for index in 0..READS {
        black_box(map.get(&key(index)).unwrap().0);
    }
    report("BTreeMap lookup (no transactions)", READS, start.elapsed());

    {
        let read = db.read()?;
        let start = Instant::now();
        for index in 0..READS {
            black_box(read.get(key(index)).unwrap().0);
        }
        report("Skrin lookup (one reused guard)", READS, start.elapsed());
    }

    let start = Instant::now();
    for index in 0..READS {
        black_box(db.read()?.get(key(index)).unwrap().0);
    }
    report("Skrin lookup (guard per lookup)", READS, start.elapsed());

    let start = Instant::now();
    for index in 0..ROWS {
        db.write(|tx| {
            tx.put(key(index), Row(index));
            Ok(())
        })?;
    }
    report(
        "Skrin volatile single-row transactions",
        ROWS,
        start.elapsed(),
    );

    let start = Instant::now();
    for batch in 0..1000 {
        db.write(|tx| {
            for offset in 0..100 {
                tx.put(key(batch * 100 + offset), Row(offset));
            }
            Ok(())
        })?;
    }
    report(
        "Skrin volatile batches (100 rows/tx)",
        100_000,
        start.elapsed(),
    );
    Ok(())
}

// Declared before the database so the database is closed before cleanup.
// Ownership is armed only after successful create-new; collisions never delete
// another file. No directory trees or user-selected existing files are removed.
struct ScratchFile {
    path: PathBuf,
    owned: bool,
}

impl Drop for ScratchFile {
    fn drop(&mut self) {
        if self.owned
            && let Err(error) = std::fs::remove_file(&self.path)
        {
            eprintln!("could not remove benchmark file {:?}: {error}", self.path);
        }
    }
}

fn synced_batch(db: &Database<Row>, batch_size: u64) -> Result<()> {
    const TRANSACTIONS: usize = 200;
    let mut samples = Vec::with_capacity(TRANSACTIONS);
    let total = Instant::now();
    for index in 0..TRANSACTIONS {
        let start = Instant::now();
        db.write(|tx| {
            for key in 0..batch_size {
                tx.put(key, Row(index as u64));
            }
            Ok(())
        })?;
        samples.push(start.elapsed());
    }
    let elapsed = total.elapsed();
    samples.sort_unstable();
    let p50 = samples[(TRANSACTIONS * 50).div_ceil(100) - 1];
    let p99 = samples[(TRANSACTIONS * 99).div_ceil(100) - 1];
    let tps = TRANSACTIONS as f64 / elapsed.as_secs_f64();
    println!(
        "Synced {batch_size} rows/tx: {tps:.0} tx/s, {:.0} rows/s, p50 {:.1} us, p99 {:.1} us",
        tps * batch_size as f64,
        p50.as_secs_f64() * 1e6,
        p99.as_secs_f64() * 1e6
    );
    Ok(())
}

fn durable(directory: &Path) -> Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let path = directory.join(format!("skrin-bench-{}-{stamp}.skrin", std::process::id()));
    let mut scratch = ScratchFile { path, owned: false };
    let db = Database::<Row>::create(&scratch.path)?;
    scratch.owned = true;
    synced_batch(&db, 1)?;
    synced_batch(&db, 100)?;
    println!("Persistent workload: {:?}", db.stats()?);
    drop(db);
    let start = Instant::now();
    let reopened = Database::<Row>::open(&scratch.path)?;
    println!(
        "Warm-cache WAL replay: {:.3} ms, {} rows",
        start.elapsed().as_secs_f64() * 1e3,
        reopened.stats()?.rows
    );
    Ok(())
}

fn main() -> Result<()> {
    let mut directory = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--bench" {
            continue; // Cargo supplies this to harness-free benchmarks.
        }
        if arg != "--durable" || directory.is_some() {
            return Err(std::io::Error::other("usage: baseline [--durable DIRECTORY]").into());
        }
        directory = Some(PathBuf::from(args.next().ok_or_else(|| {
            std::io::Error::other("missing durable benchmark directory")
        })?));
    }
    println!("Smoke baseline only; record hardware/compiler/filesystem for real comparisons.");
    memory()?;
    if let Some(directory) = directory {
        durable(&directory)?;
    } else {
        println!("No persistence measured. Add --durable DIRECTORY for synced transactions.");
    }
    Ok(())
}
