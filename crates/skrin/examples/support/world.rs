//! Application schema shared by the executable example and comparison harness.
use skrin::catalog::{CatalogDatabase, CatalogWrite, Table};
use skrin::versioned::{CatalogSnapshotWrite, SnapshotOptions};
use skrin::{Error, Result};

pub const AREA_SIZE: u64 = 64;

// Integer positions are in millimetres. Disk bytes are explicit little-endian
// u64 fields; index keys use big endian for lexicographic numeric ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, skrin::Record)]
#[skrin(table_id = 1, version = 1)]
pub struct Entity {
    pub area: u64,
    pub x: u64,
    pub y: u64,
    pub revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, skrin::Record)]
#[skrin(table_id = 2, version = 1)]
pub struct Item {
    pub owner: u64,
    pub kind: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, skrin::Record)]
#[skrin(table_id = 3, version = 1)]
pub struct Saved {
    pub item: u64,
    pub from: u64,
    pub to: u64,
    pub first: u64,
    pub count: u64,
}

skrin::catalog! {
    pub World, #[derive(Debug)] Row {
        schema: (9000, 1),
        tables: { Entities: Entity, Items: Item, Saves: Saved },
        indexes: {
            AREA_INDEX: Entities {
                id: 1, version: 1, unique: false,
                key: |row: &Entity| Ok(row.area.to_be_bytes().to_vec())
            },
            OWNER_INDEX: Items {
                id: 2, version: 1, unique: false,
                key: |row: &Item| Ok(row.owner.to_be_bytes().to_vec())
            }
        }
    }
}

pub fn entity(key: u64) -> Entity {
    Entity {
        area: key / AREA_SIZE,
        x: key % 1000,
        y: key / 1000,
        revision: 0,
    }
}
pub fn item(key: u64) -> Item {
    Item {
        owner: key,
        kind: key % 16,
    }
}
pub fn seed(db: &CatalogDatabase<World>, rows: u64) -> Result<()> {
    for begin in (0..rows).step_by(256) {
        db.write(|tx| {
            for key in begin..rows.min(begin + 256) {
                tx.insert::<Entities>(key, entity(key))?;
                tx.insert::<Items>(key, item(key))?;
            }
            Ok(())
        })?;
    }
    Ok(())
}
pub fn footprint(_: &Row) -> Result<u64> {
    // All variants own only inline integers; no external allocations.
    Ok(std::mem::size_of::<Row>() as u64)
}
pub fn snapshot_options() -> SnapshotOptions {
    SnapshotOptions {
        max_snapshots: 4,
        max_pinned_bytes: 512 * 1024 * 1024,
    }
}

/// One deterministic application request. An ID describes the entire request,
/// including the entity range, so a mismatched retry is refused.
#[derive(Clone, Copy)]
pub struct Save {
    pub id: u64,
    pub rows: u64,
    pub batch: u64,
}
impl Save {
    pub fn first(self) -> u64 {
        (self.id * self.batch) % self.rows
    }
    pub fn item(self) -> u64 {
        self.id % self.rows
    }
    pub fn from(self) -> u64 {
        (self.item() + self.id / self.rows) % self.rows
    }
    pub fn to(self) -> u64 {
        (self.from() + 1) % self.rows
    }
    pub fn saved(self) -> Saved {
        Saved {
            item: self.item(),
            from: self.from(),
            to: self.to(),
            first: self.first(),
            count: self.batch,
        }
    }
}

// Application-side adapter only, keeping the same save logic in native and
// immutable-snapshot workloads. It is not a new Skrin public API.
pub trait WorldWrite {
    fn get<T: Table<World>>(&self, key: u64) -> Result<Option<&T::Record>>;
    fn put<T: Table<World>>(&mut self, key: u64, row: T::Record) -> Result<()>;
    fn insert<T: Table<World>>(&mut self, key: u64, row: T::Record) -> Result<()>;
}
macro_rules! writer {
    ($ty:ident) => {
        impl WorldWrite for $ty<'_, World> {
            fn get<T: Table<World>>(&self, key: u64) -> Result<Option<&T::Record>> {
                self.get::<T>(key)
            }
            fn put<T: Table<World>>(&mut self, key: u64, row: T::Record) -> Result<()> {
                self.put::<T>(key, row)
            }
            fn insert<T: Table<World>>(&mut self, key: u64, row: T::Record) -> Result<()> {
                self.insert::<T>(key, row)
            }
        }
    };
}
writer!(CatalogWrite);
writer!(CatalogSnapshotWrite);

pub fn save(tx: &mut impl WorldWrite, request: Save) -> Result<bool> {
    if request.rows < 2 || request.batch == 0 || request.batch > request.rows {
        return Err(Error::InvalidOperation("invalid world save size".into()));
    }
    if let Some(old) = tx.get::<Saves>(request.id)? {
        if *old != request.saved() {
            return Err(Error::InvalidOperation(
                "save ID reused for another request".into(),
            ));
        }
        return Ok(false);
    }
    for offset in 0..request.batch {
        let key = (request.first() + offset) % request.rows;
        let mut row = *tx
            .get::<Entities>(key)?
            .ok_or_else(|| Error::InvalidOperation("missing entity".into()))?;
        row.x = row
            .x
            .checked_add(1)
            .ok_or_else(|| Error::InvalidOperation("position overflow".into()))?;
        row.revision += 1;
        tx.put::<Entities>(key, row)?;
    }
    let mut row = *tx
        .get::<Items>(request.item())?
        .ok_or_else(|| Error::InvalidOperation("missing item".into()))?;
    if row.owner != request.from() || tx.get::<Entities>(request.to())?.is_none() {
        return Err(Error::InvalidOperation(
            "unexpected item owner or missing recipient".into(),
        ));
    }
    row.owner = request.to();
    tx.put::<Items>(request.item(), row)?;
    tx.insert::<Saves>(request.id, request.saved())?;
    Ok(true)
}

// Closed-form reference results avoid retaining a second full world in memory.
pub fn expected_entity(key: u64, rows: u64, saves: u64, batch: u64) -> Entity {
    let updates = saves * batch;
    let count = updates / rows + u64::from(key < updates % rows);
    let mut row = entity(key);
    row.x += count;
    row.revision = count;
    row
}
pub fn expected_item(key: u64, rows: u64, saves: u64) -> Item {
    let moves = saves / rows + u64::from(key < saves % rows);
    Item {
        owner: (key + moves) % rows,
        ..item(key)
    }
}
pub fn verify(db: &CatalogDatabase<World>, rows: u64, saves: u64, batch: u64) -> Result<()> {
    let read = db.read()?;
    assert_eq!(read.sequence(), rows.div_ceil(256) + saves);
    assert_eq!(read.scan::<Entities>()?.count() as u64, rows);
    assert_eq!(read.scan::<Items>()?.count() as u64, rows);
    assert_eq!(read.scan::<Saves>()?.count() as u64, saves);
    for key in 0..rows {
        assert_eq!(
            *read.get::<Entities>(key)?.unwrap(),
            expected_entity(key, rows, saves, batch)
        );
        assert_eq!(
            *read.get::<Items>(key)?.unwrap(),
            expected_item(key, rows, saves)
        );
    }
    for id in 0..saves {
        assert_eq!(
            *read.get::<Saves>(id)?.unwrap(),
            Save { id, rows, batch }.saved()
        );
    }
    for area in 0..rows.div_ceil(AREA_SIZE) {
        let found = read.lookup::<Entities>(AREA_INDEX, &area.to_be_bytes())?;
        let expected = (area * AREA_SIZE..rows.min((area + 1) * AREA_SIZE)).collect::<Vec<_>>();
        assert_eq!(
            found.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            expected
        );
    }
    // One move to the next owner is a permutation during each complete round;
    // a partially completed round can leave zero/two items with an owner.
    let mut indexed_items = 0;
    for owner in 0..rows {
        for (key, row) in read.lookup::<Items>(OWNER_INDEX, &owner.to_be_bytes())? {
            assert_eq!(*row, expected_item(key, rows, saves));
            assert_eq!(row.owner, owner);
            indexed_items += 1;
        }
    }
    assert_eq!(indexed_items, rows);
    Ok(())
}
