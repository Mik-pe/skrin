//! Sustained maintenance/resource evidence; no device-cold or reservation claim.
use skrin::{Database, Decoder, Encoder, Error, Record, Result, Schema};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Row {
    version: u64,
    payload: Vec<u8>,
}
impl Record for Row {
    const SCHEMA: Schema = Schema {
        table_id: 0x5245534f55524345,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.u64(self.version)?;
        e.bytes(&self.payload)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            version: d.u64()?,
            payload: d.bytes()?.into(),
        })
    }
}
fn row(key: u64, version: u64, bytes: usize) -> Row {
    let mut state = key.wrapping_mul(0x9e3779b97f4a7c15) ^ version ^ 0x6a09e667f3bcc909;
    let payload = (0..bytes)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    Row { version, payload }
}
fn key(slot: u64, rows: u64, round: u64) -> u64 {
    slot + if slot >= rows - rows / 10 && round % 2 == 1 {
        rows
    } else {
        0
    }
}
fn verify(db: &Database<Row>, rows: u64, round: u64, bytes: usize, sequence: u64) -> Result<()> {
    let read = db.read()?;
    assert_eq!(read.sequence(), sequence);
    assert_eq!(read.len(), rows as usize);
    for slot in 0..rows {
        let key = key(slot, rows, round);
        let actual = read.get(key).expect("every expected row survives");
        assert_eq!(actual.version, round);
        assert_eq!(actual.payload, row(key, round, bytes).payload);
    }
    Ok(())
}
fn counter(path: &str, field: &str) -> Option<u64> {
    fs::read_to_string(path).ok()?.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name == field)
            .then(|| value.split_whitespace().next()?.parse().ok())
            .flatten()
    })
}
fn memory(phase: &str) {
    println!(
        "memory,phase={phase},rss_kib={:?},peak_rss_kib={:?}",
        counter("/proc/self/status", "VmRSS"),
        counter("/proc/self/status", "VmHWM")
    );
}
// st_blocks describes per-inode allocated blocks, not unique device consumption
// on compressed/reflink filesystems. Directory/FS metadata is excluded.
fn disk(path: &Path) -> Result<(u64, Option<u64>)> {
    let mut logical = 0;
    let mut allocated = Some(0u64);
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            let (bytes, blocks) = disk(&entry.path())?;
            logical += bytes;
            allocated = allocated.zip(blocks).map(|(a, b)| a + b);
        } else if metadata.is_file() {
            logical += metadata.len();
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                allocated = allocated.map(|a| a + metadata.blocks() * 512);
            }
            #[cfg(not(unix))]
            {
                allocated = None;
            }
        } else {
            return Err(Error::InvalidOperation(
                "unexpected benchmark file type".into(),
            ));
        }
    }
    Ok((logical, allocated))
}
fn files(path: &Path, paths: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            files(&entry.path(), paths)?;
        } else if metadata.is_file() {
            paths.push(entry.path());
        } else {
            return Err(Error::InvalidOperation(
                "unexpected benchmark file type".into(),
            ));
        }
    }
    Ok(())
}
fn evict(path: &Path) -> Result<()> {
    let mut paths = Vec::new();
    files(path, &mut paths)?;
    // Only this process's closed, synced benchmark files are advised. No global
    // cache drop, permissions change or unsafe FFI. Python is optional tooling.
    let status = Command::new("python3").arg("-c").arg(
        "import os,sys\nfor p in sys.argv[1:]:\n f=os.open(p,os.O_RDONLY)\n try: os.posix_fadvise(f,0,0,os.POSIX_FADV_DONTNEED)\n finally: os.close(f)"
    ).args(paths).status()?;
    if !status.success() {
        return Err(Error::InvalidOperation("cache advice failed".into()));
    }
    Ok(())
}
fn reopen(
    path: &Path,
    config: &[String],
    round: u64,
    sequence: u64,
    phase: &str,
    advice: bool,
) -> Result<()> {
    for sample in 0..3 {
        if advice {
            evict(path)?;
        }
        let status = Command::new(std::env::current_exe()?)
            .arg("--open")
            .arg(path)
            .args(config)
            .arg(round.to_string())
            .arg(sequence.to_string())
            .arg(phase)
            .arg(sample.to_string())
            .status()?;
        if !status.success() {
            return Err(Error::InvalidOperation("reopen verification failed".into()));
        }
    }
    Ok(())
}
fn number(value: &str, minimum: u64, maximum: u64) -> Result<u64> {
    let parsed = value
        .parse()
        .map_err(|_| Error::InvalidOperation("expected integer".into()))?;
    if !(minimum..=maximum).contains(&parsed) {
        return Err(Error::InvalidOperation(format!(
            "value must be {minimum}..={maximum}"
        )));
    }
    Ok(parsed)
}
fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args()
        .skip(1)
        .filter(|s| s != "--bench")
        .collect();
    if args.first().is_some_and(|s| s == "--open") {
        if args.len() != 10 {
            return Err(Error::InvalidOperation("invalid reopen arguments".into()));
        }
        let rows = number(&args[2], 10, 1_000_000)?;
        let bytes = number(&args[4], 1, 4096)? as usize;
        let round = number(&args[6], 0, 10_000)?;
        let sequence = number(&args[7], 1, u64::MAX)?;
        let read_before = counter("/proc/self/io", "read_bytes");
        let start = Instant::now();
        let db = Database::<Row>::open_dir(&args[1])?;
        let open_ms = millis(start.elapsed());
        let read_bytes = counter("/proc/self/io", "read_bytes")
            .zip(read_before)
            .map(|(a, b)| a.saturating_sub(b));
        memory("reopen_before_verification");
        verify(&db, rows, round, bytes, sequence)?;
        println!(
            "reopen,phase={},round={round},sample={},open_ms={open_ms:.3},read_bytes={read_bytes:?}",
            args[8], args[9]
        );
        return Ok(());
    }
    if !(5..=6).contains(&args.len()) || (args.len() == 6 && args[5] != "--advisory-evict") {
        return Err(Error::InvalidOperation("usage: resources EXISTING_DIRECTORY ROWS ROUNDS PAYLOAD_BYTES BATCH_ROWS [--advisory-evict]".into()));
    }
    let rows = number(&args[1], 10, 1_000_000)?;
    let rounds = number(&args[2], 1, 10_000)?;
    let bytes = number(&args[3], 1, 4096)? as usize;
    let batch = number(&args[4], 1, 1000)?;
    let advice = args.len() == 6;
    if advice && !cfg!(target_os = "linux") {
        return Err(Error::InvalidOperation(
            "advisory eviction requires Linux and Python3".into(),
        ));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let root = Path::new(&args[0]).join(format!("skrin-resources-{}-{stamp}", std::process::id()));
    fs::create_dir(&root)?;
    let work = || -> Result<()> {
        println!(
            "config,rows={rows},rounds={rounds},payload_bytes={bytes},batch_rows={batch},durability=sync_all,advisory_eviction={advice},root={}",
            root.display()
        );
        let path = root.join("database");
        let mut db = Database::<Row>::create_dir(&path)?;
        for begin in (0..rows).step_by(batch as usize) {
            db.write(|tx| {
                for slot in begin..rows.min(begin + batch) {
                    tx.insert(slot, row(slot, 0, bytes))?;
                }
                Ok(())
            })?;
        }
        let mut sequence = db.stats()?.commits;
        verify(&db, rows, 0, bytes, sequence)?;
        db.checkpoint()?;
        db.reclaim()?;
        memory("seed_checkpoint");
        let workload_start = Instant::now();
        for round in 1..=rounds {
            let mut samples = Vec::new();
            for begin in (0..rows).step_by(batch as usize) {
                let start = Instant::now();
                db.write(|tx| {
                    for slot in begin..rows.min(begin + batch) {
                        let old_key = key(slot, rows, round - 1);
                        let new_key = key(slot, rows, round);
                        if old_key == new_key {
                            tx.update(old_key, |_| Ok(row(new_key, round, bytes)))?;
                        } else {
                            assert!(tx.remove(old_key));
                            tx.insert(new_key, row(new_key, round, bytes))?;
                        }
                    }
                    Ok(())
                })?;
                samples.push(start.elapsed());
            }
            sequence += samples.len() as u64;
            samples.sort();
            let p = |percent: usize| millis(samples[(samples.len() * percent).div_ceil(100) - 1]);
            println!(
                "commits,round={round},samples={},p50_ms={:.3},p95_ms={:.3},p99_ms={:.3},tx_per_s={:.3}",
                samples.len(),
                p(50),
                p(95),
                p(99),
                samples.len() as f64 / samples.iter().sum::<Duration>().as_secs_f64()
            );
            verify(&db, rows, round, bytes, sequence)?;
            memory("after_churn");
            let before = disk(&path)?;
            drop(db);
            reopen(
                &path,
                &args[1..5],
                round,
                sequence,
                "snapshot_plus_wal_warm",
                false,
            )?;
            if advice {
                reopen(
                    &path,
                    &args[1..5],
                    round,
                    sequence,
                    "snapshot_plus_wal_advisory_evicted",
                    true,
                )?;
            }
            db = Database::<Row>::open_dir(&path)?;
            let start = Instant::now();
            let cp = db.checkpoint()?;
            let checkpoint_ms = millis(start.elapsed());
            let staged = disk(&path)?;
            memory("after_checkpoint");
            let start = Instant::now();
            let report = db.reclaim()?;
            let reclaim_ms = millis(start.elapsed());
            let retained = disk(&path)?;
            let inventory = db.storage_inventory()?;
            assert!(!inventory.entries.iter().any(|entry| entry.reclaimable));
            println!(
                "maintenance,round={round},generation={},sequence={sequence},checkpoint_ms={checkpoint_ms:.3},reclaim_ms={reclaim_ms:.3},snapshot_bytes={},before_logical={},before_allocated={:?},staged_logical={},staged_allocated={:?},retained_logical={},retained_allocated={:?},reclaimed_bytes={}",
                cp.generation,
                cp.snapshot_bytes,
                before.0,
                before.1,
                staged.0,
                staged.1,
                retained.0,
                retained.1,
                report.bytes_removed
            );
            drop(db);
            reopen(&path, &args[1..5], round, sequence, "snapshot_warm", false)?;
            if advice {
                reopen(
                    &path,
                    &args[1..5],
                    round,
                    sequence,
                    "snapshot_advisory_evicted",
                    true,
                )?;
            }
            db = Database::<Row>::open_dir(&path)?;
        }
        println!(
            "complete,elapsed_s={:.3},acknowledged_sequence={sequence}",
            workload_start.elapsed().as_secs_f64()
        );
        memory("whole_workload");
        Ok(())
    };
    let result = work();
    // Never remove a caller path; root was created exclusively by this process.
    let cleanup = fs::remove_dir_all(&root);
    result?;
    cleanup?;
    Ok(())
}
