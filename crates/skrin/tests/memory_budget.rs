//! Opt-in real Linux OOM test; only the disposable child is memory limited.
#![cfg(target_os = "linux")]

use skrin::{Database, Decoder, Encoder, Record, Result, Schema, StorageEntryKind};
use std::{fs, io::Write, path::PathBuf};

struct Row(u64);
impl Record for Row {
    const SCHEMA: Schema = Schema {
        table_id: 0x4d454d4f5259,
        version: 1,
    };
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.u64(self.0)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(d.u64()?))
    }
}

// Identical schema and explicit bytes; only the verification decoder's scratch
// allocation differs. No alternate database or recovery implementation.
struct AllocatingRow(u64);
impl Record for AllocatingRow {
    const SCHEMA: Schema = Row::SCHEMA;
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.u64(self.0)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        let value = d.u64()?;
        println!("checkpoint decoder allocating 128 MiB of touched scratch");
        std::io::stdout().flush().unwrap();
        let mut scratch = vec![0u8; 128 * 1024 * 1024];
        for page in scratch.chunks_mut(4096) {
            page[0] = 1;
        }
        std::hint::black_box(scratch);
        Ok(Self(value))
    }
}

fn root() -> PathBuf {
    std::env::var_os("SKRIN_MEMORY_BUDGET_ROOT")
        .expect("use scripts/test-memory-budget.sh")
        .into()
}
fn require_private_budget() {
    let groups = fs::read_to_string("/proc/self/cgroup").unwrap();
    let relative = groups
        .lines()
        .find_map(|line| line.strip_prefix("0::/"))
        .unwrap();
    assert!(!relative.split('/').any(|component| component == ".."));
    assert!(
        relative
            .split('/')
            .any(|part| part.starts_with("skrin-memory-budget-") && part.ends_with(".service"))
    );
    let group = PathBuf::from("/sys/fs/cgroup").join(relative);
    assert_eq!(
        fs::read_to_string(group.join("memory.max")).unwrap().trim(),
        "67108864"
    );
    assert_eq!(
        fs::read_to_string(group.join("memory.swap.max"))
            .unwrap()
            .trim(),
        "0"
    );
    println!("verified private cgroup: {relative}; memory.max=67108864; memory.swap.max=0");
}

#[test]
#[ignore = "requires its own 64 MiB cgroup; run scripts/test-memory-budget.sh"]
fn bounded_checkpoint_worker() {
    // This check precedes the deliberately large allocation and all storage work.
    require_private_budget();
    let root = root();
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    let db = Database::<AllocatingRow>::create_dir(root.join("db")).unwrap();
    db.write(|tx| tx.insert(1, AllocatingRow(42))).unwrap();
    assert_eq!(db.read().unwrap().sequence(), 1);
    fs::write(
        root.join("current-before"),
        fs::read(root.join("db/CURRENT")).unwrap(),
    )
    .unwrap();
    println!("acknowledged sequence 1 before checkpoint verification");
    std::io::stdout().flush().unwrap();
    db.checkpoint().unwrap();
    panic!("checkpoint unexpectedly returned success");
}

#[test]
#[ignore = "requires the kernel-killed worker's files; run scripts/test-memory-budget.sh"]
fn recover_after_bounded_checkpoint_worker() {
    let root = root();
    let path = root.join("db");
    assert_eq!(
        fs::read(path.join("CURRENT")).unwrap(),
        fs::read(root.join("current-before")).unwrap()
    );
    let db = Database::<Row>::open_dir(&path).unwrap();
    assert_eq!(db.read().unwrap().sequence(), 1);
    assert_eq!(db.read().unwrap().len(), 1);
    assert_eq!(db.read().unwrap().get(1).unwrap().0, 42);
    let inventory = db.storage_inventory().unwrap();
    assert!(inventory.entries.iter().any(|entry| entry.reclaimable));
    assert!(
        !inventory
            .entries
            .iter()
            .any(|entry| entry.kind == StorageEntryKind::Unknown)
    );
    let unknown = path.join("operator-note");
    fs::write(&unknown, b"preserve").unwrap();
    db.reclaim().unwrap();
    assert_eq!(fs::read(&unknown).unwrap(), b"preserve");
    assert!(
        !db.storage_inventory()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.reclaimable)
    );
    db.write(|tx| tx.insert(2, Row(84))).unwrap();
    db.checkpoint().unwrap();
    let backup = db.backup_to(root.join("backup")).unwrap();
    assert_eq!(backup.read().unwrap().sequence(), 2);
    assert_eq!(backup.read().unwrap().get(2).unwrap().0, 84);
    drop(backup);
    drop(db);
    for path in [path, root.join("backup")] {
        let restored = Database::<Row>::open_dir(path).unwrap();
        assert_eq!(restored.read().unwrap().sequence(), 2);
        assert_eq!(restored.read().unwrap().len(), 2);
        assert_eq!(restored.read().unwrap().get(1).unwrap().0, 42);
        assert_eq!(restored.read().unwrap().get(2).unwrap().0, 84);
    }
    println!(
        "verified unchanged CURRENT, acknowledged row, released lock, orphan reclamation, subsequent commit/checkpoint and independent backup reopen after kernel OOM"
    );
}
