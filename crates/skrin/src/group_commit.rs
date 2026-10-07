//! Bounded independent transactions sharing a synchronization boundary.
use crate::database::{State, WriteState};
use crate::{Database, Error, ReadTransaction, Record, Result, Stats, WriteTransaction};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, RwLockWriteGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Queue and collection limits. They bound transaction count, not arbitrary
/// memory captured by application callbacks. Callbacks are never replayed.
#[derive(Clone, Copy, Debug)]
pub struct GroupCommitOptions {
    /// Maximum queued requests, excluding the active group. Must be nonzero.
    pub queue_capacity: usize,
    /// Maximum requests collected in one group, in admission order.
    pub max_transactions: usize,
    /// Maximum collection delay from the first request's admission. Does not
    /// bound callback execution, prior queue work, lock waiting or I/O latency.
    pub max_delay: Duration,
}
impl Default for GroupCommitOptions {
    fn default() -> Self {
        Self {
            queue_capacity: 64,
            max_transactions: 16,
            max_delay: Duration::from_millis(1),
        }
    }
}
impl GroupCommitOptions {
    fn validate(self) -> Result<()> {
        if self.queue_capacity == 0
            || self.max_transactions == 0
            || self.max_transactions > 1024
            || self.max_delay > Duration::from_secs(1)
        {
            return Err(Error::InvalidOperation("group queue must be nonempty, group size 1..=1024 and collection delay at most one second".into()));
        }
        Ok(())
    }
}

/// Successful independent transaction; the value is released after shared sync.
#[derive(Debug)]
pub struct CommitReceipt<T> {
    /// Application closure's return value.
    pub value: T,
    /// This transaction's sequence; a no-op retains its preceding sequence.
    pub sequence: u64,
    /// Last sequence covered by this successful shared synchronization.
    pub synchronized_sequence: u64,
    /// Number of nonempty independent WAL frames in the synchronization group.
    pub transactions_in_group: usize,
    /// Admission to callback start, including queue/collection/lock waiting.
    pub queue_time: Duration,
}
/// An admitted request. Dropping it does not cancel execution or prove absence.
pub struct PendingCommit<T> {
    receiver: mpsc::Receiver<Result<CommitReceipt<T>>>,
}
impl<T> PendingCommit<T> {
    /// Wait for completion. A vanished worker leaves an uncertain outcome;
    /// reopen and reconcile an operation ID rather than replaying a callback.
    pub fn wait(self) -> Result<CommitReceipt<T>> {
        self.receiver.recv().unwrap_or_else(|_| {
            Err(Error::CommitUncertain(io::Error::other(
                "group worker ended before delivering the outcome",
            )))
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) struct GroupInfo {
    frames: usize,
    sequence: u64,
}
#[derive(Clone)]
pub(crate) enum GroupFailure {
    Poisoned,
    Uncertain {
        kind: io::ErrorKind,
        raw: Option<i32>,
        message: String,
    },
}
impl GroupFailure {
    fn from_error(error: &Error) -> Self {
        match error {
            Error::CommitUncertain(error) => Self::Uncertain {
                kind: error.kind(),
                raw: error.raw_os_error(),
                message: error.to_string(),
            },
            _ => Self::Poisoned,
        }
    }
    fn error(&self) -> Error {
        match self {
            Self::Poisoned => Error::Poisoned,
            Self::Uncertain { kind, raw, message } => Error::CommitUncertain(raw.map_or_else(
                || io::Error::new(*kind, message.clone()),
                io::Error::from_raw_os_error,
            )),
        }
    }
}

pub(crate) trait Engine: Send + Sync + 'static {
    type Transaction<'a>
    where
        Self: 'a;
    type Batch<'a>
    where
        Self: 'a;
    fn persistent(&self) -> Result<bool>;
    fn begin(&self) -> Result<Self::Batch<'_>>;
    fn execute<T>(
        batch: &mut Self::Batch<'_>,
        operation: impl FnOnce(&mut Self::Transaction<'_>) -> Result<T>,
    ) -> Result<(T, u64)>;
    fn failed(batch: &Self::Batch<'_>) -> bool;
    fn sequence(batch: &Self::Batch<'_>) -> u64;
    fn finish(batch: &mut Self::Batch<'_>) -> Result<()>;
}
impl<R: Record> Engine for Database<R> {
    type Transaction<'a> = WriteTransaction<'a, R>;
    type Batch<'a> = RwLockWriteGuard<'a, State<R>>;
    fn persistent(&self) -> Result<bool> {
        Ok(self.stats()?.persistent)
    }
    fn begin(&self) -> Result<Self::Batch<'_>> {
        let state = self.state.write().map_err(|_| Error::Poisoned)?;
        if state.failed {
            return Err(Error::Poisoned);
        }
        Ok(state)
    }
    fn execute<T>(
        state: &mut Self::Batch<'_>,
        operation: impl FnOnce(&mut WriteTransaction<'_, R>) -> Result<T>,
    ) -> Result<(T, u64)> {
        let mut tx = WriteTransaction {
            state: WriteState::Borrowed(state),
            changes: BTreeMap::new(),
        };
        let value = operation(&mut tx)?;
        let sequence = tx.commit_unsynced()?;
        Ok((value, sequence))
    }
    fn failed(state: &Self::Batch<'_>) -> bool {
        state.failed
    }
    fn sequence(state: &Self::Batch<'_>) -> u64 {
        state.sequence
    }
    fn finish(state: &mut Self::Batch<'_>) -> Result<()> {
        finish_state(state)
    }
}
pub(crate) fn finish_state<R>(state: &mut State<R>) -> Result<()> {
    if let Some(wal) = &state.wal
        && let Err(error) = wal.sync_group()
    {
        state.failed = true;
        return Err(Error::CommitUncertain(error));
    }
    Ok(())
}

type Completion = Box<dyn FnOnce(std::result::Result<GroupInfo, GroupFailure>) + Send>;
trait Job<E: Engine>: Send {
    fn execute(self: Box<Self>, batch: &mut E::Batch<'_>) -> (Completion, Option<GroupFailure>);
    fn refuse(self: Box<Self>, failure: GroupFailure);
}
struct Request<F, T> {
    operation: F,
    response: mpsc::Sender<Result<CommitReceipt<T>>>,
    admitted: Instant,
}
impl<E, F, T> Job<E> for Request<F, T>
where
    E: Engine,
    F: for<'a> FnOnce(&mut E::Transaction<'a>) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    fn execute(self: Box<Self>, batch: &mut E::Batch<'_>) -> (Completion, Option<GroupFailure>) {
        let Self {
            operation,
            response,
            admitted,
        } = *self;
        let queue_time = admitted.elapsed();
        let result = E::execute(batch, operation);
        let failure = if E::failed(batch) {
            Some(GroupFailure::from_error(
                result.as_ref().err().expect("failed append"),
            ))
        } else {
            None
        };
        (
            Box::new(move |group| {
                let result = match result {
                    Err(error) => Err(error),
                    Ok((value, sequence)) => group
                        .map(|info| CommitReceipt {
                            value,
                            sequence,
                            synchronized_sequence: info.sequence,
                            transactions_in_group: info.frames,
                            queue_time,
                        })
                        .map_err(|failure| failure.error()),
                };
                let _ = response.send(result);
            }),
            failure,
        )
    }
    fn refuse(self: Box<Self>, failure: GroupFailure) {
        let _ = self.response.send(Err(failure.error()));
    }
}
struct Queued<E: Engine> {
    job: Box<dyn Job<E>>,
    admitted: Instant,
}
struct QueueState<E: Engine> {
    requests: VecDeque<Queued<E>>,
    closed: bool,
}
struct Queue<E: Engine> {
    state: Mutex<QueueState<E>>,
    ready: Condvar,
    options: GroupCommitOptions,
}
pub(crate) struct Runtime<E: Engine> {
    pub(crate) engine: Arc<E>,
    queue: Arc<Queue<E>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
impl<E: Engine> Runtime<E> {
    pub(crate) fn new(engine: E, options: GroupCommitOptions) -> Result<Arc<Self>> {
        options.validate()?;
        if !engine.persistent()? {
            return Err(Error::InvalidOperation(
                "group commit requires a persistent database".into(),
            ));
        }
        let engine = Arc::new(engine);
        let queue = Arc::new(Queue {
            state: Mutex::new(QueueState {
                requests: VecDeque::new(),
                closed: false,
            }),
            ready: Condvar::new(),
            options,
        });
        let worker_engine = Arc::clone(&engine);
        let worker_queue = Arc::clone(&queue);
        let worker = thread::Builder::new()
            .name("skrin-group-commit".into())
            .spawn(move || worker(worker_engine, worker_queue))?;
        Ok(Arc::new(Self {
            engine,
            queue,
            worker: Mutex::new(Some(worker)),
        }))
    }
    fn into_engine(self: Arc<Self>) -> Result<E> {
        let runtime = Arc::try_unwrap(self).map_err(|_| Error::Busy)?;
        let engine = Arc::clone(&runtime.engine);
        drop(runtime); // Close admission, drain and join before releasing ownership.
        let engine = Arc::try_unwrap(engine).map_err(|_| Error::Busy)?;
        engine.persistent()?; // Refuse a poisoned handle; dropping it releases LOCK.
        Ok(engine)
    }
    pub(crate) fn submit<T, F>(&self, operation: F) -> Result<PendingCommit<T>>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&mut E::Transaction<'a>) -> Result<T> + Send + 'static,
    {
        let (response, receiver) = mpsc::channel();
        let mut queue = self.queue.state.lock().map_err(|_| Error::Poisoned)?;
        if queue.closed {
            return Err(Error::Poisoned);
        }
        if queue.requests.len() >= self.queue.options.queue_capacity {
            return Err(Error::QueueFull);
        }
        let admitted = Instant::now();
        queue.requests.push_back(Queued {
            admitted,
            job: Box::new(Request {
                operation,
                response,
                admitted,
            }),
        });
        self.queue.ready.notify_one();
        Ok(PendingCommit { receiver })
    }
}
impl<E: Engine> Drop for Runtime<E> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.queue.state.lock() {
            state.closed = true;
            self.queue.ready.notify_one();
        }
        if let Ok(worker) = self.worker.get_mut()
            && let Some(worker) = worker.take()
            && worker.thread().id() != thread::current().id()
        {
            let _ = worker.join();
        }
    }
}
fn worker<E: Engine>(engine: Arc<E>, queue: Arc<Queue<E>>) {
    loop {
        let mut state = queue.state.lock().expect("private group queue");
        while state.requests.is_empty() && !state.closed {
            state = queue.ready.wait(state).expect("private group queue");
        }
        if state.requests.is_empty() {
            return;
        }
        let deadline = state.requests.front().unwrap().admitted + queue.options.max_delay;
        while state.requests.len() < queue.options.max_transactions
            && !state.closed
            && Instant::now() < deadline
        {
            let remaining = deadline.saturating_duration_since(Instant::now());
            state = queue
                .ready
                .wait_timeout(state, remaining)
                .expect("private group queue")
                .0;
        }
        let count = state.requests.len().min(queue.options.max_transactions);
        let jobs: Vec<_> = state
            .requests
            .drain(..count)
            .map(|request| request.job)
            .collect();
        drop(state);
        if catch_unwind(AssertUnwindSafe(|| process(&*engine, jobs))).unwrap_or(false) {
            continue;
        }
        let mut state = queue.state.lock().expect("private group queue");
        state.closed = true;
        let pending: Vec<_> = state.requests.drain(..).collect();
        drop(state);
        for request in pending {
            request.job.refuse(GroupFailure::Poisoned);
        }
        return;
    }
}
fn process<E: Engine>(engine: &E, jobs: Vec<Box<dyn Job<E>>>) -> bool {
    let mut batch = match engine.begin() {
        Ok(batch) => batch,
        Err(_) => {
            for job in jobs {
                job.refuse(GroupFailure::Poisoned);
            }
            return false;
        }
    };
    let start_sequence = E::sequence(&batch);
    let mut completions = Vec::new();
    let mut failure = None;
    for job in jobs {
        if failure.is_some() {
            job.refuse(GroupFailure::Poisoned);
            continue;
        }
        let (completion, outcome) = job.execute(&mut batch);
        completions.push(completion);
        failure = outcome;
    }
    let frames = (E::sequence(&batch) - start_sequence) as usize;
    if failure.is_none()
        && frames > 0
        && let Err(error) = E::finish(&mut batch)
    {
        failure = Some(GroupFailure::from_error(&error));
    }
    let success = failure.is_none();
    let outcome = failure.map_or(
        Ok(GroupInfo {
            frames,
            sequence: E::sequence(&batch),
        }),
        Err,
    );
    // This drop is the only reader-publication boundary. Every successful
    // frame has reached the shared sync before either guards or responses go.
    drop(batch);
    for completion in completions {
        completion(outcome.clone());
    }
    success
}

/// A persistent single table with a bounded admission queue and shared syncs.
/// Reads still block writers. Consume the baseline handle to start the worker.
pub struct GroupCommitDatabase<R: Record> {
    runtime: Arc<Runtime<Database<R>>>,
}
impl<R: Record> Clone for GroupCommitDatabase<R> {
    fn clone(&self) -> Self {
        Self {
            runtime: Arc::clone(&self.runtime),
        }
    }
}
impl<R: Record> GroupCommitDatabase<R> {
    /// Admit one independent transaction; a full queue returns `QueueFull`
    /// without invoking the callback. Dropping its response does not cancel it.
    pub fn submit<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut WriteTransaction<'_, R>) -> Result<T> + Send + 'static,
    ) -> Result<PendingCommit<T>> {
        self.runtime.submit(operation)
    }
    /// With no other clients, drain admitted requests and recover the baseline
    /// handle for explicit offline migration. Other clients cause `Busy`.
    pub fn into_database(self) -> Result<Database<R>> {
        self.runtime.into_engine()
    }
    /// Consistent borrowed view after the latest completed synchronization.
    pub fn read(&self) -> Result<ReadTransaction<'_, R>> {
        self.runtime.engine.read()
    }
    /// Inspect published counters. Pending requests are excluded.
    pub fn stats(&self) -> Result<Stats> {
        self.runtime.engine.stats()
    }
    /// Caller-driven serialized checkpoint; queued transactions may run before
    /// or after this call. It does not cancel or reorder admitted requests.
    pub fn checkpoint(&self) -> Result<crate::Checkpoint> {
        self.runtime.engine.checkpoint()
    }
    /// Inspect safe retained/unknown storage under the permanent directory lock.
    pub fn storage_inventory(&self) -> Result<crate::StorageInventory> {
        self.runtime.engine.storage_inventory()
    }
    /// Conservative serialized cleanup; never changes selected data.
    pub fn reclaim(&self) -> Result<crate::ReclaimReport> {
        self.runtime.engine.reclaim()
    }
    /// Independently decoded, consistent backup to a new directory.
    pub fn backup_to(&self, path: impl AsRef<std::path::Path>) -> Result<Database<R>> {
        self.runtime.engine.backup_to(path)
    }
}
impl<R: Record> Database<R> {
    /// Consume this persistent handle and start bounded independent group commit.
    /// The original immediate-sync API remains the reference baseline. Only a
    /// shared successful sync releases grouped results or row/index visibility.
    pub fn into_group_commit(self, options: GroupCommitOptions) -> Result<GroupCommitDatabase<R>> {
        Ok(GroupCommitDatabase {
            runtime: Runtime::new(self, options)?,
        })
    }
}

/// Schema-bound rows and all indexes sharing bounded independent group commit.
/// Final uniqueness is checked per transaction against its preceding group view.
pub struct GroupCommitCatalog<C: crate::catalog::Catalog> {
    runtime: Arc<Runtime<crate::catalog::CatalogDatabase<C>>>,
}
impl<C: crate::catalog::Catalog> Clone for GroupCommitCatalog<C> {
    fn clone(&self) -> Self {
        Self {
            runtime: Arc::clone(&self.runtime),
        }
    }
}
impl<C: crate::catalog::Catalog> GroupCommitCatalog<C> {
    /// Admit an independent multi-table transaction; no callback replay.
    pub fn submit<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut crate::catalog::CatalogWrite<'_, C>) -> Result<T> + Send + 'static,
    ) -> Result<PendingCommit<T>> {
        self.runtime.submit(operation)
    }
    /// Borrow a coherent synchronized version of every table/index. Readers
    /// still block the writer; this API does not introduce snapshot readers.
    pub fn read(&self) -> Result<crate::catalog::CatalogRead<'_, C>> {
        self.runtime.engine.read()
    }
    /// Published counters, excluding the internal catalog descriptor.
    pub fn stats(&self) -> Result<Stats> {
        self.runtime.engine.stats()
    }
    /// Caller-driven serialized checkpoint with independently checked indexes.
    pub fn checkpoint(&self) -> Result<crate::Checkpoint> {
        self.runtime.engine.checkpoint()
    }
    /// Inspect retained/unknown files without changing selected storage.
    pub fn storage_inventory(&self) -> Result<crate::StorageInventory> {
        self.runtime.engine.storage_inventory()
    }
    /// Conservative serialized cleanup retaining active/previous generations.
    pub fn reclaim(&self) -> Result<crate::ReclaimReport> {
        self.runtime.engine.reclaim()
    }
    /// Independent, decoded and index-validated backup to a new directory.
    pub fn backup_to(
        &self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<crate::catalog::CatalogDatabase<C>> {
        self.runtime.engine.backup_to(path)
    }
    /// With no other clients, drain admitted requests and recover the baseline
    /// handle for explicit offline migration. Other clients cause `Busy`.
    pub fn into_database(self) -> Result<crate::catalog::CatalogDatabase<C>> {
        self.runtime.into_engine()
    }
}
impl<C: crate::catalog::Catalog> crate::catalog::CatalogDatabase<C> {
    /// Consume this persistent catalog and start a bounded shared-sync worker.
    /// Rows and every index remain excluded from readers until the shared sync.
    pub fn into_group_commit(self, options: GroupCommitOptions) -> Result<GroupCommitCatalog<C>> {
        Ok(GroupCommitCatalog {
            runtime: Runtime::new(self, options)?,
        })
    }
}

#[cfg(test)]
#[path = "group_commit_tests.rs"]
mod tests;
