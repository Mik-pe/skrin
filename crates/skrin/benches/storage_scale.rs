//! Same workload before/after engine changes; timings are not device certification.
use skrin::{Database, Decoder, Encoder, Error, Record, Result, Schema};
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Row {
    value: u64,
    text: String,
}
impl Record for Row {
    const SCHEMA: Schema = Schema {
        table_id: 0x5343414c45,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.u64(self.value)?;
        e.string(&self.text)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            value: d.u64()?,
            text: d.string()?.into(),
        })
    }
}
fn timed<T>(f: impl FnOnce() -> Result<T>) -> Result<(T, f64)> {
    let start = Instant::now();
    let value = f()?;
    Ok((value, start.elapsed().as_secs_f64() * 1000.0))
}
fn summary(name: &str, samples: &mut [f64]) {
    samples.sort_by(f64::total_cmp);
    println!(
        "{name},median_ms={:.3},min_ms={:.3},max_ms={:.3},samples={}",
        samples[samples.len() / 2],
        samples[0],
        samples[samples.len() - 1],
        samples.len()
    );
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args()
        .skip(1)
        .filter(|arg| arg != "--bench")
        .collect();
    if args.len() != 2 {
        return Err(Error::InvalidOperation(
            "usage: storage_scale EXISTING_DIRECTORY ROWS".into(),
        ));
    }
    let rows: u64 = args[1]
        .parse()
        .map_err(|_| Error::InvalidOperation("ROWS must be an integer".into()))?;
    if !(1..=1_000_000).contains(&rows) {
        return Err(Error::InvalidOperation("ROWS must be 1..=1000000".into()));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let root = Path::new(&args[0]).join(format!("skrin-scale-{}-{stamp}", std::process::id()));
    // Create-only: remove only this benchmark's successfully created directory.
    std::fs::create_dir(&root)?;
    let work = || -> Result<()> {
        let legacy = root.join("legacy");
        let managed = root.join("managed");
        let mut db = Database::<Row>::create(&legacy)?;
        let (_, seed_ms) = timed(|| {
            for batch in (0..rows).step_by(1000) {
                db.write(|tx| {
                    for key in batch..(batch + 1000).min(rows) {
                        tx.insert(
                            key,
                            Row {
                                value: key.wrapping_mul(17),
                                text: "s".repeat(96),
                            },
                        )?;
                    }
                    Ok(())
                })?;
            }
            Ok(())
        })?;
        println!(
            "rows={rows},encoded_record_bytes=108,seed_ms={seed_ms:.3},wal_bytes={}",
            db.stats()?.wal_bytes
        );
        let mut replay = Vec::new();
        for _ in 0..7 {
            drop(db);
            let (opened, ms) = timed(|| Database::<Row>::open(&legacy))?;
            db = opened;
            replay.push(ms);
            assert_eq!(db.read()?.len(), rows as usize);
        }
        summary("wal_replay", &mut replay);
        let (backup, backup_ms) = timed(|| db.backup_to(&managed))?;
        drop(db);
        db = backup;
        println!("backup_ms={backup_ms:.3}");
        let mut checkpoints = Vec::new();
        let mut reopen = Vec::new();
        for _ in 0..7 {
            let (cp, ms) = timed(|| db.checkpoint())?;
            checkpoints.push(ms);
            assert_eq!(cp.rows, rows as usize);
            db.prune()?;
            drop(db);
            let (opened, ms) = timed(|| Database::<Row>::open_dir(&managed))?;
            db = opened;
            reopen.push(ms);
            let read = db.read()?;
            assert_eq!(read.len(), rows as usize);
            for (key, row) in read.iter() {
                assert_eq!(row.value, key.wrapping_mul(17));
                assert_eq!(row.text, "s".repeat(96));
            }
        }
        summary("checkpoint", &mut checkpoints);
        summary("snapshot_reopen", &mut reopen);
        drop(db);
        Ok(())
    };
    let result = work();
    let cleanup = std::fs::remove_dir_all(&root);
    result?;
    cleanup?;
    Ok(())
}
