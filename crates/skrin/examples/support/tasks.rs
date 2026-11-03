use skrin::catalog::{CatalogDatabase, CatalogRead};
use skrin::versioned::{CatalogSnapshotWrite, SnapshotOptions};
use skrin::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq, skrin::Record)]
#[skrin(table_id = 1, version = 1)]
pub struct Task {
    pub request_id: String,
    pub title: String,
    pub spec_md: String,
    pub status: String,
}

#[derive(Debug, PartialEq, Eq, skrin::Record)]
#[skrin(table_id = 2, version = 1)]
pub struct Comment {
    pub operation_id: String,
    pub task_id: u64,
    pub body: String,
}

skrin::catalog! {
    pub TaskStore, Row {
        schema: (0x5441_534b, 1),
        tables: { Tasks: Task, Comments: Comment },
        indexes: {
            ByRequest: Tasks { id: 1, version: 1, unique: true, key: str => |r| &r.request_id },
            ByTitle: Tasks { id: 2, version: 1, unique: false, key: str => |r| &r.title },
            ByStatus: Tasks { id: 3, version: 1, unique: false, key: str => |r| &r.status },
            ByTask: Comments { id: 4, version: 1, unique: false, key: u64 => |r| r.task_id },
            ByOperation: Comments { id: 5, version: 1, unique: true, key: str => |r| &r.operation_id }
        }
    }
}

pub fn options() -> SnapshotOptions {
    SnapshotOptions {
        max_snapshots: 8,
        max_pinned_bytes: 64 * 1024 * 1024,
    }
}

pub fn footprint(row: &Row) -> Result<u64> {
    Ok(std::mem::size_of_val(row) as u64
        + match row {
            Row::Tasks(r) => {
                (r.request_id.capacity()
                    + r.title.capacity()
                    + r.spec_md.capacity()
                    + r.status.capacity()) as u64
            }
            Row::Comments(r) => (r.operation_id.capacity() + r.body.capacity()) as u64,
        })
}

pub const SPEC: &str =
    "# Spara text 🦀\r\n\nÅäö / 日本語 / العربية\n```rust\nlet s = \"\\0\";\n```\n\0%_'\\";
pub const COMMENT: &str = "Klart ✅\nBevarar \0, radbrytningar och e\u{301}.";

pub fn seed(db: &CatalogDatabase<TaskStore>) -> Result<()> {
    db.write(|tx| {
        if !tx.matching(ByRequest, "tasks/create/42")?.is_empty() {
            return Ok(());
        }
        tx.insert::<Tasks>(
            42,
            Task {
                request_id: "tasks/create/42".into(),
                title: "Lagring/strängar 🦀".into(),
                spec_md: SPEC.into(),
                status: "open".into(),
            },
        )?;
        assert_eq!(tx.matching(ByRequest, "tasks/create/42")?[0].0, 42);
        Ok(())
    })
}

// IDs are assigned by the caller. This models one atomic application operation;
// network effects and retries belong outside the transaction closure.
pub fn complete(tx: &mut CatalogSnapshotWrite<'_, TaskStore>) -> Result<bool> {
    if !tx.matching(ByOperation, "tasks/complete/42")?.is_empty() {
        return Ok(false);
    }
    let id = tx
        .matching(ByRequest, "tasks/create/42")?
        .first()
        .map(|(id, _)| *id)
        .ok_or(Error::MissingKey(42))?;
    tx.edit::<Tasks>(id, |task| {
        task.status = "done".into();
        Ok(())
    })?;
    tx.insert::<Comments>(
        7,
        Comment {
            operation_id: "tasks/complete/42".into(),
            task_id: id,
            body: COMMENT.into(),
        },
    )?;
    assert!(tx.matching(ByStatus, "open")?.is_empty());
    assert_eq!(tx.matching(ByStatus, "done")?[0].0, id);
    Ok(true)
}

pub fn verify(read: &CatalogRead<'_, TaskStore>) -> Result<()> {
    let task = read.get::<Tasks>(42)?.unwrap();
    assert_eq!(task.spec_md, SPEC);
    assert_eq!(task.status, "done");
    assert_eq!(read.prefix(ByTitle, "Lagring/")?.next().unwrap().0, 42);
    let comment = read
        .matching(ByOperation, "tasks/complete/42")?
        .next()
        .unwrap()
        .1;
    assert_eq!(comment.body, COMMENT);
    assert_eq!(comment.task_id, 42);
    Ok(())
}
