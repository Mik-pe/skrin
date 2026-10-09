#![cfg(feature = "derive")]

use skrin::catalog::{Catalog, CatalogDatabase, Table};
use skrin::{Decoder, Encoder, Error, Record, Result};
#[cfg(unix)]
#[path = "../examples/support/banking.rs"]
mod manual;

extern crate skrin as chest;

// Deliberately not Clone: deriving a codec does not require cloning records.
#[derive(Debug, PartialEq, Eq, chest::Record)]
#[skrin(table_id = 1, version = 1, crate = chest)]
struct Account {
    email: std::string::String,
    balance: u64,
}
#[derive(Debug, PartialEq, Eq, skrin::Record)]
#[skrin(table_id = 2, version = 1)]
struct Transfer {
    from: u64,
    to: u64,
    amount: u64,
}
skrin::catalog! {
    Bank, #[derive(Debug)] Row {
        schema: (8000, 1),
        tables: { Accounts: Account, Transfers: Transfer },
        indexes: {
            EMAIL: Accounts {
                id: 1, version: 1, unique: true,
                key: |row: &Account| Ok(row.email.as_bytes().to_vec())
            },
            BALANCE: Accounts {
                id: 2, version: 1, unique: false,
                key: |row: &Account| Ok(row.balance.to_be_bytes().to_vec())
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq, skrin::Record)]
#[skrin(table_id = 0x77, version = 3)]
struct Packet {
    tag: u8,
    count: u32,
    value: u64,
    text: String,
    payload: std::vec::Vec<u8>,
}

#[test]
fn derived_codec_matches_independent_bytes_and_rejects_every_truncation() -> Result<()> {
    let row = Packet {
        tag: 255,
        count: 0x04030201,
        value: u64::MAX,
        text: "å".into(),
        payload: vec![0, 255],
    };
    // Fixed explicit LE integers, u32 byte lengths, UTF-8, and raw blob bytes.
    let expected = [
        255, 1, 2, 3, 4, 255, 255, 255, 255, 255, 255, 255, 255, 2, 0, 0, 0, 0xc3, 0xa5, 2, 0, 0,
        0, 0, 255,
    ];
    let mut e = Encoder::default();
    row.encode(&mut e)?;
    assert_eq!(e.finish(), expected);
    let mut d = Decoder::new(&expected);
    assert_eq!(Packet::decode(&mut d)?, row);
    d.finish()?;
    for len in 0..expected.len() {
        assert!(Packet::decode(&mut Decoder::new(&expected[..len])).is_err());
    }
    let mut invalid_utf8 = expected;
    invalid_utf8[17] = 0xff;
    assert!(Packet::decode(&mut Decoder::new(&invalid_utf8)).is_err());
    let mut oversized_length = expected;
    oversized_length[13..17].fill(255);
    assert!(Packet::decode(&mut Decoder::new(&oversized_length)).is_err());
    let mut too_large = row;
    too_large.payload = vec![0; skrin::codec::MAX_RECORD_BYTES];
    assert!(matches!(
        too_large.encode(&mut Encoder::default()),
        Err(Error::LimitExceeded { .. })
    ));
    Ok(())
}

fn seed(db: &CatalogDatabase<Bank>) -> Result<()> {
    db.write(|tx| {
        tx.insert::<Accounts>(
            1,
            Account {
                email: "alice@example.test".into(),
                balance: 100,
            },
        )?;
        tx.insert::<Accounts>(
            2,
            Account {
                email: "bob@example.test".into(),
                balance: 100,
            },
        )
    })
}

#[test]
fn generated_dispatch_preserves_atomic_unique_constraints_and_snapshot_indexes() -> Result<()> {
    let db = CatalogDatabase::<Bank>::in_memory()?;
    seed(&db)?;
    assert!(matches!(
        db.write(|tx| {
            tx.put::<Accounts>(
                2,
                Account {
                    email: "alice@example.test".into(),
                    balance: 50,
                },
            )?;
            tx.insert::<Transfers>(
                u64::MAX,
                Transfer {
                    from: 1,
                    to: 2,
                    amount: 50,
                },
            )
        }),
        Err(Error::UniqueViolation { index_id: EMAIL })
    ));
    assert_eq!(db.read()?.sequence(), 1);
    assert!(db.read()?.get::<Transfers>(u64::MAX)?.is_none());
    let versioned = db.into_snapshots(
        skrin::versioned::SnapshotOptions {
            max_snapshots: 2,
            max_pinned_bytes: 1024 * 1024,
        },
        |r| {
            Ok(std::mem::size_of_val(r) as u64
                + match r {
                    Row::Accounts(a) => a.email.capacity() as u64,
                    Row::Transfers(_) => 0,
                })
        },
    )?;
    let old = versioned.snapshot()?;
    versioned.write(|tx| {
        tx.put::<Accounts>(
            1,
            Account {
                email: "bob@example.test".into(),
                balance: 70,
            },
        )?;
        tx.put::<Accounts>(
            2,
            Account {
                email: "alice@example.test".into(),
                balance: 130,
            },
        )?;
        tx.insert::<Transfers>(
            u64::MAX,
            Transfer {
                from: 1,
                to: 2,
                amount: 30,
            },
        )
    })?;
    let current = versioned.snapshot()?;
    assert_eq!(
        old.lookup::<Accounts>(EMAIL, b"alice@example.test")?[0].0,
        1
    );
    assert_eq!(
        current.lookup::<Accounts>(EMAIL, b"alice@example.test")?[0].0,
        2
    );
    assert_eq!(
        current
            .index_scan::<Accounts>(BALANCE, ..)?
            .map(|(id, a)| (id, a.balance))
            .collect::<Vec<_>>(),
        [(1, 70), (2, 130)]
    );
    assert_eq!(current.get::<Transfers>(u64::MAX)?.unwrap().amount, 30);
    let transfer = Row::Transfers(Transfer {
        from: 1,
        to: 2,
        amount: 1,
    });
    assert!(Bank::index_key(EMAIL, &transfer).is_err());
    assert!(Accounts::borrow(&transfer).is_none());
    assert!(Bank::decode(99, &mut Decoder::new(&[])).is_err());
    Ok(())
}

// An application can retain manual codecs, single-table catalogs and no indexes.
skrin::catalog! {
    Plain, PlainRow {
        schema: (99, 1),
        tables: { Packets: Packet },
        indexes: {}
    }
}
#[test]
fn one_table_no_index_catalog_is_executable() -> Result<()> {
    let db = CatalogDatabase::<Plain>::in_memory()?;
    db.write(|tx| {
        tx.insert::<Packets>(
            u64::MAX,
            Packet {
                tag: 0,
                count: 0,
                value: 0,
                text: String::new(),
                payload: Vec::new(),
            },
        )
    })?;
    assert_eq!(db.read()?.scan::<Packets>()?.count(), 1);
    Ok(())
}

#[cfg(unix)]
mod persisted {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "skrin-models-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    skrin::catalog! {
        Unsorted, UnsortedRow {
            schema: (8001, 1),
            tables: { TransfersFirst: Transfer, AccountsSecond: Account },
            indexes: {}
        }
    }
    // Manual Record implementations require no procedural macro support.
    skrin::catalog! {
        Mixed, MixedRow {
            schema: (8002, 1),
            tables: { ManualAccounts: manual::Account, DerivedTransfers: Transfer },
            indexes: {}
        }
    }
    #[test]
    fn manual_models_work_and_invalid_declarations_refuse_before_file_creation() -> Result<()> {
        let temp = Temp::new();
        let refused_path = temp.0.join("refused");
        assert!(matches!(
            CatalogDatabase::<Unsorted>::create_dir(&refused_path),
            Err(Error::InvalidOperation(_))
        ));
        assert!(!refused_path.exists());
        let db = CatalogDatabase::<Mixed>::in_memory()?;
        db.write(|tx| {
            tx.insert::<ManualAccounts>(
                1,
                manual::Account {
                    email: "manual".into(),
                    balance: 9,
                },
            )
        })?;
        assert_eq!(db.read()?.get::<ManualAccounts>(1)?.unwrap().balance, 9);
        Ok(())
    }
    #[test]
    fn generated_and_manual_codecs_have_identical_wal_and_cross_open_generations() -> Result<()> {
        let temp = Temp::new();
        let manual_path = temp.0.join("manual");
        let generated_path = temp.0.join("generated");
        let handwritten = CatalogDatabase::<manual::Banking>::create_dir(&manual_path)?;
        manual::seed(&handwritten)?;
        handwritten.write(|tx| manual::transfer(tx, u64::MAX, 1, 2, 30))?;
        let generated = CatalogDatabase::<Bank>::create_dir(&generated_path)?;
        seed(&generated)?;
        generated.write(|tx| {
            tx.update::<Accounts>(1, |a| {
                Ok(Account {
                    email: a.email.clone(),
                    balance: a.balance - 30,
                })
            })?;
            tx.update::<Accounts>(2, |a| {
                Ok(Account {
                    email: a.email.clone(),
                    balance: a.balance + 30,
                })
            })?;
            tx.insert::<Transfers>(
                u64::MAX,
                Transfer {
                    from: 1,
                    to: 2,
                    amount: 30,
                },
            )
        })?;
        for file in [
            "CURRENT",
            "g0000000000000001/snapshot",
            "g0000000000000001/wal",
        ] {
            assert_eq!(
                std::fs::read(manual_path.join(file))?,
                std::fs::read(generated_path.join(file))?,
                "{file}"
            );
        }
        drop(handwritten);
        drop(generated);
        let generated = CatalogDatabase::<Bank>::open_dir(&manual_path)?;
        assert_eq!(
            generated
                .read()?
                .lookup::<Accounts>(BALANCE, &70_u64.to_be_bytes())?[0]
                .0,
            1
        );
        generated.checkpoint()?;
        let backup_path = temp.0.join("backup");
        drop(generated.backup_to(&backup_path)?);
        drop(generated);
        for path in [&manual_path, &generated_path, &backup_path] {
            let db = CatalogDatabase::<manual::Banking>::open_dir(path)?;
            assert_eq!(db.read()?.get::<manual::Accounts>(2)?.unwrap().balance, 130);
            assert_eq!(
                db.read()?
                    .get::<manual::Transfers>(u64::MAX)?
                    .unwrap()
                    .amount,
                30
            );
        }
        Ok(())
    }
}
