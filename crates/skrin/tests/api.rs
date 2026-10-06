use skrin::{Database, Decoder, Encoder, Error, Record, Result, Schema};

#[derive(Debug, PartialEq, Eq)]
struct Account {
    balance: u64,
    name: String,
}

impl Record for Account {
    const SCHEMA: Schema = Schema {
        table_id: 100,
        version: 1,
    };

    fn encode(&self, encoder: &mut Encoder) -> Result<()> {
        encoder.u64(self.balance)?;
        encoder.string(&self.name)
    }

    fn decode(decoder: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self {
            balance: decoder.u64()?,
            name: decoder.string()?.to_owned(),
        })
    }
}

fn account(balance: u64) -> Account {
    Account {
        balance,
        name: "Skrin 🦉".into(),
    }
}

#[test]
fn typed_reads_ranges_and_read_your_writes() -> Result<()> {
    let db = Database::<Account>::in_memory();
    let value = db.write(|tx| {
        tx.insert(1, account(10))?;
        tx.insert(3, account(30))?;
        tx.put(2, account(20));
        assert_eq!(tx.get(2), Some(&account(20)));
        assert!(tx.remove(3));
        assert!(tx.get(3).is_none());
        tx.insert(3, account(40))?;
        Ok("committed")
    })?;
    assert_eq!(value, "committed");
    let read = db.read()?;
    assert_eq!(read.sequence(), 1);
    assert_eq!(read.len(), 3);
    assert_eq!(read.get(1), Some(&account(10)));
    assert_eq!(
        read.range(2..=3).map(|(key, _)| key).collect::<Vec<_>>(),
        [2, 3]
    );
    assert_eq!(
        read.iter().rev().map(|(key, _)| key).collect::<Vec<_>>(),
        [3, 2, 1]
    );
    Ok(())
}

#[test]
fn propagated_error_rolls_back_the_entire_closure() -> Result<()> {
    let db = Database::<Account>::in_memory();
    db.write(|tx| tx.insert(1, account(10)))?;
    let result = db.write(|tx| {
        tx.insert(2, account(20))?;
        tx.insert(1, account(99))
    });
    assert!(matches!(result, Err(Error::DuplicateKey(1))));
    let read = db.read()?;
    assert!(read.get(2).is_none());
    assert_eq!(read.get(1), Some(&account(10)));
    assert_eq!(read.sequence(), 1);
    Ok(())
}

#[test]
fn concurrent_transactions_do_not_lose_updates() -> Result<()> {
    let db = Database::<Account>::in_memory();
    db.write(|tx| tx.insert(1, account(0)))?;
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let db = &db;
            scope.spawn(move || {
                for _ in 0..200 {
                    db.write(|tx| {
                        let value = tx.get(1).unwrap().balance;
                        tx.put(1, account(value + 1));
                        Ok(())
                    })
                    .unwrap();
                }
            });
        }
    });
    assert_eq!(db.read()?.get(1).unwrap().balance, 1600);
    assert_eq!(db.stats()?.commits, 1601);
    Ok(())
}

#[test]
fn codec_roundtrips_explicit_little_endian_fields() -> Result<()> {
    let mut encoder = Encoder::default();
    encoder.u8(9)?;
    encoder.u32(0x0403_0201)?;
    encoder.u64(0x0807_0605_0403_0201)?;
    encoder.string("åäö 🦉")?;
    encoder.bytes(&[0, 255])?;
    let bytes = encoder.finish();
    assert_eq!(&bytes[..13], &[9, 1, 2, 3, 4, 1, 2, 3, 4, 5, 6, 7, 8]);
    let mut decoder = Decoder::new(&bytes);
    assert_eq!(decoder.u8()?, 9);
    assert_eq!(decoder.u32()?, 0x0403_0201);
    assert_eq!(decoder.u64()?, 0x0807_0605_0403_0201);
    assert_eq!(decoder.string()?, "åäö 🦉");
    assert_eq!(decoder.bytes()?, &[0, 255]);
    decoder.finish()
}

#[test]
fn codec_rejects_truncated_fields_invalid_utf8_and_trailing_data() {
    assert!(Decoder::new(&[0; 7]).u64().is_err());
    assert!(Decoder::new(&[255; 4]).bytes().is_err());
    assert!(Decoder::new(&[1, 0, 0, 0, 255]).string().is_err());
    assert!(Decoder::new(&[0]).finish().is_err());
    assert!(Decoder::new(&[]).finish().is_ok());
}

#[cfg(not(unix))]
#[test]
fn unsupported_persistence_fails_before_creating_a_file() {
    assert!(matches!(
        Database::<Account>::create("unsupported.skrin"),
        Err(Error::UnsupportedPlatform)
    ));
    assert!(matches!(
        Database::<Account>::open("unsupported.skrin"),
        Err(Error::UnsupportedPlatform)
    ));
}

#[cfg(unix)]
mod disk {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            for _ in 0..100 {
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir()
                    .join(format!("skrin-tests-{}-{stamp}-{id}", std::process::id()));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create test directory: {error}"),
                }
            }
            panic!("test directory collision limit")
        }

        fn file(&self) -> PathBuf {
            self.0.join("data.skrin")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn commits_survive_reopen_and_deletes_persist() -> Result<()> {
        let dir = TempDir::new();
        let db = Database::<Account>::create(dir.file())?;
        db.write(|tx| {
            tx.insert(1, account(10))?;
            tx.insert(2, account(20))
        })?;
        db.write(|tx| {
            tx.put(1, account(30));
            assert!(tx.remove(2));
            Ok(())
        })?;
        drop(db);
        let db = Database::<Account>::open(dir.file())?;
        assert_eq!(db.read()?.get(1), Some(&account(30)));
        assert!(db.read()?.get(2).is_none());
        assert_eq!(db.stats()?.commits, 2);
        assert_eq!(db.stats()?.recovered_tail_bytes, 0);
        db.write(|tx| tx.insert(3, account(40)))?;
        drop(db);
        let db = Database::<Account>::open(dir.file())?;
        assert_eq!(db.read()?.len(), 2);
        assert_eq!(db.stats()?.commits, 3);
        Ok(())
    }

    #[test]
    fn create_never_overwrites_and_open_never_creates() -> Result<()> {
        let dir = TempDir::new();
        assert!(matches!(
            Database::<Account>::open(dir.file()),
            Err(Error::Io(_))
        ));
        assert!(!dir.file().exists());
        let db = Database::<Account>::create(dir.file())?;
        db.write(|tx| tx.insert(1, account(10)))?;
        let before = fs::read(dir.file())?;
        assert!(matches!(
            Database::<Account>::create(dir.file()),
            Err(Error::Io(_))
        ));
        assert_eq!(fs::read(dir.file())?, before);
        drop(db);
        assert!(Database::<Account>::open(dir.file()).is_ok());
        Ok(())
    }

    #[test]
    fn an_empty_existing_file_is_not_reinitialized() -> Result<()> {
        let dir = TempDir::new();
        fs::write(dir.file(), [])?;
        assert!(matches!(
            Database::<Account>::open(dir.file()),
            Err(Error::Corrupt { .. })
        ));
        assert_eq!(fs::metadata(dir.file())?.len(), 0);
        Ok(())
    }

    #[test]
    fn lock_excludes_second_handle_and_another_process() -> Result<()> {
        let dir = TempDir::new();
        let db = Database::<Account>::create(dir.file())?;
        assert!(matches!(
            Database::<Account>::open(dir.file()),
            Err(Error::Busy)
        ));
        let status = Command::new(std::env::current_exe()?)
            .args(["--exact", "disk::child_lock_probe", "--nocapture"])
            .env("SKRIN_TEST_LOCK_PATH", dir.file())
            .status()?;
        assert!(status.success());
        drop(db);
        assert!(Database::<Account>::open(dir.file()).is_ok());
        Ok(())
    }

    #[test]
    fn child_lock_probe() {
        let Some(path) = std::env::var_os("SKRIN_TEST_LOCK_PATH") else {
            return;
        };
        assert!(matches!(Database::<Account>::open(path), Err(Error::Busy)));
    }

    #[test]
    fn committed_data_survives_process_exit_without_drop() -> Result<()> {
        let dir = TempDir::new();
        let status = Command::new(std::env::current_exe()?)
            .args(["--exact", "disk::child_commit_without_drop", "--nocapture"])
            .env("SKRIN_TEST_EXIT_PATH", dir.file())
            .status()?;
        assert!(status.success());
        let db = Database::<Account>::open(dir.file())?;
        assert_eq!(db.read()?.get(42), Some(&account(123)));
        assert_eq!(db.stats()?.commits, 1);
        Ok(())
    }

    #[test]
    fn child_commit_without_drop() {
        let Some(path) = std::env::var_os("SKRIN_TEST_EXIT_PATH") else {
            return;
        };
        let db = Database::<Account>::create(path).unwrap();
        db.write(|tx| tx.insert(42, account(123))).unwrap();
        std::process::exit(0);
    }
}

#[test]
fn updates_are_typed_read_their_own_writes_and_never_implicitly_insert() -> Result<()> {
    let db = Database::<Account>::in_memory();
    db.write(|tx| {
        tx.insert(1, account(10))?;
        tx.update(1, |row| Ok(account(row.balance + 2)))?;
        tx.update(1, |row| Ok(account(row.balance + 3)))?;
        assert_eq!(tx.get(1), Some(&account(15)));
        assert!(matches!(
            tx.update(2, |_| Ok(account(99))),
            Err(Error::MissingKey(2))
        ));
        assert!(
            tx.update(1, |_| Err(Error::Codec("cancel".into())))
                .is_err()
        );
        assert_eq!(tx.get(1), Some(&account(15)));
        Ok(())
    })?;
    assert_eq!(db.read()?.get(1), Some(&account(15)));
    assert_eq!(db.stats()?.commits, 1);
    Ok(())
}
