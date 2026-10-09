#![cfg(feature = "derive")]
use skrin::catalog::CatalogDatabase;
use skrin::versioned::SnapshotOptions;
use skrin::{Decoder, Encoder, Error, Record, Result};
#[path = "../examples/support/characters.rs"]
mod characters;
use characters::*;

// No Clone: recursive optional codecs borrow their fields during encoding.
#[derive(Debug, skrin::Record)]
#[skrin(table_id = 0x78, version = 1)]
struct Values {
    u16: u16,
    u128: u128,
    i8: i8,
    i16: i16,
    i32: core::primitive::i32,
    i64: i64,
    i128: i128,
    flag: bool,
    zero: f32,
    nan: f64,
    owner: Option<u64>,
    title: std::option::Option<String>,
    nested: core::option::Option<Option<u8>>,
    bytes: Option<Vec<u8>>,
}
fn value() -> Values {
    Values {
        u16: 0x1234,
        u128: u128::MAX,
        i8: i8::MIN,
        i16: i16::MIN,
        i32: i32::MIN,
        i64: i64::MIN,
        i128: i128::MIN,
        flag: true,
        zero: -0.0,
        nan: f64::from_bits(0x7ff8000000000042),
        owner: Some(u64::MAX),
        title: Some("å".into()),
        nested: Some(None),
        bytes: Some(vec![0, 255]),
    }
}
fn golden() -> Vec<u8> {
    let hex = include_str!("fixtures/model-values-v1.hex").trim();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
#[test]
fn expanded_derive_matches_independent_fixed_bytes_and_rejects_every_truncation() -> Result<()> {
    let expected = golden();
    let mut e = Encoder::default();
    value().encode(&mut e)?;
    assert_eq!(e.finish(), expected);
    let mut d = Decoder::new(&expected);
    let actual = Values::decode(&mut d)?;
    d.finish()?;
    assert_eq!(
        (
            actual.u16,
            actual.u128,
            actual.i8,
            actual.i16,
            actual.i32,
            actual.i64,
            actual.i128
        ),
        (
            0x1234,
            u128::MAX,
            i8::MIN,
            i16::MIN,
            i32::MIN,
            i64::MIN,
            i128::MIN
        )
    );
    assert!(actual.flag);
    assert_eq!(actual.zero.to_bits(), 0x80000000);
    assert_eq!(actual.nan.to_bits(), 0x7ff8000000000042);
    assert_eq!(actual.owner, Some(u64::MAX));
    assert_eq!(actual.title.as_deref(), Some("å"));
    assert_eq!(actual.nested, Some(None));
    assert_eq!(actual.bytes.as_deref(), Some(&[0, 255][..]));
    for length in 0..expected.len() {
        assert!(
            Values::decode(&mut Decoder::new(&expected[..length])).is_err(),
            "prefix {length}"
        );
    }
    // All boolean/optional discriminants reject every noncanonical tag.
    for position in [49, 62, 71, 78, 79, 80] {
        for tag in 2..=255 {
            let mut corrupt = expected.clone();
            corrupt[position] = tag;
            assert!(
                matches!(
                    Values::decode(&mut Decoder::new(&corrupt)),
                    Err(Error::Codec(_))
                ),
                "position {position}, tag {tag}"
            );
        }
    }
    Ok(())
}
#[test]
fn option_none_some_and_nested_none_have_distinct_exact_encodings() -> Result<()> {
    #[derive(skrin::Record)]
    #[skrin(table_id = 0x79, version = 1)]
    struct Options {
        count: Option<u32>,
        name: Option<String>,
        nested: Option<Option<bool>>,
    }
    let cases = [
        (
            Options {
                count: None,
                name: None,
                nested: None,
            },
            vec![0, 0, 0],
        ),
        (
            Options {
                count: Some(0),
                name: Some(String::new()),
                nested: Some(None),
            },
            vec![1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0],
        ),
        (
            Options {
                count: None,
                name: None,
                nested: Some(Some(false)),
            },
            vec![0, 0, 1, 1, 0],
        ),
        (
            Options {
                count: None,
                name: None,
                nested: Some(Some(true)),
            },
            vec![0, 0, 1, 1, 1],
        ),
    ];
    for (row, expected) in cases {
        let mut e = Encoder::default();
        row.encode(&mut e)?;
        assert_eq!(e.finish(), expected);
        let mut d = Decoder::new(&expected);
        let decoded = Options::decode(&mut d)?;
        d.finish()?;
        assert_eq!(
            (decoded.count, decoded.name, decoded.nested),
            (row.count, row.name, row.nested)
        );
    }
    let mut too_large = value();
    too_large.bytes = Some(vec![0; skrin::codec::MAX_RECORD_BYTES]);
    assert!(matches!(
        too_large.encode(&mut Encoder::default()),
        Err(Error::LimitExceeded { .. })
    ));
    Ok(())
}
#[test]
fn invalid_damage_rolls_back_other_staged_rows_and_indexes() -> Result<()> {
    let db = CatalogDatabase::<Game>::in_memory()?;
    seed(&db)?;
    for amount in [-1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(matches!(
            db.write(|tx| {
                damage(tx, 9, 125.0)?;
                damage(tx, 7, amount)
            }),
            Err(Error::InvalidOperation(_))
        ));
        verify(&db.read()?, false)?;
    }
    // The codec preserves float bits; the application chooses valid health.
    db.write(|tx| {
        tx.update::<Characters>(7, |old| {
            Ok(Character {
                name: old.name.clone(),
                x: old.x,
                y: old.y,
                health: f32::from_bits(0x7fc00042),
                alive: old.alive,
                team: old.team,
            })
        })
    })?;
    assert!(matches!(
        db.write(|tx| {
            damage(tx, 9, 125.0)?;
            damage(tx, 7, 1.0)
        }),
        Err(Error::InvalidOperation(_))
    ));
    let read = db.read()?;
    assert_eq!(read.sequence(), 2);
    assert_eq!(
        read.get::<Characters>(7)?.unwrap().health.to_bits(),
        0x7fc00042
    );
    assert_eq!(read.get::<Characters>(9)?.unwrap().health, 100.0);
    assert_eq!(read.matching(Alive, &true)?.count(), 2);
    assert_eq!(read.matching(Alive, &false)?.count(), 0);
    Ok(())
}
#[test]
fn signed_index_bounds_and_bool_keys_preserve_atomic_edits_and_old_frames() -> Result<()> {
    let db = CatalogDatabase::<Game>::in_memory()?;
    seed(&db)?;
    verify(&db.read()?, false)?;
    {
        let read = db.read()?;
        assert_eq!(
            read.query(Chunk, ..0)?
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            [7]
        );
        assert_eq!(
            read.matching(Chunk, &0)?
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            [9]
        );
    }
    let db = db.into_snapshots(
        SnapshotOptions {
            max_snapshots: 4,
            max_pinned_bytes: 1024 * 1024,
        },
        |row| {
            let Row::Characters(row) = row;
            Ok(std::mem::size_of::<Row>() as u64 + row.name.capacity() as u64)
        },
    )?;
    let old = db.snapshot()?;
    db.write(|tx| {
        let old = tx.get::<Characters>(7)?.unwrap();
        tx.put::<Characters>(
            7,
            Character {
                name: old.name.clone(),
                x: old.x,
                y: old.y,
                health: 0.0,
                alive: false,
                team: Some(99),
            },
        )
    })?;
    let current = db.snapshot()?;
    assert_eq!(old.matching(Alive, &true)?.count(), 2);
    assert_eq!(old.get::<Characters>(7)?.unwrap().team, None);
    assert_eq!(current.matching(Alive, &true)?.count(), 1);
    assert_eq!(current.get::<Characters>(7)?.unwrap().team, Some(99));
    assert_eq!(
        current
            .query(Chunk, i32::MIN..=i32::MAX)?
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        [7, 9]
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn character_models_survive_wal_checkpoint_and_independent_backup() -> Result<()> {
    struct Temp(std::path::PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let path = std::env::temp_dir().join(format!(
        "skrin-character-models-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path)?;
    let temp = Temp(path);
    let primary = temp.0.join("primary");
    let backup = temp.0.join("backup");
    let db = CatalogDatabase::<Game>::create_dir(&primary)?;
    seed(&db)?;
    db.write(|tx| damage(tx, 7, 125.0))?;
    verify(&db.read()?, true)?;
    drop(db);
    let db = CatalogDatabase::<Game>::open_dir(&primary)?;
    verify(&db.read()?, true)?;
    db.checkpoint()?;
    drop(db.backup_to(&backup)?);
    drop(db);
    for path in [&primary, &backup] {
        verify(&CatalogDatabase::<Game>::open_dir(path)?.read()?, true)?;
    }
    Ok(())
}
