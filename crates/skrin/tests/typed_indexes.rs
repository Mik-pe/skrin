use skrin::catalog::{Catalog, CatalogDatabase, Index, IndexDefinition, IndexKey};
use skrin::versioned::SnapshotOptions;
use skrin::{Decoder, Encoder, Error, Record, Result, Schema};
use std::ops::Bound::{Excluded, Included, Unbounded};
use std::ops::RangeBounds;

#[derive(Debug)]
struct Value {
    small: u8,
    count: u32,
    score: u64,
    name: String,
    bytes: Vec<u8>,
}
impl Record for Value {
    const SCHEMA: Schema = Schema {
        table_id: 1,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.u8(self.small)?;
        e.u32(self.count)?;
        e.u64(self.score)?;
        e.string(&self.name)?;
        e.bytes(&self.bytes)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            small: d.u8()?,
            count: d.u32()?,
            score: d.u64()?,
            name: d.string()?.into(),
            bytes: d.bytes()?.into(),
        })
    }
}
skrin::catalog! {
    Values, Row {
        schema: (80, 1),
        tables: { Entries: Value },
        indexes: {
            Small: Entries { id: 1, version: 1, unique: false, key: u8 => |r| r.small },
            Count: Entries { id: 2, version: 1, unique: false, key: u32 => |r| r.count },
            Score: Entries { id: 3, version: 1, unique: false, key: u64 => |r| r.score },
            Name: Entries { id: 4, version: 1, unique: false, key: str => |r| &r.name },
            Bytes: Entries { id: 5, version: 1, unique: false, key: [u8] => |r| &r.bytes }
        }
    }
}
fn options() -> SnapshotOptions {
    SnapshotOptions {
        max_snapshots: 4,
        max_pinned_bytes: 64 * 1024 * 1024,
    }
}
fn footprint(_: &Row) -> Result<u64> {
    Ok(256)
}
fn seeded() -> Result<CatalogDatabase<Values>> {
    let db = CatalogDatabase::in_memory()?;
    db.write(|tx| {
        for (id, score, name, bytes) in [
            (0, u64::MAX, "å", vec![255]),
            (5, 256, "z", vec![1, 0]),
            (9, 1, "Ada", vec![0]),
            (2, 0, "", vec![]),
            (u64::MAX, 1, "Ada", vec![0]),
        ] {
            tx.insert::<Entries>(
                id,
                Value {
                    small: score.min(u8::MAX as u64) as u8,
                    count: score.min(u32::MAX as u64) as u32,
                    score,
                    name: name.into(),
                    bytes,
                },
            )?;
        }
        Ok(())
    })?;
    Ok(db)
}

#[test]
fn cursor_seek_matches_independent_order_for_all_bounds_and_missing_positions() -> Result<()> {
    let db = seeded()?;
    let expected = [(0, 2), (1, 9), (1, u64::MAX), (256, 5), (u64::MAX, 0)];
    let ends = [
        Unbounded,
        Included(0),
        Excluded(0),
        Included(1),
        Excluded(1),
        Included(256),
        Excluded(256),
        Included(u64::MAX),
        Excluded(u64::MAX),
    ];
    macro_rules! verify {
        ($view:expr) => {{
            let view = $view;
            for lower in ends {
                for upper in ends {
                    let bounds = (lower, upper);
                    let invalid = match (lower, upper) {
                        (Included(a) | Excluded(a), Included(b) | Excluded(b)) => {
                            a > b
                                || (a == b && matches!((lower, upper), (Excluded(_), Excluded(_))))
                        }
                        _ => false,
                    };
                    for score in [0, 1, 2, 255, 256, 257, u64::MAX] {
                        for id in [0, 2, 5, 9, 10, u64::MAX] {
                            let found = view.query_after(Score, bounds, (&score, id));
                            if invalid {
                                assert!(matches!(found, Err(Error::InvalidOperation(_))));
                            } else {
                                assert_eq!(
                                    found?.map(|(id, r)| (r.score, id)).collect::<Vec<_>>(),
                                    expected
                                        .into_iter()
                                        .filter(|position| {
                                            bounds.contains(&position.0) && *position > (score, id)
                                        })
                                        .collect::<Vec<_>>(),
                                    "{bounds:?}, cursor=({score}, {id})"
                                );
                            }
                        }
                    }
                }
            }
        }};
    }
    verify!(db.read()?);
    let db = db.into_snapshots(options(), footprint)?;
    verify!(db.snapshot()?);
    Ok(())
}

#[test]
fn native_numeric_bounds_match_independent_order_in_both_read_views() -> Result<()> {
    let db = seeded()?;
    let expected = [(0, 2), (1, 9), (1, u64::MAX), (256, 5), (u64::MAX, 0)];
    let ends = [
        Unbounded,
        Included(0),
        Excluded(0),
        Included(1),
        Excluded(1),
        Included(256),
        Excluded(256),
        Included(u64::MAX),
        Excluded(u64::MAX),
    ];
    let read = db.read()?;
    for lower in ends {
        for upper in ends {
            let bounds = (lower, upper);
            let invalid = match (lower, upper) {
                (Included(a) | Excluded(a), Included(b) | Excluded(b)) => {
                    a > b || (a == b && matches!((lower, upper), (Excluded(_), Excluded(_))))
                }
                _ => false,
            };
            let result = read.query(Score, bounds);
            if invalid {
                assert!(matches!(result, Err(Error::InvalidOperation(_))));
            } else {
                assert_eq!(
                    result?.map(|(id, r)| (r.score, id)).collect::<Vec<_>>(),
                    expected
                        .into_iter()
                        .filter(|(score, _)| bounds.contains(score))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
    assert_eq!(read.query(Small, 0..=u8::MAX)?.count(), 5);
    assert_eq!(
        read.matching(Small, &u8::MAX)?
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        [0, 5]
    );
    assert_eq!(read.matching(Count, &u32::MAX)?.next().unwrap().0, 0);
    drop(read);
    let db = db.into_snapshots(options(), footprint)?;
    let frame = db.snapshot()?;
    for lower in ends {
        for upper in ends {
            let bounds = (lower, upper);
            let invalid = match (lower, upper) {
                (Included(a) | Excluded(a), Included(b) | Excluded(b)) => {
                    a > b || (a == b && matches!((lower, upper), (Excluded(_), Excluded(_))))
                }
                _ => false,
            };
            let result = frame.query(Score, bounds);
            if invalid {
                assert!(matches!(result, Err(Error::InvalidOperation(_))));
            } else {
                assert_eq!(
                    result?.map(|(id, r)| (r.score, id)).collect::<Vec<_>>(),
                    expected
                        .into_iter()
                        .filter(|(score, _)| bounds.contains(score))
                        .collect::<Vec<_>>()
                );
            }
        }
    }
    assert_eq!(frame.query(Score, ..)?.count(), 5);
    assert_eq!(frame.query(Score, u64::MAX..)?.next().unwrap().0, 0);
    assert_eq!(
        frame
            .matching(Score, &1)?
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        [9, u64::MAX]
    );
    Ok(())
}

#[test]
fn borrowed_strings_and_bytes_share_projection_encoding_and_retained_order() -> Result<()> {
    assert_eq!(u64::MAX.encode_key()?, [255; 8]);
    assert_eq!(256_u32.encode_key()?, [0, 0, 1, 0]);
    assert_eq!(255_u8.encode_key()?, [255]);
    assert_eq!("å".encode_key()?, [0xc3, 0xa5]);
    let db = seeded()?;
    {
        let read = db.read()?;
        assert_eq!(read.query(Name, ..)?.count(), 5);
        let oversized = vec![b'a'; skrin::codec::MAX_RECORD_BYTES + 1];
        assert!(matches!(
            read.matching(Name, std::str::from_utf8(&oversized).unwrap()),
            Err(Error::LimitExceeded { .. })
        ));
        assert!(matches!(
            read.query(Bytes, (Included(oversized.as_slice()), Unbounded)),
            Err(Error::LimitExceeded { .. })
        ));

        assert_eq!(
            read.matching(Name, "Ada")?
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            [9, u64::MAX]
        );
        assert_eq!(
            read.query(Name, (Included("Ada"), Excluded("å")))?
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            [9, u64::MAX, 5]
        );
        assert_eq!(read.matching(Bytes, &[0])?.count(), 2);
        assert_eq!(
            read.query(Bytes, (Excluded(&[][..]), Included(&[255][..])))?
                .count(),
            4
        );
    }
    let db = db.into_snapshots(options(), footprint)?;
    let old = db.snapshot()?;
    db.write(|tx| {
        tx.put::<Entries>(
            9,
            Value {
                small: 0,
                count: 0,
                score: 0,
                name: "new".into(),
                bytes: vec![],
            },
        )
    })?;
    let current = db.snapshot()?;
    assert_eq!(old.matching(Name, "Ada")?.count(), 2);
    assert_eq!(current.matching(Name, "Ada")?.count(), 1);
    assert_eq!(
        current
            .matching(Bytes, &[])?
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        [2, 9]
    );
    assert_eq!(
        old.query(Name, (Included("Ada"), Excluded("å")))?
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        [9, u64::MAX, 5]
    );
    assert_eq!(
        old.query(Bytes, (Excluded(&[][..]), Included(&[255][..])))?
            .count(),
        4
    );
    Ok(())
}

#[test]
fn queries_borrow_the_view_without_retaining_input_keys_or_bound_types() -> Result<()> {
    let db = seeded()?;
    let read = db.read()?;
    let found = read.matching(Name, &String::from("Ada"))?;
    assert_eq!(found.count(), 2);
    let resumed = read.query_after(Name, .., (String::from("Ada").as_str(), 9))?;
    assert_eq!(
        resumed.map(|(id, _)| id).collect::<Vec<_>>(),
        [u64::MAX, 5, 0]
    );
    let resumed = read.query_after(Bytes, .., (vec![0].as_slice(), 9))?;
    assert_eq!(
        resumed.map(|(id, _)| id).collect::<Vec<_>>(),
        [u64::MAX, 5, 0]
    );
    let bounded = read.query(
        Name,
        (Included(String::from("Ada").as_str()), Excluded("å")),
    )?;
    assert_eq!(bounded.count(), 3);
    // The original owned byte bounds can also be dropped before traversal.
    let lower = vec![0];
    let raw = read.index_scan::<Entries>(5, lower.clone()..)?;
    drop(lower);
    assert_eq!(raw.count(), 4);
    drop(read);
    let db = db.into_snapshots(options(), footprint)?;
    let frame = db.snapshot()?;
    let found = frame.matching(Name, &String::from("Ada"))?;
    assert_eq!(found.count(), 2);
    let resumed = frame.query_after(Name, .., (String::from("Ada").as_str(), 9))?;
    assert_eq!(
        resumed.map(|(id, _)| id).collect::<Vec<_>>(),
        [u64::MAX, 5, 0]
    );
    let resumed = frame.query_after(Bytes, .., (vec![0].as_slice(), 9))?;
    assert_eq!(
        resumed.map(|(id, _)| id).collect::<Vec<_>>(),
        [u64::MAX, 5, 0]
    );
    let bounded = frame.query(
        Name,
        (Included(String::from("Ada").as_str()), Excluded("å")),
    )?;
    assert_eq!(bounded.count(), 3);
    Ok(())
}

// A marker can be handwritten, but it must match every persisted definition field.
struct Mismatched<const FIELD: u8>;
impl<const FIELD: u8> Index<Values> for Mismatched<FIELD> {
    type Table = Entries;
    type Key = u64;
    const DEFINITION: IndexDefinition = IndexDefinition {
        id: if FIELD == 0 { 99 } else { 3 },
        table_id: if FIELD == 1 { 99 } else { 1 },
        version: if FIELD == 2 { 2 } else { 1 },
        unique: FIELD == 3,
    };
    fn project(r: &Value) -> Result<Vec<u8>> {
        Score::project(r)
    }
}
#[test]
fn invalid_bounds_and_definitions_are_refused_on_empty_indexes() -> Result<()> {
    let db = CatalogDatabase::<Values>::in_memory()?;
    let read = db.read()?;
    assert!(matches!(
        read.query(Score, (Included(2), Included(1))),
        Err(Error::InvalidOperation(_))
    ));
    assert!(matches!(
        read.query(Score, (Excluded(1), Excluded(1))),
        Err(Error::InvalidOperation(_))
    ));
    assert_eq!(read.query(Score, ..)?.count(), 0);
    macro_rules! reject {
        ($view:ident) => {
            assert!(matches!(
                $view.query(Mismatched::<0>, ..),
                Err(Error::InvalidOperation(_))
            ));
            assert!(matches!(
                $view.query(Mismatched::<1>, ..),
                Err(Error::InvalidOperation(_))
            ));
            assert!(matches!(
                $view.matching(Mismatched::<2>, &0),
                Err(Error::InvalidOperation(_))
            ));
            assert!(matches!(
                $view.matching(Mismatched::<3>, &0),
                Err(Error::InvalidOperation(_))
            ));
            assert!(matches!(
                $view.query_after(Mismatched::<2>, .., (&0, u64::MAX)),
                Err(Error::InvalidOperation(_))
            ));
            assert_eq!(
                $view.query_after(Score, .., (&u64::MAX, u64::MAX))?.count(),
                0
            );
        };
    }
    reject!(read);
    drop(read);
    let db = db.into_snapshots(options(), footprint)?;
    let frame = db.snapshot()?;
    assert!(matches!(
        frame.query(Score, (Included(2), Included(1))),
        Err(Error::InvalidOperation(_))
    ));
    assert!(matches!(
        frame.query(Score, (Excluded(1), Excluded(1))),
        Err(Error::InvalidOperation(_))
    ));
    reject!(frame);
    Ok(())
}

// Custom keys exercise fallible projections and bounds through the real catalog.
struct Fallible(u64);
impl IndexKey for Fallible {
    fn encode_key(&self) -> Result<Vec<u8>> {
        match self.0 {
            2 => Err(Error::Codec("refused key".into())),
            3 => Ok(vec![0; skrin::codec::MAX_RECORD_BYTES + 1]),
            n => n.encode_key(),
        }
    }
}
skrin::catalog! {
    Custom, CustomRow {
        schema: (81, 1), tables: { CustomEntries: Value },
        indexes: { CustomScore: CustomEntries {
            id: 1, version: 1, unique: false, key: Fallible => |r| fallible_projection(r)?
        } }
    }
}
fn fallible_projection(r: &Value) -> Result<Fallible> {
    if r.score == 4 {
        Err(Error::Codec("refused projection".into()))
    } else {
        Ok(Fallible(r.score))
    }
}
#[test]
fn custom_codec_errors_and_size_limits_propagate_without_changes() -> Result<()> {
    let db = CatalogDatabase::<Custom>::in_memory()?;
    let read = db.read()?;
    assert!(matches!(
        read.matching(CustomScore, &Fallible(2)),
        Err(Error::Codec(_))
    ));
    assert!(matches!(
        read.query(CustomScore, Fallible(2)..),
        Err(Error::Codec(_))
    ));
    assert!(matches!(
        read.matching(CustomScore, &Fallible(3)),
        Err(Error::LimitExceeded { .. })
    ));
    assert!(matches!(
        read.query(CustomScore, ..=Fallible(3)),
        Err(Error::LimitExceeded { .. })
    ));
    assert!(matches!(
        read.query_after(CustomScore, .., (&Fallible(2), 0)),
        Err(Error::Codec(_))
    ));
    assert!(matches!(
        read.query_after(CustomScore, .., (&Fallible(3), 0)),
        Err(Error::LimitExceeded { .. })
    ));
    drop(read);
    let db = db.into_snapshots(options(), footprint_custom)?;
    let frame = db.snapshot()?;
    assert!(matches!(
        frame.matching(CustomScore, &Fallible(2)),
        Err(Error::Codec(_))
    ));
    assert!(matches!(
        frame.query(CustomScore, ..=Fallible(3)),
        Err(Error::LimitExceeded { .. })
    ));
    assert!(matches!(
        frame.query_after(CustomScore, .., (&Fallible(2), 0)),
        Err(Error::Codec(_))
    ));
    assert!(matches!(
        frame.query_after(CustomScore, .., (&Fallible(3), 0)),
        Err(Error::LimitExceeded { .. })
    ));
    assert!(matches!(
        db.write(|tx| tx.insert::<CustomEntries>(
            1,
            Value {
                small: 0,
                count: 0,
                score: 2,
                name: String::new(),
                bytes: vec![]
            }
        )),
        Err(Error::Codec(_))
    ));
    for score in [3, 4] {
        let result = db.write(|tx| {
            tx.insert::<CustomEntries>(
                1,
                Value {
                    small: 0,
                    count: 0,
                    score,
                    name: String::new(),
                    bytes: vec![],
                },
            )
        });
        if score == 3 {
            assert!(matches!(result, Err(Error::LimitExceeded { .. })));
        } else {
            assert!(matches!(result, Err(Error::Codec(_))));
        }
    }
    assert_eq!(db.snapshot()?.sequence()?, 0);
    assert_eq!(Custom::INDEXES[0], CustomScore::DEFINITION);
    Ok(())
}
fn footprint_custom(_: &CustomRow) -> Result<u64> {
    Ok(256)
}

skrin::catalog! {
    Pairs, PairRow {
        schema: (82, 1), tables: { PairEntries: Value },
        indexes: {
            CountScore: PairEntries { id: 1, version: 1, unique: false, key: (u32,u64) => |r| (r.count,r.score) },
            CountSmall: PairEntries { id: 2, version: 1, unique: true, key: (u32,u8) => |r| (r.count,r.small) }
        }
    }
}
fn pair_value(count: u32, score: u64, small: u8) -> Value {
    Value {
        count,
        score,
        small,
        name: format!("{count}:{score}:{small}"),
        bytes: vec![small, 0, 255],
    }
}
fn seed_pairs(db: &CatalogDatabase<Pairs>) -> Result<()> {
    db.write(|tx| {
        for (id, count, score, small) in [
            (0, 0, u64::MAX, 0),
            (2, 1, 0, 0),
            (9, 1, 1, 1),
            (u64::MAX, 1, 1, 2),
            (5, u32::MAX, 0, 0),
        ] {
            tx.insert::<PairEntries>(id, pair_value(count, score, small))?;
        }
        Ok(())
    })
}
macro_rules! verify_pairs {
    ($view:expr) => {{
        let view = $view;
        let expected = [
            ((0, u64::MAX), 0),
            ((1, 0), 2),
            ((1, 1), 9),
            ((1, 1), u64::MAX),
            ((u32::MAX, 0), 5),
        ];
        assert_eq!(
            view.query(CountScore, ..)?
                .map(|(id, r)| ((r.count, r.score), id))
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            view.matching(CountScore, &(1, 1))?
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            [9, u64::MAX]
        );
        // Native tuple bounds and cursors, including absent positions and maximum IDs.
        for lower in [
            Unbounded,
            Included((0, 0)),
            Included((1, 0)),
            Excluded((1, 1)),
        ] {
            for upper in [Unbounded, Included((1, 1)), Excluded((u32::MAX, 0))] {
                let bounds = (lower, upper);
                for cursor in [
                    ((0, 0), 0),
                    ((1, 0), 2),
                    ((1, 1), 9),
                    ((1, 1), 10),
                    ((1, 1), u64::MAX),
                    ((u32::MAX, u64::MAX), u64::MAX),
                ] {
                    assert_eq!(
                        view.query_after(CountScore, bounds, (&cursor.0, cursor.1))?
                            .map(|(id, r)| ((r.count, r.score), id))
                            .collect::<Vec<_>>(),
                        expected
                            .into_iter()
                            .filter(|p| bounds.contains(&p.0) && *p > cursor)
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
        assert!(matches!(
            view.query(CountScore, (Included((2, 0)), Included((1, 0)))),
            Err(Error::InvalidOperation(_))
        ));
        let scan = {
            let temporary = (1, 1);
            view.query_after(CountScore, temporary..=temporary, (&temporary, 9))?
        };
        assert_eq!(scan.map(|(id, _)| id).collect::<Vec<_>>(), [u64::MAX]);
    }};
}
#[test]
fn compound_queries_constraints_and_old_frames_share_one_version() -> Result<()> {
    let db = CatalogDatabase::<Pairs>::in_memory()?;
    seed_pairs(&db)?;
    verify_pairs!(db.read()?);
    // Replacing one row and colliding with another unique compound key must roll back both indexes.
    assert!(matches!(
        db.write(|tx| {
            tx.put::<PairEntries>(0, pair_value(7, 8, 9))?;
            tx.insert::<PairEntries>(77, pair_value(1, 9, 1))
        }),
        Err(Error::UniqueViolation { index_id: 2 })
    ));
    assert_eq!(db.read()?.sequence(), 1);
    verify_pairs!(db.read()?);
    let db = db.into_snapshots(options(), |_| Ok(256))?;
    let old = db.snapshot()?;
    verify_pairs!(&old);
    db.write(|tx| {
        tx.remove::<PairEntries>(9)?;
        tx.put::<PairEntries>(2, pair_value(3, 4, 5))
    })?;
    verify_pairs!(&old);
    let current = db.snapshot()?;
    assert_eq!(
        current
            .query(CountScore, ..)?
            .map(|(id, r)| ((r.count, r.score), id))
            .collect::<Vec<_>>(),
        [
            ((0, u64::MAX), 0),
            ((1, 1), u64::MAX),
            ((3, 4), 2),
            ((u32::MAX, 0), 5)
        ]
    );
    assert_eq!(current.matching(CountSmall, &(1, 1))?.count(), 0);
    assert_eq!(
        current
            .matching(CountSmall, &(3, 5))?
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        [2]
    );
    assert_eq!(old.sequence()?, 1);
    assert_eq!(current.sequence()?, 2);
    Ok(())
}
#[cfg(unix)]
#[test]
fn compound_indexes_rebuild_from_wal_checkpoint_and_independent_backup() -> Result<()> {
    let parent = std::env::temp_dir().join(format!(
        "skrin-compound-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&parent).unwrap();
    let root = parent.join("db");
    let backup = parent.join("backup");
    let db = CatalogDatabase::<Pairs>::create_dir(&root)?;
    seed_pairs(&db)?;
    drop(db);
    let db = CatalogDatabase::<Pairs>::open_dir(&root)?;
    verify_pairs!(db.read()?);
    db.checkpoint()?;
    drop(db.backup_to(&backup)?);
    drop(db);
    for path in [&root, &backup] {
        let db = CatalogDatabase::<Pairs>::open_dir(path)?;
        assert_eq!(db.read()?.sequence(), 1);
        verify_pairs!(db.read()?);
        // Verify complete decoded model state as well as index order after recovery.
        let read = db.read()?;
        for (id, count, score, small) in [
            (0, 0, u64::MAX, 0),
            (2, 1, 0, 0),
            (9, 1, 1, 1),
            (u64::MAX, 1, 1, 2),
            (5, u32::MAX, 0, 0),
        ] {
            let row = read.get::<PairEntries>(id)?.unwrap();
            let expected = pair_value(count, score, small);
            assert_eq!(
                (row.count, row.score, row.small, &row.name, &row.bytes),
                (
                    expected.count,
                    expected.score,
                    expected.small,
                    &expected.name,
                    &expected.bytes
                )
            );
        }
    }
    std::fs::remove_dir_all(parent).unwrap();
    Ok(())
}
