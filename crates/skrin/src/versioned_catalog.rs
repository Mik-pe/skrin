use super::*;
use crate::catalog::{
    self, Catalog, CatalogDatabase, CatalogWrite, IndexDefinition, Indexes, Stored, Table,
};
use crate::version_index::{IndexIter, VersionIndex};
use std::marker::PhantomData;
use std::ops::Bound;

struct SharedCatalog<C>(PhantomData<C>);
impl<C: Catalog> Catalog for SharedCatalog<C> {
    const SCHEMA: Schema = C::SCHEMA;
    const TABLES: &'static [Schema] = C::TABLES;
    const INDEXES: &'static [IndexDefinition] = C::INDEXES;
    type Row = Arc<C::Row>;
    fn table_id(row: &Self::Row) -> u64 {
        C::table_id(row)
    }
    fn encode(row: &Self::Row, e: &mut Encoder) -> Result<()> {
        C::encode(row, e)
    }
    fn decode(id: u64, d: &mut Decoder<'_>) -> Result<Self::Row> {
        Ok(Arc::new(C::decode(id, d)?))
    }
    fn index_key(id: u64, row: &Self::Row) -> Result<Vec<u8>> {
        C::index_key(id, row)
    }
}
struct SharedTable<T>(PhantomData<T>);
impl<C: Catalog, T: Table<C>> Table<SharedCatalog<C>> for SharedTable<T> {
    type Record = T::Record;
    fn into_row(row: Self::Record) -> Arc<C::Row> {
        Arc::new(T::into_row(row))
    }
    fn borrow(row: &Arc<C::Row>) -> Option<&Self::Record> {
        T::borrow(row)
    }
}
fn shared<C: Catalog>(database: CatalogDatabase<C>) -> Result<CatalogDatabase<SharedCatalog<C>>> {
    let indexes = database.indexes.into_inner().map_err(|_| Error::Poisoned)?;
    let database = database.database.map_records(|_, r| {
        Ok(match r.data {
            None => Stored::metadata(),
            Some((key, row)) => Stored::row(key, Arc::new(row)),
        })
    })?;
    Ok(CatalogDatabase {
        database,
        indexes: RwLock::new(indexes),
    })
}
fn native<C: Catalog>(database: CatalogDatabase<SharedCatalog<C>>) -> Result<CatalogDatabase<C>> {
    let indexes = database.indexes.into_inner().map_err(|_| Error::Poisoned)?;
    let database = database.database.map_records(|_, r| {
        Ok(match r.data {
            None => Stored::metadata(),
            Some((key, row)) => Stored::row(key, Arc::try_unwrap(row).map_err(|_| Error::Busy)?),
        })
    })?;
    Ok(CatalogDatabase {
        database,
        indexes: RwLock::new(indexes),
    })
}
type Posting = (u64, Arc<[u8]>, u64);
struct CatalogView<C: Catalog> {
    rows: VersionTree<(u64, u64), Arc<C::Row>>,
    postings: VersionIndex<Posting, Arc<C::Row>>,
}
impl<C: Catalog> Clone for CatalogView<C> {
    fn clone(&self) -> Self {
        Self {
            rows: self.rows.clone(),
            postings: self.postings.clone(),
        }
    }
}
impl<C: Catalog> View for CatalogView<C> {
    fn bytes(&self) -> u64 {
        self.rows
            .accounted_bytes()
            .saturating_add(self.postings.accounted_bytes())
    }
}
fn posting_bytes(key: &[u8]) -> Result<u64> {
    (key.len() as u64)
        .checked_add(2 * std::mem::size_of::<usize>() as u64)
        .ok_or_else(|| Error::InvalidOperation("index footprint overflow".into()))
}
impl<C: Catalog> CatalogView<C> {
    fn build(
        database: &CatalogDatabase<SharedCatalog<C>>,
        footprint: fn(&C::Row) -> Result<u64>,
    ) -> Result<(Self, u64)> {
        let indexes = database.indexes.read().map_err(|_| Error::Poisoned)?;
        let read = database.database.read()?;
        let rows = VersionTree::try_from_sorted(
            indexes.primary.len(),
            indexes.primary.iter().map(|(&address, slot)| {
                let row = &read.state.rows[slot]
                    .data
                    .as_ref()
                    .expect("validated native row")
                    .1;
                Ok((
                    address,
                    row.clone(),
                    row_bytes(
                        row.as_ref(),
                        footprint,
                        VersionTree::<(u64, u64), Arc<C::Row>>::node_bytes(),
                    )?,
                ))
            }),
        )?;
        let posting_count = indexes
            .secondary
            .values()
            .flat_map(|entries| entries.values())
            .map(|posting| posting.len())
            .sum();
        let stored_rows = &read.state.rows;
        let postings = VersionIndex::try_from_sorted(
            posting_count,
            indexes.secondary.iter().flat_map(|(&id, entries)| {
                entries.iter().flat_map(move |(key, posting)| {
                    let shared: Arc<[u8]> = key.as_slice().into();
                    posting.into_iter().map(move |(primary, slot)| {
                        let row = stored_rows[&slot]
                            .data
                            .as_ref()
                            .expect("validated native row")
                            .1
                            .clone();
                        Ok(((id, shared.clone(), primary), row, posting_bytes(key)?))
                    })
                })
            }),
        )?;
        Ok((Self { rows, postings }, read.sequence()))
    }
    fn changed(
        &self,
        delta: &catalog::Delta,
        changes: &BTreeMap<u64, Option<Stored<SharedCatalog<C>>>>,
        footprint: fn(&C::Row) -> Result<u64>,
    ) -> Result<Self> {
        let mut view = self.clone();
        let mut removed = Vec::new();
        let mut added = Vec::new();
        for p in &delta.removed {
            if !p.retain_address {
                view.rows = view.rows.remove(&p.address);
            }
            for (id, key) in &p.keys {
                removed.push((*id, Arc::from(key.as_slice()), p.address.1));
            }
        }
        let mut replacements = Vec::new();
        for p in &delta.added {
            let row = &changes[&p.slot]
                .as_ref()
                .expect("projected staged row")
                .data
                .as_ref()
                .expect("native staged row")
                .1;
            let bytes = row_bytes(
                row.as_ref(),
                footprint,
                VersionTree::<(u64, u64), Arc<C::Row>>::node_bytes(),
            )?;
            if p.retain_address {
                replacements.push((p.address, row.clone(), bytes));
            } else {
                view.rows = view.rows.insert(p.address, row.clone(), bytes);
            }
            // A covering posting must advance even when its key did not change:
            // old frames keep the old row; every current index uses the new row.
            for index in C::INDEXES.iter().filter(|i| i.table_id == p.address.0) {
                let key = C::index_key(index.id, row)?;
                if key.len() > crate::codec::MAX_RECORD_BYTES {
                    return Err(Error::LimitExceeded {
                        limit: crate::codec::MAX_RECORD_BYTES,
                    });
                }
                added.push((
                    (index.id, Arc::from(key.as_slice()), p.address.1),
                    row.clone(),
                    posting_bytes(&key)?,
                ));
            }
        }
        replacements.sort_unstable_by_key(|(address, _, _)| *address);
        view.rows = view.rows.replace_many(&replacements);
        view.postings = view.postings.changed(removed, added)?;
        Ok(view)
    }
}
struct CatalogEngine<C: Catalog> {
    database: CatalogDatabase<SharedCatalog<C>>,
    publisher: Publisher<CatalogView<C>>,
    footprint: fn(&C::Row) -> Result<u64>,
}
impl<C: Catalog> CatalogEngine<C> {
    fn new(
        database: CatalogDatabase<C>,
        options: SnapshotOptions,
        footprint: fn(&C::Row) -> Result<u64>,
    ) -> Result<Self> {
        options.validate()?;
        let database = shared(database)?;
        let (view, sequence) = CatalogView::build(&database, footprint)?;
        Ok(Self {
            database,
            publisher: Publisher::new(sequence, view, options)?,
            footprint,
        })
    }
    fn inspect<T>(&self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            let failed = self
                .database
                .database
                .state
                .read()
                .map_or(true, |state| state.failed);
            if failed || self.database.indexes.read().is_err() {
                self.publisher.failed.store(true, Ordering::Release);
            }
        }
        result
    }
    fn into_database(self) -> Result<CatalogDatabase<C>> {
        self.publisher.no_pins()?;
        drop(self.publisher);
        native(self.database)
    }
}
/// Immutable catalog snapshots and serialized immediate-sync native writes.
/// All rows and mandatory indexes share one published sequence/root.
pub struct SnapshotCatalog<C: Catalog> {
    engine: Arc<CatalogEngine<C>>,
}
impl<C: Catalog> Clone for SnapshotCatalog<C> {
    fn clone(&self) -> Self {
        Self {
            engine: self.engine.clone(),
        }
    }
}
impl<C: Catalog> CatalogDatabase<C> {
    /// Enable coherent immutable row/index versions. The trusted, pure footprint
    /// function must include the enum's inline size and all owned capacities/
    /// nested allocations. Engine tree/index accounting is added separately.
    /// The conversion consumes this handle and changes no persistent bytes.
    pub fn into_snapshots(
        self,
        options: SnapshotOptions,
        footprint: fn(&C::Row) -> Result<u64>,
    ) -> Result<SnapshotCatalog<C>> {
        Ok(SnapshotCatalog {
            engine: Arc::new(CatalogEngine::new(self, options, footprint)?),
        })
    }
}
/// Native rows and every index at one synchronized catalog sequence. Clones
/// share a lease. A lease retains memory, not disk generations or directory LOCK.
pub struct CatalogSnapshot<C: Catalog> {
    lease: Arc<Lease<CatalogView<C>>>,
}
impl<C: Catalog> Clone for CatalogSnapshot<C> {
    fn clone(&self) -> Self {
        Self {
            lease: self.lease.clone(),
        }
    }
}
impl<C: Catalog> CatalogSnapshot<C> {
    /// Lazily query a schema-owned index using native key bounds. Returns an
    /// error for reversed/equal-excluded bounds or a mismatched definition.
    /// Compose Rust predicates, projections and limits on the returned rows.
    pub fn query<I: catalog::Index<C>>(
        &self,
        _index: I,
        range: impl RangeBounds<I::Key>,
    ) -> Result<CatalogSnapshotIndexScan<'_, C, I::Table>> {
        self.lease.check()?;
        crate::typed_index::validate::<C, I>()?;
        self.index_scan::<I::Table>(
            I::DEFINITION.id,
            crate::typed_index::bounds::<I::Key>(range)?,
        )
    }
    /// Resume strictly after `(index key, primary key)` within the original
    /// native bounds, seeking directly in this version's posting tree. The
    /// cursor need not exist; before-range cursors start normally and beyond-
    /// range cursors return no rows. Keep this same snapshot and predicates
    /// across pages for a stable traversal. Input keys are not retained.
    pub fn query_after<I: catalog::Index<C>>(
        &self,
        _index: I,
        range: impl RangeBounds<I::Key>,
        cursor: (&I::Key, u64),
    ) -> Result<CatalogSnapshotIndexScan<'_, C, I::Table>> {
        self.lease.check()?;
        crate::typed_index::validate::<C, I>()?;
        let resume = crate::typed_index::resume(range, cursor)?;
        self.index_scan_from::<I::Table>(I::DEFINITION.id, resume.keys, resume.after, resume.empty)
    }
    /// Lazily select equal native index keys in primary-key order. The index
    /// marker determines the record/key types; no table or ID argument is needed.
    pub fn matching<I: catalog::Index<C>>(
        &self,
        _index: I,
        key: &I::Key,
    ) -> Result<CatalogSnapshotIndexScan<'_, C, I::Table>> {
        self.lease.check()?;
        crate::typed_index::validate::<C, I>()?;
        self.index_scan::<I::Table>(I::DEFINITION.id, crate::typed_index::EqualKey::new(key)?)
    }
    /// The synchronized sequence shared by rows and every index.
    pub fn sequence(&self) -> Result<u64> {
        self.lease.check()?;
        Ok(self.lease.version.sequence)
    }
    /// Borrow a typed native row at this version.
    pub fn get<T: Table<C>>(&self, key: u64) -> Result<Option<&T::Record>> {
        self.lease.check()?;
        let id = catalog::table::<C, T>()?;
        Ok(self
            .lease
            .version
            .view
            .rows
            .get(&(id, key))
            .and_then(|r| T::borrow(r)))
    }
    /// Ordered native primary-key scan of one declared table.
    pub fn scan<T: Table<C>>(&self) -> Result<impl Iterator<Item = (u64, &T::Record)>> {
        self.lease.check()?;
        let id = catalog::table::<C, T>()?;
        Ok(self
            .lease
            .version
            .view
            .rows
            .range((id, 0)..=(id, u64::MAX))
            .filter_map(|((_, key), row)| T::borrow(row).map(|r| (*key, r))))
    }
    /// Indexed equality, ordered by primary key within the matching byte key.
    pub fn lookup<T: Table<C>>(&self, id: u64, key: &[u8]) -> Result<Vec<(u64, &T::Record)>> {
        self.index_range::<T>(id, key.to_vec()..=key.to_vec())
    }
    /// Lazy indexed scan ordered by byte key then primary key, borrowing rows
    /// and postings from this coherent version. Rust `filter`/`map`/`take` can
    /// stop without materializing all matches. Invalid bounds panic as for
    /// BTreeMap, including an empty index. Admission/poison is checked when
    /// creating the iterator; already returned iterators cannot be revoked.
    pub fn index_scan<T: Table<C>>(
        &self,
        id: u64,
        range: impl RangeBounds<Vec<u8>>,
    ) -> Result<CatalogSnapshotIndexScan<'_, C, T>> {
        self.index_scan_from::<T>(id, range, None, false)
    }
    fn index_scan_from<T: Table<C>>(
        &self,
        id: u64,
        range: impl RangeBounds<Vec<u8>>,
        after: Option<u64>,
        finished: bool,
    ) -> Result<CatalogSnapshotIndexScan<'_, C, T>> {
        self.lease.check()?;
        catalog::index::<C, T>(id)?;
        catalog::validate_index_bounds(&range);
        let lower = match range.start_bound() {
            Bound::Unbounded => Bound::Included((id, Arc::from([]), 0)),
            Bound::Included(key) => match after {
                None => Bound::Included((id, Arc::from(key.as_slice()), 0)),
                Some(primary) => Bound::Excluded((id, Arc::from(key.as_slice()), primary)),
            },
            Bound::Excluded(key) => Bound::Excluded((id, Arc::from(key.as_slice()), u64::MAX)),
        };
        let upper = match range.end_bound() {
            Bound::Unbounded => Bound::Unbounded,
            Bound::Included(key) => Bound::Included((id, Arc::from(key.as_slice()), u64::MAX)),
            Bound::Excluded(key) => Bound::Excluded((id, Arc::from(key.as_slice()), 0)),
        };
        Ok(CatalogSnapshotIndexScan {
            entries: self.lease.version.view.postings.range((lower, upper)),
            id,
            finished,
            marker: PhantomData,
        })
    }
    /// Collect an indexed scan into a vector. Use `index_scan` to stop early
    /// or filter/project before collecting. Ordering and bounds are identical.
    pub fn index_range<T: Table<C>>(
        &self,
        id: u64,
        range: impl RangeBounds<Vec<u8>>,
    ) -> Result<Vec<(u64, &T::Record)>> {
        let rows = self.index_scan::<T>(id, range)?.collect();
        self.lease.check()?;
        Ok(rows)
    }
}
type PostingBounds = (Bound<Posting>, Bound<Posting>);
/// Lazy row/index traversal borrowing one immutable catalog snapshot. Encoded
/// bounds are owned; the original query keys can be dropped after construction.
/// Poison is checked at construction. An already returned iterator cannot be
/// revoked by a later uncertain write, just like an already borrowed record.
pub struct CatalogSnapshotIndexScan<'a, C: Catalog, T: Table<C>> {
    entries: IndexIter<'a, Posting, Arc<C::Row>, PostingBounds>,
    id: u64,
    finished: bool,
    marker: PhantomData<fn() -> T>,
}
impl<'a, C: Catalog, T: Table<C>> Iterator for CatalogSnapshotIndexScan<'a, C, T> {
    type Item = (u64, &'a T::Record);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        for ((id, _, primary), row) in self.entries.by_ref() {
            if *id != self.id {
                self.finished = true;
                return None;
            }
            if let Some(row) = T::borrow(row) {
                return Some((*primary, row));
            }
        }
        self.finished = true;
        None
    }
}
/// Multi-table staging with the original final-view uniqueness algorithm.
/// Reads observe preceding group writes plus staging, not an independent pin.
pub struct CatalogSnapshotWrite<'a, C: Catalog> {
    transaction: CatalogWrite<'a, SharedCatalog<C>>,
}
impl<C: Catalog> CatalogSnapshotWrite<'_, C> {
    /// Read a typed row, including staging and preceding group transactions.
    pub fn get<T: Table<C>>(&self, key: u64) -> Result<Option<&T::Record>> {
        self.transaction.get::<SharedTable<T>>(key)
    }
    /// Insert without overwriting a logical key.
    pub fn insert<T: Table<C>>(&mut self, key: u64, row: T::Record) -> Result<()> {
        self.transaction.insert::<SharedTable<T>>(key, row)
    }
    /// Insert or replace; validate final uniqueness before append.
    pub fn put<T: Table<C>>(&mut self, key: u64, row: T::Record) -> Result<()> {
        self.transaction.put::<SharedTable<T>>(key, row)
    }
    /// Replace an existing typed row without Clone.
    pub fn update<T: Table<C>>(
        &mut self,
        key: u64,
        update: impl FnOnce(&T::Record) -> Result<T::Record>,
    ) -> Result<()> {
        self.transaction.update::<SharedTable<T>>(key, update)
    }
    /// Edit a clone of one current staged row; retained row/index frames stay
    /// immutable. Clones only this row once. A callback error leaves this
    /// statement unchanged; propagate it to roll back the entire transaction.
    /// Use `update` to build a replacement without `Clone`.
    pub fn edit<T: Table<C>>(
        &mut self,
        key: u64,
        edit: impl FnOnce(&mut T::Record) -> Result<()>,
    ) -> Result<()>
    where
        T::Record: Clone,
    {
        self.transaction.edit::<SharedTable<T>>(key, edit)
    }
    /// Remove a logical key, reporting whether it existed in this staged view.
    pub fn remove<T: Table<C>>(&mut self, key: u64) -> Result<bool> {
        self.transaction.remove::<SharedTable<T>>(key)
    }
    /// Indexed equality including staging; temporary swap duplicates are visible.
    pub fn lookup<T: Table<C>>(&self, id: u64, key: &[u8]) -> Result<Vec<(u64, &T::Record)>> {
        self.transaction.lookup::<SharedTable<T>>(id, key)
    }
    /// Indexed range including staging, ordered by byte key then primary key.
    pub fn index_range<T: Table<C>>(
        &self,
        id: u64,
        range: impl RangeBounds<Vec<u8>>,
    ) -> Result<Vec<(u64, &T::Record)>> {
        self.transaction.index_range::<SharedTable<T>>(id, range)
    }
}
struct CatalogVersionBatch<'a, C: Catalog> {
    indexes: RwLockWriteGuard<'a, Indexes>,
    state: RwLockWriteGuard<'a, State<Stored<SharedCatalog<C>>>>,
    publisher: &'a Publisher<CatalogView<C>>,
    view: CatalogView<C>,
    footprint: fn(&C::Row) -> Result<u64>,
}
impl<C: Catalog> Drop for CatalogVersionBatch<'_, C> {
    fn drop(&mut self) {
        if self.state.failed || std::thread::panicking() {
            self.publisher.failed.store(true, Ordering::Release);
        }
    }
}
impl<C: Catalog> Engine for CatalogEngine<C> {
    type Transaction<'a> = CatalogSnapshotWrite<'a, C>;
    type Batch<'a> = CatalogVersionBatch<'a, C>;
    fn persistent(&self) -> Result<bool> {
        self.publisher.check()?;
        Ok(self.database.stats()?.persistent)
    }
    fn begin(&self) -> Result<Self::Batch<'_>> {
        self.publisher.check()?;
        let indexes = self.database.indexes.write().map_err(|_| Error::Poisoned)?;
        let state = self
            .database
            .database
            .state
            .write()
            .map_err(|_| Error::Poisoned)?;
        if state.failed {
            self.publisher.failed.store(true, Ordering::Release);
            return Err(Error::Poisoned);
        }
        let view = self.publisher.root()?.view.clone();
        Ok(CatalogVersionBatch {
            indexes,
            state,
            publisher: &self.publisher,
            view,
            footprint: self.footprint,
        })
    }
    fn execute<T>(
        batch: &mut Self::Batch<'_>,
        operation: impl FnOnce(&mut CatalogSnapshotWrite<'_, C>) -> Result<T>,
    ) -> Result<(T, u64)> {
        let (value, sequence, delta, view) = {
            let mut tx = CatalogSnapshotWrite {
                transaction: CatalogWrite {
                    indexes: &batch.indexes,
                    transaction: crate::WriteTransaction {
                        state: WriteState::Borrowed(&mut batch.state),
                        changes: BTreeMap::new(),
                    },
                    staged: BTreeMap::new(),
                    next_slot: batch.indexes.next_slot,
                },
            };
            let value = operation(&mut tx)?;
            let delta = batch.indexes.prepare(
                &tx.transaction.transaction.state.rows,
                &tx.transaction.transaction.changes,
            )?;
            let view =
                batch
                    .view
                    .changed(&delta, &tx.transaction.transaction.changes, batch.footprint)?;
            view.validate_accounting()?;
            let sequence = tx.transaction.transaction.commit_unsynced()?;
            (value, sequence, delta, view)
        };
        batch.indexes.apply(delta);
        batch.view = view;
        Ok((value, sequence))
    }
    fn failed(batch: &Self::Batch<'_>) -> bool {
        batch.state.failed
    }
    fn sequence(batch: &Self::Batch<'_>) -> u64 {
        batch.state.sequence
    }
    fn finish(batch: &mut Self::Batch<'_>) -> Result<()> {
        finish_state(&mut batch.state)?;
        if let Err(error) = batch
            .publisher
            .publish(batch.state.sequence, batch.view.clone())
        {
            batch.state.failed = true;
            return Err(error);
        }
        Ok(())
    }
}
impl<C: Catalog> SnapshotCatalog<C> {
    /// Serialized published engine counters; pending unsynchronized work is
    /// excluded. Unlike retention inspection, this may wait for writer I/O.
    pub fn stats(&self) -> Result<crate::Stats> {
        self.engine.inspect(self.engine.database.stats())
    }

    /// Capture a coherent synchronized row/index version. BudgetExceeded is
    /// reader admission backpressure; release leases to restore capacity.
    pub fn snapshot(&self) -> Result<CatalogSnapshot<C>> {
        Ok(CatalogSnapshot {
            lease: self.engine.publisher.capture()?,
        })
    }
    /// Observe oldest pin, pinned versions and conservative accounted bytes.
    pub fn retention(&self) -> Result<RetentionStats> {
        self.engine.publisher.retention()
    }
    /// One independent immediate-sync transaction; old snapshots continue
    /// reading during the writer callback, append, sync and checkpoint.
    pub fn write<T>(
        &self,
        operation: impl FnOnce(&mut CatalogSnapshotWrite<'_, C>) -> Result<T>,
    ) -> Result<T> {
        let mut batch = self.engine.begin()?;
        let start = CatalogEngine::<C>::sequence(&batch);
        let (value, sequence) = CatalogEngine::<C>::execute(&mut batch, operation)?;
        if sequence != start {
            CatalogEngine::<C>::finish(&mut batch)?;
        }
        Ok(value)
    }
    /// Recover the native baseline with exclusive controller ownership and no
    /// leases. Use it for offline migration; Busy consumes this client.
    pub fn into_database(self) -> Result<CatalogDatabase<C>> {
        Arc::try_unwrap(self.engine)
            .map_err(|_| Error::Busy)?
            .into_database()
    }
    /// Enable bounded independent shared-sync requests on this versioned catalog.
    pub fn into_group_commit(self, options: GroupCommitOptions) -> Result<GroupSnapshotCatalog<C>> {
        Ok(GroupSnapshotCatalog {
            runtime: Runtime::new(
                Arc::try_unwrap(self.engine).map_err(|_| Error::Busy)?,
                options,
            )?,
        })
    }
    /// Verified serialized checkpoint without retaining disk files for pins.
    pub fn checkpoint(&self) -> Result<crate::Checkpoint> {
        self.checkpoint_with_options(MaintenanceOptions::default())
    }
    /// Serialized checkpoint with original encoded-resource/codec validation.
    pub fn checkpoint_with_options(
        &self,
        options: MaintenanceOptions,
    ) -> Result<crate::Checkpoint> {
        let _panic = self.engine.publisher.panic_guard();
        self.engine
            .inspect(self.engine.database.checkpoint_with_options(options))
    }
    /// Independently decoded and constraint-checked native backup to a new path.
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<CatalogDatabase<C>> {
        self.engine.publisher.check()?;
        native(self.engine.inspect(self.engine.database.backup_to(path))?)
    }
    /// Read-only selected/retained/orphan inventory under permanent ownership.
    pub fn storage_inventory(&self) -> Result<crate::StorageInventory> {
        self.engine
            .inspect(self.engine.database.storage_inventory())
    }
    /// Conservative cleanup retaining active and previous disk generations.
    pub fn reclaim(&self) -> Result<crate::ReclaimReport> {
        let _panic = self.engine.publisher.panic_guard();
        self.engine.inspect(self.engine.database.reclaim())
    }
}
/// Independent group commit publishing rows and every index after shared sync.
pub struct GroupSnapshotCatalog<C: Catalog> {
    runtime: Arc<Runtime<CatalogEngine<C>>>,
}
impl<C: Catalog> Clone for GroupSnapshotCatalog<C> {
    fn clone(&self) -> Self {
        Self {
            runtime: self.runtime.clone(),
        }
    }
}
impl<C: Catalog> GroupSnapshotCatalog<C> {
    /// Read-only selected, retained and unknown storage under permanent ownership.
    pub fn storage_inventory(&self) -> Result<crate::StorageInventory> {
        self.runtime
            .engine
            .inspect(self.runtime.engine.database.storage_inventory())
    }

    /// Serialized published engine counters; pending unsynchronized work is
    /// excluded. Unlike retention inspection, this may wait for writer I/O.
    pub fn stats(&self) -> Result<crate::Stats> {
        self.runtime
            .engine
            .inspect(self.runtime.engine.database.stats())
    }

    /// Admit one independent callback; dropping its response does not cancel.
    pub fn submit<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut CatalogSnapshotWrite<'_, C>) -> Result<T> + Send + 'static,
    ) -> Result<PendingCommit<T>> {
        self.runtime.submit(operation)
    }
    /// Capture a synchronized coherent version without waiting for writer I/O.
    pub fn snapshot(&self) -> Result<CatalogSnapshot<C>> {
        Ok(CatalogSnapshot {
            lease: self.runtime.engine.publisher.capture()?,
        })
    }
    /// Observe oldest pin, live versions and conservative accounted bytes.
    pub fn retention(&self) -> Result<RetentionStats> {
        self.runtime.engine.publisher.retention()
    }
    /// Drain admitted work and recover the native baseline for offline migration;
    /// exclusive client ownership and no live leases are required.
    pub fn into_database(self) -> Result<CatalogDatabase<C>> {
        self.runtime.into_engine()?.into_database()
    }
    /// Serialized verified checkpoint; pinned roots remain readable in memory.
    pub fn checkpoint(&self) -> Result<crate::Checkpoint> {
        let _panic = self.runtime.engine.publisher.panic_guard();
        self.runtime
            .engine
            .inspect(self.runtime.engine.database.checkpoint())
    }
    /// Conservative explicit disk cleanup retaining active and previous.
    pub fn reclaim(&self) -> Result<crate::ReclaimReport> {
        let _panic = self.runtime.engine.publisher.panic_guard();
        self.runtime
            .engine
            .inspect(self.runtime.engine.database.reclaim())
    }
    /// Independently decoded and constraint-checked backup to a new directory.
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<CatalogDatabase<C>> {
        self.runtime.engine.publisher.check()?;
        native(
            self.runtime
                .engine
                .inspect(self.runtime.engine.database.backup_to(path))?,
        )
    }
}

#[cfg(test)]
#[path = "versioned_catalog_tests.rs"]
mod tests;
