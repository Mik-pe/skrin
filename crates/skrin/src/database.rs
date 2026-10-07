use crate::directory::Directory;
use crate::log::{Recovered, Wal, encode_transaction};
use crate::{Error, Record, Result};
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut, RangeBounds};
use std::path::Path;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub(crate) struct State<R> {
    pub(crate) rows: BTreeMap<u64, R>,
    pub(crate) wal: Option<Wal>,
    directory: Option<Directory>,
    pub(crate) sequence: u64,
    recovered_tail_bytes: u64,
    pub(crate) failed: bool,
}

/// A single typed table with atomic transactions and `u64` primary keys.
///
/// Share a database across threads using `Arc`. Only one process/handle may
/// open its file. Never nest transactions or acquire another transaction while
/// holding a guard: this initial lock-based implementation can deadlock.
pub struct Database<R: Record> {
    pub(crate) state: RwLock<State<R>>,
}

/// An inexpensive inspection of the current engine state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    /// Number of live records.
    pub rows: usize,
    /// Last committed sequence. Empty or rolled-back transactions do not count.
    pub commits: u64,
    /// Current log size, including its header; zero for an in-memory database.
    pub wal_bytes: u64,
    /// Incomplete suffix discarded during this handle's open, if any.
    pub recovered_tail_bytes: u64,
    /// Whether commits use a synced file rather than volatile memory.
    pub persistent: bool,
}

impl<R: Record> Database<R> {
    /// Create a volatile database. This mode performs no encoding or disk I/O.
    pub fn in_memory() -> Self {
        Self {
            state: RwLock::new(State {
                rows: BTreeMap::new(),
                wal: None,
                directory: None,
                sequence: 0,
                recovered_tail_bytes: 0,
                failed: false,
            }),
        }
    }

    /// Create a new persistent database without overwriting any existing file.
    /// Its parent directory must already exist. A failed initialization may
    /// leave a file behind; Skrin will never silently overwrite that file.
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let wal = Wal::create::<R>(path.as_ref())?;
        Ok(Self::from_recovered(
            wal,
            Recovered {
                rows: BTreeMap::new(),
                sequence: 0,
                discarded: 0,
            },
        ))
    }

    /// Open an existing database and replay its validated log.
    ///
    /// Only an incomplete final frame may be truncated; complete corrupt frames
    /// and schema mismatches are errors. An empty/truncated file is not a new
    /// database. Complete recovered frames are synced before this returns.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let (wal, recovered) = Wal::open::<R>(path.as_ref())?;
        Ok(Self::from_recovered(wal, recovered))
    }

    pub(crate) fn from_recovered(wal: Wal, recovered: Recovered<R>) -> Self {
        Self {
            state: RwLock::new(State {
                rows: recovered.rows,
                wal: Some(wal),
                directory: None,
                sequence: recovered.sequence,
                recovered_tail_bytes: recovered.discarded,
                failed: false,
            }),
        }
    }

    /// Acquire a consistent, borrowed read view. Keep it short: writers block
    /// until all read guards are dropped. This is not an MVCC snapshot.
    pub fn read(&self) -> Result<ReadTransaction<'_, R>> {
        let state = self.state.read().map_err(|_| Error::Poisoned)?;
        if state.failed {
            return Err(Error::Poisoned);
        }
        Ok(ReadTransaction { state })
    }

    /// Begin a serialized write transaction. Drop it to discard staged changes.
    /// It excludes other readers/writers until committed or dropped.
    pub fn begin_write(&self) -> Result<WriteTransaction<'_, R>> {
        let state = self.state.write().map_err(|_| Error::Poisoned)?;
        if state.failed {
            return Err(Error::Poisoned);
        }
        Ok(WriteTransaction {
            state: WriteState::Owned(state),
            changes: BTreeMap::new(),
        })
    }

    /// Run a transaction, committing only if the closure returns `Ok`.
    ///
    /// The return value is released only after commit succeeds. Keep the closure
    /// short and free of external side effects. A panic discards staging and
    /// poisons the lock, requiring this handle to be closed and reopened.
    pub fn write<T>(
        &self,
        operation: impl FnOnce(&mut WriteTransaction<'_, R>) -> Result<T>,
    ) -> Result<T> {
        let mut transaction = self.begin_write()?;
        let result = operation(&mut transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    /// Inspect the database. Do not call while holding another transaction.
    pub fn stats(&self) -> Result<Stats> {
        let read = self.read()?;
        Ok(Stats {
            rows: read.state.rows.len(),
            commits: read.state.sequence,
            wal_bytes: read.state.wal.as_ref().map_or(0, |wal| wal.bytes),
            recovered_tail_bytes: read.state.recovered_tail_bytes,
            persistent: read.state.wal.is_some(),
        })
    }
}

/// A consistent borrowed view. Dropping this guard releases its read lock.
pub struct ReadTransaction<'a, R: Record> {
    pub(crate) state: RwLockReadGuard<'a, State<R>>,
}

impl<R: Record> ReadTransaction<'_, R> {
    /// Look up a native Rust value without deserialization or cloning.
    pub fn get(&self, key: u64) -> Option<&R> {
        self.state.rows.get(&key)
    }

    /// Number of live records in this view.
    pub fn len(&self) -> usize {
        self.state.rows.len()
    }

    /// Whether this view contains no records.
    pub fn is_empty(&self) -> bool {
        self.state.rows.is_empty()
    }

    /// Iterate records in ascending primary-key order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (u64, &R)> {
        self.state.rows.iter().map(|(&key, row)| (key, row))
    }

    /// Scan a primary-key range. Invalid bounds panic as for `BTreeMap::range`.
    pub fn range(&self, range: impl RangeBounds<u64>) -> impl Iterator<Item = (u64, &R)> {
        self.state.rows.range(range).map(|(&key, row)| (key, row))
    }

    /// Last committed sequence visible to this read guard.
    pub fn sequence(&self) -> u64 {
        self.state.sequence
    }
}

/// Staged atomic changes. Untouched records are never cloned.
///
/// Statement errors leave that statement unapplied; callers using the manual
/// API may still commit earlier staging. `Database::write` rolls everything
/// back when an error is propagated out of its closure.
pub struct WriteTransaction<'a, R: Record> {
    pub(crate) state: WriteState<'a, R>,
    pub(crate) changes: BTreeMap<u64, Option<R>>,
}

pub(crate) enum WriteState<'a, R> {
    Owned(RwLockWriteGuard<'a, State<R>>),
    Borrowed(&'a mut State<R>),
}
impl<R> Deref for WriteState<'_, R> {
    type Target = State<R>;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Owned(state) => state,
            Self::Borrowed(state) => state,
        }
    }
}
impl<R> DerefMut for WriteState<'_, R> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Owned(state) => state,
            Self::Borrowed(state) => state,
        }
    }
}

impl<R: Record> WriteTransaction<'_, R> {
    /// Read your own staged changes, falling back to committed records.
    pub fn get(&self, key: u64) -> Option<&R> {
        match self.changes.get(&key) {
            Some(row) => row.as_ref(),
            None => self.state.rows.get(&key),
        }
    }

    /// Insert a new key. Existing records are never silently overwritten.
    pub fn insert(&mut self, key: u64, row: R) -> Result<()> {
        if self.get(key).is_some() {
            return Err(Error::DuplicateKey(key));
        }
        self.changes.insert(key, Some(row));
        Ok(())
    }

    /// Replace an existing row using the transaction's current view. The
    /// callback must return a complete replacement; no `Clone` is required.
    /// A missing key or callback error leaves this statement's staging unchanged.
    pub fn update(&mut self, key: u64, update: impl FnOnce(&R) -> Result<R>) -> Result<()> {
        let row = self.get(key).ok_or(Error::MissingKey(key))?;
        let replacement = update(row)?;
        self.put(key, replacement);
        Ok(())
    }

    /// Insert or replace a record explicitly.
    pub fn put(&mut self, key: u64, row: R) {
        self.changes.insert(key, Some(row));
    }

    /// Delete a key, returning whether it existed in this transaction's view.
    pub fn remove(&mut self, key: u64) -> bool {
        if self.get(key).is_none() {
            return false;
        }
        if self.state.rows.contains_key(&key) {
            self.changes.insert(key, None);
        } else {
            self.changes.remove(&key);
        }
        true
    }

    /// Discard all staged changes. Dropping the transaction does the same.
    pub fn rollback(self) {}

    /// Validate/encode, append and sync, then publish all changes under the
    /// exclusive lock. Returns the committed sequence, unchanged for a no-op.
    ///
    /// An I/O error poisons the handle and returns `CommitUncertain`: reopening
    /// can reveal that the transaction committed. Retrying external operations
    /// requires an application-level idempotency key.
    pub fn commit(self) -> Result<u64> {
        self.commit_inner(false)
    }

    // Only the group worker calls this while retaining the exclusive guard.
    // It releases neither the response nor the guard until the shared sync.
    pub(crate) fn commit_unsynced(self) -> Result<u64> {
        if !matches!(self.state, WriteState::Borrowed(_)) {
            return Err(Error::InvalidOperation(
                "deferred commit requires an enclosing publication guard".into(),
            ));
        }
        self.commit_inner(true)
    }

    fn commit_inner(mut self, grouped: bool) -> Result<u64> {
        if self.changes.is_empty() {
            return Ok(self.state.sequence);
        }
        let sequence = self
            .state
            .sequence
            .checked_add(1)
            .ok_or(Error::SequenceExhausted)?;
        if self.state.wal.is_some() {
            // User codec errors happen before touching the log and do not
            // poison the handle. No whole-database copy occurs here.
            let frame = encode_transaction(sequence, &self.changes)?;
            if let Some(wal) = &mut self.state.wal
                && let Err(error) = if grouped {
                    wal.append_unsynced(&frame)
                } else {
                    wal.append(&frame)
                }
            {
                self.state.failed = true;
                return Err(Error::CommitUncertain(error));
            }
        }
        for (key, row) in std::mem::take(&mut self.changes) {
            match row {
                Some(row) => {
                    self.state.rows.insert(key, row);
                }
                None => {
                    self.state.rows.remove(&key);
                }
            }
        }
        self.state.sequence = sequence;
        Ok(sequence)
    }
}

#[cfg(test)]
#[path = "database_tests.rs"]
mod tests;

#[path = "maintenance.rs"]
mod maintenance;
