use crate::catalog::{Catalog, CatalogDatabase, CatalogWrite, Stored};
use crate::log::{Wal, encode_transaction, file_header};
use crate::test_support::TestStorage;
use crate::versioned::{CatalogSnapshotWrite, SnapshotOptions, SnapshotWrite};
use crate::{Database, Decoder, Encoder, Error, Record, Result, Schema, WriteTransaction};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Value {
    number: u64,
    name: String,
}
impl Record for Value {
    const SCHEMA: Schema = Schema {
        table_id: 1,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        if self.number == 999 {
            return Err(Error::Codec("refused value".into()));
        }
        e.u64(self.number)?;
        e.string(&self.name)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            number: d.u64()?,
            name: d.string()?.into(),
        })
    }
}
crate::catalog! {
    Values, Row {
        schema:(18000,1),tables:{Entries:Value},
        indexes:{Number:Entries {id:1,version:1,unique:true,key:u64 => |r|r.number}}
    }
}
fn value(number: u64) -> Value {
    Value {
        number,
        name: format!("row-{number}"),
    }
}
fn options() -> SnapshotOptions {
    SnapshotOptions {
        max_snapshots: 4,
        max_pinned_bytes: 16 * 1024 * 1024,
    }
}

// These adapters call the four production writers; there is no test database.
trait Edits {
    fn get(&self, key: u64) -> Result<Option<&Value>>;
    fn edit(&mut self, key: u64, edit: impl FnOnce(&mut Value) -> Result<()>) -> Result<()>;
}
macro_rules! single_writer {
    ($ty:ident) => {
        impl Edits for $ty<'_, Value> {
            fn get(&self, key: u64) -> Result<Option<&Value>> {
                Ok(self.get(key))
            }
            fn edit(
                &mut self,
                key: u64,
                edit: impl FnOnce(&mut Value) -> Result<()>,
            ) -> Result<()> {
                self.edit(key, edit)
            }
        }
    };
}
single_writer!(WriteTransaction);
single_writer!(SnapshotWrite);
macro_rules! catalog_writer {
    ($ty:ident) => {
        impl Edits for $ty<'_, Values> {
            fn get(&self, key: u64) -> Result<Option<&Value>> {
                self.get::<Entries>(key)
            }
            fn edit(
                &mut self,
                key: u64,
                edit: impl FnOnce(&mut Value) -> Result<()>,
            ) -> Result<()> {
                self.edit::<Entries>(key, edit)
            }
        }
    };
}
catalog_writer!(CatalogWrite);
catalog_writer!(CatalogSnapshotWrite);
fn exercise(tx: &mut impl Edits) -> Result<()> {
    tx.edit(1, |row| {
        assert_eq!(*row, value(1));
        row.number = 101;
        row.name = "edited".into();
        Ok(())
    })?;
    assert!(matches!(
        tx.edit(1000, |_| panic!("missing row callback ran")),
        Err(Error::MissingKey(1000))
    ));
    assert!(
        matches!(tx.edit(1,|row| {row.number=77;row.name="failed".into();Err(Error::InvalidOperation("callback".into()))}),Err(Error::InvalidOperation(s)) if s=="callback")
    );
    assert_eq!(
        tx.get(1)?.unwrap(),
        &Value {
            number: 101,
            name: "edited".into()
        }
    );
    tx.edit(1, |row| {
        row.number += 1;
        Ok(())
    })?;
    assert_eq!(tx.get(1)?.unwrap().number, 102);
    assert_eq!(tx.get(2)?, Some(&value(2)));
    Ok(())
}
fn reject(tx: &mut impl Edits) -> Result<()> {
    tx.edit(1, |r| {
        r.number = 200;
        Ok(())
    })?;
    tx.edit(2, |r| {
        r.number = 201;
        Ok(())
    })?;
    Err(Error::InvalidOperation("whole closure".into()))
}
#[test]
fn single_edits_preserve_failed_statements_and_roll_back_whole_closures() -> Result<()> {
    let db = Database::<Value>::in_memory();
    db.write(|tx| {
        for id in 0..100 {
            tx.insert(id, value(id))?;
        }
        Ok(())
    })?;
    db.write(|tx| exercise(tx))?;
    assert!(
        matches!(db.write(|tx| reject(tx)),Err(Error::InvalidOperation(s)) if s=="whole closure")
    );
    assert_eq!(db.read()?.sequence(), 2);
    assert_eq!(
        db.read()?.get(1).unwrap(),
        &Value {
            number: 102,
            name: "edited".into()
        }
    );
    assert_eq!(db.read()?.get(2), Some(&value(2)));
    let db = db.into_snapshots(options(), |_| Ok(128))?;
    let old = db.snapshot()?;
    db.write(|tx| {
        tx.edit(1, |row| {
            row.number = 300;
            Ok(())
        })
    })?;
    assert_eq!(old.get(1)?.unwrap().number, 102);
    assert_eq!(db.snapshot()?.get(1)?.unwrap().number, 300);
    assert!(
        matches!(db.write(|tx| reject(tx)),Err(Error::InvalidOperation(s)) if s=="whole closure")
    );
    assert_eq!(db.snapshot()?.get(2)?, Some(&value(2)));
    // Run the full statement sequence against a freshly seeded snapshot writer too.
    let fresh = Database::<Value>::in_memory();
    fresh.write(|tx| {
        for id in 0..100 {
            tx.insert(id, value(id))?;
        }
        Ok(())
    })?;
    let fresh = fresh.into_snapshots(options(), |_| Ok(128))?;
    fresh.write(|tx| exercise(tx))?;
    assert_eq!(fresh.snapshot()?.get(1)?.unwrap().number, 102);
    Ok(())
}
#[test]
fn catalog_edits_keep_unique_indexes_and_retained_frames_atomic() -> Result<()> {
    let db = CatalogDatabase::<Values>::in_memory()?;
    db.write(|tx| {
        for id in 0..100 {
            tx.insert::<Entries>(id, value(id))?;
        }
        Ok(())
    })?;
    db.write(|tx| exercise(tx))?;
    assert!(
        matches!(db.write(|tx| reject(tx)),Err(Error::InvalidOperation(s)) if s=="whole closure")
    );
    assert_eq!(
        db.read()?
            .matching(Number, &102)?
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        [1]
    );
    let collision = |tx: &mut CatalogWrite<'_, Values>| {
        tx.edit::<Entries>(1, |r| {
            r.number = 3;
            Ok(())
        })?;
        tx.edit::<Entries>(2, |r| {
            r.number = 200;
            Ok(())
        })
    };
    assert!(matches!(
        db.write(collision),
        Err(Error::UniqueViolation { index_id: 1 })
    ));
    assert_eq!(db.read()?.sequence(), 2);
    assert_eq!(db.read()?.get::<Entries>(2)?, Some(&value(2)));
    let db = db.into_snapshots(options(), |_| Ok(128))?;
    let old = db.snapshot()?;
    db.write(|tx| {
        tx.edit::<Entries>(1, |r| {
            r.number = 300;
            Ok(())
        })
    })?;
    assert_eq!(
        old.matching(Number, &102)?
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        db.snapshot()?
            .matching(Number, &300)?
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        [1]
    );
    assert!(matches!(
        db.write(|tx| {
            tx.edit::<Entries>(1, |r| {
                r.number = 3;
                Ok(())
            })?;
            tx.edit::<Entries>(2, |r| {
                r.number = 200;
                Ok(())
            })
        }),
        Err(Error::UniqueViolation { index_id: 1 })
    ));
    assert!(
        matches!(db.write(|tx| reject(tx)),Err(Error::InvalidOperation(s)) if s=="whole closure")
    );
    assert_eq!(db.snapshot()?.sequence()?, 3);
    assert_eq!(db.snapshot()?.get::<Entries>(2)?, Some(&value(2)));
    let fresh = CatalogDatabase::<Values>::in_memory()?;
    fresh.write(|tx| {
        for id in 0..100 {
            tx.insert::<Entries>(id, value(id))?;
        }
        Ok(())
    })?;
    let fresh = fresh.into_snapshots(options(), |_| Ok(128))?;
    fresh.write(|tx| exercise(tx))?;
    assert_eq!(
        fresh
            .snapshot()?
            .matching(Number, &102)?
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        [1]
    );
    Ok(())
}
fn single_disk(disk: &TestStorage) -> Result<Database<Value>> {
    let (wal, recovered) = Wal::recover::<Value>(Box::new(disk.clone()))?;
    Ok(Database::from_recovered(wal, recovered))
}
fn catalog_disk(disk: &TestStorage) -> Result<CatalogDatabase<Values>> {
    let (wal, recovered) = Wal::recover::<Stored<Values>>(Box::new(disk.clone()))?;
    CatalogDatabase::wrap(Database::from_recovered(wal, recovered))
}
fn fail(disk: &TestStorage, cutoff: Option<usize>, full: bool) {
    match (cutoff, full) {
        (Some(n), false) => disk.fail_write_after(n),
        (Some(n), true) => disk.fail_enospc_after(n),
        (None, false) => disk.fail_sync(),
        (None, true) => disk.fail_sync_enospc(),
    }
}
#[test]
fn edited_single_rows_obey_real_wal_faults_in_both_read_modes() -> Result<()> {
    let disk = TestStorage::new(file_header(Value::SCHEMA));
    let db = single_disk(&disk)?;
    db.write(|tx| tx.insert(1, value(1)))?;
    drop(db);
    let before = disk.image();
    let length = encode_transaction(2, &BTreeMap::from([(1, Some(value(2)))]))?.len();
    for snapshot in [false, true] {
        for full in [false, true] {
            for cutoff in (0..length).map(Some).chain([None]) {
                let disk = TestStorage::new(before.clone());
                let db = single_disk(&disk)?;
                if snapshot {
                    let db = db.into_snapshots(options(), |_| Ok(128))?;
                    let old = db.snapshot()?;
                    fail(&disk, cutoff, full);
                    assert!(matches!(
                        db.write(|tx| tx.edit(1, |r| {
                            *r = value(2);
                            Ok(())
                        })),
                        Err(Error::CommitUncertain(_))
                    ));
                    assert!(matches!(old.get(1), Err(Error::Poisoned)));
                    assert!(matches!(db.snapshot(), Err(Error::Poisoned)));
                    drop(old);
                    drop(db);
                } else {
                    fail(&disk, cutoff, full);
                    assert!(matches!(
                        db.write(|tx| tx.edit(1, |r| {
                            *r = value(2);
                            Ok(())
                        })),
                        Err(Error::CommitUncertain(_))
                    ));
                    assert!(matches!(db.read(), Err(Error::Poisoned)));
                    drop(db);
                }
                disk.clear_faults();
                let recovered = single_disk(&disk)?;
                assert_eq!(
                    recovered.read()?.get(1),
                    Some(&value(if cutoff.is_none() { 2 } else { 1 }))
                );
                if cutoff.is_some() {
                    assert_eq!(disk.image(), before);
                }
            }
        }
    }
    Ok(())
}
#[test]
fn edited_catalog_rows_and_indexes_obey_real_wal_faults_in_both_read_modes() -> Result<()> {
    let mut initial = file_header(Values::SCHEMA);
    initial.extend(encode_transaction(
        1,
        &BTreeMap::from([(0, Some(Stored::<Values>::metadata()))]),
    )?);
    let disk = TestStorage::new(initial);
    let db = catalog_disk(&disk)?;
    db.write(|tx| tx.insert::<Entries>(1, value(1)))?;
    let before = disk.image();
    db.write(|tx| {
        tx.edit::<Entries>(1, |r| {
            *r = value(2);
            Ok(())
        })
    })?;
    let length = disk.image().len() - before.len();
    drop(db);
    for snapshot in [false, true] {
        for full in [false, true] {
            for cutoff in (0..length).map(Some).chain([None]) {
                let disk = TestStorage::new(before.clone());
                let db = catalog_disk(&disk)?;
                if snapshot {
                    let db = db.into_snapshots(options(), |_| Ok(128))?;
                    let old = db.snapshot()?;
                    fail(&disk, cutoff, full);
                    assert!(matches!(
                        db.write(|tx| tx.edit::<Entries>(1, |r| {
                            *r = value(2);
                            Ok(())
                        })),
                        Err(Error::CommitUncertain(_))
                    ));
                    assert!(matches!(old.matching(Number, &1), Err(Error::Poisoned)));
                    assert!(matches!(db.snapshot(), Err(Error::Poisoned)));
                    drop(old);
                    drop(db);
                } else {
                    fail(&disk, cutoff, full);
                    assert!(matches!(
                        db.write(|tx| tx.edit::<Entries>(1, |r| {
                            *r = value(2);
                            Ok(())
                        })),
                        Err(Error::CommitUncertain(_))
                    ));
                    assert!(matches!(db.read(), Err(Error::Poisoned)));
                    drop(db);
                }
                disk.clear_faults();
                let recovered = catalog_disk(&disk)?;
                let read = recovered.read()?;
                let expected = if cutoff.is_none() { 2 } else { 1 };
                assert_eq!(read.get::<Entries>(1)?, Some(&value(expected)));
                assert_eq!(
                    read.matching(Number, &expected)?
                        .map(|(id, _)| id)
                        .collect::<Vec<_>>(),
                    [1]
                );
                assert_eq!(read.matching(Number, &(3 - expected))?.count(), 0);
                if cutoff.is_some() {
                    assert_eq!(disk.image(), before);
                }
            }
        }
    }
    Ok(())
}
#[test]
fn edit_codec_errors_do_not_append_or_publish_and_leave_handles_usable() -> Result<()> {
    for snapshot in [false, true] {
        let disk = TestStorage::new(file_header(Value::SCHEMA));
        let db = single_disk(&disk)?;
        db.write(|tx| tx.insert(1, value(1)))?;
        let before = disk.image();
        let syncs = disk.syncs();
        if snapshot {
            let db = db.into_snapshots(options(), |_| Ok(128))?;
            assert!(matches!(
                db.write(|tx| tx.edit(1, |r| {
                    r.number = 999;
                    Ok(())
                })),
                Err(Error::Codec(_))
            ));
            assert_eq!(db.snapshot()?.get(1)?, Some(&value(1)));
            assert_eq!(disk.image(), before);
            assert_eq!(disk.syncs(), syncs);
            db.write(|tx| {
                tx.edit(1, |r| {
                    r.number = 2;
                    Ok(())
                })
            })?;
        } else {
            assert!(matches!(
                db.write(|tx| tx.edit(1, |r| {
                    r.number = 999;
                    Ok(())
                })),
                Err(Error::Codec(_))
            ));
            assert_eq!(db.read()?.get(1), Some(&value(1)));
            assert_eq!(disk.image(), before);
            assert_eq!(disk.syncs(), syncs);
            db.write(|tx| {
                tx.edit(1, |r| {
                    r.number = 2;
                    Ok(())
                })
            })?;
        }
    }
    Ok(())
}

#[test]
fn catalog_edit_codec_errors_leave_both_versions_and_bytes_unchanged() -> Result<()> {
    for snapshot in [false, true] {
        let mut initial = file_header(Values::SCHEMA);
        initial.extend(encode_transaction(
            1,
            &BTreeMap::from([(0, Some(Stored::<Values>::metadata()))]),
        )?);
        let disk = TestStorage::new(initial);
        let db = catalog_disk(&disk)?;
        db.write(|tx| tx.insert::<Entries>(1, value(1)))?;
        let before = disk.image();
        let syncs = disk.syncs();
        if snapshot {
            let db = db.into_snapshots(options(), |_| Ok(128))?;
            let old = db.snapshot()?;
            assert!(matches!(
                db.write(|tx| tx.edit::<Entries>(1, |r| {
                    r.number = 999;
                    Ok(())
                })),
                Err(Error::Codec(_))
            ));
            assert_eq!(old.get::<Entries>(1)?, Some(&value(1)));
            assert_eq!(
                db.snapshot()?
                    .matching(Number, &1)?
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>(),
                [1]
            );
            assert_eq!(disk.image(), before);
            assert_eq!(disk.syncs(), syncs);
            db.write(|tx| {
                tx.edit::<Entries>(1, |r| {
                    r.number = 2;
                    Ok(())
                })
            })?;
            assert_eq!(old.get::<Entries>(1)?, Some(&value(1)));
        } else {
            assert!(matches!(
                db.write(|tx| tx.edit::<Entries>(1, |r| {
                    r.number = 999;
                    Ok(())
                })),
                Err(Error::Codec(_))
            ));
            assert_eq!(
                db.read()?
                    .matching(Number, &1)?
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>(),
                [1]
            );
            assert_eq!(disk.image(), before);
            assert_eq!(disk.syncs(), syncs);
            db.write(|tx| {
                tx.edit::<Entries>(1, |r| {
                    r.number = 2;
                    Ok(())
                })
            })?;
        }
    }
    Ok(())
}

static CLONES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct Counted(u64);
impl Clone for Counted {
    fn clone(&self) -> Self {
        CLONES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(self.0)
    }
}
impl Record for Counted {
    const SCHEMA: Schema = Schema {
        table_id: 19000,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.u64(self.0)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(d.u64()?))
    }
}
crate::catalog! {
    Counts,CountRow {schema:(19001,1),tables:{CountEntries:Counted},indexes:{}}
}
#[test]
fn edit_clones_only_the_selected_row_once_in_all_four_writers() -> Result<()> {
    use std::sync::atomic::Ordering::Relaxed;
    macro_rules! seed {
        ($db:expr $(,$table:ty)?) => {$db.write(|tx| {
            for id in 0..1000 {tx.insert $(::<$table>)? (id,Counted(id))?;}Ok(())
        })?};
    }
    macro_rules! check {
        ($db:expr $(,$table:ty)?) => {{
            let before=CLONES.load(Relaxed);
            $db.write(|tx|tx.edit $(::<$table>)? (1,|r| {r.0+=1;Ok(())}))?;
            assert_eq!(CLONES.load(Relaxed),before+1);
            assert!(matches!($db.write(|tx|tx.edit $(::<$table>)? (1000,|_|panic!("missing callback"))),Err(Error::MissingKey(1000))));
            assert_eq!(CLONES.load(Relaxed),before+1);
            assert!(matches!($db.write(|tx|tx.edit $(::<$table>)? (1,|r| {assert_eq!(r.0,2);r.0=100;Err(Error::InvalidOperation("discard".into()))})),Err(Error::InvalidOperation(s)) if s=="discard"));
            assert_eq!(CLONES.load(Relaxed),before+2);
            $db.write(|tx|tx.edit $(::<$table>)? (1,|r| {assert_eq!(r.0,2);Ok(())}))?;
            assert_eq!(CLONES.load(Relaxed),before+3);
        }};
    }
    let db = Database::<Counted>::in_memory();
    seed!(db);
    check!(db);
    let db = Database::<Counted>::in_memory();
    seed!(db);
    let db = db.into_snapshots(options(), |_| Ok(8))?;
    check!(db);
    let db = CatalogDatabase::<Counts>::in_memory()?;
    seed!(db, CountEntries);
    check!(db, CountEntries);
    let db = CatalogDatabase::<Counts>::in_memory()?;
    seed!(db, CountEntries);
    let db = db.into_snapshots(options(), |_| Ok(8))?;
    check!(db, CountEntries);
    Ok(())
}
