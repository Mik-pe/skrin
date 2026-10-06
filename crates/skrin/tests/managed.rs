#![cfg(unix)]
use skrin::{Database, Decoder, Encoder, Error, Record, Result, Schema};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, PartialEq, Eq)]
struct Person {
    name: String,
}
impl Record for Person {
    const SCHEMA: Schema = Schema {
        table_id: 55,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.string(&self.name)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            name: d.string()?.into(),
        })
    }
}
#[derive(Debug, PartialEq, Eq)]
struct PersonV2 {
    first: String,
    last: String,
    active: bool,
}
impl Record for PersonV2 {
    const SCHEMA: Schema = Schema {
        table_id: 55,
        version: 2,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.string(&self.first)?;
        e.string(&self.last)?;
        e.u8(u8::from(self.active))
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        let first = d.string()?.into();
        let last = d.string()?.into();
        let active = match d.u8()? {
            0 => false,
            1 => true,
            _ => return Err(Error::Codec("bad bool".into())),
        };
        Ok(Self {
            first,
            last,
            active,
        })
    }
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "skrin-managed-{}-{stamp}-{}",
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
        let _ = fs::remove_file(&self.0);
    }
}
fn person(name: &str) -> Person {
    Person { name: name.into() }
}
fn generation(path: &Temp, id: u64) -> PathBuf {
    path.0.join(format!("g{id:016x}"))
}

#[test]
fn checkpoint_reopen_and_retention_preserve_commit_sequence() -> Result<()> {
    let path = Temp::new();
    let mut db = Database::<Person>::create_dir(&path.0)?;
    for round in 0..16 {
        for update in 0..10 {
            db.write(|tx| {
                tx.put(1, person(&format!("{round}-{update}")));
                tx.put(2, person("removed"));
                tx.remove(2);
                Ok(())
            })?;
        }
        assert!(db.stats()?.wal_bytes > 56);
        let before = db.stats()?;
        let checkpoint = db.checkpoint()?;
        assert_eq!(checkpoint.sequence, before.commits);
        assert_eq!(checkpoint.rows, 1);
        assert_eq!(db.stats()?.wal_bytes, 56);
        db.prune()?;
        drop(db);
        db = Database::open_dir(&path.0)?;
        assert_eq!(db.stats()?.commits, (round + 1) * 10);
        assert_eq!(db.read()?.get(1), Some(&person(&format!("{round}-9"))));
        let count = fs::read_dir(&path.0)?
            .filter(|entry| entry.as_ref().unwrap().file_type().unwrap().is_dir())
            .count();
        assert_eq!(count, 2);
    }
    db.write(|tx| {
        tx.remove(1);
        Ok(())
    })?;
    db.checkpoint()?;
    let sequence = db.stats()?.commits;
    drop(db);
    let db = Database::<Person>::open_dir(&path.0)?;
    assert!(db.read()?.is_empty());
    assert_eq!(db.stats()?.commits, sequence);
    Ok(())
}

#[test]
fn backup_is_independent_and_imports_v1_without_rewriting_source() -> Result<()> {
    let path = Temp::new();
    let backup_path = Temp::new();
    let db = Database::<Person>::create(&path.0)?;
    db.write(|tx| tx.insert(42, person("Micke Skrin")))?;
    let original = fs::read(&path.0)?;
    let backup = db.backup_to(&backup_path.0)?;
    assert_eq!(backup.stats()?.commits, 1);
    assert_eq!(backup.read()?.get(42), Some(&person("Micke Skrin")));
    assert_eq!(fs::read(&path.0)?, original);
    db.write(|tx| {
        tx.put(42, person("changed source"));
        Ok(())
    })?;
    assert_eq!(backup.read()?.get(42), Some(&person("Micke Skrin")));
    drop(backup);
    let restored = Database::<Person>::open_dir(&backup_path.0)?;
    assert_eq!(restored.read()?.get(42), Some(&person("Micke Skrin")));
    assert!(db.backup_to(&backup_path.0).is_err());
    assert!(db.checkpoint().is_err());
    Ok(())
}

#[test]
fn real_schema_migration_splits_a_field_adds_a_default_and_preserves_history() -> Result<()> {
    let path = Temp::new();
    let backup_path = Temp::new();
    let db = Database::<Person>::create_dir(&path.0)?;
    db.write(|tx| tx.insert(42, person("Micke Skrin")))?;
    let migrated = db.migrate::<PersonV2>("split-name-v2", |_, row| {
        let (first, last) = row.name.split_once(' ').unwrap();
        Ok(PersonV2 {
            first: first.into(),
            last: last.into(),
            active: true,
        })
    })?;
    assert_eq!(migrated.stats()?.commits, 1);
    assert_eq!(migrated.read()?.get(42).unwrap().first, "Micke");
    migrated.checkpoint()?;
    migrated.prune()?;
    let backup = migrated.backup_to(&backup_path.0)?;
    assert_eq!(
        backup.generation_info()?.unwrap().migrations,
        migrated.generation_info()?.unwrap().migrations
    );
    drop(backup);
    drop(migrated);
    assert!(matches!(
        Database::<Person>::open_dir(&path.0),
        Err(Error::SchemaMismatch { .. })
    ));
    let reopened = Database::<PersonV2>::open_dir(&path.0)?;
    assert_eq!(reopened.read()?.get(42).unwrap().last, "Skrin");
    let info = reopened.generation_info()?.unwrap();
    assert_eq!(info.migrations[0].id, "split-name-v2");
    assert_eq!(info.migrations[0].at_sequence, 1);
    Ok(())
}

#[test]
fn migration_conversion_error_leaves_source_openable_and_retryable() -> Result<()> {
    let path = Temp::new();
    let db = Database::<Person>::create_dir(&path.0)?;
    db.write(|tx| {
        tx.insert(1, person("one"))?;
        tx.insert(2, person("two"))
    })?;
    let original = fs::read(path.0.join("CURRENT"))?;
    let outcome = db.migrate::<PersonV2>("v2", |key, row| {
        if key == 2 {
            return Err(Error::Codec("refuse row".into()));
        }
        Ok(PersonV2 {
            first: row.name,
            last: String::new(),
            active: true,
        })
    });
    assert!(outcome.is_err());
    assert_eq!(fs::read(path.0.join("CURRENT"))?, original);
    let db = Database::<Person>::open_dir(&path.0)?;
    assert_eq!(db.read()?.len(), 2);
    let new = db.migrate::<PersonV2>("v2", |_, row| {
        Ok(PersonV2 {
            first: row.name,
            last: String::new(),
            active: true,
        })
    })?;
    assert_eq!(new.read()?.len(), 2);
    Ok(())
}

#[test]
fn no_implicit_schema_change_or_destination_overwrite() -> Result<()> {
    let path = Temp::new();
    let db = Database::<Person>::create_dir(&path.0)?;
    assert!(Database::<Person>::create_dir(&path.0).is_err());
    assert!(db.backup_to(&path.0).is_err());
    let outcome = db.migrate::<Person>("not-an-upgrade", |_, row| Ok(row));
    assert!(matches!(outcome, Err(Error::InvalidOperation(_))));
    let db = Database::<Person>::open_dir(&path.0)?;
    assert!(db.read()?.is_empty());
    Ok(())
}

#[test]
fn internal_segments_cannot_be_opened_as_standalone_databases() -> Result<()> {
    let path = Temp::new();
    let db = Database::<Person>::create_dir(&path.0)?;
    let wal = generation(&path, 1).join("wal");
    drop(db);
    assert!(matches!(
        Database::<Person>::open(wal),
        Err(Error::UnsupportedFormat(2))
    ));
    Ok(())
}

#[test]
fn manifest_is_authoritative_and_cross_generation_files_are_rejected() -> Result<()> {
    let path = Temp::new();
    let db = Database::<Person>::create_dir(&path.0)?;
    db.write(|tx| tx.insert(1, person("kept")))?;
    let previous = db.checkpoint()?.generation;
    let active = db.checkpoint()?.generation;
    drop(db);
    let current = path.0.join("CURRENT");
    let manifest = fs::read(&current)?;
    fs::remove_file(&current)?;
    assert!(Database::<Person>::open_dir(&path.0).is_err());
    fs::write(&current, manifest)?;
    let wal = generation(&path, active).join("wal");
    let bytes = fs::read(&wal)?;
    fs::copy(generation(&path, previous).join("wal"), &wal)?;
    assert!(Database::<Person>::open_dir(&path.0).is_err());
    fs::write(&wal, bytes)?;
    let snap = generation(&path, active).join("snapshot");
    let bytes = fs::read(&snap)?;
    fs::copy(generation(&path, previous).join("snapshot"), &snap)?;
    assert!(Database::<Person>::open_dir(&path.0).is_err());
    fs::write(snap, bytes)?;
    assert_eq!(
        Database::<Person>::open_dir(&path.0)?.read()?.get(1),
        Some(&person("kept"))
    );
    Ok(())
}

#[test]
fn pruning_never_removes_unknown_files_or_active_generations() -> Result<()> {
    let path = Temp::new();
    let db = Database::<Person>::create_dir(&path.0)?;
    fs::write(generation(&path, 1).join("user-note.txt"), "must survive")?;
    fs::write(path.0.join("unrelated.txt"), "also survives")?;
    for _ in 0..4 {
        db.checkpoint()?;
    }
    let report = db.prune()?;
    assert_eq!(report.skipped, 1);
    assert_eq!(
        fs::read_to_string(generation(&path, 1).join("user-note.txt"))?,
        "must survive"
    );
    assert!(generation(&path, 4).exists() && generation(&path, 5).exists());
    assert!(!generation(&path, 2).exists() && !generation(&path, 3).exists());
    assert!(path.0.join("unrelated.txt").exists());
    Ok(())
}

#[test]
fn stable_lock_excludes_processes_after_every_generation_swap() -> Result<()> {
    let path = Temp::new();
    let db = Database::<Person>::create_dir(&path.0)?;
    for _ in 0..3 {
        db.checkpoint()?;
        assert!(matches!(
            Database::<Person>::open_dir(&path.0),
            Err(Error::Busy)
        ));
        let status = Command::new(std::env::current_exe()?)
            .args(["--exact", "child_directory_lock_probe", "--nocapture"])
            .env("SKRIN_DIRECTORY_LOCK_PROBE", &path.0)
            .status()?;
        assert!(status.success());
    }
    drop(db);
    Database::<Person>::open_dir(&path.0)?;
    Ok(())
}

#[test]
fn child_directory_lock_probe() {
    let Some(path) = std::env::var_os("SKRIN_DIRECTORY_LOCK_PROBE") else {
        return;
    };
    assert!(matches!(
        Database::<Person>::open_dir(path),
        Err(Error::Busy)
    ));
}

#[test]
fn snapshots_can_exceed_the_per_transaction_limit() -> Result<()> {
    struct Chunk(Vec<u8>);
    impl Record for Chunk {
        const SCHEMA: Schema = Schema {
            table_id: 91,
            version: 1,
        };
        fn encode(&self, e: &mut Encoder) -> Result<()> {
            e.bytes(&self.0)
        }
        fn decode(d: &mut Decoder<'_>) -> Result<Self> {
            Ok(Self(d.bytes()?.to_vec()))
        }
    }
    let path = Temp::new();
    let memory = Database::<Chunk>::in_memory();
    memory.write(|tx| {
        for key in 0..9 {
            tx.insert(key, Chunk(vec![key as u8; 2 * 1024 * 1024]))?;
        }
        Ok(())
    })?;
    let backup = memory.backup_to(&path.0)?;
    assert_eq!(backup.stats()?.commits, 1);
    drop(backup);
    let reopened = Database::<Chunk>::open_dir(&path.0)?;
    let read = reopened.read()?;
    assert_eq!(read.len(), 9);
    for (key, row) in read.iter() {
        assert_eq!(row.0.len(), 2 * 1024 * 1024);
        assert!(row.0.iter().all(|&byte| byte == key as u8));
    }
    Ok(())
}
