//! Opt-in Linux filesystem test; scripts/test-storage-full.sh supplies a mount.
#![cfg(target_os = "linux")]

use skrin::{
    Database, Decoder, Encoder, Error, MaintenanceOptions, Record, Result, Schema, StorageEntryKind,
};
use std::fs::{self, File};
use std::io::{self, Write};
use std::process::Command;

struct Row(Vec<u8>);
impl Record for Row {
    const SCHEMA: Schema = Schema {
        table_id: 0x454e4f535043,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.bytes(&self.0)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(d.bytes()?.into()))
    }
}

#[test]
#[ignore = "requires a private bounded tmpfs; run scripts/test-storage-full.sh"]
fn real_storage_full_preserves_preparation_and_commit_outcomes() {
    let root = std::env::var_os("SKRIN_STORAGE_FULL_ROOT").expect("use the namespace wrapper");
    // Refuse an ordinary filesystem or an unbounded/shared machine tmpfs even
    // when invoked manually. The wrapper creates an exclusive empty mount.
    let output = Command::new("stat")
        .args(["--file-system", "--format=%T:%S:%b", "--"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(output.status.success());
    let fields: Vec<_> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .trim()
        .split(':')
        .collect();
    assert_eq!(fields[0], "tmpfs");
    let capacity = fields[1].parse::<u64>().unwrap() * fields[2].parse::<u64>().unwrap();
    assert!((1024 * 1024..=8 * 1024 * 1024).contains(&capacity));
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        0,
        "private empty mount required"
    );

    let root = std::path::PathBuf::from(root);
    for reserve_file_data in [false, true] {
        let options = MaintenanceOptions {
            reserve_file_data,
            ..Default::default()
        };
        let path = root.join(format!("database-{reserve_file_data}"));
        let db = Database::<Row>::create_dir(&path).unwrap();
        db.write(|tx| tx.insert(1, Row(vec![7; 1024]))).unwrap();
        let current = fs::read(path.join("CURRENT")).unwrap();
        let filler_path = root.join(format!("filler-{reserve_file_data}"));
        let mut filler = File::create_new(&filler_path).unwrap();
        let block = [0u8; 64 * 1024];
        let mut filled = 0;
        loop {
            match filler.write(&block) {
                Ok(0) => panic!("filling the test mount made no progress"),
                Ok(bytes) => {
                    filled += bytes as u64;
                    assert!(filled <= capacity);
                }
                Err(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::StorageFull);
                    println!(
                        "actual tmpfs ENOSPC after {filled} filler bytes; reserve_file_data={reserve_file_data}"
                    );
                    break;
                }
            }
        }
        drop(filler);

        match db.checkpoint_with_options(options) {
            Err(Error::Io(error)) => assert_eq!(error.kind(), io::ErrorKind::StorageFull),
            result => panic!("expected clean storage-full preparation refusal: {result:?}"),
        }
        assert_eq!(fs::read(path.join("CURRENT")).unwrap(), current);
        assert_eq!(db.read().unwrap().get(1).unwrap().0, vec![7; 1024]);
        assert!(matches!(Database::<Row>::open_dir(&path), Err(Error::Busy)));
        let unknown: Vec<_> = db
            .storage_inventory()
            .unwrap()
            .entries
            .into_iter()
            .filter(|entry| entry.kind == StorageEntryKind::Unknown)
            .map(|entry| entry.path)
            .collect();
        assert!(
            !unknown.is_empty(),
            "partial ownership stage needs operator inspection"
        );

        // This exceeds any spare bytes in the existing WAL's final allocated page.
        match db.write(|tx| tx.update(1, |_| Ok(Row(vec![9; 128 * 1024])))) {
            Err(Error::CommitUncertain(error)) => {
                assert_eq!(error.kind(), io::ErrorKind::StorageFull)
            }
            result => panic!("expected uncertain storage-full append: {result:?}"),
        }
        assert!(matches!(db.read(), Err(Error::Poisoned)));
        drop(db);
        fs::remove_file(filler_path).unwrap();

        let db = Database::<Row>::open_dir(&path).unwrap();
        assert_eq!(db.read().unwrap().sequence(), 1);
        assert_eq!(db.read().unwrap().len(), 1);
        assert_eq!(db.read().unwrap().get(1).unwrap().0, vec![7; 1024]);
        db.reclaim().unwrap();
        for relative in &unknown {
            assert!(path.join(relative).exists());
        }
        assert!(
            !db.storage_inventory()
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.reclaimable)
        );

        // Operator path: preserve the unknown stage and make an independently
        // decoded backup of the selected, recovered data without repairing OWNER.
        let backup_path = root.join(format!("verified-backup-{reserve_file_data}"));
        let backup = db.backup_to_with_options(&backup_path, options).unwrap();
        assert_eq!(backup.read().unwrap().sequence(), 1);
        assert_eq!(backup.read().unwrap().get(1).unwrap().0, vec![7; 1024]);
        drop(backup);
        let restored = Database::<Row>::open_dir(backup_path).unwrap();
        assert_eq!(restored.read().unwrap().sequence(), 1);
        assert_eq!(restored.read().unwrap().get(1).unwrap().0, vec![7; 1024]);
        for relative in unknown {
            assert!(path.join(relative).exists());
        }
        println!(
            "verified preparation refusal, poisoned append, selected recovery and preserved unknown stage/backup; reserve_file_data={reserve_file_data}"
        );
    }
}
