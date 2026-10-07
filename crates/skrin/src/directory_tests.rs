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

#[test]
fn checkpoint_verification_keeps_only_one_decoded_row_alive_and_preserves_native_rows() {
    use std::cell::Cell;
    thread_local! {
        static LIVE: Cell<usize> = const { Cell::new(0) };
        static PEAK: Cell<usize> = const { Cell::new(0) };
    }
    struct Tracked {
        value: u64,
        decoded: bool,
    }
    impl Drop for Tracked {
        fn drop(&mut self) {
            if self.decoded {
                LIVE.with(|n| n.set(n.get() - 1));
            }
        }
    }
    impl Record for Tracked {
        const SCHEMA: Schema = Schema {
            table_id: 921,
            version: 1,
        };
        fn encode(&self, e: &mut Encoder) -> Result<()> {
            e.u64(self.value)
        }
        fn decode(d: &mut Decoder<'_>) -> Result<Self> {
            let value = d.u64()?;
            LIVE.with(|n| {
                n.set(n.get() + 1);
                PEAK.with(|p| p.set(p.get().max(n.get())));
            });
            Ok(Self {
                value,
                decoded: true,
            })
        }
    }
    let path = Temp::new();
    let db = Database::<Tracked>::create_dir(&path.0).unwrap();
    db.write(|tx| {
        for key in 0..2048 {
            tx.insert(
                key,
                Tracked {
                    value: key,
                    decoded: false,
                },
            )?;
        }
        Ok(())
    })
    .unwrap();
    let address = db.read().unwrap().get(42).unwrap() as *const Tracked as usize;
    db.checkpoint().unwrap();
    assert_eq!(LIVE.with(Cell::get), 0);
    assert_eq!(PEAK.with(Cell::get), 1);
    assert_eq!(
        db.read().unwrap().get(42).unwrap() as *const Tracked as usize,
        address
    );
    let backup_path = Temp::new();
    let backup = db.backup_to(&backup_path.0).unwrap();
    assert_eq!(LIVE.with(Cell::get), 2048);
    assert_eq!(PEAK.with(Cell::get), 2048);
    drop(backup);
    assert_eq!(LIVE.with(Cell::get), 0);
}

#[test]
fn reclaim_failures_are_restartable_and_preserve_active_and_previous_generations() {
    for cutoff in 0..100 {
        let (path, db) = seed();
        db.checkpoint().unwrap();
        db.checkpoint().unwrap();
        assert!(
            db.checkpoint_with_options(MaintenanceOptions {
                max_record_bytes: 0,
                ..Default::default()
            })
            .is_err()
        );
        faults::arm(cutoff);
        let result = db.reclaim();
        faults::clear();
        check(&db);
        db.reclaim().unwrap();
        let inventory = db.storage_inventory().unwrap();
        assert_eq!(inventory.reclaimable_file_bytes, 0);
        assert_eq!(
            inventory
                .entries
                .iter()
                .filter(|e| matches!(
                    e.kind,
                    StorageEntryKind::ActiveGeneration | StorageEntryKind::RetainedGeneration
                ))
                .count(),
            2
        );
        drop(db);
        check(&Database::<Item>::open_dir(&path.0).unwrap());
        if result.is_ok() {
            assert!(cutoff > 10);
            return;
        }
    }
    panic!("did not traverse every reclamation failure boundary");
}

#[test]
fn persistence_projection_preserves_acknowledged_writes_through_checkpoint_and_migration() {
    use crate::persistence_model as model;
    for migration in [false, true] {
        let (path, db) = seed();
        model::start(&path.0, false);
        // Observe a real acknowledged WAL synchronization, not only a static seed.
        db.write(|tx| tx.update(1, |_| Ok(Item(11)))).unwrap();
        model::boundary();
        if migration {
            let new = db
                .migrate::<NewItem>("projection-v2", |_, old| Ok(NewItem(old.0 + 100)))
                .unwrap();
            drop(new);
        } else {
            db.checkpoint().unwrap();
            drop(db);
        }
        let images = model::finish();
        assert!(images.len() > 20);
        assert!(images.iter().any(|image| image.early_manifest));
        for (cut, image) in images.iter().enumerate() {
            image.restore(&path.0);
            match Database::<Item>::open_dir(&path.0) {
                Ok(db) => {
                    assert_eq!(
                        db.stats().unwrap().commits,
                        if cut == 0 { 1 } else { 2 },
                        "cut {cut}"
                    );
                    assert_eq!(
                        db.read()
                            .unwrap()
                            .iter()
                            .map(|(k, r)| (k, r.0))
                            .collect::<Vec<_>>(),
                        [(1, if cut == 0 { 10 } else { 11 }), (2, 20)]
                    );
                }
                Err(Error::SchemaMismatch { .. }) if migration => {
                    let db = Database::<NewItem>::open_dir(&path.0).unwrap();
                    assert_eq!(db.stats().unwrap().commits, 2, "cut {cut}");
                    assert_eq!(
                        db.read()
                            .unwrap()
                            .iter()
                            .map(|(k, r)| (k, r.0))
                            .collect::<Vec<_>>(),
                        [(1, 111), (2, 120)]
                    );
                }
                Err(error) => panic!(
                    "migration={migration}, cut={cut}, early={}: {error}",
                    image.early_manifest
                ),
            }
        }
        // The successful final boundary must select the completed operation.
        images.last().unwrap().restore(&path.0);
        if migration {
            assert!(Database::<NewItem>::open_dir(&path.0).is_ok());
        } else {
            assert_eq!(
                Database::<Item>::open_dir(&path.0)
                    .unwrap()
                    .generation_info()
                    .unwrap()
                    .unwrap()
                    .generation,
                2
            );
        }
    }
}

#[test]
fn persistence_projection_detects_an_omitted_prepublication_parent_sync() {
    use crate::persistence_model as model;
    let (path, db) = seed();
    model::start(&path.0, true);
    db.checkpoint().unwrap();
    drop(db);
    let images = model::finish();
    let mut detected = 0;
    for image in &images {
        image.restore(&path.0);
        if Database::<Item>::open_dir(&path.0).is_err() {
            assert!(image.early_manifest);
            detected += 1;
        }
    }
    assert!(
        detected > 0,
        "negative control must detect a manifest selecting a nondurable generation name"
    );
}

#[test]
fn persistence_projection_cleanup_is_restartable_without_losing_retained_generations() {
    use crate::persistence_model as model;
    let (path, db) = seed();
    db.checkpoint().unwrap();
    db.checkpoint().unwrap();
    db.checkpoint().unwrap();
    model::start(&path.0, false);
    db.reclaim().unwrap();
    drop(db);
    let images = model::finish();
    assert!(images.len() > 10);
    for image in &images {
        image.restore(&path.0);
        let db = Database::<Item>::open_dir(&path.0).unwrap();
        check(&db);
        db.reclaim().unwrap();
        assert_eq!(
            db.storage_inventory()
                .unwrap()
                .entries
                .iter()
                .filter(|e| e.reclaimable)
                .count(),
            0
        );
        assert!(path.0.join(generation_name(3)).is_dir());
        assert!(path.0.join(generation_name(4)).is_dir());
    }
}

#[test]
fn full_projection_preserves_commit_and_publication_across_unsynced_survival() {
    use crate::persistence_model::{self as model, Omission};
    for migration in [false, true] {
        let (path, db) = seed();
        model::start_full(&path.0, Omission::None, 1);
        db.write(|tx| tx.update(1, |_| Ok(Item(11)))).unwrap();
        model::acknowledged(2);
        if migration {
            let new = db
                .migrate::<NewItem>("survival-v2", |_, old| Ok(NewItem(old.0 + 100)))
                .unwrap();
            model::published(new.generation_info().unwrap().unwrap().generation);
            drop(new);
        } else {
            let cp = db.checkpoint().unwrap();
            model::published(cp.generation);
            drop(db);
        }
        let images = model::finish();
        assert!(images.iter().any(|i| i.unsynced_file));
        assert!(images.iter().any(|i| i.partial_append));
        assert!(images.iter().any(|i| i.early_manifest));
        for (cut, image) in images.iter().enumerate() {
            image.restore(&path.0);
            match Database::<Item>::open_dir(&path.0) {
                Ok(db) => {
                    let sequence = db.stats().unwrap().commits;
                    assert!(
                        (image.acknowledged_sequence.unwrap()..=2).contains(&sequence),
                        "cut {cut}"
                    );
                    if let Some(generation) = image.published_generation {
                        assert!(!migration, "acknowledged migration reverted at cut {cut}");
                        assert_eq!(
                            db.generation_info().unwrap().unwrap().generation,
                            generation
                        );
                    }
                    let read = db.read().unwrap();
                    assert_eq!(read.get(1).unwrap().0, if sequence == 1 { 10 } else { 11 });
                    assert_eq!(read.get(2).unwrap().0, 20);
                }
                Err(Error::SchemaMismatch { .. }) if migration => {
                    let db = Database::<NewItem>::open_dir(&path.0).unwrap();
                    assert_eq!(db.stats().unwrap().commits, 2, "cut {cut}");
                    let read = db.read().unwrap();
                    assert_eq!(read.get(1).unwrap().0, 111);
                    assert_eq!(read.get(2).unwrap().0, 120);
                    if let Some(generation) = image.published_generation {
                        assert_eq!(
                            db.generation_info().unwrap().unwrap().generation,
                            generation
                        );
                    }
                }
                Err(error) => panic!(
                    "migration={migration} cut={cut} unsynced={} partial={}: {error}",
                    image.unsynced_file, image.partial_append
                ),
            }
        }
    }
}

#[test]
fn full_projection_negative_controls_detect_each_required_sync_guarantee() {
    use crate::persistence_model::{self as model, Omission};
    for omission in [
        Omission::SnapshotSync,
        Omission::NewWalSync,
        Omission::AppendWalSync,
        Omission::OwnerSync,
        Omission::ManifestSync,
        Omission::GenerationSync,
        Omission::PrepublicationParentSync,
        Omission::PublicationParentSync,
    ] {
        let (path, db) = seed();
        model::start_full(&path.0, omission, 1);
        db.write(|tx| tx.update(1, |_| Ok(Item(11)))).unwrap();
        model::acknowledged(2);
        let cp = db.checkpoint().unwrap();
        model::published(cp.generation);
        drop(db);
        let images = model::finish();
        let mut violation = false;
        for image in &images {
            image.restore(&path.0);
            match Database::<Item>::open_dir(&path.0) {
                Err(_) => {
                    violation = true;
                }
                Ok(db) => {
                    if db.stats().unwrap().commits < image.acknowledged_sequence.unwrap()
                        || image
                            .published_generation
                            .is_some_and(|g| db.generation_info().unwrap().unwrap().generation != g)
                    {
                        violation = true;
                    }
                }
            }
        }
        assert!(violation, "negative control must detect {omission:?}");
    }
}

#[test]
fn enospc_at_every_checkpoint_boundary_keeps_old_or_new_generation_usable() {
    let mut clean = 0;
    let mut uncertain = 0;
    for cutoff in 0..100 {
        let (path, db) = seed();
        let current = fs::read(path.0.join("CURRENT")).unwrap();
        faults::enospc_after(cutoff);
        let result = db.checkpoint();
        faults::clear();
        assert!(matches!(
            Database::<Item>::open_dir(&path.0),
            Err(Error::Busy)
        ));
        match result {
            Err(Error::Io(error)) => {
                clean += 1;
                assert_eq!(error.kind(), io::ErrorKind::StorageFull);
                check(&db);
                assert_eq!(fs::read(path.0.join("CURRENT")).unwrap(), current);
                db.reclaim().unwrap();
            }
            Err(Error::MaintenanceUncertain(error)) => {
                uncertain += 1;
                assert_eq!(error.kind(), io::ErrorKind::StorageFull);
                assert!(matches!(db.read(), Err(Error::Poisoned)));
            }
            Ok(_) => {
                assert!(clean > 20 && uncertain >= 3);
                return;
            }
            result => panic!("cutoff {cutoff}: {result:?}"),
        }
        drop(db);
        check(&Database::<Item>::open_dir(&path.0).unwrap());
    }
    panic!("did not cover every ENOSPC boundary");
}

#[test]
fn full_projection_cleanup_retains_active_previous_and_all_acknowledged_rows() {
    use crate::persistence_model::{self as model, Omission};
    let (path, db) = seed();
    for _ in 0..3 {
        db.checkpoint().unwrap();
    }
    model::start_full(&path.0, Omission::None, 1);
    db.reclaim().unwrap();
    drop(db);
    let images = model::finish();
    assert!(images.len() > 10);
    for image in images {
        image.restore(&path.0);
        let db = Database::<Item>::open_dir(&path.0).unwrap();
        check(&db);
        assert!(path.0.join(generation_name(3)).is_dir());
        assert!(path.0.join(generation_name(4)).is_dir());
        db.reclaim().unwrap();
        assert_eq!(
            db.storage_inventory()
                .unwrap()
                .entries
                .iter()
                .filter(|e| e.reclaimable)
                .count(),
            0
        );
    }
}

#[test]
fn enospc_migrations_preserve_old_current_or_select_only_the_complete_new_schema() {
    let mut clean = 0;
    let mut uncertain = 0;
    for cutoff in 0..100 {
        let (path, db) = seed();
        let current = fs::read(path.0.join("CURRENT")).unwrap();
        faults::enospc_after(cutoff);
        let result = db.migrate::<NewItem>("space-v2", |_, old| Ok(NewItem(old.0 + 100)));
        faults::clear();
        let done = match result {
            Err(Error::Io(error)) => {
                clean += 1;
                assert_eq!(error.kind(), io::ErrorKind::StorageFull);
                assert_eq!(fs::read(path.0.join("CURRENT")).unwrap(), current);
                false
            }
            Err(Error::MaintenanceUncertain(error)) => {
                uncertain += 1;
                assert_eq!(error.kind(), io::ErrorKind::StorageFull);
                false
            }
            Ok(db) => {
                drop(db);
                true
            }
            result => panic!(
                "unexpected migration result at cutoff {cutoff}: {}",
                result.err().unwrap()
            ),
        };
        match Database::<Item>::open_dir(&path.0) {
            Ok(db) => {
                assert!(!done);
                check(&db);
            }
            Err(Error::SchemaMismatch { .. }) => {
                let db = Database::<NewItem>::open_dir(&path.0).unwrap();
                let read = db.read().unwrap();
                assert_eq!(read.sequence(), 1);
                assert_eq!(read.get(1).unwrap().0, 110);
                assert_eq!(read.get(2).unwrap().0, 120);
            }
            Err(error) => panic!("cutoff {cutoff}: {error}"),
        }
        if done {
            assert!(clean > 20 && uncertain >= 3);
            return;
        }
    }
    panic!("did not cover every migration ENOSPC boundary");
}

#[cfg(target_os = "linux")]
#[test]
fn allocation_reserves_blocks_without_extending_logical_file_or_format() {
    use std::os::unix::fs::MetadataExt;
    let path = Temp::new();
    fs::create_dir(&path.0).unwrap();
    let file = File::create_new(path.0.join("reserved")).unwrap();
    crate::reservation::reserve(&file, 0, 256 * 1024).unwrap();
    let metadata = file.metadata().unwrap();
    assert_eq!(metadata.len(), 0);
    assert!(metadata.blocks() * 512 >= 256 * 1024);
}

#[cfg(target_os = "linux")]
#[test]
fn required_allocation_failure_never_falls_back_or_publishes() {
    let options = MaintenanceOptions {
        reserve_file_data: true,
        ..Default::default()
    };
    for kind in [io::ErrorKind::StorageFull, io::ErrorKind::Unsupported] {
        for allocation in 0..4 {
            let (path, db) = seed();
            let current = fs::read(path.0.join("CURRENT")).unwrap();
            crate::reservation::faults::arm(allocation, kind);
            let result = db.checkpoint_with_options(options);
            crate::reservation::faults::clear();
            match result {
                Err(Error::Io(error)) => assert_eq!(error.kind(), kind),
                result => panic!("allocation {allocation}: {result:?}"),
            }
            check(&db);
            assert_eq!(fs::read(path.0.join("CURRENT")).unwrap(), current);
            assert!(matches!(
                Database::<Item>::open_dir(&path.0),
                Err(Error::Busy)
            ));
            db.reclaim().unwrap();
            drop(db);
            check(&Database::<Item>::open_dir(&path.0).unwrap());
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn required_reservation_preserves_acknowledgments_in_real_recovery_images() {
    use crate::persistence_model::{self as model, Omission};
    let (path, db) = seed();
    let estimate = db
        .estimate_checkpoint(MaintenanceOptions::default())
        .unwrap();
    model::start_full(&path.0, Omission::None, 1);
    db.write(|tx| tx.update(1, |_| Ok(Item(11)))).unwrap();
    model::acknowledged(2);
    let cp = db
        .checkpoint_with_options(MaintenanceOptions {
            reserve_file_data: true,
            max_new_file_bytes: estimate.new_file_bytes,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(cp.snapshot_bytes, estimate.snapshot_bytes);
    model::published(cp.generation);
    drop(db);
    for image in model::finish() {
        image.restore(&path.0);
        let db = Database::<Item>::open_dir(&path.0).unwrap();
        let read = db.read().unwrap();
        let sequence = read.sequence();
        assert!((image.acknowledged_sequence.unwrap()..=2).contains(&sequence));
        assert_eq!(read.get(1).unwrap().0, if sequence == 1 { 10 } else { 11 });
        assert_eq!(read.get(2).unwrap().0, 20);
        if let Some(generation) = image.published_generation {
            assert_eq!(
                db.generation_info().unwrap().unwrap().generation,
                generation
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn reservation_refuses_nondeterministic_growth_and_shrink_before_publication() {
    use crate::{Decoder, Encoder};
    use std::cell::Cell;
    thread_local! { static LENGTHS: Cell<(usize,usize,usize)> = const { Cell::new((1,1,0)) }; }
    struct Changing;
    impl Record for Changing {
        const SCHEMA: Schema = Schema {
            table_id: 91234,
            version: 1,
        };
        fn encode(&self, encoder: &mut Encoder) -> Result<()> {
            let length = LENGTHS.with(|cell| {
                let (first, second, calls) = cell.get();
                cell.set((first, second, calls + 1));
                if calls == 0 { first } else { second }
            });
            encoder.bytes(&vec![1; length])
        }
        fn decode(decoder: &mut Decoder<'_>) -> Result<Self> {
            decoder.bytes()?;
            Ok(Self)
        }
    }
    for (first, second) in [(1, 2), (2, 1)] {
        let path = Temp::new();
        let db = Database::<Changing>::in_memory();
        db.write(|tx| tx.insert(1, Changing)).unwrap();
        LENGTHS.with(|cell| cell.set((first, second, 0)));
        let result = db.backup_to_with_options(
            &path.0,
            MaintenanceOptions {
                reserve_file_data: true,
                ..Default::default()
            },
        );
        assert!(matches!(result, Err(Error::Codec(_))));
        assert!(!path.0.join("CURRENT").exists());
        assert_eq!(db.read().unwrap().sequence(), 1);
        assert_eq!(db.read().unwrap().len(), 1);
    }
}
