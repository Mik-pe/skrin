//! End-to-end maintenance smoke measurement, not a comparative performance claim.
use skrin::{Database, Decoder, Encoder, Error, Record, Result, Schema};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Row(u64);
impl Record for Row {
    const SCHEMA: Schema = Schema {
        table_id: 0x4d41494e54,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.u64(self.0)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(d.u64()?))
    }
}
struct Scratch {
    path: PathBuf,
    owned: bool,
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if self.owned
            && let Err(error) = std::fs::remove_dir_all(&self.path)
        {
            eprintln!(
                "could not remove owned benchmark directory {:?}: {error}",
                self.path
            );
        }
    }
}
fn bytes(path: &Path) -> std::io::Result<u64> {
    let mut size = 0;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        size += if entry.file_type()?.is_dir() {
            bytes(&entry.path())?
        } else {
            entry.metadata()?.len()
        };
    }
    Ok(size)
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os()
        .skip(1)
        .filter(|arg| arg != "--bench")
        .collect();
    if args.len() != 1 {
        return Err(Error::InvalidOperation(
            "usage: maintenance EXISTING_DIRECTORY".into(),
        ));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_nanos();
    let mut scratch = Scratch {
        path: Path::new(&args[0]).join(format!("skrin-maint-{}-{stamp}", std::process::id())),
        owned: false,
    };
    let mut db = Database::<Row>::create_dir(&scratch.path)?;
    scratch.owned = true; // only this newly created directory can ever be removed
    println!("Synced maintenance smoke test; warm-cache reopen; no comparative claim.");
    for round in 0..8 {
        for transaction in 0..200 {
            db.write(|tx| {
                for key in 0..100 {
                    tx.put(key, Row(round * 200 + transaction));
                }
                Ok(())
            })?;
        }
        let before = bytes(&scratch.path)?;
        let wal_before = db.stats()?.wal_bytes;
        let started = Instant::now();
        let checkpoint = db.checkpoint()?;
        let checkpoint_ms = started.elapsed().as_secs_f64() * 1e3;
        let peak = bytes(&scratch.path)?;
        let started = Instant::now();
        let pruned = db.prune()?;
        let prune_ms = started.elapsed().as_secs_f64() * 1e3;
        let after = bytes(&scratch.path)?;
        let sequence = db.stats()?.commits;
        drop(db);
        let started = Instant::now();
        db = Database::<Row>::open_dir(&scratch.path)?;
        let reopen_ms = started.elapsed().as_secs_f64() * 1e3;
        assert_eq!(db.stats()?.commits, sequence);
        assert_eq!(db.read()?.get(42).unwrap().0, round * 200 + 199);
        println!(
            "round {round}: checkpoint {checkpoint_ms:.3} ms, prune {prune_ms:.3} ms, reopen {reopen_ms:.3} ms; WAL {wal_before} -> {} B, snapshot {} B; disk before/peak/after {before}/{peak}/{after} B; removed {} generations",
            db.stats()?.wal_bytes,
            checkpoint.snapshot_bytes,
            pruned.generations_removed
        );
    }
    drop(db);
    Ok(())
}
