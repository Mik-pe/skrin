use super::*;
use crate::log::file_header;
use crate::test_support::{Item, TestStorage};
use crate::{Decoder, Encoder, Schema};
use std::panic::{AssertUnwindSafe, catch_unwind};

fn reopen<R: Record>(storage: TestStorage) -> Result<Database<R>> {
    let (wal, recovered) = Wal::recover::<R>(Box::new(storage))?;
    Ok(Database::from_recovered(wal, recovered))
}

fn durable<R: Record>() -> (Database<R>, TestStorage) {
    let storage = TestStorage::new(file_header(R::SCHEMA));
    (reopen(storage.clone()).unwrap(), storage)
}

fn seed(db: &Database<Item>) {
    db.write(|tx| {
        tx.insert(1, Item(10))?;
        tx.insert(2, Item(20))
    })
    .unwrap();
}

fn change(db: &Database<Item>) -> Result<()> {
    db.write(|tx| {
        tx.put(1, Item(15));
        assert!(tx.remove(2));
        tx.insert(3, Item(15))
    })
}

fn rows(db: &Database<Item>) -> Vec<(u64, u64)> {
    db.read()
        .unwrap()
        .iter()
        .map(|(key, row)| (key, row.0))
        .collect()
}

#[test]
fn every_short_write_rolls_back_after_recovery_and_poisons_the_handle() {
    let (db, storage) = durable::<Item>();
    seed(&db);
    let baseline = storage.image();
    drop(db);
    let changes = BTreeMap::from([(1, Some(Item(15))), (2, None), (3, Some(Item(15)))]);
    let length = encode_transaction(2, &changes).unwrap().len();
    for cutoff in 0..length {
        let storage = TestStorage::new(baseline.clone());
        let db = reopen::<Item>(storage.clone()).unwrap();
        storage.fail_write_after(cutoff);
        assert!(matches!(change(&db), Err(Error::CommitUncertain(_))));
        assert!(matches!(db.read(), Err(Error::Poisoned)));
        assert!(matches!(db.begin_write(), Err(Error::Poisoned)));
        assert!(matches!(db.stats(), Err(Error::Poisoned)));
        drop(db);
        storage.clear_faults();
        let db = reopen::<Item>(storage.clone()).unwrap();
        assert_eq!(rows(&db), [(1, 10), (2, 20)], "cutoff {cutoff}");
        assert_eq!(storage.image(), baseline, "cutoff {cutoff}");
        change(&db).unwrap();
        assert_eq!(db.stats().unwrap().commits, 2);
        assert_eq!(rows(&db), [(1, 15), (3, 15)]);
    }
}

#[test]
fn sync_failure_has_an_uncertain_outcome_not_a_false_rollback_promise() {
    let (db, storage) = durable::<Item>();
    seed(&db);
    assert_eq!(storage.syncs(), 2); // Recovery/open and the first commit.
    storage.fail_sync();
    assert!(matches!(change(&db), Err(Error::CommitUncertain(_))));
    assert!(matches!(db.read(), Err(Error::Poisoned)));
    drop(db);
    storage.clear_faults();
    let db = reopen::<Item>(storage.clone()).unwrap();
    assert_eq!(rows(&db), [(1, 15), (3, 15)]);
    assert_eq!(db.stats().unwrap().commits, 2);
    assert_eq!(storage.syncs(), 3); // Complete uncertain frame synced on reopen.
}

#[test]
fn recovery_sync_failure_does_not_expose_an_unsynced_view() {
    let storage = TestStorage::new(file_header(Item::SCHEMA));
    storage.fail_sync();
    assert!(matches!(reopen::<Item>(storage.clone()), Err(Error::Io(_))));
    storage.clear_faults();
    assert!(reopen::<Item>(storage).is_ok());
}

#[test]
fn a_panicking_transaction_leaves_no_changes_and_requires_reopen() {
    let (db, storage) = durable::<Item>();
    seed(&db);
    let before = storage.image();
    let panic = catch_unwind(AssertUnwindSafe(|| {
        db.write::<()>(|tx| {
            tx.put(1, Item(999));
            tx.remove(2);
            panic!("application panic before commit")
        })
    }));
    assert!(panic.is_err());
    assert!(matches!(db.read(), Err(Error::Poisoned)));
    assert_eq!(storage.image(), before);
    drop(db);
    let db = reopen::<Item>(storage).unwrap();
    assert_eq!(rows(&db), [(1, 10), (2, 20)]);
}

struct Fallible(u64);

impl Record for Fallible {
    const SCHEMA: Schema = Schema {
        table_id: 8,
        version: 1,
    };

    fn encode(&self, encoder: &mut Encoder) -> Result<()> {
        if self.0 == 0 {
            return Err(Error::Codec("zero rejected by application".into()));
        }
        encoder.u64(self.0)
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(decoder.u64()?))
    }
}

#[test]
fn codec_error_happens_before_io_and_does_not_poison_the_handle() {
    let (db, storage) = durable::<Fallible>();
    let before = storage.image();
    let result = db.write(|tx| {
        tx.insert(1, Fallible(1))?;
        tx.insert(2, Fallible(0))
    });
    assert!(matches!(result, Err(Error::Codec(_))));
    assert_eq!(storage.image(), before);
    assert_eq!(db.stats().unwrap().commits, 0);
    assert!(db.read().unwrap().is_empty());
    db.write(|tx| tx.insert(1, Fallible(3))).unwrap();
    assert_eq!(db.read().unwrap().get(1).unwrap().0, 3);
}

#[test]
fn memory_mode_does_not_invoke_the_codec() {
    let db = Database::<Fallible>::in_memory();
    db.write(|tx| tx.insert(1, Fallible(0))).unwrap();
    assert_eq!(db.read().unwrap().get(1).unwrap().0, 0);
    assert!(!db.stats().unwrap().persistent);
}

struct Blob(Vec<u8>);

impl Record for Blob {
    const SCHEMA: Schema = Schema {
        table_id: 9,
        version: 1,
    };

    fn encode(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.bytes(&self.0)
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(decoder.bytes()?.to_vec()))
    }
}

#[test]
fn record_and_transaction_limits_are_checked_before_any_log_write() {
    let (db, storage) = durable::<Blob>();
    let before = storage.image();
    let too_big = vec![0; crate::codec::MAX_RECORD_BYTES];
    assert!(matches!(
        db.write(|tx| tx.insert(1, Blob(too_big))),
        Err(Error::LimitExceeded { .. })
    ));
    assert_eq!(storage.image(), before);
    let result = db.write(|tx| {
        let maximum = crate::codec::MAX_RECORD_BYTES - 4;
        tx.insert(1, Blob(vec![0; maximum]))?;
        tx.insert(2, Blob(vec![0; maximum]))
    });
    assert!(matches!(result, Err(Error::LimitExceeded { .. })));
    assert_eq!(storage.image(), before);
    assert!(db.read().unwrap().is_empty());
    db.write(|tx| tx.insert(1, Blob(vec![1, 2, 3]))).unwrap();
}

#[test]
fn sequence_exhaustion_is_a_clean_error_before_io() {
    let (db, storage) = durable::<Item>();
    db.state.write().unwrap().sequence = u64::MAX;
    let before = storage.image();
    assert!(matches!(
        db.write(|tx| tx.insert(1, Item(2))),
        Err(Error::SequenceExhausted)
    ));
    assert_eq!(storage.image(), before);
    assert!(db.read().unwrap().is_empty());
}

#[test]
fn no_op_and_rollback_do_not_append_or_sync() {
    let (db, storage) = durable::<Item>();
    seed(&db);
    let before = storage.image();
    let syncs = storage.syncs();
    db.write(|tx| {
        tx.insert(9, Item(9))?;
        assert!(tx.remove(9));
        assert!(!tx.remove(99));
        Ok(())
    })
    .unwrap();
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(1, Item(77));
        tx.rollback();
    }
    {
        let mut tx = db.begin_write().unwrap();
        tx.put(2, Item(88));
    }
    let result = db.write(|tx| {
        tx.put(1, Item(55));
        tx.insert(2, Item(44))
    });
    assert!(matches!(result, Err(Error::DuplicateKey(2))));
    assert_eq!(storage.image(), before);
    assert_eq!(storage.syncs(), syncs);
    assert_eq!(rows(&db), [(1, 10), (2, 20)]);
}

#[test]
fn deterministic_transactions_match_an_independent_reference_model() {
    let (mut db, storage) = durable::<Item>();
    let mut model = vec![None; 64];
    let mut random = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        random
    };
    for round in 0..600 {
        let mut expected = model.clone();
        let mut tx = db.begin_write().unwrap();
        for _ in 0..1 + next() % 12 {
            let key = (next() % 64) as usize;
            let value = next();
            match next() % 3 {
                0 => {
                    tx.put(key as u64, Item(value));
                    expected[key] = Some(value);
                }
                1 => {
                    assert_eq!(tx.remove(key as u64), expected[key].is_some());
                    expected[key] = None;
                }
                _ => {
                    let result = tx.insert(key as u64, Item(value));
                    if expected[key].is_some() {
                        assert!(matches!(result, Err(Error::DuplicateKey(_))));
                    } else {
                        result.unwrap();
                        expected[key] = Some(value);
                    }
                }
            }
            assert_eq!(tx.get(key as u64).map(|row| row.0), expected[key]);
        }
        if round % 5 == 0 {
            tx.rollback();
        } else {
            tx.commit().unwrap();
            model = expected;
        }
        if round % 17 == 0 {
            drop(db);
            db = reopen(storage.clone()).unwrap();
        }
        let actual = rows(&db);
        let expected: Vec<_> = model
            .iter()
            .enumerate()
            .filter_map(|(key, value)| value.map(|value| (key as u64, value)))
            .collect();
        assert_eq!(actual, expected, "round {round}");
    }
}
