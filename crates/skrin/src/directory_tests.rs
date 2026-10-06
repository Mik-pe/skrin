use super::*;
use crate::Database;
use crate::test_support::Item;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "skrin-dir-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        assert!(!path.exists());
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn seed() -> (Temp, Database<Item>) {
    let path = Temp::new();
    let db = Database::<Item>::create_dir(&path.0).unwrap();
    db.write(|tx| {
        tx.insert(1, Item(10))?;
        tx.insert(2, Item(20))
    })
    .unwrap();
    (path, db)
}

fn check(db: &Database<Item>) {
    let read = db.read().unwrap();
    assert_eq!(read.sequence(), 1);
    assert_eq!(
        read.iter()
            .map(|(key, row)| (key, row.0))
            .collect::<Vec<_>>(),
        [(1, 10), (2, 20)]
    );
}

struct NewItem(u64);
impl Record for NewItem {
    const SCHEMA: Schema = Schema {
        table_id: Item::SCHEMA.table_id,
        version: 2,
    };
    fn encode(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.string("new schema")?;
        encoder.u64(self.0)
    }
    fn decode(decoder: &mut Decoder<'_>) -> Result<Self> {
        if decoder.string()? != "new schema" {
            return Err(Error::Codec("wrong schema".into()));
        }
        Ok(Self(decoder.u64()?))
    }
}

#[test]
fn every_checkpoint_boundary_recovers_exact_rows_and_sequence() {
    let mut clean_failures = 0;
    let mut uncertain_failures = 0;
    for cutoff in 0..100 {
        let (path, db) = seed();
        faults::arm(cutoff);
        let outcome = db.checkpoint();
        faults::clear();
        // Even a poisoned handle still owns the same stable lock.
        assert!(matches!(
            Database::<Item>::open_dir(&path.0),
            Err(Error::Busy)
        ));
        match outcome {
            Err(Error::MaintenanceUncertain(_)) => {
                uncertain_failures += 1;
                assert!(matches!(db.read(), Err(Error::Poisoned)));
                assert!(matches!(db.checkpoint(), Err(Error::Poisoned)));
                assert!(matches!(db.prune(), Err(Error::Poisoned)));
            }
            Err(_) => {
                clean_failures += 1;
                check(&db);
                // Retry must skip staged/orphan generations instead of overwriting.
                db.checkpoint().unwrap();
            }
            Ok(_) => {
                drop(db);
                check(&Database::<Item>::open_dir(&path.0).unwrap());
                assert!(clean_failures > 20 && uncertain_failures >= 3);
                return;
            }
        }
        drop(db);
        let reopened = Database::<Item>::open_dir(&path.0).unwrap();
        check(&reopened);
        reopened
            .write(|tx| {
                tx.put(1, Item(11));
                Ok(())
            })
            .unwrap();
        drop(reopened);
        let reopened = Database::<Item>::open_dir(&path.0).unwrap();
        assert_eq!(reopened.stats().unwrap().commits, 2);
        assert_eq!(reopened.read().unwrap().get(1).unwrap().0, 11);
    }
    panic!("did not exercise every checkpoint boundary");
}

#[test]
fn every_migration_boundary_recovers_only_a_complete_old_or_new_schema() {
    let mut reached_success = false;
    for cutoff in 0..100 {
        let (path, db) = seed();
        faults::arm(cutoff);
        let outcome = db.migrate::<NewItem>("add-tag-v2", |_, old| Ok(NewItem(old.0 + 100)));
        faults::clear();
        let success = outcome.is_ok();
        drop(outcome);
        match Database::<Item>::open_dir(&path.0) {
            Ok(old) => check(&old),
            Err(Error::SchemaMismatch { found, .. }) => {
                assert_eq!(found, NewItem::SCHEMA);
                let new = Database::<NewItem>::open_dir(&path.0).unwrap();
                assert_eq!(new.stats().unwrap().commits, 1);
                assert_eq!(
                    new.read()
                        .unwrap()
                        .iter()
                        .map(|(key, row)| (key, row.0))
                        .collect::<Vec<_>>(),
                    [(1, 110), (2, 120)]
                );
                let info = new.generation_info().unwrap().unwrap();
                assert_eq!(info.migrations.len(), 1);
                assert_eq!(info.migrations[0].id, "add-tag-v2");
            }
            Err(error) => panic!("cutoff {cutoff}: {error}"),
        }
        if success {
            reached_success = true;
            break;
        }
    }
    assert!(reached_success);
}

#[test]
fn cleanup_failures_are_restartable_and_never_poison_the_active_generation() {
    for cutoff in 0..100 {
        let (path, db) = seed();
        for _ in 0..4 {
            db.checkpoint().unwrap();
        }
        faults::arm(cutoff);
        let outcome = db.prune();
        faults::clear();
        check(&db);
        let success = outcome.is_ok();
        db.prune().unwrap();
        let generations = fs::read_dir(&path.0)
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                entry.file_name().to_str().and_then(parse_generation)
            })
            .count();
        assert_eq!(generations, 2, "cutoff {cutoff}");
        drop(db);
        check(&Database::<Item>::open_dir(&path.0).unwrap());
        if success {
            return;
        }
    }
    panic!("did not exercise every cleanup boundary");
}

#[test]
fn every_bit_and_truncation_of_a_manifest_is_rejected_without_wal_repair() {
    let (path, db) = seed();
    let checkpoint = db.checkpoint().unwrap();
    drop(db);
    let current = path.0.join("CURRENT");
    let original = fs::read(&current).unwrap();
    let wal = path
        .0
        .join(generation_name(checkpoint.generation))
        .join("wal");
    let mut wal_bytes = fs::read(&wal).unwrap();
    wal_bytes.extend_from_slice(b"TX");
    fs::write(&wal, &wal_bytes).unwrap();
    for end in 0..original.len() {
        fs::write(&current, &original[..end]).unwrap();
        assert!(Database::<Item>::open_dir(&path.0).is_err(), "cutoff {end}");
        assert_eq!(fs::read(&current).unwrap(), original[..end]);
        assert_eq!(fs::read(&wal).unwrap(), wal_bytes);
    }
    for bit in 0..original.len() * 8 {
        let mut changed = original.clone();
        changed[bit / 8] ^= 1 << (bit % 8);
        fs::write(&current, &changed).unwrap();
        assert!(Database::<Item>::open_dir(&path.0).is_err(), "bit {bit}");
        assert_eq!(fs::read(&current).unwrap(), changed);
        assert_eq!(fs::read(&wal).unwrap(), wal_bytes);
    }
    fs::write(&current, &original).unwrap();
    let db = Database::<Item>::open_dir(&path.0).unwrap();
    check(&db);
    assert_eq!(db.stats().unwrap().recovered_tail_bytes, 2);
}

#[test]
fn every_snapshot_bit_and_truncation_is_rejected_without_repair() {
    let (path, db) = seed();
    let checkpoint = db.checkpoint().unwrap();
    drop(db);
    let snapshot = path
        .0
        .join(generation_name(checkpoint.generation))
        .join("snapshot");
    let original = fs::read(&snapshot).unwrap();
    for end in 0..original.len() {
        fs::write(&snapshot, &original[..end]).unwrap();
        assert!(Database::<Item>::open_dir(&path.0).is_err(), "cutoff {end}");
        assert_eq!(fs::read(&snapshot).unwrap(), original[..end]);
    }
    for bit in 0..original.len() * 8 {
        let mut changed = original.clone();
        changed[bit / 8] ^= 1 << (bit % 8);
        fs::write(&snapshot, &changed).unwrap();
        assert!(Database::<Item>::open_dir(&path.0).is_err(), "bit {bit}");
        assert_eq!(fs::read(&snapshot).unwrap(), changed);
    }
    fs::write(&snapshot, &original).unwrap();
    check(&Database::<Item>::open_dir(&path.0).unwrap());
}

#[test]
fn checksummed_but_invalid_manifest_metadata_is_rejected() {
    let (path, db) = seed();
    drop(db);
    let current = path.0.join("CURRENT");
    let original = fs::read(&current).unwrap();
    let manifest = Manifest::decode(&original).unwrap();
    for mutation in 0..4 {
        let mut changed = manifest.clone();
        match mutation {
            0 => changed.info.generation = 0,
            1 => changed.info.previous_generation = changed.info.generation,
            2 => changed.rows = u64::MAX,
            _ => changed.info.migrations.push(Migration {
                id: "bad".into(),
                from_version: 1,
                to_version: 2,
                at_sequence: 0,
            }),
        }
        fs::write(&current, changed.encode().unwrap()).unwrap();
        assert!(Database::<Item>::open_dir(&path.0).is_err());
    }
    fs::write(&current, &original).unwrap();
}

#[test]
fn publication_rejects_a_record_that_cannot_decode_its_own_encoding() {
    struct Broken;
    impl Record for Broken {
        const SCHEMA: Schema = Item::SCHEMA;
        fn encode(&self, e: &mut Encoder) -> Result<()> {
            e.u64(1)
        }
        fn decode(_: &mut Decoder<'_>) -> Result<Self> {
            Err(Error::Codec("broken".into()))
        }
    }
    let path = Temp::new();
    let db = Database::<Broken>::create_dir(&path.0).unwrap();
    db.write(|tx| tx.insert(1, Broken)).unwrap();
    let manifest = fs::read(path.0.join("CURRENT")).unwrap();
    assert!(db.checkpoint().is_err());
    assert!(db.read().is_ok());
    assert_eq!(fs::read(path.0.join("CURRENT")).unwrap(), manifest);
}

#[test]
fn process_exit_at_each_publication_boundary_never_selects_partial_state() {
    for operation in ["checkpoint", "migration"] {
        let mut completed = false;
        for cutoff in 0..100 {
            let (path, db) = seed();
            drop(db);
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "directory::tests::child_exit_at_publication_boundary",
                ])
                .env("SKRIN_EXIT_ROOT", &path.0)
                .env("SKRIN_EXIT_AFTER", cutoff.to_string())
                .env("SKRIN_EXIT_OPERATION", operation)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(
                status.success() || status.code() == Some(73),
                "{operation} cutoff {cutoff}"
            );
            match Database::<Item>::open_dir(&path.0) {
                Ok(old) => check(&old),
                Err(Error::SchemaMismatch { .. }) if operation == "migration" => {
                    let new = Database::<NewItem>::open_dir(&path.0).unwrap();
                    assert_eq!(new.stats().unwrap().commits, 1);
                    assert_eq!(
                        new.read()
                            .unwrap()
                            .iter()
                            .map(|(key, row)| (key, row.0))
                            .collect::<Vec<_>>(),
                        [(1, 110), (2, 120)]
                    );
                }
                Err(error) => panic!("{operation} cutoff {cutoff}: {error}"),
            }
            if status.success() {
                completed = true;
                break;
            }
        }
        assert!(completed);
    }
}

#[test]
fn child_exit_at_publication_boundary() {
    let Some(path) = std::env::var_os("SKRIN_EXIT_ROOT") else {
        return;
    };
    let db = Database::<Item>::open_dir(path).unwrap();
    let after = std::env::var("SKRIN_EXIT_AFTER").unwrap().parse().unwrap();
    faults::exit_after(after);
    if std::env::var("SKRIN_EXIT_OPERATION").unwrap() == "migration" {
        let _db = db
            .migrate::<NewItem>("process-v2", |_, old| Ok(NewItem(old.0 + 100)))
            .unwrap();
    } else {
        db.checkpoint().unwrap();
    }
}

#[test]
fn generation_formats_match_independently_encoded_fixtures() {
    fn bytes(hex: &str) -> Vec<u8> {
        hex.split_whitespace()
            .map(|byte| u8::from_str_radix(byte, 16).unwrap())
            .collect()
    }
    let (path, db) = seed();
    let checkpoint = db.checkpoint().unwrap();
    assert_eq!(checkpoint.generation, 2);
    let gen_path = path.0.join(generation_name(2));
    assert_eq!(
        fs::read(path.0.join("CURRENT")).unwrap(),
        bytes(include_str!("../tests/fixtures/manifest-v1.hex"))
    );
    assert_eq!(
        fs::read(gen_path.join("snapshot")).unwrap(),
        bytes(include_str!("../tests/fixtures/snapshot-v1.hex"))
    );
    assert_eq!(
        fs::read(gen_path.join("wal")).unwrap(),
        bytes(include_str!("../tests/fixtures/segment-v2.hex"))
    );
}
