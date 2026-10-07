//! Non-recursive, conservative inspection/reclamation under stable directory ownership.
use super::*;

/// Classification of an immediate database-directory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StorageEntryKind {
    /// LOCK or the authoritative CURRENT manifest.
    Metadata,
    /// The selected generation; never eligible for removal.
    ActiveGeneration,
    /// The immediately previous generation; never eligible for removal.
    RetainedGeneration,
    /// Recognized generation older than active and not retained.
    ObsoleteGeneration,
    /// Recognized, unpublished generation newer than the active one.
    OrphanGeneration,
    /// Complete, validated temporary manifest with matching table/name identity.
    TemporaryManifest,
    /// Unknown name, ownership, contents or file type; never removed.
    Unknown,
}

/// One inspected immediate entry. Paths are relative to the database root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageEntry {
    /// Relative immediate entry path, never an arbitrary deletion target.
    pub path: PathBuf,
    /// Interpretation based on name, type and validated ownership metadata.
    pub kind: StorageEntryKind,
    /// Observed regular-file lengths, including direct children of reserved
    /// generation directories. Symlinks and unknown nested directories are not followed.
    pub file_bytes: u64,
    /// Eligible for `reclaim`, not a promise that a later deletion will succeed.
    pub reclaimable: bool,
}

/// Read-only inventory. This is neither a filesystem free-space query nor a
/// full corruption scan; unknown directory trees are deliberately not traversed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageInventory {
    /// Generation selected by the validated CURRENT manifest.
    pub generation: u64,
    /// Deterministically sorted by relative path.
    pub entries: Vec<StorageEntry>,
    /// Sum of the regular-file lengths observed in the entries.
    pub observed_file_bytes: u64,
    /// Sum of observed lengths for recognized removable candidates.
    pub reclaimable_file_bytes: u64,
}

/// Cleanup of recognized obsolete/orphan generations and temporary manifests.
/// Active + previous generations, LOCK, CURRENT and unknown contents survive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReclaimReport {
    /// Obsolete or unpublished generation directories completely removed.
    pub generations_removed: u64,
    /// Complete, validated temporary manifest files removed.
    pub temporary_manifests_removed: u64,
    /// Logical file lengths, not released filesystem block counts.
    pub bytes_removed: u64,
    /// Reserved-name candidates with unrecognized contents, preserved untouched.
    pub skipped: u64,
}

fn parse_temporary(name: &str) -> Option<u64> {
    let hex = name.strip_prefix("CURRENT-")?.strip_suffix(".tmp")?;
    if hex.len() != 16 {
        return None;
    }
    let value = u64::from_str_radix(hex, 16).ok()?;
    (value != 0 && format!("{value:016x}") == hex).then_some(value)
}

fn sum(left: u64, right: u64) -> Result<u64> {
    left.checked_add(right)
        .ok_or_else(|| invalid("storage inventory size overflow"))
}

/// Empty reserved directories are restartable final-deletion remnants. A
/// nonempty directory always needs the exact OWNER plus only known regular files.
fn recognized_generation(path: &Path, table: u64, generation: u64) -> Result<(bool, u64)> {
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Ok((false, 0));
    }
    let mut bytes = 0;
    let mut count = 0;
    let mut known = true;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        count += 1;
        let regular = entry.file_type()?.is_file();
        if regular {
            bytes = sum(bytes, entry.metadata()?.len())?;
        }
        if !regular
            || !matches!(
                entry.file_name().to_str(),
                Some("OWNER" | "snapshot" | "wal")
            )
        {
            known = false;
        }
    }
    if count == 0 {
        return Ok((true, 0));
    }
    let marker = read_bounded(&path.join("OWNER"), 28);
    Ok((
        known && matches!(marker, Ok(ref bytes) if *bytes == owner_marker(table, generation)),
        bytes,
    ))
}

impl Directory {
    /// Refuse cleanup if the disk identity changed. No WAL repair or writes.
    fn validate_current(&self) -> Result<()> {
        let current = Manifest::decode(&read_bounded(&self.root.join("CURRENT"), MAX_MANIFEST)?)?;
        if current != self.manifest {
            return Err(Error::corrupt(
                0,
                "CURRENT changed outside the owning handle",
            ));
        }
        Ok(())
    }

    fn recognized_temporary(&self, path: &Path, generation: u64) -> bool {
        let Ok(bytes) = read_bounded(path, MAX_MANIFEST) else {
            return false;
        };
        let Ok(manifest) = Manifest::decode(&bytes) else {
            return false;
        };
        manifest.info.generation == generation
            && manifest.info.schema.table_id == self.manifest.info.schema.table_id
            && manifest.info.previous_generation <= self.manifest.info.generation
    }

    pub(crate) fn inventory(&self) -> Result<StorageInventory> {
        self.validate_current()?;
        let info = &self.manifest.info;
        let mut inventory = StorageInventory {
            generation: info.generation,
            entries: Vec::new(),
            observed_file_bytes: 0,
            reclaimable_file_bytes: 0,
        };
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let kind = entry.file_type()?;
            let mut item = StorageEntry {
                path: name.clone().into(),
                kind: StorageEntryKind::Unknown,
                file_bytes: if kind.is_file() {
                    entry.metadata()?.len()
                } else {
                    0
                },
                reclaimable: false,
            };
            if matches!(name.to_str(), Some("LOCK" | "CURRENT")) && kind.is_file() {
                item.kind = StorageEntryKind::Metadata;
            } else if let Some(generation) = name.to_str().and_then(parse_generation) {
                let (known, bytes) = if kind.is_dir() {
                    recognized_generation(&entry.path(), info.schema.table_id, generation)?
                } else {
                    (false, item.file_bytes)
                };
                item.file_bytes = bytes;
                if generation == info.generation {
                    item.kind = StorageEntryKind::ActiveGeneration;
                } else if generation == info.previous_generation {
                    item.kind = StorageEntryKind::RetainedGeneration;
                } else if known {
                    item.kind = if generation < info.generation {
                        StorageEntryKind::ObsoleteGeneration
                    } else {
                        StorageEntryKind::OrphanGeneration
                    };
                    item.reclaimable = true;
                }
            } else if let Some(generation) = name.to_str().and_then(parse_temporary)
                && kind.is_file()
                && self.recognized_temporary(&entry.path(), generation)
            {
                item.kind = StorageEntryKind::TemporaryManifest;
                item.reclaimable =
                    generation != info.generation && generation != info.previous_generation;
            }
            inventory.observed_file_bytes = sum(inventory.observed_file_bytes, item.file_bytes)?;
            if item.reclaimable {
                inventory.reclaimable_file_bytes =
                    sum(inventory.reclaimable_file_bytes, item.file_bytes)?;
            }
            inventory.entries.push(item);
        }
        inventory.entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(inventory)
    }

    pub(crate) fn cleanup(&self, include_orphans: bool) -> Result<ReclaimReport> {
        let inventory = self.inventory()?;
        let mut report = ReclaimReport::default();
        for entry in inventory.entries {
            if entry.kind == StorageEntryKind::Unknown {
                let generation = entry.path.to_str().and_then(parse_generation);
                let temporary = entry.path.to_str().and_then(parse_temporary);
                if include_orphans && (generation.is_some() || temporary.is_some())
                    || generation.is_some_and(|g| {
                        g < self.manifest.info.generation
                            && g != self.manifest.info.previous_generation
                    })
                {
                    report.skipped += 1;
                }
                continue;
            }
            if !entry.reclaimable
                || !include_orphans && entry.kind != StorageEntryKind::ObsoleteGeneration
            {
                continue;
            }
            let path = self.root.join(&entry.path);
            if entry.kind == StorageEntryKind::TemporaryManifest {
                let generation = entry
                    .path
                    .to_str()
                    .and_then(parse_temporary)
                    .expect("classified temporary manifest");
                if !self.recognized_temporary(&path, generation) {
                    report.skipped += 1;
                    continue;
                }
                boundary()?;
                fs::remove_file(&path)?;
                report.temporary_manifests_removed += 1;
                report.bytes_removed = sum(report.bytes_removed, entry.file_bytes)?;
            } else {
                let generation = entry
                    .path
                    .to_str()
                    .and_then(parse_generation)
                    .expect("classified generation");
                if !recognized_generation(&path, self.manifest.info.schema.table_id, generation)?.0
                {
                    report.skipped += 1;
                    continue;
                }
                // Never recurse, never follow symlinks, retain OWNER until the
                // data files' deletions are durable so a retry can prove ownership.
                for name in ["snapshot", "wal", "OWNER"] {
                    let file = path.join(name);
                    match fs::symlink_metadata(&file) {
                        Ok(metadata) if metadata.is_file() => {
                            boundary()?;
                            fs::remove_file(&file)?;
                            report.bytes_removed = sum(report.bytes_removed, metadata.len())?;
                            sync_directory(&path)?;
                        }
                        Ok(_) => return Err(invalid("generation changed during cleanup")),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                boundary()?;
                fs::remove_dir(&path)?;
                report.generations_removed += 1;
            }
        }
        sync_directory(&self.root)?;
        Ok(report)
    }
}
