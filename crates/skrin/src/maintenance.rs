use super::*;
use crate::directory::validate_migration;
use crate::{
    Checkpoint, CheckpointPolicy, GenerationInfo, MaintenanceEstimate, MaintenanceOptions,
    PruneReport,
};

impl<R: Record> Database<R> {
    /// Create a new managed database directory with a stable lock and a sealed
    /// snapshot. The parent must exist; existing paths are never adopted.
    /// Prefer this backend when checkpoints, backup and migrations are needed.
    pub fn create_dir(path: impl AsRef<Path>) -> Result<Self> {
        let (directory, wal, recovered) = Directory::create::<R>(
            path.as_ref(),
            &BTreeMap::new(),
            0,
            Vec::new(),
            MaintenanceOptions::default(),
        )?;
        Ok(Self::from_directory(directory, wal, recovered))
    }

    /// Open exactly the generation selected by a valid CURRENT manifest.
    /// Never scans for a newest generation or falls back to an older schema.
    pub fn open_dir(path: impl AsRef<Path>) -> Result<Self> {
        let (directory, wal, recovered) = Directory::open::<R>(path.as_ref())?;
        Ok(Self::from_directory(directory, wal, recovered))
    }

    fn from_directory(directory: Directory, wal: Wal, recovered: Recovered<R>) -> Self {
        let mut database = Self::from_recovered(wal, recovered);
        database
            .state
            .get_mut()
            .expect("a new lock is not poisoned")
            .directory = Some(directory);
        database
    }

    /// Inspect generation metadata and migration history. Returns `None` for
    /// a standalone v1 file or an in-memory database. Does not scan the disk.
    pub fn generation_info(&self) -> Result<Option<GenerationInfo>> {
        Ok(self.read()?.state.directory.as_ref().map(Directory::info))
    }

    /// Publish a verified snapshot and a fresh WAL under the same directory
    /// lock. Serialized with all transactions; this is not background work.
    /// Does not consume a transaction sequence. Call `prune` for disk retention.
    ///
    /// Preparation failures leave the current handle usable. A publication
    /// error returns `MaintenanceUncertain` and poisons the handle until reopen.
    pub fn checkpoint(&self) -> Result<Checkpoint> {
        self.checkpoint_with_options(MaintenanceOptions::default())
    }

    /// Checkpoint with encoded-resource limits enforced during serialization.
    /// No available-space guess is treated as a reservation. A budget refusal
    /// is before publication and leaves this handle usable.
    pub fn checkpoint_with_options(&self, options: MaintenanceOptions) -> Result<Checkpoint> {
        options.validate()?;
        let mut state = self.state.write().map_err(|_| Error::Poisoned)?;
        Self::checkpoint_locked(&mut state, options)
    }

    /// Measure the current snapshot's exact encoded requirements without disk
    /// mutation. This traverses/encodes every row and is optional; checkpoint
    /// does not pay for an extra estimation pass. Enforce limits at execution too.
    pub fn estimate_checkpoint(&self, options: MaintenanceOptions) -> Result<MaintenanceEstimate> {
        let read = self.read()?;
        read.state
            .directory
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidOperation("estimation requires a directory database".into())
            })?
            .estimate(&read.state.rows, read.state.sequence, options)
    }

    /// Synchronously checkpoint only when a threshold is reached and new commits
    /// exist. Threshold evaluation and publication share the exclusive lock.
    /// Call `prune` or `reclaim` separately: a cleanup failure must never disguise
    /// a successful checkpoint or write as a rolled-back transaction.
    pub fn checkpoint_if_needed(
        &self,
        policy: CheckpointPolicy,
        options: MaintenanceOptions,
    ) -> Result<Option<Checkpoint>> {
        options.validate()?;
        let mut state = self.state.write().map_err(|_| Error::Poisoned)?;
        if state.failed {
            return Err(Error::Poisoned);
        }
        let info = state
            .directory
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidOperation("checkpoint policy requires a directory database".into())
            })?
            .info();
        let commits = state.sequence - info.checkpoint_sequence;
        let wal_bytes = state.wal.as_ref().map_or(0, |wal| wal.bytes);
        if commits == 0
            || !(policy.wal_bytes.is_some_and(|limit| wal_bytes >= limit)
                || policy.commits.is_some_and(|limit| commits >= limit))
        {
            return Ok(None);
        }
        Self::checkpoint_locked(&mut state, options).map(Some)
    }

    fn checkpoint_locked(state: &mut State<R>, options: MaintenanceOptions) -> Result<Checkpoint> {
        if state.failed {
            return Err(Error::Poisoned);
        }
        let State {
            rows,
            directory,
            sequence,
            ..
        } = state;
        let directory = directory.as_mut().ok_or_else(|| {
            Error::InvalidOperation(
                "checkpoints require a directory database; use backup_to to import a v1 file"
                    .into(),
            )
        })?;
        let history = directory.info().migrations;
        match directory.install::<R>(rows, *sequence, history, options, 0, |_, _| Ok(())) {
            Ok((wal, checkpoint)) => {
                // Keep the original native rows; decoding is validation, not a
                // reason to replace every object during checkpoint publication.
                state.wal = Some(wal);
                Ok(checkpoint)
            }
            Err(error) => {
                if matches!(error, Error::MaintenanceUncertain(_)) {
                    state.failed = true;
                }
                Err(error)
            }
        }
    }

    /// Remove recognized obsolete generations, retaining the active and the
    /// previously active generations. Unknown files are left untouched. This
    /// never changes the active manifest, and cleanup errors do not poison it.
    pub fn prune(&self) -> Result<PruneReport> {
        let state = self.state.write().map_err(|_| Error::Poisoned)?;
        if state.failed {
            return Err(Error::Poisoned);
        }
        state
            .directory
            .as_ref()
            .ok_or_else(|| Error::InvalidOperation("pruning requires a directory database".into()))?
            .prune()
    }

    /// Inspect immediate storage entries without creating, repairing, syncing
    /// or removing files. Unknown nested directories and symlinks are not followed.
    /// The stable directory owner is held and writers are excluded for the scan.
    pub fn storage_inventory(&self) -> Result<crate::StorageInventory> {
        let read = self.read()?;
        read.state
            .directory
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidOperation("storage inventory requires a directory database".into())
            })?
            .inventory()
    }

    /// Remove recognized obsolete/orphan generations and complete temporary
    /// manifests, including failed stages newer than CURRENT. Active + previous
    /// generations and all unknown/partial ownership markers survive. No recursive
    /// deletion, automatic recovery, or background work. Errors do not poison the
    /// active generation; re-inspect/retry to determine partial cleanup progress.
    pub fn reclaim(&self) -> Result<crate::ReclaimReport> {
        let state = self.state.write().map_err(|_| Error::Poisoned)?;
        if state.failed {
            return Err(Error::Poisoned);
        }
        state
            .directory
            .as_ref()
            .ok_or_else(|| {
                Error::InvalidOperation("reclamation requires a directory database".into())
            })?
            .cleanup(true)
    }

    /// Capture a consistent backup into a NEW managed directory and return the
    /// independently decoded backup handle. Works for memory, v1 files and
    /// directory sources. Existing destinations are never overwritten.
    ///
    /// A read guard excludes writers throughout encoding and verification.
    /// This also provides the explicit v1-to-directory import path. Preserve
    /// independent backups: pruning's previous generation is not a backup policy.
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<Self> {
        self.backup_to_with_options(path, MaintenanceOptions::default())
    }

    /// Backup with explicit limits on the new directory's encoded file sizes.
    /// Unlike checkpoint verification, the returned independent backup handle
    /// necessarily owns another resident table. Codec allocations remain trusted.
    pub fn backup_to_with_options(
        &self,
        path: impl AsRef<Path>,
        options: MaintenanceOptions,
    ) -> Result<Self> {
        let read = self.read()?;
        let history = read
            .state
            .directory
            .as_ref()
            .map_or_else(Vec::new, |directory| directory.info().migrations);
        let (directory, wal, recovered) = Directory::create::<R>(
            path.as_ref(),
            &read.state.rows,
            read.state.sequence,
            history,
            options,
        )?;
        Ok(Self::from_directory(directory, wal, recovered))
    }

    /// Consume this directory handle and migrate to a newer explicit record
    /// schema. The closure receives owned old-schema values, so no `Clone` or
    /// reinterpretation of old bytes as the new Rust type is needed.
    ///
    /// The table identity and primary keys are preserved. Conversion, encoding
    /// and full decode validation finish before publication. On any error the
    /// consumed handle is closed; reopen using the schema selected by CURRENT.
    /// A failed conversion leaves the original generation active. On an
    /// uncertain publication, inspect the reported schema rather than retrying
    /// a non-idempotent external operation. The closure must have no side effects.
    pub fn migrate<N: Record>(
        self,
        id: &str,
        convert: impl FnMut(u64, R) -> Result<N>,
    ) -> Result<Database<N>> {
        self.migrate_with_options(id, MaintenanceOptions::default(), convert)
    }

    /// Offline migration with row/encoded-file/record budgets. Conversion still
    /// constructs the destination native table; arbitrary application allocations
    /// are not bounded. A refusal closes this consumed handle without publishing.
    pub fn migrate_with_options<N: Record>(
        self,
        id: &str,
        options: MaintenanceOptions,
        mut convert: impl FnMut(u64, R) -> Result<N>,
    ) -> Result<Database<N>> {
        options.validate()?;
        let state = self.state.into_inner().map_err(|_| Error::Poisoned)?;
        if state.failed {
            return Err(Error::Poisoned);
        }
        let State {
            rows,
            wal,
            directory,
            sequence,
            ..
        } = state;
        let mut directory = directory.ok_or_else(|| {
            Error::InvalidOperation(
                "migrations require a directory database; first import with backup_to".into(),
            )
        })?;
        let history = validate_migration(&directory.info(), N::SCHEMA, id, sequence)?;
        options.check_rows(rows.len() as u64)?;
        let mut migrated = BTreeMap::new();
        for (key, row) in rows {
            migrated.insert(key, convert(key, row)?);
        }
        let (new_wal, _) =
            directory.install::<N>(&migrated, sequence, history, options, 0, |_, _| Ok(()))?;
        let recovered = Recovered {
            rows: migrated,
            sequence,
            discarded: 0,
        };
        drop(wal); // Keep directory ownership across the entire schema handoff.
        Ok(Database::from_directory(directory, new_wal, recovered))
    }
}
