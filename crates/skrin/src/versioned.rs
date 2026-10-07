//! Opt-in immutable read versions with bounded, cooperative reader accounting.
//!
//! The ordinary APIs remain lock-based. These handles publish a new root only
//! after synchronization; holding an old snapshot does not block commits or
//! checkpoint/cleanup. Native records must have immutable value semantics.
use crate::database::{State, WriteState};
use crate::group_commit::{Engine, GroupCommitOptions, PendingCommit, Runtime, finish_state};
use crate::version_tree::VersionTree;
use crate::{Database, Decoder, Encoder, Error, MaintenanceOptions, Record, Result, Schema};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeBounds;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockWriteGuard, Weak};

/// Limits on admitted snapshot leases. These are conservative accounted bytes,
/// not a hard allocator/process limit. The current table, transaction staging,
/// callbacks and allocator overhead remain application responsibilities.
#[derive(Clone, Copy, Debug)]
pub struct SnapshotOptions {
    /// Maximum live leases; cloning one snapshot shares its existing lease.
    pub max_snapshots: usize,
    /// Sum of complete pinned roots, deliberately counting shared data again
    /// for each separately captured lease. Includes tree nodes and Arc counters.
    pub max_pinned_bytes: u64,
}
impl SnapshotOptions {
    fn validate(self) -> Result<()> {
        if self.max_snapshots == 0 || self.max_pinned_bytes == 0 {
            return Err(Error::InvalidOperation(
                "snapshot limits must be nonzero".into(),
            ));
        }
        Ok(())
    }
}
/// Explicit observations of reader retention, including the current version
/// when pinned. No allocator/RSS or physical-disk reservation is implied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionStats {
    /// Published sequence; pending unsynchronized transactions are excluded.
    pub published_sequence: u64,
    /// Live independently captured leases; clones count once.
    pub snapshots: usize,
    /// Distinct pinned sequences, including the current sequence when pinned.
    pub pinned_versions: usize,
    /// Oldest live pinned sequence, or `None` without readers.
    pub oldest_pinned_sequence: Option<u64>,
    /// Conservative sum of the full accounted size of each pinned lease.
    pub pinned_bytes: u64,
    /// Full accounted size of the current published immutable root.
    pub current_bytes: u64,
}
trait View: Clone + Send + Sync + 'static {
    fn bytes(&self) -> u64;
    fn validate_accounting(&self) -> Result<()> {
        if self.bytes() == u64::MAX {
            return Err(Error::InvalidOperation("version footprint overflow".into()));
        }
        Ok(())
    }
}
struct Version<V> {
    sequence: u64,
    view: V,
}
struct Lease<V> {
    version: Arc<Version<V>>,
    failed: Arc<AtomicBool>,
}
impl<V> Lease<V> {
    fn check(&self) -> Result<()> {
        if self.failed.load(Ordering::Acquire) {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }
}
struct Publisher<V: View> {
    current: RwLock<Arc<Version<V>>>,
    leases: Mutex<Vec<Weak<Lease<V>>>>,
    options: SnapshotOptions,
    failed: Arc<AtomicBool>,
}
impl<V: View> Publisher<V> {
    fn new(sequence: u64, view: V, options: SnapshotOptions) -> Result<Self> {
        options.validate()?;
        view.validate_accounting()?;
        Ok(Self {
            current: RwLock::new(Arc::new(Version { sequence, view })),
            leases: Mutex::new(Vec::new()),
            options,
            failed: Arc::new(AtomicBool::new(false)),
        })
    }
    fn check(&self) -> Result<()> {
        if self.failed.load(Ordering::Acquire) {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }
    fn capture(&self) -> Result<Arc<Lease<V>>> {
        self.check()?;
        let current = self.current.read().map_err(|_| Error::Poisoned)?;
        let mut leases = self.leases.lock().map_err(|_| Error::Poisoned)?;
        leases.retain(|lease| lease.strong_count() != 0);
        let bytes = leases
            .iter()
            .filter_map(Weak::upgrade)
            .fold(0u64, |total, lease| {
                total.saturating_add(lease.version.view.bytes())
            });
        let count = leases.len().saturating_add(1);
        if count > self.options.max_snapshots {
            return Err(Error::BudgetExceeded {
                resource: "snapshot leases",
                limit: self.options.max_snapshots as u64,
                required: count as u64,
            });
        }
        let required = bytes.checked_add(current.view.bytes());
        if required.is_none_or(|n| n > self.options.max_pinned_bytes) {
            return Err(Error::BudgetExceeded {
                resource: "pinned snapshot bytes",
                limit: self.options.max_pinned_bytes,
                required: required.unwrap_or(u64::MAX),
            });
        }
        self.check()?;
        let lease = Arc::new(Lease {
            version: current.clone(),
            failed: self.failed.clone(),
        });
        leases.push(Arc::downgrade(&lease));
        Ok(lease)
    }
    fn retention(&self) -> Result<RetentionStats> {
        self.check()?;
        let current = self.current.read().map_err(|_| Error::Poisoned)?;
        let mut leases = self.leases.lock().map_err(|_| Error::Poisoned)?;
        leases.retain(|lease| lease.strong_count() != 0);
        let live: Vec<_> = leases.iter().filter_map(Weak::upgrade).collect();
        let versions: BTreeSet<_> = live.iter().map(|l| l.version.sequence).collect();
        Ok(RetentionStats {
            published_sequence: current.sequence,
            snapshots: live.len(),
            pinned_versions: versions.len(),
            oldest_pinned_sequence: versions.first().copied(),
            pinned_bytes: live
                .iter()
                .fold(0u64, |n, l| n.saturating_add(l.version.view.bytes())),
            current_bytes: current.view.bytes(),
        })
    }
    fn root(&self) -> Result<Arc<Version<V>>> {
        self.check()?;
        Ok(self.current.read().map_err(|_| Error::Poisoned)?.clone())
    }
    fn publish(&self, sequence: u64, view: V) -> Result<()> {
        // The caller retains the exclusive production state guard. Failure of
        // this private lock is treated as poisoned; no response is released.
        let mut current = self.current.write().map_err(|_| Error::Poisoned)?;
        *current = Arc::new(Version { sequence, view });
        Ok(())
    }
    fn panic_guard(&self) -> PanicGuard<'_> {
        PanicGuard(&self.failed)
    }
    fn no_pins(&self) -> Result<()> {
        if self.retention()?.snapshots != 0 {
            return Err(Error::Busy);
        }
        Ok(())
    }
}
struct PanicGuard<'a>(&'a AtomicBool);
impl Drop for PanicGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.store(true, Ordering::Release);
        }
    }
}
struct Shared<R>(Arc<R>);
impl<R: Record> Record for Shared<R> {
    const SCHEMA: Schema = R::SCHEMA;
    fn encode(&self, e: &mut Encoder) -> Result<()> {
        self.0.encode(e)
    }
    fn decode(d: &mut Decoder<'_>) -> Result<Self> {
        Ok(Self(Arc::new(R::decode(d)?)))
    }
}
fn unshare<R>(value: Shared<R>) -> Result<R> {
    Arc::try_unwrap(value.0).map_err(|_| Error::Busy)
}
struct Rows<R>(VersionTree<u64, Arc<R>>);
impl<R> Clone for Rows<R> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<R: Record> View for Rows<R> {
    fn bytes(&self) -> u64 {
        self.0.accounted_bytes()
    }
}
struct SingleEngine<R: Record> {
    database: Database<Shared<R>>,
    publisher: Publisher<Rows<R>>,
    footprint: fn(&R) -> Result<u64>,
}
fn arc_prefix<T>() -> u64 {
    // Account Arc control counters and native alignment padding, separately
    // from allocator rounding/metadata which cannot be measured portably.
    (2 * std::mem::size_of::<usize>()).next_multiple_of(std::mem::align_of::<T>()) as u64
}
fn row_bytes<R>(row: &R, footprint: fn(&R) -> Result<u64>, node_bytes: u64) -> Result<u64> {
    let bytes = footprint(row)?;
    if bytes < std::mem::size_of::<R>() as u64 {
        return Err(Error::InvalidOperation(
            "row footprint must include its inline native size".into(),
        ));
    }
    bytes
        .checked_add(node_bytes)
        .and_then(|n| n.checked_add(arc_prefix::<R>()))
        .ok_or_else(|| Error::InvalidOperation("row footprint overflow".into()))
}
impl<R: Record> SingleEngine<R> {
    fn new(
        database: Database<R>,
        options: SnapshotOptions,
        footprint: fn(&R) -> Result<u64>,
    ) -> Result<Self> {
        options.validate()?;
        let database = database.map_records(|_, r| Ok(Shared(Arc::new(r))))?;
        let (rows, sequence) = {
            let state = database.read()?;
            let mut rows = VersionTree::default();
            for (&key, row) in &state.state.rows {
                rows = rows.insert(
                    key,
                    row.0.clone(),
                    row_bytes(&*row.0, footprint, VersionTree::<u64, Arc<R>>::node_bytes())?,
                );
            }
            (Rows(rows), state.sequence())
        };
        Ok(Self {
            database,
            publisher: Publisher::new(sequence, rows, options)?,
            footprint,
        })
    }
    fn inspect<T>(&self, result: Result<T>) -> Result<T> {
        if result.is_err()
            && self
                .database
                .state
                .read()
                .map_or(true, |state| state.failed)
        {
            self.publisher.failed.store(true, Ordering::Release);
        }
        result
    }
    fn into_database(self) -> Result<Database<R>> {
        self.publisher.no_pins()?;
        drop(self.publisher);
        self.database.map_records(|_, r| unshare(r))
    }
}
/// Immediate-sync writes and independent immutable snapshots of a single table.
/// Consume a baseline handle; the one-time conversion moves rows into Arcs.
/// Clone the controller to share it; snapshot clones share one admitted lease.
pub struct SnapshotDatabase<R: Record> {
    engine: Arc<SingleEngine<R>>,
}
impl<R: Record> Clone for SnapshotDatabase<R> {
    fn clone(&self) -> Self {
        Self {
            engine: self.engine.clone(),
        }
    }
}
impl<R: Record> Database<R> {
    /// Enable immutable snapshots with explicit reader limits and a trusted,
    /// pure native-memory assessment (inline size plus owned capacities and
    /// nested data). This never changes schema/format or sync semantics. A failed
    /// conversion consumes the handle and leaves persistent bytes unchanged.
    pub fn into_snapshots(
        self,
        options: SnapshotOptions,
        footprint: fn(&R) -> Result<u64>,
    ) -> Result<SnapshotDatabase<R>> {
        Ok(SnapshotDatabase {
            engine: Arc::new(SingleEngine::new(self, options, footprint)?),
        })
    }
}
/// Immutable native rows at one synchronized sequence. Holding this lease
/// retains memory, not a filesystem generation or the controller's LOCK.
/// New read calls refuse after a writer failure/panic; an already returned
/// immutable reference cannot be revoked. Records must obey value semantics.
pub struct Snapshot<R: Record> {
    lease: Arc<Lease<Rows<R>>>,
}
impl<R: Record> Clone for Snapshot<R> {
    fn clone(&self) -> Self {
        Self {
            lease: self.lease.clone(),
        }
    }
}
impl<R: Record> Snapshot<R> {
    /// The committed sequence shared by every row in this immutable view.
    pub fn sequence(&self) -> Result<u64> {
        self.lease.check()?;
        Ok(self.lease.version.sequence)
    }
    /// Borrow a native row without decoding or cloning it.
    pub fn get(&self, key: u64) -> Result<Option<&R>> {
        self.lease.check()?;
        Ok(self.lease.version.view.0.get(&key).map(Arc::as_ref))
    }
    /// Number of live rows in this version.
    pub fn len(&self) -> Result<usize> {
        self.lease.check()?;
        Ok(self.lease.version.view.0.len())
    }
    /// Whether this version has no rows.
    pub fn is_empty(&self) -> Result<bool> {
        self.lease.check()?;
        Ok(self.lease.version.view.0.is_empty())
    }
    /// Iterate native rows in primary-key order.
    pub fn iter(&self) -> Result<impl Iterator<Item = (u64, &R)>> {
        self.range(..)
    }
    /// Ordered primary-key range; invalid bounds panic as for BTreeMap.
    pub fn range(&self, range: impl RangeBounds<u64>) -> Result<impl Iterator<Item = (u64, &R)>> {
        self.lease.check()?;
        Ok(self
            .lease
            .version
            .view
            .0
            .range(range)
            .map(|(k, r)| (*k, r.as_ref())))
    }
}
/// Native transaction facade over shared rows. Changes remain private until the
/// enclosing immediate/group sync; callbacks are never automatically replayed.
pub struct SnapshotWrite<'a, R: Record> {
    transaction: crate::WriteTransaction<'a, Shared<R>>,
}
impl<R: Record> SnapshotWrite<'_, R> {
    /// Read earlier staging, then the writer's preceding committed view.
    pub fn get(&self, key: u64) -> Option<&R> {
        self.transaction.get(key).map(|r| r.0.as_ref())
    }
    /// Refuse existing keys, including earlier staging.
    pub fn insert(&mut self, key: u64, row: R) -> Result<()> {
        self.transaction.insert(key, Shared(Arc::new(row)))
    }
    /// Explicit insertion or replacement.
    pub fn put(&mut self, key: u64, row: R) {
        self.transaction.put(key, Shared(Arc::new(row)));
    }
    /// Replace an existing row without Clone.
    pub fn update(&mut self, key: u64, update: impl FnOnce(&R) -> Result<R>) -> Result<()> {
        let next = update(self.get(key).ok_or(Error::MissingKey(key))?)?;
        self.put(key, next);
        Ok(())
    }
    /// Remove a key, reporting whether it existed in this staged view.
    pub fn remove(&mut self, key: u64) -> bool {
        self.transaction.remove(key)
    }
}
struct SingleBatch<'a, R: Record> {
    state: RwLockWriteGuard<'a, State<Shared<R>>>,
    publisher: &'a Publisher<Rows<R>>,
    view: Rows<R>,
    footprint: fn(&R) -> Result<u64>,
}
impl<R: Record> Drop for SingleBatch<'_, R> {
    fn drop(&mut self) {
        if self.state.failed || std::thread::panicking() {
            self.publisher.failed.store(true, Ordering::Release);
        }
    }
}
impl<R: Record> Engine for SingleEngine<R> {
    type Transaction<'a> = SnapshotWrite<'a, R>;
    type Batch<'a> = SingleBatch<'a, R>;
    fn persistent(&self) -> Result<bool> {
        self.publisher.check()?;
        Ok(self.database.stats()?.persistent)
    }
    fn begin(&self) -> Result<Self::Batch<'_>> {
        self.publisher.check()?;
        let state = self.database.state.write().map_err(|_| Error::Poisoned)?;
        if state.failed {
            self.publisher.failed.store(true, Ordering::Release);
            return Err(Error::Poisoned);
        }
        let view = self.publisher.root()?.view.clone();
        Ok(SingleBatch {
            state,
            publisher: &self.publisher,
            view,
            footprint: self.footprint,
        })
    }
    fn execute<T>(
        batch: &mut Self::Batch<'_>,
        operation: impl FnOnce(&mut SnapshotWrite<'_, R>) -> Result<T>,
    ) -> Result<(T, u64)> {
        let mut tx = SnapshotWrite {
            transaction: crate::WriteTransaction {
                state: WriteState::Borrowed(&mut batch.state),
                changes: BTreeMap::new(),
            },
        };
        let value = operation(&mut tx)?;
        let mut view = batch.view.clone();
        for (&key, row) in &tx.transaction.changes {
            view.0 = match row {
                Some(row) => view.0.insert(
                    key,
                    row.0.clone(),
                    row_bytes(
                        &*row.0,
                        batch.footprint,
                        VersionTree::<u64, Arc<R>>::node_bytes(),
                    )?,
                ),
                None => view.0.remove(&key),
            };
        }
        view.validate_accounting()?;
        let sequence = tx.transaction.commit_unsynced()?;
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
impl<R: Record> SnapshotDatabase<R> {
    /// Serialized published engine counters; pending unsynchronized work is
    /// excluded. Unlike retention inspection, this may wait for writer I/O.
    pub fn stats(&self) -> Result<crate::Stats> {
        self.engine.inspect(self.engine.database.stats())
    }

    /// Capture the last synchronized immutable version. Admission limits return
    /// BudgetExceeded; release leases to restore capacity. Does not wait for WAL I/O.
    pub fn snapshot(&self) -> Result<Snapshot<R>> {
        Ok(Snapshot {
            lease: self.engine.publisher.capture()?,
        })
    }
    /// Inspect current accounted memory and reader retention without writer I/O.
    pub fn retention(&self) -> Result<RetentionStats> {
        self.engine.publisher.retention()
    }
    /// Run one serialized immediate-sync transaction. Snapshots continue reading
    /// their preceding coherent roots during the callback and synchronization.
    pub fn write<T>(
        &self,
        operation: impl FnOnce(&mut SnapshotWrite<'_, R>) -> Result<T>,
    ) -> Result<T> {
        let mut batch = self.engine.begin()?;
        let start = SingleEngine::<R>::sequence(&batch);
        let (value, sequence) = SingleEngine::<R>::execute(&mut batch, operation)?;
        if sequence != start {
            SingleEngine::<R>::finish(&mut batch)?;
        }
        Ok(value)
    }
    /// With exclusive controller ownership and no leases, recover the original
    /// native baseline for offline migration. A refusal consumes this client.
    pub fn into_database(self) -> Result<Database<R>> {
        Arc::try_unwrap(self.engine)
            .map_err(|_| Error::Busy)?
            .into_database()
    }
    /// Enable the same bounded shared-sync worker on this versioned table.
    pub fn into_group_commit(
        self,
        options: GroupCommitOptions,
    ) -> Result<GroupSnapshotDatabase<R>> {
        Ok(GroupSnapshotDatabase {
            runtime: Runtime::new(
                Arc::try_unwrap(self.engine).map_err(|_| Error::Busy)?,
                options,
            )?,
        })
    }
    /// Serialized verified checkpoint; existing leases retain memory only.
    pub fn checkpoint(&self) -> Result<crate::Checkpoint> {
        self.checkpoint_with_options(MaintenanceOptions::default())
    }
    /// Serialized checkpoint with the baseline encoded-resource limits.
    pub fn checkpoint_with_options(
        &self,
        options: MaintenanceOptions,
    ) -> Result<crate::Checkpoint> {
        let _panic = self.engine.publisher.panic_guard();
        self.engine
            .inspect(self.engine.database.checkpoint_with_options(options))
    }
    /// Independently decoded backup into a new directory, returned as a native
    /// baseline; old leases do not keep obsolete disk generations alive.
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<Database<R>> {
        self.engine.publisher.check()?;
        self.engine
            .inspect(self.engine.database.backup_to(path))?
            .map_records(|_, r| unshare(r))
    }
    /// Inspect selected, retained and unknown files under permanent ownership.
    pub fn storage_inventory(&self) -> Result<crate::StorageInventory> {
        self.engine
            .inspect(self.engine.database.storage_inventory())
    }
    /// Conservative explicit cleanup retaining active and previous generations.
    pub fn reclaim(&self) -> Result<crate::ReclaimReport> {
        let _panic = self.engine.publisher.panic_guard();
        self.engine.inspect(self.engine.database.reclaim())
    }
}
/// Bounded independent group commit with immutable, synchronized read versions.
pub struct GroupSnapshotDatabase<R: Record> {
    runtime: Arc<Runtime<SingleEngine<R>>>,
}
impl<R: Record> Clone for GroupSnapshotDatabase<R> {
    fn clone(&self) -> Self {
        Self {
            runtime: self.runtime.clone(),
        }
    }
}
impl<R: Record> GroupSnapshotDatabase<R> {
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

    /// Admit an independent transaction; dropping its response does not cancel.
    pub fn submit<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut SnapshotWrite<'_, R>) -> Result<T> + Send + 'static,
    ) -> Result<PendingCommit<T>> {
        self.runtime.submit(operation)
    }
    /// Capture the last synchronized coherent version without waiting for WAL I/O.
    pub fn snapshot(&self) -> Result<Snapshot<R>> {
        Ok(Snapshot {
            lease: self.runtime.engine.publisher.capture()?,
        })
    }
    /// Observe oldest pin, live versions and conservative accounted bytes.
    pub fn retention(&self) -> Result<RetentionStats> {
        self.runtime.engine.publisher.retention()
    }
    /// Drain queued work with exclusive client ownership and no leases; return
    /// the native baseline for offline migration.
    pub fn into_database(self) -> Result<Database<R>> {
        self.runtime.into_engine()?.into_database()
    }
    /// Explicit serialized checkpoint; leases continue reading memory versions.
    pub fn checkpoint(&self) -> Result<crate::Checkpoint> {
        let _panic = self.runtime.engine.publisher.panic_guard();
        self.runtime
            .engine
            .inspect(self.runtime.engine.database.checkpoint())
    }
    /// Explicit cleanup retaining active and previous disk generations.
    pub fn reclaim(&self) -> Result<crate::ReclaimReport> {
        let _panic = self.runtime.engine.publisher.panic_guard();
        self.runtime
            .engine
            .inspect(self.runtime.engine.database.reclaim())
    }
    /// Consistent independently decoded backup into a new directory.
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<Database<R>> {
        self.runtime.engine.publisher.check()?;
        self.runtime
            .engine
            .inspect(self.runtime.engine.database.backup_to(path))?
            .map_records(|_, r| unshare(r))
    }
}

#[path = "versioned_catalog.rs"]
mod catalogs;
pub use catalogs::{CatalogSnapshot, CatalogSnapshotWrite, GroupSnapshotCatalog, SnapshotCatalog};

#[cfg(test)]
#[path = "versioned_tests.rs"]
mod tests;
