//! Varied resident queries with prepared covering SQLite controls. No durability
//! comparison; every materialized result is checked outside its timing.
use skrin::catalog::CatalogDatabase;
use skrin::versioned::SnapshotOptions;
use skrin::{Error, Result};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, skrin::Record)]
#[skrin(table_id = 1, version = 1)]
struct Entity {
    area: u64,
    x: u64,
    y: u64,
    revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, skrin::Record)]
#[skrin(table_id = 2, version = 1)]
struct Item {
    owner: u64,
    kind: u64,
}
skrin::catalog! {
    Queries, Row {
        schema: (13000, 1), tables: { Entities: Entity, Items: Item },
        indexes: {
            Area: Entities { id: 1, version: 1, unique: false, key: u64 => |r| r.area },
            Owner: Items { id: 2, version: 1, unique: false, key: u64 => |r| r.owner },
            OwnerKind: Items { id: 3, version: 1, unique: false, key: (u64, u64) => |r| (r.owner, r.kind) }
        }
    }
}
fn entity(id: u64) -> Entity {
    Entity {
        area: id / 64,
        x: id % 1000 + u64::from(id < 64),
        y: id / 1000,
        revision: u64::from(id < 64),
    }
}
fn item(id: u64) -> Item {
    Item {
        owner: id / 64,
        kind: id % 16,
    }
}
#[derive(Clone, Copy)]
struct Request {
    area: u64,
    owner: u64,
    entity_after: u64,
    item_after: u64,
    kind: u64,
}
fn request(i: u64, rows: u64) -> Request {
    let groups = rows.div_ceil(64);
    let area = i * 7919 % groups;
    let owner = i * 3571 % groups;
    Request {
        area,
        owner,
        entity_after: area * 64 + i * 17 % 64,
        item_after: owner * 64 + i * 31 % 48,
        kind: i * 13 % 16,
    }
}
type EntityRows = Vec<(u64, Entity)>;
type JoinedRows = Vec<(u64, Item, Entity)>;
fn bounded(rows: u64, r: Request) -> EntityRows {
    (r.area * 64..rows.min((r.area + 2) * 64))
        .map(|id| (id, entity(id)))
        .collect()
}
fn filtered(rows: u64, r: Request) -> EntityRows {
    bounded(rows, r)
        .into_iter()
        .filter(|(_, e)| e.x % 2 == 0)
        .take(16)
        .collect()
}
fn page(rows: u64, r: Request) -> EntityRows {
    bounded(rows, r)
        .into_iter()
        .filter(|(id, e)| *id > r.entity_after && e.x % 2 == 0)
        .take(16)
        .collect()
}
fn joined(rows: u64, r: Request) -> JoinedRows {
    (r.owner * 64..rows.min((r.owner + 1) * 64))
        .filter(|id| *id > r.item_after && id % 16 == r.kind)
        .take(4)
        .map(|id| (id, item(id), entity(r.owner)))
        .collect()
}
fn measure<T: PartialEq + std::fmt::Debug>(
    mode: &str,
    workload: &str,
    rows: u64,
    count: usize,
    mut query: impl FnMut(Request) -> Result<Vec<T>>,
    expected: fn(u64, Request) -> Vec<T>,
) -> Result<()> {
    for i in count..count + 128 {
        let r = request(i as u64, rows);
        assert_eq!(query(r)?, expected(rows, r));
    }
    let mut samples = Vec::with_capacity(count);
    let mut returned = 0;
    for i in 0..count {
        let r = request(i as u64, rows);
        let start = Instant::now();
        let result = query(std::hint::black_box(r))?;
        samples.push(start.elapsed());
        returned += result.len();
        assert_eq!(result, expected(rows, r));
    }
    samples.sort_unstable();
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    println!(
        "query,mode={mode},workload={workload},rows={rows},samples={count},returned_rows={returned},p50_us={:.3},p99_us={:.3},max_us={:.3},exact_full_rows=verified",
        us(samples[(count - 1) / 2]),
        us(samples[(count * 99).div_ceil(100) - 1]),
        us(*samples.last().unwrap())
    );
    Ok(())
}
macro_rules! native_queries {
    ($view:expr, $mode:expr, $rows:expr, $count:expr) => {{
        let view = $view;
        measure(
            $mode,
            "bounded",
            $rows,
            $count,
            |r| {
                Ok(view
                    .query(Area, r.area..=r.area + 1)?
                    .map(|(id, e)| (id, *e))
                    .collect())
            },
            bounded,
        )?;
        measure(
            $mode,
            "filtered",
            $rows,
            $count,
            |r| {
                Ok(view
                    .query(Area, r.area..=r.area + 1)?
                    .filter(|(_, e)| e.x % 2 == 0)
                    .take(16)
                    .map(|(id, e)| (id, *e))
                    .collect())
            },
            filtered,
        )?;
        measure(
            $mode,
            "page",
            $rows,
            $count,
            |r| {
                Ok(view
                    .query_after(Area, r.area..=r.area + 1, (&r.area, r.entity_after))?
                    .filter(|(_, e)| e.x % 2 == 0)
                    .take(16)
                    .map(|(id, e)| (id, *e))
                    .collect())
            },
            page,
        )?;
        measure(
            $mode,
            "owner_join",
            $rows,
            $count,
            |r| {
                let mut owner = None;
                view.query_after(Owner, r.owner..=r.owner, (&r.owner, r.item_after))?
                    .filter(|(_, i)| i.kind == r.kind)
                    .take(4)
                    .map(|(id, i)| {
                        if owner.is_none() {
                            owner = view.get::<Entities>(i.owner)?.copied();
                        }
                        Ok((
                            id,
                            *i,
                            owner.ok_or_else(|| Error::InvalidOperation("missing owner".into()))?,
                        ))
                    })
                    .collect()
            },
            joined,
        )?;
        measure(
            $mode,
            "compound_join",
            $rows,
            $count,
            |r| {
                let key = (r.owner, r.kind);
                let mut owner = None;
                view.query_after(OwnerKind, key..=key, (&key, r.item_after))?
                    .take(4)
                    .map(|(id, i)| {
                        if owner.is_none() {
                            owner = view.get::<Entities>(i.owner)?.copied();
                        }
                        Ok((
                            id,
                            *i,
                            owner.ok_or_else(|| Error::InvalidOperation("missing owner".into()))?,
                        ))
                    })
                    .collect()
            },
            joined,
        )?;
    }};
}
fn main() -> Result<()> {
    let raw: Vec<_> = std::env::args().skip(1).collect();
    let sqlite_first = raw.iter().any(|a| a == "--sqlite-first");
    let args: Vec<_> = raw
        .into_iter()
        .filter(|a| a != "--bench" && a != "--sqlite-first")
        .collect();
    let (rows, count) = match args.as_slice() {
        [] => (Some(10000), Some(2000)),
        [n] => (n.parse().ok(), Some(2000)),
        [n, c] => (n.parse().ok(), c.parse().ok()),
        _ => (None, None),
    };
    let (rows, count) = match (rows, count) {
        (Some(n), Some(c)) if (64..=1000000).contains(&n) && (16..=20000).contains(&c) => (n, c),
        _ => {
            return Err(Error::InvalidOperation(
                "usage: game_queries [ROWS=64..1000000 [SAMPLES=16..20000]] [--sqlite-first]"
                    .into(),
            ));
        }
    };
    println!(
        "workload,resident_in_memory=true,durability=none,varied_requests=true,full_rows_materialized=true,owner_group=64,kinds=16,filter_page_limit=16,join_limit=4,setup_warmup_verification_outside_timing=true"
    );
    if sqlite_first {
        sqlite(rows, count)?;
    }
    let db = CatalogDatabase::<Queries>::in_memory()?;
    for begin in (0..rows).step_by(256) {
        db.write(|tx| {
            for id in begin..rows.min(begin + 256) {
                tx.insert::<Entities>(id, entity(id))?;
                tx.insert::<Items>(id, item(id))?;
            }
            Ok(())
        })?;
    }
    {
        let read = db.read()?;
        assert_eq!(read.sequence(), rows.div_ceil(256));
        for id in 0..rows {
            assert_eq!(read.get::<Entities>(id)?, Some(&entity(id)));
            assert_eq!(read.get::<Items>(id)?, Some(&item(id)));
        }
        native_queries!(&read, "native", rows, count);
    }
    let db = db.into_snapshots(
        SnapshotOptions {
            max_snapshots: 1,
            max_pinned_bytes: 1024 * 1024 * 1024,
        },
        |_| Ok(std::mem::size_of::<Row>() as u64),
    )?;
    let frame = db.snapshot()?;
    native_queries!(&frame, "snapshot", rows, count);
    assert_eq!(frame.sequence()?, rows.div_ceil(256));
    drop(frame);
    drop(db);
    if !sqlite_first {
        sqlite(rows, count)?;
    }
    Ok(())
}
fn sql<T>(r: rusqlite::Result<T>) -> Result<T> {
    r.map_err(|e| Error::InvalidOperation(format!("SQLite benchmark: {e}")))
}
fn entity_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(u64, Entity)> {
    Ok((
        r.get(0)?,
        Entity {
            area: r.get(1)?,
            x: r.get(2)?,
            y: r.get(3)?,
            revision: r.get(4)?,
        },
    ))
}
const BOUNDED: &str =
    "SELECT id,area,x,y,revision FROM entities WHERE area>=?1 AND area<=?2 ORDER BY area,id";
const FILTERED: &str = "SELECT id,area,x,y,revision FROM entities WHERE area>=?1 AND area<=?2 AND x%2=0 ORDER BY area,id LIMIT 16";
const PAGE: &str = "SELECT id,area,x,y,revision FROM entities WHERE area=?1 AND id>?2 AND x%2=0 UNION ALL SELECT id,area,x,y,revision FROM entities WHERE area>?1 AND area<=?3 AND x%2=0 ORDER BY area,id LIMIT 16";
const JOIN: &str = "SELECT i.id,i.owner,i.kind,e.area,e.x,e.y,e.revision FROM items i JOIN entities e ON e.id=i.owner WHERE i.owner=?1 AND i.kind=?2 AND i.id>?3 ORDER BY i.id LIMIT 4";
fn plan(
    conn: &rusqlite::Connection,
    name: &str,
    query: &str,
    args: &[u64],
    index: &str,
    seek: &str,
) -> Result<()> {
    let mut stmt = sql(conn.prepare(&format!("EXPLAIN QUERY PLAN {query}")))?;
    let steps = sql(sql(
        stmt.query_map(rusqlite::params_from_iter(args), |r| r.get::<_, String>(3))
    )?
    .collect::<rusqlite::Result<Vec<_>>>())?;
    assert!(
        steps.iter().any(|s| s.contains(index) && s.contains(seek)),
        "{name}: {steps:?}"
    );
    assert!(
        !steps
            .iter()
            .any(|s| s.contains("TEMP B-TREE") || s.starts_with("SCAN")),
        "{name}: {steps:?}"
    );
    println!("sqlite_plan,workload={name},steps={steps:?}");
    Ok(())
}
fn sqlite(rows: u64, count: usize) -> Result<()> {
    let mut conn = sql(rusqlite::Connection::open_in_memory())?;
    sql(conn.execute_batch("CREATE TABLE entities(id INTEGER PRIMARY KEY,area INTEGER NOT NULL,x INTEGER NOT NULL,y INTEGER NOT NULL,revision INTEGER NOT NULL); CREATE INDEX entities_area ON entities(area,id,x,y,revision); CREATE TABLE items(id INTEGER PRIMARY KEY,owner INTEGER NOT NULL,kind INTEGER NOT NULL); CREATE INDEX items_owner_kind ON items(owner,kind,id);"))?;
    {
        let tx = sql(conn.transaction())?;
        {
            let mut e = sql(tx.prepare("INSERT INTO entities VALUES (?1,?2,?3,?4,?5)"))?;
            let mut i = sql(tx.prepare("INSERT INTO items VALUES (?1,?2,?3)"))?;
            for id in 0..rows {
                let row = entity(id);
                let it = item(id);
                sql(e.execute(rusqlite::params![id, row.area, row.x, row.y, row.revision]))?;
                sql(i.execute(rusqlite::params![id, it.owner, it.kind]))?;
            }
        }
        sql(tx.commit())?;
    }
    {
        let mut q = sql(conn.prepare("SELECT id,area,x,y,revision FROM entities ORDER BY id"))?;
        let mut result = sql(q.query([]))?;
        for id in 0..rows {
            assert_eq!(
                entity_row(sql(result.next())?.unwrap())
                    .map_err(|e| Error::Codec(e.to_string()))?,
                (id, entity(id))
            );
        }
        assert!(sql(result.next())?.is_none());
        let mut q = sql(conn.prepare("SELECT id,owner,kind FROM items ORDER BY id"))?;
        let mut result = sql(q.query([]))?;
        for id in 0..rows {
            let r = sql(result.next())?.unwrap();
            assert_eq!(
                (
                    sql(r.get::<_, u64>(0))?,
                    sql(r.get::<_, u64>(1))?,
                    sql(r.get::<_, u64>(2))?
                ),
                (id, item(id).owner, item(id).kind)
            );
        }
        assert!(sql(result.next())?.is_none());
    }
    println!(
        "sqlite_control,version={},prepared_statement_reused=true,covering_indexes=true,transaction_seed_outside_timing=true",
        rusqlite::version()
    );
    plan(
        &conn,
        "bounded",
        BOUNDED,
        &[0, 1],
        "USING COVERING INDEX entities_area",
        "area>?",
    )?;
    plan(
        &conn,
        "filtered",
        FILTERED,
        &[0, 1],
        "USING COVERING INDEX entities_area",
        "area>?",
    )?;
    plan(
        &conn,
        "page",
        PAGE,
        &[0, 32, 1],
        "USING COVERING INDEX entities_area",
        "area=? AND id>?",
    )?;
    plan(
        &conn,
        "compound_join",
        JOIN,
        &[0, 0, 0],
        "USING COVERING INDEX items_owner_kind",
        "owner=? AND kind=? AND id>?",
    )?;
    for (name, text, expected) in [
        (
            "bounded",
            BOUNDED,
            bounded as fn(u64, Request) -> EntityRows,
        ),
        ("filtered", FILTERED, filtered),
    ] {
        let mut q = sql(conn.prepare(text))?;
        measure(
            "sqlite_covering",
            name,
            rows,
            count,
            |r| {
                sql(sql(q.query_map([r.area, r.area + 1], entity_row))?
                    .collect::<rusqlite::Result<_>>())
            },
            expected,
        )?;
    }
    let mut q = sql(conn.prepare(PAGE))?;
    measure(
        "sqlite_covering",
        "page",
        rows,
        count,
        |r| {
            sql(
                sql(q.query_map([r.area, r.entity_after, r.area + 1], entity_row))?
                    .collect::<rusqlite::Result<_>>(),
            )
        },
        page,
    )?;
    let mut q = sql(conn.prepare(JOIN))?;
    measure(
        "sqlite_covering",
        "compound_join",
        rows,
        count,
        |r| {
            sql(sql(q.query_map([r.owner, r.kind, r.item_after], |r| {
                Ok((
                    r.get(0)?,
                    Item {
                        owner: r.get(1)?,
                        kind: r.get(2)?,
                    },
                    Entity {
                        area: r.get(3)?,
                        x: r.get(4)?,
                        y: r.get(5)?,
                        revision: r.get(6)?,
                    },
                ))
            }))?
            .collect::<rusqlite::Result<_>>())
        },
        joined,
    )
}
