//! SQLite is a benchmark adapter only. No SQL reaches the Skrin example/API.
use super::world::*;
use super::{BenchResult, invalid};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use std::path::Path;
use std::time::Duration;

pub fn open(path: &Path) -> BenchResult<Connection> {
    let connection =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(Duration::from_secs(30))?;
    connection.set_prepared_statement_cache_capacity(32);
    if !connection.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )? {
        return Err(invalid(
            "SQLite refused explicit checkpoint-on-close policy",
        ));
    }
    // Same caller-driven maintenance policy as Skrin. FULL is never downgraded.
    // mmap is off; a bounded 64 MiB SQLite page cache is reported in methodology.
    connection.execute_batch("PRAGMA synchronous=FULL; PRAGMA fullfsync=ON; PRAGMA checkpoint_fullfsync=ON; PRAGMA wal_autocheckpoint=0; PRAGMA cache_size=-65536; PRAGMA mmap_size=0;")?;
    let mode: String = connection.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    let sync: u64 = connection.query_row("PRAGMA synchronous", [], |r| r.get(0))?;
    let automatic: u64 = connection.query_row("PRAGMA wal_autocheckpoint", [], |r| r.get(0))?;
    if mode != "wal" || sync != 2 || automatic != 0 {
        return Err(invalid(
            "SQLite did not preserve WAL/FULL/explicit checkpoint settings",
        ));
    }
    for (pragma, expected) in [
        ("PRAGMA fullfsync", 1),
        ("PRAGMA checkpoint_fullfsync", 1),
        ("PRAGMA mmap_size", 0),
        ("PRAGMA cache_size", -65536),
    ] {
        let value: i64 = connection.query_row(pragma, [], |r| r.get(0))?;
        if value != expected {
            return Err(invalid("SQLite comparator setting mismatch"));
        }
    }
    Ok(connection)
}
pub fn create(path: &Path, rows: u64) -> BenchResult<Connection> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let connection =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let mode: String = connection.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    if mode != "wal" {
        return Err(invalid("SQLite refused WAL"));
    }
    drop(connection);
    let mut connection = open(path)?;
    connection.execute_batch("CREATE TABLE entities(id INTEGER PRIMARY KEY, area INTEGER NOT NULL, x INTEGER NOT NULL, y INTEGER NOT NULL, revision INTEGER NOT NULL); CREATE INDEX by_area ON entities(area); CREATE TABLE items(id INTEGER PRIMARY KEY, owner INTEGER NOT NULL, kind INTEGER NOT NULL); CREATE INDEX by_owner ON items(owner); CREATE TABLE saves(id INTEGER PRIMARY KEY, item INTEGER NOT NULL, from_owner INTEGER NOT NULL, to_owner INTEGER NOT NULL, first_entity INTEGER NOT NULL, entity_count INTEGER NOT NULL);")?;
    for begin in (0..rows).step_by(256) {
        let tx = connection.transaction()?;
        for key in begin..rows.min(begin + 256) {
            let e = entity(key);
            tx.prepare_cached("INSERT INTO entities VALUES(?1,?2,?3,?4,?5)")?
                .execute(params![key, e.area, e.x, e.y, e.revision])?;
            let i = item(key);
            tx.prepare_cached("INSERT INTO items VALUES(?1,?2,?3)")?
                .execute(params![key, i.owner, i.kind])?;
        }
        tx.commit()?;
    }
    Ok(connection)
}
fn decode_entity(r: &Row<'_>, start: usize) -> rusqlite::Result<Entity> {
    Ok(Entity {
        area: r.get(start)?,
        x: r.get(start + 1)?,
        y: r.get(start + 2)?,
        revision: r.get(start + 3)?,
    })
}
pub fn get_entity(c: &Connection, key: u64) -> BenchResult<Option<Entity>> {
    Ok(
        c.prepare_cached("SELECT area,x,y,revision FROM entities WHERE id=?1")?
            .query_row([key], |r| decode_entity(r, 0))
            .optional()?,
    )
}
pub fn get_item(c: &Connection, key: u64) -> BenchResult<Option<Item>> {
    Ok(
        c.prepare_cached("SELECT owner,kind FROM items WHERE id=?1")?
            .query_row([key], |r| {
                Ok(Item {
                    owner: r.get(0)?,
                    kind: r.get(1)?,
                })
            })
            .optional()?,
    )
}
pub fn get_saved(c: &Connection, id: u64) -> BenchResult<Option<Saved>> {
    Ok(c.prepare_cached(
        "SELECT item,from_owner,to_owner,first_entity,entity_count FROM saves WHERE id=?1",
    )?
    .query_row([id], |r| {
        Ok(Saved {
            item: r.get(0)?,
            from: r.get(1)?,
            to: r.get(2)?,
            first: r.get(3)?,
            count: r.get(4)?,
        })
    })
    .optional()?)
}
pub fn area(c: &Connection, key: u64) -> BenchResult<Vec<(u64, Entity)>> {
    Ok(
        c.prepare_cached("SELECT id,area,x,y,revision FROM entities WHERE area=?1 ORDER BY id")?
            .query_map([key], |r| Ok((r.get(0)?, decode_entity(r, 1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    )
}
pub fn inventory(c: &Connection, key: u64) -> BenchResult<Vec<(u64, Item)>> {
    Ok(
        c.prepare_cached("SELECT id,owner,kind FROM items WHERE owner=?1 ORDER BY id")?
            .query_map([key], |r| {
                Ok((
                    r.get(0)?,
                    Item {
                        owner: r.get(1)?,
                        kind: r.get(2)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    )
}
pub fn saved_count(c: &Connection) -> BenchResult<u64> {
    Ok(c.prepare_cached("SELECT COALESCE(MAX(id)+1,0) FROM saves")?
        .query_row([], |r| r.get(0))?)
}
pub fn counts(c: &Connection) -> BenchResult<(u64, u64, u64)> {
    Ok(c.query_row("SELECT (SELECT COUNT(*) FROM entities),(SELECT COUNT(*) FROM items),(SELECT COUNT(*) FROM saves)", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?)
}
pub fn save(c: &mut Connection, request: Save) -> BenchResult<bool> {
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(old) = get_saved(&tx, request.id)? {
        if old != request.saved() {
            return Err(invalid("save ID reused for another request"));
        }
        tx.commit()?;
        return Ok(false);
    }
    for offset in 0..request.batch {
        let key = (request.first() + offset) % request.rows;
        // Same application get/modify/replace path and values as Skrin. Do not
        // substitute SQL arithmetic for the typed world workload.
        let mut e = get_entity(&tx, key)?.ok_or_else(|| invalid("missing entity"))?;
        e.x += 1;
        e.revision += 1;
        assert_eq!(
            tx.prepare_cached("UPDATE entities SET area=?2,x=?3,y=?4,revision=?5 WHERE id=?1")?
                .execute(params![key, e.area, e.x, e.y, e.revision])?,
            1
        );
    }
    let mut i = get_item(&tx, request.item())?.ok_or_else(|| invalid("missing item"))?;
    if i.owner != request.from() || get_entity(&tx, request.to())?.is_none() {
        return Err(invalid("unexpected item owner or missing recipient"));
    }
    i.owner = request.to();
    assert_eq!(
        tx.prepare_cached("UPDATE items SET owner=?2,kind=?3 WHERE id=?1")?
            .execute(params![request.item(), i.owner, i.kind])?,
        1
    );
    let s = request.saved();
    tx.prepare_cached("INSERT INTO saves VALUES(?1,?2,?3,?4,?5,?6)")?
        .execute(params![request.id, s.item, s.from, s.to, s.first, s.count])?;
    tx.commit()?;
    Ok(true)
}
pub fn read(c: &Connection) -> BenchResult<Transaction<'_>> {
    Ok(c.unchecked_transaction()?)
}
pub fn checkpoint(c: &Connection) -> BenchResult<()> {
    let (busy, wal, done): (u64, i64, i64) =
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    if busy != 0 || wal != 0 || done != 0 {
        return Err(invalid("SQLite checkpoint incomplete"));
    }
    Ok(())
}
