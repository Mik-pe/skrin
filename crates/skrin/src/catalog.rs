//! Schema-bound typed tables and derived indexes sharing one durable transaction.
//!
//! Index keys are application-defined byte strings ordered lexicographically.
//! Projections and codecs must be deterministic and have immutable value semantics.
//! Readers block writers. Indexes are rebuilt and checked before recovery repairs
//! a tail; ordinary commits touch only changed rows and their index postings.
use crate::directory::Directory;
use crate::{Database, Decoder, Encoder, Error, MaintenanceOptions, Record, Result, Schema};
use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::ops::RangeBounds;
use std::path::Path;
use std::sync::{RwLock, RwLockReadGuard};

/// A stable derived-index definition, owned by the application catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexDefinition {
    /// Globally unique index identity within this catalog.
    pub id: u64,
    /// Table whose records feed this index.
    pub table_id: u64,
    /// Increase when projection semantics or key encoding changes.
    pub version: u32,
    /// Whether two live rows may share one projected key.
    pub unique: bool,
}

/// Complete application schema. `Row` is normally an enum of native record types.
///
/// Keep the catalog identity distinct from legacy single-table identities.
/// Tables/indexes are strictly ID-sorted, fixed at open, and persisted exactly.
/// Codec dispatch must reject mismatched table/enum variants. Every index on a
/// table is mandatory; there is no optional registration after opening.
pub trait Catalog: Send + Sync + 'static {
    /// Stable catalog identity/version, independent of individual table versions.
    const SCHEMA: Schema;
    /// Complete, strictly ID-sorted set of table record schemas.
    const TABLES: &'static [Schema];
    /// Complete, strictly ID-sorted set of derived index definitions.
    const INDEXES: &'static [IndexDefinition];
    /// Native heterogeneous row representation. `Clone` is not required.
    type Row: Send + Sync + 'static;
    /// Determine the table identity of a native row.
    fn table_id(row: &Self::Row) -> u64;
    /// Encode the table's native row with its explicit codec.
    fn encode(row: &Self::Row, encoder: &mut Encoder) -> Result<()>;
    /// Decode a native row for this exact table definition.
    fn decode(table_id: u64, decoder: &mut Decoder<'_>) -> Result<Self::Row>;
    /// Produce one deterministic key for a declared index on this row's table.
    fn index_key(index_id: u64, row: &Self::Row) -> Result<Vec<u8>>;
}

/// Typed table marker binding a record to one application catalog.
pub trait Table<C: Catalog> {
    /// The real typed record and its persistent table identity.
    type Record: Record;
    /// Wrap a typed record in the application's native enum.
    fn into_row(record: Self::Record) -> C::Row;
    /// Borrow this table's variant, returning `None` for other tables.
    fn borrow(row: &C::Row) -> Option<&Self::Record>;
}

fn invalid(reason: impl Into<String>) -> Error {
    Error::InvalidOperation(reason.into())
}

fn descriptor<C: Catalog>() -> Result<Vec<u8>> {
    if C::TABLES.is_empty()
        || C::TABLES.windows(2).any(|w| w[0].table_id >= w[1].table_id)
        || C::INDEXES.windows(2).any(|w| w[0].id >= w[1].id)
        || C::INDEXES
            .iter()
            .any(|i| !C::TABLES.iter().any(|t| t.table_id == i.table_id))
    {
        return Err(invalid(
            "catalog requires nonempty ID-sorted tables and ID-sorted indexes on declared tables",
        ));
    }
    let mut e = Encoder::default();
    e.bytes(b"SKRCAT01")?;
    e.u32(u32::try_from(C::TABLES.len()).map_err(|_| invalid("too many tables"))?)?;
    for t in C::TABLES {
        e.u64(t.table_id)?;
        e.u32(t.version)?;
    }
    e.u32(u32::try_from(C::INDEXES.len()).map_err(|_| invalid("too many indexes"))?)?;
    for i in C::INDEXES {
        e.u64(i.id)?;
        e.u64(i.table_id)?;
        e.u32(i.version)?;
        e.u8(u8::from(i.unique))?;
    }
    Ok(e.finish())
}

fn table<C: Catalog, T: Table<C>>() -> Result<u64> {
    let schema = T::Record::SCHEMA;
    if !C::TABLES.contains(&schema) {
        return Err(invalid("typed table is not present in this catalog"));
    }
    Ok(schema.table_id)
}

fn index<C: Catalog, T: Table<C>>(id: u64) -> Result<&'static IndexDefinition> {
    let table_id = table::<C, T>()?;
    C::INDEXES
        .iter()
        .find(|i| i.id == id && i.table_id == table_id)
        .ok_or_else(|| invalid("index is not defined for this typed table"))
}

// Physical row zero is the catalog descriptor. Other slots are internal only;
// logical table/key pairs retain all 64 bits of the user's primary key.
struct Stored<C: Catalog> {
    data: Option<(u64, C::Row)>,
    marker: PhantomData<C>,
}
impl<C: Catalog> Stored<C> {
    fn metadata() -> Self {
        Self {
            data: None,
            marker: PhantomData,
        }
    }
    fn row(key: u64, row: C::Row) -> Self {
        Self {
            data: Some((key, row)),
            marker: PhantomData,
        }
    }
}
impl<C: Catalog> Record for Stored<C> {
    const SCHEMA: Schema = C::SCHEMA;
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        e.u8(1)?; // Deliberately versioned catalog row envelope.
        match &self.data {
            None => {
                e.u8(0)?;
                e.bytes(&descriptor::<C>()?)
            }
            Some((key, row)) => {
                let id = C::table_id(row);
                if !C::TABLES.iter().any(|t| t.table_id == id) {
                    return Err(invalid("undeclared row table"));
                }
                e.u8(1)?;
                e.u64(id)?;
                e.u64(*key)?;
                C::encode(row, e)
            }
        }
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        if d.u8()? != 1 {
            return Err(invalid("unsupported catalog row envelope"));
        }
        match d.u8()? {
            0 => {
                if d.bytes()? != descriptor::<C>()? {
                    return Err(invalid(
                        "persisted table/index definitions differ from catalog",
                    ));
                }
                Ok(Self::metadata())
            }
            1 => {
                let id = d.u64()?;
                let key = d.u64()?;
                if !C::TABLES.iter().any(|t| t.table_id == id) {
                    return Err(invalid("undeclared row table"));
                }
                let row = C::decode(id, d)?;
                if C::table_id(&row) != id {
                    return Err(invalid("decoded row belongs to another table"));
                }
                Ok(Self::row(key, row))
            }
            _ => Err(invalid("unknown catalog row kind")),
        }
    }
}

#[derive(Default)]
struct Indexes {
    primary: BTreeMap<(u64, u64), u64>,
    secondary: BTreeMap<u64, BTreeMap<Vec<u8>, BTreeSet<u64>>>,
    next_slot: u64,
}
#[derive(PartialEq, Eq)]
struct Projected {
    slot: u64,
    address: (u64, u64),
    keys: Vec<(u64, Vec<u8>)>,
}
struct Delta {
    removed: Vec<Projected>,
    added: Vec<Projected>,
}
fn project<C: Catalog>(slot: u64, row: &Stored<C>) -> Result<Projected> {
    let (key, native) = row
        .data
        .as_ref()
        .ok_or_else(|| invalid("unexpected catalog metadata"))?;
    let table_id = C::table_id(native);
    if !C::TABLES.iter().any(|t| t.table_id == table_id) {
        return Err(invalid("undeclared row table"));
    }
    let mut keys = Vec::new();
    for i in C::INDEXES.iter().filter(|i| i.table_id == table_id) {
        let key = C::index_key(i.id, native)?;
        if key.len() > crate::codec::MAX_RECORD_BYTES {
            return Err(Error::LimitExceeded {
                limit: crate::codec::MAX_RECORD_BYTES,
            });
        }
        keys.push((i.id, key));
    }
    Ok(Projected {
        slot,
        address: (table_id, *key),
        keys,
    })
}
impl Indexes {
    fn build<C: Catalog>(rows: &BTreeMap<u64, Stored<C>>) -> Result<Self> {
        if rows.get(&0).is_none_or(|r| r.data.is_some()) {
            return Err(invalid("missing catalog descriptor at slot zero"));
        }
        let mut indexes = Self::default();
        let changes: BTreeMap<_, _> = rows
            .iter()
            .filter(|(k, _)| **k != 0)
            .map(|(&k, r)| (k, Some(r)))
            .collect();
        let delta = indexes.prepare_refs::<C>(&BTreeMap::new(), &changes)?;
        indexes.apply(delta);
        indexes.next_slot = rows
            .last_key_value()
            .map_or(1, |(&k, _)| k.saturating_add(1));
        Ok(indexes)
    }
    fn prepare<C: Catalog>(
        &self,
        rows: &BTreeMap<u64, Stored<C>>,
        changes: &BTreeMap<u64, Option<Stored<C>>>,
    ) -> Result<Delta> {
        if changes.contains_key(&0) {
            return Err(invalid(
                "catalog descriptor cannot be changed by a transaction",
            ));
        }
        let refs = changes.iter().map(|(&k, r)| (k, r.as_ref())).collect();
        self.prepare_refs(rows, &refs)
    }
    fn prepare_refs<C: Catalog>(
        &self,
        rows: &BTreeMap<u64, Stored<C>>,
        changes: &BTreeMap<u64, Option<&Stored<C>>>,
    ) -> Result<Delta> {
        let mut removed = Vec::new();
        let mut added = Vec::new();
        let mut addresses = BTreeSet::new();
        let mut unique = BTreeSet::new();
        for (&slot, replacement) in changes {
            if let Some(old) = rows.get(&slot) {
                removed.push(project::<C>(slot, old)?);
            }
            if let Some(row) = replacement {
                let p = project::<C>(slot, row)?;
                if !addresses.insert(p.address) {
                    return Err(Error::DuplicateKey(p.address.1));
                }
                if let Some(existing) = self.primary.get(&p.address)
                    && !changes.contains_key(existing)
                {
                    return Err(Error::DuplicateKey(p.address.1));
                }
                for (id, key) in &p.keys {
                    let definition = C::INDEXES
                        .iter()
                        .find(|i| i.id == *id)
                        .expect("declared projection");
                    if definition.unique {
                        if !unique.insert((*id, key.clone())) {
                            return Err(Error::UniqueViolation { index_id: *id });
                        }
                        if let Some(keys) = self.secondary.get(id).and_then(|i| i.get(key)) {
                            for primary in keys {
                                let old_slot = self.primary[&(p.address.0, *primary)];
                                if !changes.contains_key(&old_slot) {
                                    return Err(Error::UniqueViolation { index_id: *id });
                                }
                            }
                        }
                    }
                }
                added.push(p);
            }
        }
        Ok(Delta { removed, added })
    }
    fn apply(&mut self, delta: Delta) {
        for p in delta.removed {
            self.primary.remove(&p.address);
            for (id, key) in p.keys {
                let index = self.secondary.get_mut(&id).expect("existing index");
                let posting = index.get_mut(&key).expect("existing posting");
                posting.remove(&p.address.1);
                if posting.is_empty() {
                    index.remove(&key);
                }
            }
        }
        for p in delta.added {
            self.next_slot = self.next_slot.max(p.slot.saturating_add(1));
            self.primary.insert(p.address, p.slot);
            for (id, key) in p.keys {
                self.secondary
                    .entry(id)
                    .or_default()
                    .entry(key)
                    .or_default()
                    .insert(p.address.1);
            }
        }
    }
}

fn validate_decoded<C: Catalog>(indexes: &mut Indexes, slot: u64, row: &Stored<C>) -> Result<()> {
    if slot == 0 {
        if row.data.is_some() {
            return Err(invalid("invalid catalog descriptor"));
        }
        return Ok(());
    }
    let refs = BTreeMap::from([(slot, Some(row))]);
    let delta = indexes.prepare_refs(&BTreeMap::new(), &refs)?;
    indexes.apply(delta);
    Ok(())
}

/// Multi-table database using the same WAL, locking and managed generations as
/// `Database`. Rows and all derived indexes publish under one catalog lock.
pub struct CatalogDatabase<C: Catalog> {
    database: Database<Stored<C>>,
    indexes: RwLock<Indexes>,
}
impl<C: Catalog> CatalogDatabase<C> {
    /// Create an explicitly volatile catalog, validating its full definition.
    pub fn in_memory() -> Result<Self> {
        descriptor::<C>()?;
        let database = Database::in_memory();
        // Metadata is initialization, not an application commit.
        database
            .begin_write()?
            .state
            .rows
            .insert(0, Stored::<C>::metadata());
        Self::wrap(database)
    }
    /// Create a NEW managed directory. Legacy files are never reinterpreted.
    pub fn create_dir(path: impl AsRef<Path>) -> Result<Self> {
        descriptor::<C>()?;
        let rows = BTreeMap::from([(0, Stored::<C>::metadata())]);
        let (directory, wal, recovered) = Directory::create(
            path.as_ref(),
            &rows,
            0,
            Vec::new(),
            MaintenanceOptions::default(),
        )?;
        Self::wrap(Database::from_directory(directory, wal, recovered))
    }
    /// Explicitly import one legacy typed table into a NEW catalog directory.
    /// The caller supplies a value copy/conversion because records need not be
    /// `Clone`. Source bytes remain untouched, the committed sequence is retained,
    /// and native plus decoded destination constraints precede publication.
    pub fn import_table<T: Table<C>>(
        source: &Database<T::Record>,
        path: impl AsRef<Path>,
        mut copy: impl FnMut(u64, &T::Record) -> Result<T::Record>,
    ) -> Result<Self> {
        descriptor::<C>()?;
        let table_id = table::<C, T>()?;
        let read = source.read()?;
        let mut rows = BTreeMap::from([(0, Stored::<C>::metadata())]);
        let mut slot = 1u64;
        for (key, record) in read.iter() {
            let native = T::into_row(copy(key, record)?);
            if C::table_id(&native) != table_id || T::borrow(&native).is_none() {
                return Err(invalid("import produced another table variant"));
            }
            rows.insert(slot, Stored::row(key, native));
            slot = slot.checked_add(1).ok_or(Error::SequenceExhausted)?;
        }
        let _native_indexes = Indexes::build(&rows)?;
        let mut decoded_indexes = Indexes::default();
        let (directory, wal, recovered) = Directory::create_checked(
            path.as_ref(),
            &rows,
            read.sequence(),
            Vec::new(),
            MaintenanceOptions::default(),
            |slot, row| validate_decoded::<C>(&mut decoded_indexes, slot, row),
        )?;
        Self::wrap(Database::from_directory(directory, wal, recovered))
    }
    /// Validate catalog definitions and every recovered atomic index delta
    /// before tail repair, sync or a usable handle is exposed.
    pub fn open_dir(path: impl AsRef<Path>) -> Result<Self> {
        descriptor::<C>()?;
        let mut indexes = None;
        let (directory, wal, recovered) =
            Directory::open_checked(path.as_ref(), |rows, changes| {
                match &mut indexes {
                    None => {
                        indexes = Some(Indexes::build::<C>(rows)?);
                    }
                    Some(indexes) => {
                        let delta = indexes.prepare::<C>(rows, changes)?;
                        indexes.apply(delta);
                    }
                }
                Ok(())
            })?;
        Ok(Self {
            database: Database::from_directory(directory, wal, recovered),
            indexes: RwLock::new(indexes.expect("validated snapshot")),
        })
    }
    fn wrap(database: Database<Stored<C>>) -> Result<Self> {
        let indexes = Indexes::build(&database.read()?.state.rows)?;
        Ok(Self {
            database,
            indexes: RwLock::new(indexes),
        })
    }
    /// Borrow typed native rows and deterministic index lookups. Blocks writers.
    pub fn read(&self) -> Result<CatalogRead<'_, C>> {
        let indexes = self.indexes.read().map_err(|_| Error::Poisoned)?;
        let transaction = self.database.read()?;
        Ok(CatalogRead {
            indexes,
            transaction,
        })
    }
    /// Stage all tables, validate final uniqueness (including swaps), sync one
    /// WAL frame, and publish rows/indexes before returning the closure result.
    /// No automatic callback retry or external side effects are supported.
    pub fn write<V>(
        &self,
        operation: impl FnOnce(&mut CatalogWrite<'_, C>) -> Result<V>,
    ) -> Result<V> {
        let mut indexes = self.indexes.write().map_err(|_| Error::Poisoned)?;
        let (result, delta) = {
            let transaction = self.database.begin_write()?;
            let next_slot = indexes.next_slot;
            let mut tx = CatalogWrite {
                indexes: &indexes,
                transaction,
                staged: BTreeMap::new(),
                next_slot,
            };
            let result = operation(&mut tx)?;
            let delta = indexes.prepare(&tx.transaction.state.rows, &tx.transaction.changes)?;
            tx.transaction.commit()?;
            (result, delta)
        };
        indexes.apply(delta);
        Ok(result)
    }
    /// Inspect engine counters; the internal catalog descriptor is excluded.
    pub fn stats(&self) -> Result<crate::Stats> {
        let _guard = self.indexes.read().map_err(|_| Error::Poisoned)?;
        let mut stats = self.database.stats()?;
        stats.rows -= 1;
        Ok(stats)
    }
    /// Explicit, serialized checkpoint; reported rows exclude the descriptor.
    pub fn checkpoint(&self) -> Result<crate::Checkpoint> {
        let _guard = self.indexes.write().map_err(|_| Error::Poisoned)?;
        let mut report = self.database.checkpoint_checked(|slot, rows, decoded| {
            if slot == 0 {
                return Ok(());
            }
            let native = rows
                .get(&slot)
                .ok_or_else(|| invalid("checkpoint changed an internal row slot"))?;
            if project::<C>(slot, native)? != project::<C>(slot, decoded)? {
                return Err(invalid(
                    "checkpoint codec changed a logical key or index projection",
                ));
            }
            Ok(())
        })?;
        report.rows -= 1;
        Ok(report)
    }
    /// Reclaim only recognized abandoned/obsolete files; keep active + previous.
    pub fn reclaim(&self) -> Result<crate::ReclaimReport> {
        let _guard = self.indexes.write().map_err(|_| Error::Poisoned)?;
        self.database.reclaim()
    }
    /// Independently decoded backup into a NEW directory, including constraints.
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<Self> {
        let _guard = self.indexes.read().map_err(|_| Error::Poisoned)?;
        let mut validator = Indexes::default();
        Self::wrap(self.database.backup_checked(
            path.as_ref(),
            MaintenanceOptions::default(),
            |slot, row| validate_decoded::<C>(&mut validator, slot, row),
        )?)
    }
    /// Offline whole-catalog migration with explicit native old/new enums.
    /// Logical table/key identity is preserved; target indexes are checked before
    /// publication. The old catalog handle is consumed even on a refusal.
    pub fn migrate<N: Catalog>(
        self,
        id: &str,
        mut convert: impl FnMut(u64, u64, C::Row) -> Result<N::Row>,
    ) -> Result<CatalogDatabase<N>> {
        descriptor::<N>()?;
        let mut validator = Indexes::default();
        let mut decoded_validator = Indexes::default();
        let database = self.database.migrate_checked::<Stored<N>>(
            id,
            MaintenanceOptions::default(),
            |slot, row| {
                let Some((key, native)) = row.data else {
                    return Ok(Stored::metadata());
                };
                let table_id = C::table_id(&native);
                let next = convert(table_id, key, native)?;
                if N::table_id(&next) != table_id {
                    return Err(invalid("migration must preserve logical table identity"));
                }
                let next = Stored::<N>::row(key, next);
                let changes = BTreeMap::from([(slot, Some(&next))]);
                let delta = validator.prepare_refs(&BTreeMap::new(), &changes)?;
                validator.apply(delta);
                Ok(next)
            },
            |slot, row| validate_decoded::<N>(&mut decoded_validator, slot, row),
        )?;
        CatalogDatabase::wrap(database)
    }
}

/// Consistent borrowed catalog view. Keep guards short and never nest operations.
pub struct CatalogRead<'a, C: Catalog> {
    indexes: RwLockReadGuard<'a, Indexes>,
    transaction: crate::ReadTransaction<'a, Stored<C>>,
}
impl<C: Catalog> CatalogRead<'_, C> {
    /// Borrow a typed primary-key row without cloning or decoding.
    pub fn get<T: Table<C>>(&self, key: u64) -> Result<Option<&T::Record>> {
        let id = table::<C, T>()?;
        Ok(self
            .indexes
            .primary
            .get(&(id, key))
            .and_then(|slot| self.transaction.get(*slot))
            .and_then(|r| r.data.as_ref())
            .and_then(|(_, r)| T::borrow(r)))
    }
    /// Explicit full-table scan in logical primary-key order.
    pub fn scan<T: Table<C>>(&self) -> Result<impl Iterator<Item = (u64, &T::Record)>> {
        let id = table::<C, T>()?;
        Ok(self
            .indexes
            .primary
            .range((id, 0)..=(id, u64::MAX))
            .filter_map(|(&(_, key), &slot)| {
                self.transaction
                    .get(slot)
                    .and_then(|r| r.data.as_ref())
                    .and_then(|(_, r)| T::borrow(r))
                    .map(|r| (key, r))
            }))
    }
    /// Indexed equality lookup ordered by primary key among equal index keys.
    pub fn lookup<T: Table<C>>(&self, id: u64, key: &[u8]) -> Result<Vec<(u64, &T::Record)>> {
        self.index_range::<T>(id, key.to_vec()..=key.to_vec())
    }
    /// Indexed range scan in byte-key then primary-key order. Invalid bounds
    /// panic as for `BTreeMap::range`. Use explicit order-preserving key codecs.
    pub fn index_range<T: Table<C>>(
        &self,
        id: u64,
        range: impl RangeBounds<Vec<u8>>,
    ) -> Result<Vec<(u64, &T::Record)>> {
        index::<C, T>(id)?;
        let mut result = Vec::new();
        if let Some(entries) = self.indexes.secondary.get(&id) {
            for (_, posting) in entries.range(range) {
                for &key in posting {
                    if let Some(row) = self.get::<T>(key)? {
                        result.push((key, row));
                    }
                }
            }
        }
        Ok(result)
    }
    /// Sequence shared by every table and index in this view.
    pub fn sequence(&self) -> u64 {
        self.transaction.sequence()
    }
}

/// Staged multi-table write view. Errors propagated from the closure roll back
/// everything. Uniqueness is checked on the final view, permitting key swaps.
pub struct CatalogWrite<'a, C: Catalog> {
    indexes: &'a Indexes,
    transaction: crate::WriteTransaction<'a, Stored<C>>,
    staged: BTreeMap<(u64, u64), u64>,
    next_slot: u64,
}
impl<C: Catalog> CatalogWrite<'_, C> {
    fn slot(&self, address: (u64, u64)) -> Option<u64> {
        self.staged
            .get(&address)
            .or_else(|| self.indexes.primary.get(&address))
            .copied()
    }
    /// Read a typed row, including earlier staged changes.
    pub fn get<T: Table<C>>(&self, key: u64) -> Result<Option<&T::Record>> {
        let id = table::<C, T>()?;
        Ok(self
            .slot((id, key))
            .and_then(|slot| self.transaction.get(slot))
            .and_then(|r| r.data.as_ref())
            .and_then(|(_, r)| T::borrow(r)))
    }
    /// Insert a new logical key, refusing duplicates including staging.
    pub fn insert<T: Table<C>>(&mut self, key: u64, row: T::Record) -> Result<()> {
        if self.get::<T>(key)?.is_some() {
            return Err(Error::DuplicateKey(key));
        }
        self.put::<T>(key, row)
    }
    /// Insert or replace explicitly; validate final unique constraints at commit.
    pub fn put<T: Table<C>>(&mut self, key: u64, row: T::Record) -> Result<()> {
        let id = table::<C, T>()?;
        let native = T::into_row(row);
        if C::table_id(&native) != id || T::borrow(&native).is_none() {
            return Err(invalid("typed table wrapper produced another variant"));
        }
        let slot = match self.slot((id, key)) {
            Some(slot) => slot,
            None => {
                let slot = self.next_slot;
                self.next_slot = slot.checked_add(1).ok_or(Error::SequenceExhausted)?;
                self.staged.insert((id, key), slot);
                slot
            }
        };
        self.transaction.put(slot, Stored::row(key, native));
        Ok(())
    }
    /// Replace an existing typed row without requiring `Clone`.
    pub fn update<T: Table<C>>(
        &mut self,
        key: u64,
        update: impl FnOnce(&T::Record) -> Result<T::Record>,
    ) -> Result<()> {
        let row = self.get::<T>(key)?.ok_or(Error::MissingKey(key))?;
        let next = update(row)?;
        self.put::<T>(key, next)
    }
    /// Remove a logical key. Delete/reinsert reuses its staging slot.
    pub fn remove<T: Table<C>>(&mut self, key: u64) -> Result<bool> {
        let id = table::<C, T>()?;
        Ok(self
            .slot((id, key))
            .is_some_and(|slot| self.transaction.remove(slot)))
    }
    /// Indexed equality on the current staged view. Temporary duplicates are
    /// visible during a swap; uniqueness is validated at commit.
    pub fn lookup<T: Table<C>>(&self, id: u64, key: &[u8]) -> Result<Vec<(u64, &T::Record)>> {
        self.index_range::<T>(id, key.to_vec()..=key.to_vec())
    }
    /// Indexed range on the staged view, in byte-key then logical primary order.
    /// Only matching committed postings and the transaction's delta are examined.
    pub fn index_range<T: Table<C>>(
        &self,
        id: u64,
        range: impl RangeBounds<Vec<u8>>,
    ) -> Result<Vec<(u64, &T::Record)>> {
        let definition = index::<C, T>(id)?;
        let mut matches = BTreeSet::new();
        if let Some(entries) = self.indexes.secondary.get(&id) {
            for (key, posting) in
                entries.range::<Vec<u8>, _>((range.start_bound(), range.end_bound()))
            {
                for &primary in posting {
                    let slot = self.indexes.primary[&(definition.table_id, primary)];
                    if !self.transaction.changes.contains_key(&slot) {
                        matches.insert((key.clone(), primary));
                    }
                }
            }
        }
        for row in self.transaction.changes.values().flatten() {
            if let Some((primary, native)) = &row.data
                && C::table_id(native) == definition.table_id
            {
                let projected = C::index_key(id, native)?;
                if range.contains(&projected) {
                    matches.insert((projected, *primary));
                }
            }
        }
        let mut result = Vec::new();
        for (_, key) in matches {
            if let Some(row) = self.get::<T>(key)? {
                result.push((key, row));
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
