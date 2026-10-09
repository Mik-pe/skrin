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
