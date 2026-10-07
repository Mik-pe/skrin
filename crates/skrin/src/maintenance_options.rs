//! Explicit encoded-resource limits, not a reservation of filesystem blocks or RAM.
use crate::{Encoder, Error, Result, codec::MAX_RECORD_BYTES};

/// Limits for one checkpoint, backup or offline migration.
///
/// Limits are enforced before writing the affected bytes or decoding a record.
/// They bound logical encoded file/record sizes and row count, **not** allocator
/// capacity, application codec allocations, resident data, filesystem metadata,
/// physical blocks, or available free space. Existing generations are excluded.
/// A refusal may leave an unpublished stage; inspect/reclaim it explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaintenanceOptions {
    /// Maximum combined lengths of newly written snapshot, WAL prelude, OWNER
    /// and temporary CURRENT manifest (plus LOCK for a new backup directory).
    pub max_new_file_bytes: u64,
    /// Maximum encoded size of one record, at most `MAX_RECORD_BYTES`.
    /// A smaller value bounds the codec's encoder before it can grow past it.
    pub max_record_bytes: usize,
    /// Maximum number of records in the resulting snapshot.
    pub max_rows: u64,
}

impl Default for MaintenanceOptions {
    fn default() -> Self {
        Self {
            max_new_file_bytes: u64::MAX,
            max_record_bytes: MAX_RECORD_BYTES,
            max_rows: u64::MAX,
        }
    }
}

impl MaintenanceOptions {
    pub(crate) fn validate(self) -> Result<()> {
        if self.max_record_bytes > MAX_RECORD_BYTES {
            return Err(Error::InvalidOperation(
                "maintenance record limit exceeds the file-format maximum".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn check_rows(self, rows: u64) -> Result<()> {
        check("snapshot rows", self.max_rows, rows)
    }

    pub(crate) fn check_bytes(self, bytes: u64) -> Result<()> {
        check("new file bytes", self.max_new_file_bytes, bytes)
    }

    pub(crate) fn encoder(self) -> Encoder {
        Encoder::with_limit(self.max_record_bytes)
    }
}

fn check(resource: &'static str, limit: u64, required: u64) -> Result<()> {
    if required > limit {
        Err(Error::BudgetExceeded {
            resource,
            limit,
            required,
        })
    } else {
        Ok(())
    }
}

/// An exact encoded-size preflight of the current rows and migration history.
///
/// Estimation calls the real codec but does not create files. It is not a disk
/// reservation: another transaction or process can change requirements/space
/// after the read guard is released. Pass limits to the maintenance operation
/// itself to enforce them again. No filesystem allocation/RSS estimate is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaintenanceEstimate {
    /// Snapshot row count.
    pub rows: u64,
    /// Committed sequence measured by the estimate.
    pub sequence: u64,
    /// Complete snapshot length, including framing/checksums.
    pub snapshot_bytes: u64,
    /// Snapshot + OWNER + fresh WAL + new CURRENT lengths; excludes old files.
    pub new_file_bytes: u64,
    /// Largest encoded record seen (zero for an empty snapshot).
    pub largest_record_bytes: usize,
}

/// Thresholds for caller-driven `checkpoint_if_needed`.
///
/// Either enabled threshold triggers a checkpoint, but only after at least one
/// new committed transaction. `None` disables a threshold; both `None` disable
/// this policy. Zero triggers at the next call after a commit. No background
/// worker is created and commits never secretly run maintenance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointPolicy {
    /// Current WAL bytes including its header.
    pub wal_bytes: Option<u64>,
    /// Committed transactions since the last checkpoint/migration.
    pub commits: Option<u64>,
}

impl Default for CheckpointPolicy {
    fn default() -> Self {
        Self {
            wal_bytes: Some(64 * 1024 * 1024),
            commits: Some(10_000),
        }
    }
}
