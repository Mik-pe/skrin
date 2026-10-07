//! Stable ownership and explicit publication of complete storage generations.
use crate::log::file_storage::LockedFile;
use crate::log::{Recovered, Storage, Wal, checksum, supported_platform, sync_parent};
use crate::{
    Decoder, Encoder, Error, MaintenanceEstimate, MaintenanceOptions, Record, Result, Schema,
    snapshot,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const MAX_MANIFEST: usize = 64 * 1024;
const MAX_MIGRATIONS: usize = 128;

/// One explicit, successfully published application-schema transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Migration {
    /// Stable application-assigned identifier; never reused within the history.
    pub id: String,
    /// Source record schema version.
    pub from_version: u32,
    /// Destination record schema version, strictly greater than the source.
    pub to_version: u32,
    /// Committed sequence preserved by the migration.
    pub at_sequence: u64,
}

/// Durable metadata for the active directory generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationInfo {
    /// Active generation; a generation number is not a transaction sequence.
    pub generation: u64,
    /// Previously active generation retained by pruning, or zero at creation.
    pub previous_generation: u64,
    /// Committed sequence captured by the active snapshot.
    pub checkpoint_sequence: u64,
    /// Persistent record schema.
    pub schema: Schema,
    /// Successfully published schema transitions, preserved by backups.
    pub migrations: Vec<Migration>,
}

/// The result of a completed checkpoint, excluding any subsequent pruning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    /// Newly published generation.
    pub generation: u64,
    /// Exact committed sequence captured without inventing extra commits.
    pub sequence: u64,
    /// Number of live records captured.
    pub rows: usize,
    /// Size of the sealed snapshot on disk.
    pub snapshot_bytes: u64,
}

/// Explicit cleanup retains the active and immediately previous generations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Number of obsolete generation directories successfully removed.
    pub generations_removed: u64,
    /// Bytes removed from recognized obsolete engine files.
    pub bytes_removed: u64,
    /// Candidate directories left untouched because their contents were unknown.
    pub skipped: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Manifest {
    info: GenerationInfo,
    rows: u64,
}

pub(crate) struct Directory {
    root: PathBuf,
    // Never replace/unlink LOCK. Its lifetime spans every WAL replacement.
    _lock: LockedFile,
    manifest: Manifest,
}

pub(crate) fn boundary() -> io::Result<()> {
    #[cfg(all(test, unix))]
    {
        crate::persistence_model::boundary();
        faults::hit()
    }
    #[cfg(not(all(test, unix)))]
    Ok(())
}

fn invalid(reason: &str) -> Error {
    Error::InvalidOperation(reason.into())
}

pub(crate) fn validate_migration(
    info: &GenerationInfo,
    target: Schema,
    id: &str,
    sequence: u64,
) -> Result<Vec<Migration>> {
    if target.table_id != info.schema.table_id || target.version <= info.schema.version {
        return Err(invalid(
            "migration must preserve table identity and increase the schema version",
        ));
    }
    if !valid_id(id) || info.migrations.len() >= MAX_MIGRATIONS {
        return Err(invalid(
            "migration ID must be 1..128 ASCII letters/digits/._-; history limit is 128",
        ));
    }
    if info.migrations.iter().any(|entry| entry.id == id) {
        return Err(invalid("migration ID has already been applied"));
    }
    let mut history = info.migrations.clone();
    history.push(Migration {
        id: id.into(),
        from_version: info.schema.version,
        to_version: target.version,
        at_sequence: sequence,
    });
    Ok(history)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}

impl Manifest {
    fn encode(&self) -> Result<Vec<u8>> {
        let mut encoder = Encoder::default();
        encoder.u64(self.info.generation)?;
        encoder.u64(self.info.previous_generation)?;
        encoder.u64(self.info.schema.table_id)?;
        encoder.u32(self.info.schema.version)?;
        encoder.u64(self.info.checkpoint_sequence)?;
        encoder.u64(self.rows)?;
        encoder.u32(self.info.migrations.len() as u32)?;
        for entry in &self.info.migrations {
            encoder.string(&entry.id)?;
            encoder.u32(entry.from_version)?;
            encoder.u32(entry.to_version)?;
            encoder.u64(entry.at_sequence)?;
        }
        let mut bytes = b"SKRDIR01".to_vec();
        bytes.extend_from_slice(&encoder.finish());
        bytes.extend_from_slice(&checksum(&bytes).to_le_bytes());
        if bytes.len() > MAX_MANIFEST {
            return Err(Error::LimitExceeded {
                limit: MAX_MANIFEST,
            });
        }
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 12
            || &bytes[..8] != b"SKRDIR01"
            || checksum(&bytes[..bytes.len() - 4])
                != u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().unwrap())
        {
            return Err(Error::corrupt(0, "invalid CURRENT manifest or checksum"));
        }
        let decode = || -> Result<Self> {
            let mut decoder = Decoder::new(&bytes[8..bytes.len() - 4]);
            let generation = decoder.u64()?;
            let previous_generation = decoder.u64()?;
            let schema = Schema {
                table_id: decoder.u64()?,
                version: decoder.u32()?,
            };
            let checkpoint_sequence = decoder.u64()?;
            let rows = decoder.u64()?;
            let count = decoder.u32()? as usize;
            if generation == 0 || previous_generation >= generation || count > MAX_MIGRATIONS {
                return Err(invalid("invalid generation lineage or migration count"));
            }
            let mut migrations: Vec<Migration> = Vec::new();
            let mut ids = BTreeSet::new();
            for _ in 0..count {
                let id = decoder.string()?.to_owned();
                let from_version = decoder.u32()?;
                let to_version = decoder.u32()?;
                let at_sequence = decoder.u64()?;
                if !valid_id(&id)
                    || !ids.insert(id.clone())
                    || from_version >= to_version
                    || at_sequence > checkpoint_sequence
                    || migrations.last().is_some_and(|last| {
                        last.to_version != from_version || last.at_sequence > at_sequence
                    })
                {
                    return Err(invalid("invalid migration history"));
                }
                migrations.push(Migration {
                    id,
                    from_version,
                    to_version,
                    at_sequence,
                });
            }
            if migrations
                .last()
                .is_some_and(|last| last.to_version != schema.version)
            {
                return Err(invalid(
                    "migration history does not end at the active schema",
                ));
            }
            decoder.finish()?;
            Ok(Self {
                rows,
                info: GenerationInfo {
                    generation,
                    previous_generation,
                    schema,
                    checkpoint_sequence,
                    migrations,
                },
            })
        };
        decode().map_err(|error| Error::corrupt(0, error.to_string()))
    }
}

fn require_regular(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(Error::corrupt(
            0,
            "managed storage requires regular files, not links or special files",
        ));
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    require_regular(path)?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::LimitExceeded { limit });
    }
    Ok(bytes)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    boundary()?;
    File::open(path)?.sync_all()?;
    #[cfg(all(test, unix))]
    crate::persistence_model::directory_synced(path);
    boundary()
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    boundary()?;
    let mut file = File::create_new(path)?;
    let middle = bytes.len() / 2;
    file.write_all(&bytes[..middle])?;
    boundary()?;
    file.write_all(&bytes[middle..])?;
    boundary()?;
    file.sync_all()?;
    #[cfg(all(test, unix))]
    crate::persistence_model::file_synced(&file);
    boundary()?;
    Ok(())
}

fn generation_name(generation: u64) -> String {
    format!("g{generation:016x}")
}

fn parse_generation(name: &str) -> Option<u64> {
    if name.len() != 17 || !name.starts_with('g') {
        return None;
    }
    let value = u64::from_str_radix(&name[1..], 16).ok()?;
    (value != 0 && generation_name(value) == name).then_some(value)
}

fn owner_marker(table: u64, generation: u64) -> Vec<u8> {
    let mut bytes = b"SKROWN01".to_vec();
    bytes.extend_from_slice(&table.to_le_bytes());
    bytes.extend_from_slice(&generation.to_le_bytes());
    bytes.extend_from_slice(&checksum(&bytes).to_le_bytes());
    bytes
}

impl Directory {
    pub(crate) fn info(&self) -> GenerationInfo {
        self.manifest.info.clone()
    }

    pub(crate) fn create<R: Record>(
        path: &Path,
        rows: &BTreeMap<u64, R>,
        sequence: u64,
        history: Vec<Migration>,
        options: MaintenanceOptions,
    ) -> Result<(Self, Wal, Recovered<R>)> {
        supported_platform()?;
        options.validate()?;
        options.check_rows(rows.len() as u64)?;
        // Reject impossible minimum budgets without even creating the destination.
        options.check_bytes(8 + 28 + 56 + 60 + 48)?;
        fs::create_dir(path)?; // create-only, never adopt an existing directory
        sync_parent(path)?;
        let root = fs::canonicalize(path)?;
        let mut lock = LockedFile::acquire(File::create_new(root.join("LOCK"))?)?;
        lock.write_all(b"SKRLOCK1")?;
        lock.sync()?;
        sync_directory(&root)?;
        let manifest = Manifest {
            rows: 0,
            info: GenerationInfo {
                generation: 0,
                previous_generation: 0,
                checkpoint_sequence: 0,
                schema: R::SCHEMA,
                migrations: Vec::new(),
            },
        };
        let mut directory = Self {
            root,
            _lock: lock,
            manifest,
        };
        let mut decoded = BTreeMap::new();
        let (wal, _) =
            directory.install::<R>(rows, sequence, history, options, 8, |key, row| {
                decoded.insert(key, row);
                Ok(())
            })?;
        Ok((
            directory,
            wal,
            Recovered {
                rows: decoded,
                sequence,
                discarded: 0,
            },
        ))
    }

    pub(crate) fn open<R: Record>(path: &Path) -> Result<(Self, Wal, Recovered<R>)> {
        supported_platform()?;
        let root = fs::canonicalize(path)?;
        require_regular(&root.join("LOCK"))?;
        let mut lock = LockedFile::acquire(
            File::options()
                .read(true)
                .write(true)
                .open(root.join("LOCK"))?,
        )?;
        let mut magic = Vec::new();
        (&mut lock).take(9).read_to_end(&mut magic)?;
        if magic != b"SKRLOCK1" {
            return Err(Error::corrupt(0, "invalid directory lock identity"));
        }
        let manifest = Manifest::decode(&read_bounded(&root.join("CURRENT"), MAX_MANIFEST)?)?;
        if manifest.info.schema != R::SCHEMA {
            return Err(Error::SchemaMismatch {
                expected: R::SCHEMA,
                found: manifest.info.schema,
            });
        }
        let generation = manifest.info.generation;
        let dir = root.join(generation_name(generation));
        if !fs::symlink_metadata(&dir)?.is_dir() {
            return Err(Error::corrupt(
                0,
                "active generation must be a real directory",
            ));
        }
        require_regular(&dir.join("snapshot"))?;
        require_regular(&dir.join("wal"))?;
        if read_bounded(&dir.join("OWNER"), 28)? != owner_marker(R::SCHEMA.table_id, generation) {
            return Err(Error::corrupt(0, "generation ownership mismatch"));
        }
        let rows = snapshot::read::<R>(
            &dir.join("snapshot"),
            generation,
            manifest.info.checkpoint_sequence,
            manifest.rows,
        )?;
        let (wal, recovered) = Wal::open_segment::<R>(
            &dir.join("wal"),
            rows,
            manifest.info.checkpoint_sequence,
            generation,
        )?;
        // Reconfirm publication durability, including an earlier uncertain rename.
        sync_directory(&root)?;
        sync_parent(&root)?;
        Ok((
            Self {
                root,
                _lock: lock,
                manifest,
            },
            wal,
            recovered,
        ))
    }

    fn next_manifest<R: Record>(
        &self,
        count: usize,
        sequence: u64,
        history: Vec<Migration>,
    ) -> Result<Manifest> {
        Ok(Manifest {
            rows: count as u64,
            info: GenerationInfo {
                generation: self
                    .manifest
                    .info
                    .generation
                    .checked_add(1)
                    .ok_or(Error::SequenceExhausted)?,
                previous_generation: self.manifest.info.generation,
                checkpoint_sequence: sequence,
                schema: R::SCHEMA,
                migrations: history,
            },
        })
    }

    pub(crate) fn estimate<R: Record>(
        &self,
        rows: &BTreeMap<u64, R>,
        sequence: u64,
        options: MaintenanceOptions,
    ) -> Result<MaintenanceEstimate> {
        let manifest = self.next_manifest::<R>(rows.len(), sequence, self.info().migrations)?;
        snapshot::estimate(
            rows,
            sequence,
            28 + 56 + manifest.encode()?.len() as u64,
            options,
        )
    }

    pub(crate) fn install<R: Record>(
        &mut self,
        rows: &BTreeMap<u64, R>,
        sequence: u64,
        history: Vec<Migration>,
        options: MaintenanceOptions,
        extra_bytes: u64,
        accept: impl FnMut(u64, R) -> Result<()>,
    ) -> Result<(Wal, Checkpoint)> {
        options.validate()?;
        options.check_rows(rows.len() as u64)?;
        let mut manifest = self.next_manifest::<R>(rows.len(), sequence, history)?;
        let overhead = 28 + 56 + manifest.encode()?.len() as u64 + extra_bytes;
        options.check_bytes(overhead + 48)?;
        let mut generation = self.manifest.info.generation;
        let mut staged = None;
        for _ in 0..1024 {
            generation = generation.checked_add(1).ok_or(Error::SequenceExhausted)?;
            let path = self.root.join(generation_name(generation));
            boundary()?;
            match fs::create_dir(&path) {
                Ok(()) => {
                    staged = Some(path);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let dir = staged.ok_or_else(|| {
            invalid("too many orphan generations; inspect/reclaim storage before retrying")
        })?;
        manifest.info.generation = generation;
        let encoded_manifest = manifest.encode()?;
        Manifest::decode(&encoded_manifest)?;
        write_new(
            &dir.join("OWNER"),
            &owner_marker(R::SCHEMA.table_id, generation),
        )?;
        let bytes = snapshot::write::<R>(
            &dir.join("snapshot"),
            generation,
            sequence,
            rows,
            overhead,
            options,
        )?;
        let wal = Wal::create_segment::<R>(&dir.join("wal"), generation, sequence)?;
        boundary()?;
        // One real decoder, one row at a time. Checkpoints/migrations discard
        // verification rows instead of constructing a second resident table.
        snapshot::visit::<R>(
            &dir.join("snapshot"),
            generation,
            sequence,
            rows.len() as u64,
            options,
            accept,
        )?;
        sync_directory(&dir)?;
        // The generation's name must be durable BEFORE CURRENT can refer to it.
        sync_directory(&self.root)?;
        let temporary = self.root.join(format!("CURRENT-{generation:016x}.tmp"));
        write_new(&temporary, &encoded_manifest)?;
        boundary()?;
        let publish = || -> io::Result<()> {
            fs::rename(&temporary, self.root.join("CURRENT"))?;
            #[cfg(all(test, unix))]
            crate::persistence_model::manifest_renamed(&self.root, &temporary);
            boundary()?;
            sync_directory(&self.root)
        };
        publish().map_err(Error::MaintenanceUncertain)?;
        self.manifest = manifest;
        Ok((
            wal,
            Checkpoint {
                generation,
                sequence,
                rows: rows.len(),
                snapshot_bytes: bytes,
            },
        ))
    }

    pub(crate) fn prune(&self) -> Result<PruneReport> {
        let report = self.cleanup(false)?;
        Ok(PruneReport {
            generations_removed: report.generations_removed,
            bytes_removed: report.bytes_removed,
            skipped: report.skipped,
        })
    }
}

#[cfg(all(test, unix))]
pub(crate) mod faults {
    use std::cell::Cell;
    use std::io;
    thread_local! {
        static FAIL_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
        static EXIT_ON_HIT: Cell<bool> = const { Cell::new(false) };
    }
    pub(crate) fn arm(after: usize) {
        FAIL_AFTER.with(|value| value.set(Some(after)));
    }
    pub(crate) fn exit_after(after: usize) {
        arm(after);
        EXIT_ON_HIT.with(|value| value.set(true));
    }
    pub(crate) fn clear() {
        FAIL_AFTER.with(|value| value.set(None));
    }
    pub(crate) fn active() -> bool {
        FAIL_AFTER.with(|value| value.get().is_some())
    }
    pub(crate) fn hit() -> io::Result<()> {
        FAIL_AFTER.with(|value| match value.get() {
            None => Ok(()),
            Some(0) => {
                value.set(None);
                if EXIT_ON_HIT.with(Cell::get) {
                    std::process::exit(73);
                }
                Err(io::Error::other("injected maintenance boundary failure"))
            }
            Some(left) => {
                value.set(Some(left - 1));
                Ok(())
            }
        })
    }
}

#[cfg(all(test, unix))]
#[path = "directory_tests.rs"]
mod tests;

#[path = "inventory.rs"]
mod inventory;
pub use inventory::{ReclaimReport, StorageEntry, StorageEntryKind, StorageInventory};
