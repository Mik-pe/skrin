//! Functional comparison of complete native and external SQL game journeys.
//! SQL is confined to this development-only control; no timings are compared.
use skrin::catalog::CatalogDatabase;
use skrin::{Error, Result};
#[derive(Clone, Debug, PartialEq, skrin::Record)]
#[skrin(table_id = 1, version = 1)]
struct Character {
    name: String,
    x: i32,
    y: i32,
    health: f32,
    alive: bool,
    team: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, skrin::Record)]
#[skrin(table_id = 2, version = 1)]
struct Item {
    owner: u64,
    kind: u64,
}
skrin::catalog! {
    Game,Row {
        schema:(14000,1),tables:{Characters:Character,Items:Item},
        indexes:{
            Position:Characters { id:1,version:1,unique:false,key:i32 => |r|r.x },
            OwnerKind:Items { id:2,version:1,unique:false,key:(u64,u64) => |r|(r.owner,r.kind) }
        }
    }
}
fn player(id: u64, edited: bool) -> Character {
    Character {
        name: format!("player-{id}"),
        x: if id == 7 {
            if edited { 128 } else { -32 }
        } else {
            16
        },
        y: -8,
        health: if id == 7 && edited { 0.0 } else { 100.0 },
        alive: id != 7 || !edited,
        team: if id == 7 { None } else { Some(42) },
    }
}
fn item(id: u64, edited: bool) -> Item {
    Item {
        owner: if id == 101 && edited { 9 } else { 7 },
        kind: if id == 101 { 1 } else { 2 },
    }
}
fn native_verify(db: &CatalogDatabase<Game>, edited: bool) -> Result<()> {
    let read = db.read()?;
    assert_eq!(read.sequence(), if edited { 2 } else { 1 });
    assert_eq!(read.scan::<Characters>()?.count(), 2);
    assert_eq!(read.scan::<Items>()?.count(), 2);
    for id in [7, 9] {
        assert_eq!(read.get::<Characters>(id)?, Some(&player(id, edited)));
    }
    for id in [101, 103] {
        assert_eq!(read.get::<Items>(id)?, Some(&item(id, edited)));
    }
    let visible = read
        .query(Position, -64..64)?
        .filter(|(_, p)| p.y >= -16 && p.alive && p.health > 0.0)
        .take(8)
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    assert_eq!(visible, if edited { vec![9] } else { vec![7, 9] });
    let owner = if edited { 9 } else { 7 };
    let joined = read
        .matching(OwnerKind, &(owner, 1))?
        .take(4)
        .map(|(id, it)| {
            Ok((
                id,
                it,
                read.get::<Characters>(it.owner)?
                    .ok_or_else(|| Error::InvalidOperation("missing owner".into()))?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(
        joined,
        vec![(101, &item(101, edited), &player(owner, edited))]
    );
    Ok(())
}
fn native(path: &std::path::Path, create: bool) -> Result<()> {
    let db = if create {
        CatalogDatabase::<Game>::create_dir(path)?
    } else {
        CatalogDatabase::<Game>::open_dir(path)?
    };
    if create {
        db.write(|tx| {
            for id in [7, 9] {
                tx.insert::<Characters>(id, player(id, false))?;
            }
            for id in [101, 103] {
                tx.insert::<Items>(id, item(id, false))?;
            }
            Ok(())
        })?;
        native_verify(&db, false)?;
        db.write(|tx| {
            tx.edit::<Characters>(7, |p| {
                p.x = 128;
                p.health = 0.0;
                p.alive = false;
                Ok(())
            })?;
            tx.edit::<Items>(101, |i| {
                i.owner = 9;
                Ok(())
            })
        })?;
        native_verify(&db, true)?;
        let rejected = db.write(|tx| {
            tx.edit::<Characters>(9, |p| {
                p.health = 0.0;
                p.alive = false;
                Ok(())
            })?;
            tx.edit::<Items>(103, |i| {
                i.owner = 9;
                Ok(())
            })?;
            Err::<(), _>(Error::InvalidOperation("rejected edit".into()))
        });
        assert!(matches!(rejected,Err(Error::InvalidOperation(s)) if s=="rejected edit"));
    }
    native_verify(&db, true)?;
    println!(
        "native,phase={},full_models_and_join=verified,atomic_edit=verified,error_rollback=verified",
        if create { "create" } else { "verify" }
    );
    Ok(())
}
fn sql<T>(r: rusqlite::Result<T>) -> Result<T> {
    r.map_err(|e| Error::InvalidOperation(format!("SQLite comparison: {e}")))
}
fn sql_character(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Character> {
    Ok(Character {
        name: row.get(offset)?,
        x: row.get(offset + 1)?,
        y: row.get(offset + 2)?,
        health: row.get(offset + 3)?,
        alive: row.get(offset + 4)?,
        team: row.get(offset + 5)?,
    })
}
fn sqlite_verify(conn: &rusqlite::Connection, edited: bool) -> Result<()> {
    let mut q =
        sql(conn.prepare("SELECT id,name,x,y,health,alive,team FROM characters ORDER BY id"))?;
    let rows = sql(
        sql(q.query_map([], |r| Ok((r.get::<_, u64>(0)?, sql_character(r, 1)?))))?
            .collect::<rusqlite::Result<Vec<_>>>(),
    )?;
    assert_eq!(rows, vec![(7, player(7, edited)), (9, player(9, edited))]);
    let mut q = sql(conn.prepare("SELECT id,owner,kind FROM items ORDER BY id"))?;
    let rows = sql(sql(q.query_map([], |r| {
        Ok((
            r.get::<_, u64>(0)?,
            Item {
                owner: r.get(1)?,
                kind: r.get(2)?,
            },
        ))
    }))?
    .collect::<rusqlite::Result<Vec<_>>>())?;
    assert_eq!(
        rows,
        vec![(101, item(101, edited)), (103, item(103, edited))]
    );
    let mut q=sql(conn.prepare("SELECT id FROM characters WHERE x>=?1 AND x<?2 AND y>=?3 AND alive=1 AND health>0 ORDER BY x,id LIMIT 8"))?;
    let rows = sql(sql(q.query_map([-64, 64, -16], |r| r.get::<_, u64>(0)))?
        .collect::<rusqlite::Result<Vec<_>>>())?;
    assert_eq!(rows, if edited { vec![9] } else { vec![7, 9] });
    let owner = if edited { 9 } else { 7 };
    let mut q=sql(conn.prepare("SELECT i.id,i.owner,i.kind,p.name,p.x,p.y,p.health,p.alive,p.team FROM items i JOIN characters p ON p.id=i.owner WHERE i.owner=?1 AND i.kind=?2 ORDER BY i.id LIMIT 4"))?;
    let rows = sql(sql(q.query_map([owner, 1], |r| {
        Ok((
            r.get::<_, u64>(0)?,
            Item {
                owner: r.get(1)?,
                kind: r.get(2)?,
            },
            sql_character(r, 3)?,
        ))
    }))?
    .collect::<rusqlite::Result<Vec<_>>>())?;
    assert_eq!(rows, vec![(101, item(101, edited), player(owner, edited))]);
    Ok(())
}
fn sqlite(path: &std::path::Path, create: bool) -> Result<()> {
    if create {
        assert!(!path.exists());
    } else {
        assert!(path.is_file());
    }
    let mut conn = sql(rusqlite::Connection::open(path))?;
    sql(conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA fullfsync=ON; PRAGMA checkpoint_fullfsync=ON;"))?;
    if create {
        sql(conn.execute_batch("CREATE TABLE characters(id INTEGER PRIMARY KEY,name TEXT NOT NULL,x INTEGER NOT NULL,y INTEGER NOT NULL,health REAL NOT NULL,alive INTEGER NOT NULL CHECK(alive IN(0,1)),team INTEGER);CREATE INDEX character_position ON characters(x,id);CREATE TABLE items(id INTEGER PRIMARY KEY,owner INTEGER NOT NULL,kind INTEGER NOT NULL);CREATE INDEX item_owner_kind ON items(owner,kind,id);"))?;
        let tx = sql(conn.transaction())?;
        for id in [7, 9] {
            let p = player(id, false);
            sql(tx.execute(
                "INSERT INTO characters VALUES(?1,?2,?3,?4,?5,?6,?7)",
                rusqlite::params![id, p.name, p.x, p.y, p.health, p.alive, p.team],
            ))?;
        }
        for id in [101, 103] {
            let i = item(id, false);
            sql(tx.execute(
                "INSERT INTO items VALUES(?1,?2,?3)",
                rusqlite::params![id, i.owner, i.kind],
            ))?;
        }
        sql(tx.commit())?;
        sqlite_verify(&conn, false)?;
        let tx = sql(conn.transaction())?;
        assert_eq!(
            sql(tx.execute(
                "UPDATE characters SET x=128,health=0,alive=0 WHERE id=7",
                []
            ))?,
            1
        );
        assert_eq!(
            sql(tx.execute("UPDATE items SET owner=9 WHERE id=101", []))?,
            1
        );
        sql(tx.commit())?;
        sqlite_verify(&conn, true)?;
        // This application error propagates through an ordinary Rust closure.
        // rusqlite's default transaction DropBehavior rolls back its staging.
        let rejected = (|| -> Result<()> {
            let tx = sql(conn.transaction())?;
            assert_eq!(
                sql(tx.execute("UPDATE characters SET health=0,alive=0 WHERE id=9", []))?,
                1
            );
            assert_eq!(
                sql(tx.execute("UPDATE items SET owner=9 WHERE id=103", []))?,
                1
            );
            Err(Error::InvalidOperation("rejected edit".into()))
        })();
        assert!(matches!(rejected,Err(Error::InvalidOperation(s)) if s=="rejected edit"));
    }
    sqlite_verify(&conn, true)?;
    println!(
        "sqlite,phase={},full_models_and_join=verified,atomic_edit=verified,error_with_transaction_drop_rollback=verified",
        if create { "create" } else { "verify" }
    );
    Ok(())
}
fn main() -> Result<()> {
    let args = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect::<Vec<_>>();
    if args.is_empty() {
        #[cfg(unix)]
        {
            let root = std::env::temp_dir().join(format!(
                "skrin-api-journey-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| Error::InvalidOperation(e.to_string()))?
                    .as_nanos()
            ));
            std::fs::create_dir(&root)?;
            let executable = std::env::current_exe()?;
            for (engine, path) in [
                ("native", root.join("native")),
                ("sqlite", root.join("sqlite.db")),
            ] {
                for phase in ["create", "verify"] {
                    let result = std::process::Command::new(&executable)
                        .arg(phase)
                        .arg(engine)
                        .arg(&path)
                        .status()?;
                    if !result.success() {
                        return Err(Error::InvalidOperation(format!(
                            "{engine} {phase} failed; evidence retained at {}",
                            root.display()
                        )));
                    }
                }
            }
            std::fs::remove_dir_all(root)?;
            println!(
                "journeys,independent_process_reopen=verified,durable_create_save_and_error_rollback=verified,no_performance_claim=true"
            );
            return Ok(());
        }
        #[cfg(not(unix))]
        {
            return Err(Error::UnsupportedPlatform);
        }
    }
    if args.len() != 3 || !["create", "verify"].contains(&args[0].as_str()) {
        return Err(Error::InvalidOperation(
            "usage: api_journey [create|verify native|sqlite PATH]".into(),
        ));
    }
    let path = std::path::Path::new(&args[2]);
    match args[1].as_str() {
        "native" => native(path, args[0] == "create"),
        "sqlite" => sqlite(path, args[0] == "create"),
        _ => Err(Error::InvalidOperation("unknown comparison engine".into())),
    }
}
