use super::*;
use crate::directory::validate_migration;
use crate::{Checkpoint, GenerationInfo, PruneReport};

impl<R: Record> Database<R> {
    /// Create a new managed database directory with a stable lock and a sealed
    /// snapshot. The parent must exist; existing paths are never adopted.
    /// Prefer this backend when checkpoints, backup and migrations are needed.
    pub fn create_dir(path: impl AsRef<Path>) -> Result<Self> {
        let (directory, wal, recovered) =
            Directory::create::<R>(path.as_ref(), &BTreeMap::new(), 0, Vec::new())?;
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
        let mut state = self.state.write().map_err(|_| Error::Poisoned)?;
        if state.failed {
            return Err(Error::Poisoned);
        }
        let State {
            rows,
            directory,
            sequence,
            ..
        } = &mut *state;
        let directory = directory.as_mut().ok_or_else(|| {
            Error::InvalidOperation(
                "checkpoints require a directory database; use backup_to to import a v1 file"
                    .into(),
            )
        })?;
        let history = directory.info().migrations;
        match directory.install::<R>(rows, *sequence, history) {
            Ok((wal, recovered, checkpoint)) => {
                // Drop the old WAL while stable directory ownership is still held.
                state.wal = Some(wal);
                state.rows = recovered.rows;
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

    /// Capture a consistent backup into a NEW managed directory and return the
    /// independently decoded backup handle. Works for memory, v1 files and
    /// directory sources. Existing destinations are never overwritten.
    ///
    /// A read guard excludes writers throughout encoding and verification.
    /// This also provides the explicit v1-to-directory import path. Preserve
    /// independent backups: pruning's previous generation is not a backup policy.
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<Self> {
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
        mut convert: impl FnMut(u64, R) -> Result<N>,
    ) -> Result<Database<N>> {
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
        let mut migrated = BTreeMap::new();
        for (key, row) in rows {
            migrated.insert(key, convert(key, row)?);
        }
        let (new_wal, recovered, _) = directory.install::<N>(&migrated, sequence, history)?;
        drop(wal); // Keep directory ownership across the entire schema handoff.
        Ok(Database::from_directory(directory, new_wal, recovered))
    }
}
