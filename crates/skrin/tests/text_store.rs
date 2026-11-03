#![cfg(feature = "derive")]

#[path = "../examples/support/tasks.rs"]
// Persistent workflow helpers are covered by Unix tests and the portable example.
#[cfg_attr(not(unix), allow(dead_code))]
mod tasks;
use skrin::catalog::CatalogDatabase;
use skrin::{Error, Result};
use tasks::*;

#[test]
fn string_uniqueness_uses_exact_bytes_and_staged_reads_allow_atomic_swaps() -> Result<()> {
    let db = CatalogDatabase::<TaskStore>::in_memory()?;
    let keys = ["", "\0", "é", "e\u{301}", "a", "A", "%_'\\", "日本語🦀"];
    db.write(|tx| {
        for (id, key) in keys.iter().enumerate() {
            tx.insert::<Tasks>(
                id as u64,
                Task {
                    request_id: (*key).into(),
                    title: "shared title".into(),
                    spec_md: SPEC.into(),
                    status: "open".into(),
                },
            )?;
        }
        Ok(())
    })?;
    for (id, key) in keys.iter().enumerate() {
        assert_eq!(
            db.read()?.matching(ByRequest, key)?.next().unwrap().0,
            id as u64
        );
    }
    macro_rules! duplicate {
        ($db:expr) => {
            $db.write(|tx| {
                tx.edit::<Tasks>(1, |task| {
                    task.request_id.clear();
                    Ok(())
                })?;
                assert_eq!(
                    tx.matching(ByRequest, "")?
                        .iter()
                        .map(|(id, _)| *id)
                        .collect::<Vec<_>>(),
                    [0, 1]
                );
                tx.insert::<Comments>(
                    50,
                    Comment {
                        operation_id: "rollback".into(),
                        task_id: 1,
                        body: COMMENT.into(),
                    },
                )
            })
        };
    }
    assert!(matches!(
        duplicate!(db),
        Err(Error::UniqueViolation { index_id: 1 })
    ));
    assert!(db.read()?.get::<Comments>(50)?.is_none());
    let db = db.into_snapshots(options(), footprint)?;
    let old = db.snapshot()?;
    assert!(matches!(
        duplicate!(db),
        Err(Error::UniqueViolation { index_id: 1 })
    ));
    assert!(db.snapshot()?.get::<Comments>(50)?.is_none());
    db.write(|tx| {
        tx.edit::<Tasks>(0, |task| {
            task.request_id = "\0".into();
            Ok(())
        })?;
        assert_eq!(tx.matching(ByRequest, "\0")?.len(), 2);
        tx.edit::<Tasks>(1, |task| {
            task.request_id.clear();
            Ok(())
        })?;
        assert_eq!(tx.query(ByRequest, ..)?.len(), keys.len());
        Ok(())
    })?;
    assert_eq!(old.matching(ByRequest, "")?.next().unwrap().0, 0);
    assert_eq!(db.snapshot()?.matching(ByRequest, "")?.next().unwrap().0, 1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn typed_group_requests_read_preceding_commits_and_avoid_duplicate_comments() -> Result<()> {
    let path = std::env::temp_dir().join(format!(
        "skrin-text-group-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = CatalogDatabase::<TaskStore>::create_dir(&path)?;
    seed(&db)?;
    let sequence = db.stats()?.commits;
    seed(&db)?;
    assert_eq!(db.stats()?.commits, sequence);
    db.write(|tx| {
        tx.insert::<Comments>(
            0,
            Comment {
                operation_id: "ordinary-comment".into(),
                task_id: 42,
                body: "A question".into(),
            },
        )
    })?;
    let db = db
        .into_snapshots(options(), footprint)?
        .into_group_commit(skrin::group_commit::GroupCommitOptions::default())?;
    let first = db.submit(complete)?;
    let retry = db.submit(complete)?;
    assert!(first.wait()?.value);
    assert!(!retry.wait()?.value);
    let frame = db.snapshot()?;
    assert_eq!(frame.matching(ByTask, &42)?.count(), 2);
    assert_eq!(frame.matching(ByStatus, "done")?.count(), 1);
    drop(frame);
    verify(&db.into_database()?.read()?)?;
    verify(&CatalogDatabase::<TaskStore>::open_dir(&path)?.read()?)?;
    std::fs::remove_dir_all(path).unwrap();
    Ok(())
}

#[test]
fn string_codec_preserves_byte_lengths_and_enforces_the_record_budget() -> Result<()> {
    let value = "\0é🦀\r\n";
    let mut encoder = skrin::Encoder::default();
    encoder.string(value)?;
    let bytes = encoder.finish();
    assert_eq!(&bytes[..4], &(value.len() as u32).to_le_bytes());
    assert_eq!(&bytes[4..], value.as_bytes());
    let mut decoder = skrin::Decoder::new(&bytes);
    assert_eq!(decoder.string()?, value);
    decoder.finish()?;
    let max = skrin::codec::MAX_RECORD_BYTES;
    let boundary = "a".repeat(max - 4);
    let mut encoder = skrin::Encoder::default();
    encoder.string(&boundary)?;
    assert_eq!(encoder.finish().len(), max);
    assert!(
        matches!(skrin::Encoder::default().string(&(boundary + "a")),
        Err(Error::LimitExceeded { limit }) if limit == max)
    );
    // Arbitrary non-UTF-8 bytes belong in bytes/Vec<u8>, not a lossy text codec.
    assert!(matches!(
        skrin::Decoder::new(&[1, 0, 0, 0, 255]).string(),
        Err(Error::Codec(_))
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn exact_text_survives_wal_snapshot_backup_and_refused_oversized_commit() -> Result<()> {
    let parent = std::env::temp_dir().join(format!(
        "skrin-text-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&parent).unwrap();
    let path = parent.join("db");
    let backup = parent.join("backup");
    let db = CatalogDatabase::<TaskStore>::create_dir(&path)?;
    seed(&db)?;
    let long_text = SPEC.repeat(4096);
    db.write(|tx| {
        tx.edit::<Tasks>(42, |task| {
            task.spec_md = long_text.clone();
            Ok(())
        })
    })?;
    let sequence = db.stats()?.commits;
    let bytes = db.stats()?.wal_bytes;
    assert!(matches!(
        db.write(|tx| {
            tx.edit::<Tasks>(42, |task| {
                task.spec_md = "x".repeat(skrin::codec::MAX_RECORD_BYTES);
                Ok(())
            })?;
            tx.insert::<Comments>(
                9,
                Comment {
                    operation_id: "oversized".into(),
                    task_id: 42,
                    body: "must roll back".into(),
                },
            )
        }),
        Err(Error::LimitExceeded { .. })
    ));
    assert_eq!(db.stats()?.commits, sequence);
    assert_eq!(db.stats()?.wal_bytes, bytes);
    drop(db);
    let check = |db: &CatalogDatabase<TaskStore>| -> Result<()> {
        let read = db.read()?;
        assert_eq!(read.get::<Tasks>(42)?.unwrap().spec_md, long_text);
        assert!(read.get::<Comments>(9)?.is_none());
        assert_eq!(read.prefix(ByTitle, "Lagring/")?.next().unwrap().0, 42);
        Ok(())
    };
    let db = CatalogDatabase::<TaskStore>::open_dir(&path)?;
    check(&db)?;
    db.checkpoint()?;
    check(&db.backup_to(&backup)?)?;
    drop(db);
    for path in [&path, &backup] {
        let db = CatalogDatabase::<TaskStore>::open_dir(path)?;
        check(&db)?;
        assert_eq!(db.stats()?.commits, sequence);
    }
    std::fs::remove_dir_all(parent).unwrap();
    Ok(())
}
