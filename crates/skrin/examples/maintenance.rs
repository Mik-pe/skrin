//! Bounded, caller-driven maintenance. Run with a NEW directory path on Unix.
use skrin::{
    CheckpointPolicy, Database, Decoder, Encoder, Error, MaintenanceOptions, Record, Result, Schema,
};

struct Counter(u64);
impl Record for Counter {
    const SCHEMA: Schema = Schema {
        table_id: 0x434f554e544552,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.u64(self.0)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(d.u64()?))
    }
}
fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .ok_or_else(|| Error::InvalidOperation("usage: maintenance NEW_DIRECTORY".into()))?;
    if args.next().is_some() {
        return Err(Error::InvalidOperation(
            "expected exactly one NEW_DIRECTORY".into(),
        ));
    }
    let db = Database::<Counter>::create_dir(&path)?;
    let options = MaintenanceOptions {
        max_new_file_bytes: 1024 * 1024,
        max_record_bytes: 8,
        max_rows: 128,
    };
    let policy = CheckpointPolicy {
        wal_bytes: Some(32 * 1024),
        commits: Some(100),
    };
    for update in 0..500 {
        db.write(|tx| {
            tx.put(update % 128, Counter(update));
            Ok(())
        })?;
        // Success above is independent of any later maintenance/cleanup outcome.
        if let Some(checkpoint) = db.checkpoint_if_needed(policy, options)? {
            let reclaimed = db.reclaim()?;
            println!(
                "checkpoint sequence {}, snapshot {} B; reclaimed {} B",
                checkpoint.sequence, checkpoint.snapshot_bytes, reclaimed.bytes_removed
            );
        }
    }
    let estimate = db.estimate_checkpoint(options)?;
    println!(
        "next checkpoint: {} rows, {} new file bytes, largest record {} B",
        estimate.rows, estimate.new_file_bytes, estimate.largest_record_bytes
    );
    let inventory = db.storage_inventory()?;
    println!(
        "storage generation {}, {} observed regular-file bytes, {} reclaimable",
        inventory.generation, inventory.observed_file_bytes, inventory.reclaimable_file_bytes
    );
    drop(db);
    let db = Database::<Counter>::open_dir(path)?;
    assert_eq!(db.stats()?.commits, 500);
    assert_eq!(db.read()?.len(), 128);
    assert_eq!(db.read()?.get(499 % 128).unwrap().0, 499);
    println!("verified 500 committed updates after reopening");
    Ok(())
}
