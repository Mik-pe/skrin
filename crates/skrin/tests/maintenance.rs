#![cfg(unix)]
use skrin::{
    CheckpointPolicy, Database, Decoder, Encoder, Error, MaintenanceOptions, Record, Result,
    Schema, StorageEntryKind,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "skrin-limits-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn db(&self) -> PathBuf {
        self.0.join("db")
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Row(u64);
impl Record for Row {
    const SCHEMA: Schema = Schema {
        table_id: 314,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.u64(self.0)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(d.u64()?))
    }
}
struct NewRow(u64);
impl Record for NewRow {
    const SCHEMA: Schema = Schema {
        table_id: 314,
        version: 2,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.string("migrated")?;
        e.u64(self.0)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        assert_eq!(d.string()?, "migrated");
        Ok(Self(d.u64()?))
    }
}
fn seed() -> (Temp, Database<Row>) {
    let path = Temp::new();
    let db = Database::create_dir(path.db()).unwrap();
    db.write(|tx| {
        tx.insert(1, Row(10))?;
        tx.insert(2, Row(20))
    })
    .unwrap();
    (path, db)
}
fn rows(db: &Database<Row>) -> Vec<(u64, u64)> {
    db.read().unwrap().iter().map(|(k, r)| (k, r.0)).collect()
}
// Captures exact test-owned namespace contents; never follows symlinks.
fn tree(path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            let bytes = if kind.is_file() {
                fs::read(entry.path()).unwrap()
            } else if kind.is_symlink() {
                fs::read_link(entry.path())
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else {
                Vec::new()
            };
            out.insert(entry.path().strip_prefix(root).unwrap().to_owned(), bytes);
            if kind.is_dir() {
                visit(root, &entry.path(), out);
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(path, path, &mut out);
    out
}

#[test]
fn estimates_are_read_only_and_exact_byte_budgets_are_enforced() {
    let (path, db) = seed();
    let original = tree(&path.db());
    let estimate = db
        .estimate_checkpoint(MaintenanceOptions::default())
        .unwrap();
    assert_eq!(estimate.rows, 2);
    assert_eq!(estimate.sequence, 1);
    assert_eq!(estimate.snapshot_bytes, 48 + 2 * 24);
    assert_eq!(
        estimate.new_file_bytes,
        estimate.snapshot_bytes + 28 + 56 + 60
    );
    assert_eq!(estimate.largest_record_bytes, 8);
    db.storage_inventory().unwrap();
    assert_eq!(tree(&path.db()), original);
    assert!(matches!(
        db.checkpoint_with_options(MaintenanceOptions {
            max_rows: 1,
            ..Default::default()
        }),
        Err(Error::BudgetExceeded {
            resource: "snapshot rows",
            required: 2,
            ..
        })
    ));
    assert_eq!(tree(&path.db()), original);
    let options = MaintenanceOptions {
        max_new_file_bytes: estimate.new_file_bytes - 1,
        ..Default::default()
    };
    assert!(matches!(
        db.checkpoint_with_options(options),
        Err(Error::BudgetExceeded {
            resource: "new file bytes",
            ..
        })
    ));
    assert_eq!(rows(&db), [(1, 10), (2, 20)]);
    let inventory = db.storage_inventory().unwrap();
    assert_eq!(
        inventory
            .entries
            .iter()
            .filter(|e| e.kind == StorageEntryKind::OrphanGeneration)
            .count(),
        1
    );
    assert!(inventory.reclaimable_file_bytes < options.max_new_file_bytes);
    assert_eq!(db.reclaim().unwrap().generations_removed, 1);
    assert_eq!(tree(&path.db()), original);
    let exact = MaintenanceOptions {
        max_new_file_bytes: estimate.new_file_bytes,
        max_rows: 2,
        max_record_bytes: 8,
        ..Default::default()
    };
    let checkpoint = db.checkpoint_with_options(exact).unwrap();
    assert_eq!(checkpoint.snapshot_bytes, estimate.snapshot_bytes);
    assert_eq!(db.stats().unwrap().commits, 1);
    drop(db);
    assert_eq!(
        rows(&Database::open_dir(path.db()).unwrap()),
        [(1, 10), (2, 20)]
    );
}

#[test]
fn record_limits_apply_before_publication_and_failed_stages_can_be_reclaimed_repeatedly() {
    let (path, db) = seed();
    let original = tree(&path.db());
    for _ in 0..40 {
        assert!(matches!(
            db.checkpoint_with_options(MaintenanceOptions {
                max_record_bytes: 7,
                ..Default::default()
            }),
            Err(Error::LimitExceeded { limit: 7 })
        ));
        assert_eq!(rows(&db), [(1, 10), (2, 20)]);
        db.reclaim().unwrap();
        assert_eq!(tree(&path.db()), original);
    }
    assert!(matches!(
        db.checkpoint_with_options(MaintenanceOptions {
            max_record_bytes: skrin::codec::MAX_RECORD_BYTES + 1,
            ..Default::default()
        }),
        Err(Error::InvalidOperation(_))
    ));
    assert_eq!(tree(&path.db()), original);
    db.write(|tx| tx.update(1, |_| Ok(Row(99)))).unwrap();
    db.checkpoint().unwrap();
}

#[test]
fn backup_budget_accounts_for_new_lock_and_never_mutates_source() {
    let (path, db) = seed();
    let original = tree(&path.db());
    let small = path.0.join("too-small");
    assert!(matches!(
        db.backup_to_with_options(
            &small,
            MaintenanceOptions {
                max_new_file_bytes: 199,
                ..Default::default()
            }
        ),
        Err(Error::BudgetExceeded { .. })
    ));
    assert!(!small.exists());
    assert_eq!(tree(&path.db()), original);
    let estimate = db
        .estimate_checkpoint(MaintenanceOptions::default())
        .unwrap();
    let backup = db
        .backup_to_with_options(
            path.0.join("backup"),
            MaintenanceOptions {
                max_new_file_bytes: estimate.new_file_bytes + 8,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        backup.storage_inventory().unwrap().observed_file_bytes,
        estimate.new_file_bytes + 8
    );
    assert_eq!(rows(&backup), rows(&db));
    assert_eq!(tree(&path.db()), original);
}

#[test]
fn migration_budget_failure_preserves_old_schema_and_can_be_retried() {
    let (path, db) = seed();
    let original_current = fs::read(path.db().join("CURRENT")).unwrap();
    let result = db.migrate_with_options::<NewRow>(
        "v2",
        MaintenanceOptions {
            max_record_bytes: 8,
            ..Default::default()
        },
        |_, r| Ok(NewRow(r.0 + 1)),
    );
    assert!(matches!(result, Err(Error::LimitExceeded { limit: 8 })));
    assert_eq!(
        fs::read(path.db().join("CURRENT")).unwrap(),
        original_current
    );
    let db = Database::<Row>::open_dir(path.db()).unwrap();
    assert_eq!(rows(&db), [(1, 10), (2, 20)]);
    db.reclaim().unwrap();
    let new = db
        .migrate_with_options::<NewRow>(
            "v2",
            MaintenanceOptions {
                max_rows: 2,
                max_new_file_bytes: 1024,
                max_record_bytes: 20,
                ..Default::default()
            },
            |_, r| Ok(NewRow(r.0 + 1)),
        )
        .unwrap();
    assert_eq!(new.read().unwrap().get(1).unwrap().0, 11);
    assert_eq!(new.generation_info().unwrap().unwrap().migrations.len(), 1);
    drop(new);
    assert!(matches!(
        Database::<Row>::open_dir(path.db()),
        Err(Error::SchemaMismatch { .. })
    ));
}

#[test]
fn checkpoint_policy_is_explicit_and_noops_do_not_consume_sequences() {
    let path = Temp::new();
    let db = Database::<Row>::create_dir(path.db()).unwrap();
    let policy = CheckpointPolicy {
        wal_bytes: None,
        commits: Some(2),
    };
    let opts = MaintenanceOptions::default();
    assert!(db.checkpoint_if_needed(policy, opts).unwrap().is_none());
    db.write(|tx| tx.insert(1, Row(1))).unwrap();
    assert!(db.checkpoint_if_needed(policy, opts).unwrap().is_none());
    db.write(|_| Ok(())).unwrap();
    db.write(|tx| {
        tx.insert(3, Row(3))?;
        assert!(tx.remove(3));
        Ok(())
    })
    .unwrap();
    assert!(db.checkpoint_if_needed(policy, opts).unwrap().is_none());
    db.write(|tx| {
        tx.put(1, Row(2));
        Ok(())
    })
    .unwrap();
    assert_eq!(
        db.checkpoint_if_needed(policy, opts)
            .unwrap()
            .unwrap()
            .sequence,
        2
    );
    let generation = db.generation_info().unwrap().unwrap().generation;
    assert!(db.checkpoint_if_needed(policy, opts).unwrap().is_none());
    assert_eq!(
        db.generation_info().unwrap().unwrap().generation,
        generation
    );
    db.write(|tx| {
        tx.put(1, Row(3));
        Ok(())
    })
    .unwrap();
    assert!(
        db.checkpoint_if_needed(
            CheckpointPolicy {
                wal_bytes: None,
                commits: None
            },
            opts
        )
        .unwrap()
        .is_none()
    );
    let bytes_policy = CheckpointPolicy {
        wal_bytes: Some(56),
        commits: None,
    };
    assert!(matches!(
        db.checkpoint_if_needed(
            bytes_policy,
            MaintenanceOptions {
                max_rows: 0,
                ..opts
            }
        ),
        Err(Error::BudgetExceeded { .. })
    ));
    // A failed separately invoked checkpoint does not undo or relabel the write.
    assert_eq!(db.stats().unwrap().commits, 3);
    assert_eq!(rows(&db), [(1, 3)]);
    assert_eq!(
        db.checkpoint_if_needed(bytes_policy, opts)
            .unwrap()
            .unwrap()
            .sequence,
        3
    );
}

fn crc(bytes: &[u8]) -> u32 {
    let mut n = !0u32;
    for &b in bytes {
        n ^= u32::from(b);
        for _ in 0..8 {
            n = (n >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(n & 1));
        }
    }
    !n
}
fn fix_crc(bytes: &mut [u8]) {
    let len = bytes.len() - 4;
    let n = crc(&bytes[..len]);
    bytes[len..].copy_from_slice(&n.to_le_bytes());
}

#[test]
fn inventory_and_reclaim_preserve_unknown_contents_symlinks_and_retention() {
    let (path, db) = seed();
    db.checkpoint().unwrap();
    db.checkpoint().unwrap(); // active3 / previous2
    let before = db.storage_inventory().unwrap();
    assert!(
        before
            .entries
            .iter()
            .any(|e| e.kind == StorageEntryKind::ObsoleteGeneration)
    );
    assert!(
        db.checkpoint_with_options(MaintenanceOptions {
            max_record_bytes: 0,
            ..Default::default()
        })
        .is_err()
    ); // orphan4
    let root = path.db();
    fs::write(root.join("notes"), b"keep me").unwrap();
    fs::create_dir(root.join("unrelated")).unwrap();
    fs::write(root.join("unrelated/data"), b"do not visit").unwrap();
    let bad = root.join("g0000000000000005");
    fs::create_dir(&bad).unwrap();
    fs::write(bad.join("OWNER"), b"untrusted").unwrap();
    std::os::unix::fs::symlink(
        root.join("g0000000000000003"),
        root.join("g0000000000000006"),
    )
    .unwrap();
    let mut temporary = fs::read(root.join("CURRENT")).unwrap();
    temporary[8..16].copy_from_slice(&7u64.to_le_bytes());
    fix_crc(&mut temporary);
    fs::write(root.join("CURRENT-0000000000000007.tmp"), &temporary).unwrap();
    temporary[8..16].copy_from_slice(&8u64.to_le_bytes());
    temporary[24..32].copy_from_slice(&999u64.to_le_bytes());
    fix_crc(&mut temporary);
    fs::write(root.join("CURRENT-0000000000000008.tmp"), &temporary).unwrap();
    fs::write(root.join("CURRENT-0000000000000009.tmp"), b"partial").unwrap();
    let capture = tree(&root);
    let inventory = db.storage_inventory().unwrap();
    assert_eq!(capture, tree(&root));
    assert_eq!(
        inventory.entries.iter().filter(|e| e.reclaimable).count(),
        3
    );
    assert_eq!(
        inventory
            .entries
            .iter()
            .find(|e| e.path == Path::new("g0000000000000006"))
            .unwrap()
            .file_bytes,
        0
    );
    let report = db.reclaim().unwrap();
    assert_eq!(report.generations_removed, 2);
    assert_eq!(report.temporary_manifests_removed, 1);
    assert_eq!(report.skipped, 4);
    assert_eq!(report.bytes_removed, inventory.reclaimable_file_bytes);
    let after = tree(&root);
    for (name, contents) in capture {
        let first = name.components().next().unwrap().as_os_str();
        if [
            "g0000000000000001",
            "g0000000000000004",
            "CURRENT-0000000000000007.tmp",
        ]
        .iter()
        .any(|n| first == *n)
        {
            assert!(!after.contains_key(&name));
        } else {
            assert_eq!(after.get(&name), Some(&contents), "preserved {name:?}");
        }
    }
    assert_eq!(rows(&db), [(1, 10), (2, 20)]);
    assert_eq!(db.reclaim().unwrap().generations_removed, 0);
    drop(db);
    assert_eq!(rows(&Database::open_dir(root).unwrap()), [(1, 10), (2, 20)]);
}

#[test]
fn changed_current_refuses_cleanup_without_repair_or_file_deletion() {
    let (path, db) = seed();
    db.checkpoint().unwrap();
    db.checkpoint().unwrap();
    let file = path.db().join("CURRENT");
    let original = fs::read(&file).unwrap();
    let mut corrupt = original.clone();
    corrupt[8] ^= 1;
    fs::write(&file, &corrupt).unwrap();
    let before = tree(&path.db());
    assert!(db.storage_inventory().is_err());
    assert!(db.reclaim().is_err());
    assert!(db.prune().is_err());
    assert_eq!(tree(&path.db()), before);
    fs::write(file, original).unwrap();
    db.reclaim().unwrap();
}

#[test]
fn managed_open_refuses_symlinked_metadata_generation_and_data_files() {
    for victim in [
        "LOCK",
        "CURRENT",
        "g0000000000000001/OWNER",
        "g0000000000000001/snapshot",
        "g0000000000000001/wal",
        "g0000000000000001",
    ] {
        let (path, db) = seed();
        drop(db);
        let original = path.db().join(victim);
        let saved = path.0.join("saved");
        fs::rename(&original, &saved).unwrap();
        std::os::unix::fs::symlink(&saved, &original).unwrap();
        assert!(
            Database::<Row>::open_dir(path.db()).is_err(),
            "victim {victim}"
        );
        assert!(
            fs::symlink_metadata(original)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
}

#[test]
fn memory_and_legacy_backends_refuse_directory_only_maintenance() {
    let path = Temp::new();
    for db in [
        Database::<Row>::in_memory(),
        Database::create(path.0.join("legacy")).unwrap(),
    ] {
        assert!(db.storage_inventory().is_err());
        assert!(db.reclaim().is_err());
        assert!(
            db.estimate_checkpoint(MaintenanceOptions::default())
                .is_err()
        );
        assert!(
            db.checkpoint_if_needed(CheckpointPolicy::default(), MaintenanceOptions::default())
                .is_err()
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn reserved_checkpoint_backup_and_migration_keep_data_and_conversion_identity() {
    let (path, db) = seed();
    let options = MaintenanceOptions {
        reserve_file_data: true,
        ..Default::default()
    };
    let estimate = db.estimate_checkpoint(options).unwrap();
    let cp = db.checkpoint_with_options(options).unwrap();
    assert_eq!(cp.snapshot_bytes, estimate.snapshot_bytes);
    let original = tree(&path.db());
    let backup = db
        .backup_to_with_options(path.0.join("reserved-backup"), options)
        .unwrap();
    assert_eq!(rows(&backup), rows(&db));
    assert_eq!(tree(&path.db()), original);
    drop(backup);
    assert_eq!(
        rows(&Database::<Row>::open_dir(path.0.join("reserved-backup")).unwrap()),
        [(1, 10), (2, 20)]
    );
    let conversions = std::cell::Cell::new(0);
    let new = db
        .migrate_with_options::<NewRow>("reserved-v2", options, |_, row| {
            conversions.set(conversions.get() + 1);
            Ok(NewRow(row.0 + 1))
        })
        .unwrap();
    assert_eq!(conversions.get(), 2);
    assert_eq!(new.read().unwrap().get(1).unwrap().0, 11);
    drop(new);
    assert!(matches!(
        Database::<Row>::open_dir(path.db()),
        Err(Error::SchemaMismatch { .. })
    ));
    let reopened = Database::<NewRow>::open_dir(path.db()).unwrap();
    assert_eq!(reopened.read().unwrap().get(2).unwrap().0, 21);
    assert_eq!(reopened.read().unwrap().sequence(), 1);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn unsupported_reservation_refuses_before_creating_a_destination() {
    let path = Temp::new();
    let db = Database::<Row>::in_memory();
    let result = db.backup_to_with_options(
        path.db(),
        MaintenanceOptions {
            reserve_file_data: true,
            ..Default::default()
        },
    );
    assert!(matches!(result, Err(Error::InvalidOperation(_))));
    assert!(!path.db().exists());
    assert_eq!(db.read().unwrap().len(), 0);
}
