use crate::Schema;
use std::{fmt, io};

/// Result type used by Skrin and application codecs.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors are explicit about corruption and uncertain commit outcomes.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// An I/O error outside a commit attempt.
    Io(io::Error),
    /// A write or sync failed. The transaction may exist after reopening.
    /// This handle is poisoned and must not be reused, even for reads.
    CommitUncertain(io::Error),
    /// Generation publication may have succeeded. Close and reopen the handle.
    MaintenanceUncertain(io::Error),
    /// The operation is invalid for this backend, schema or maintenance state.
    InvalidOperation(String),
    /// Another handle or process owns the database file lock.
    Busy,
    /// Group admission queue is full; the callback was not invoked/admitted.
    QueueFull,
    /// A prior commit failure or panic requires closing and reopening.
    Poisoned,
    /// A complete structure failed validation. No automatic repair is attempted.
    Corrupt { offset: u64, reason: String },
    /// The file uses a storage format this implementation does not understand.
    UnsupportedFormat(u32),
    /// The record type does not match the persistent schema.
    SchemaMismatch { expected: Schema, found: Schema },
    /// An insert would overwrite an existing key; use `put` to replace it.
    DuplicateKey(u64),
    /// An update requires an existing key; it never silently inserts a row.
    MissingKey(u64),
    /// Final transaction view contains duplicate keys for a unique index.
    UniqueViolation { index_id: u64 },
    /// An application codec rejected a value or input.
    Codec(String),
    /// A record or transaction exceeded a documented byte limit.
    LimitExceeded { limit: usize },
    /// A maintenance operation would exceed an explicit encoded-resource limit.
    /// `required` is the minimum needed at the rejected step, not a reservation.
    BudgetExceeded {
        resource: &'static str,
        limit: u64,
        required: u64,
    },
    /// The monotonically increasing transaction sequence has been exhausted.
    SequenceExhausted,
    /// Persistent storage is unavailable on this platform.
    UnsupportedPlatform,
}

impl Error {
    pub(crate) fn corrupt(offset: u64, reason: impl Into<String>) -> Self {
        Self::Corrupt {
            offset,
            reason: reason.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "storage I/O: {error}"),
            Self::CommitUncertain(error) => {
                write!(f, "commit outcome uncertain; close and reopen: {error}")
            }
            Self::MaintenanceUncertain(error) => {
                write!(
                    f,
                    "maintenance outcome uncertain; close and reopen: {error}"
                )
            }
            Self::InvalidOperation(reason) => write!(f, "invalid operation: {reason}"),
            Self::QueueFull => f.write_str("group commit queue is full; request was not admitted"),
            Self::Busy => f.write_str("database is already open by another handle"),
            Self::Poisoned => f.write_str("database handle is poisoned; close and reopen"),
            Self::Corrupt { offset, reason } => {
                write!(f, "corrupt storage at byte {offset}: {reason}")
            }
            Self::UnsupportedFormat(version) => {
                write!(f, "unsupported storage format version {version}")
            }
            Self::SchemaMismatch { expected, found } => {
                write!(f, "schema mismatch: expected {expected:?}, found {found:?}")
            }
            Self::UniqueViolation { index_id } => write!(f, "unique index {index_id} violation"),
            Self::MissingKey(key) => write!(f, "missing primary key {key}"),
            Self::DuplicateKey(key) => write!(f, "duplicate primary key {key}"),
            Self::Codec(reason) => write!(f, "record codec: {reason}"),
            Self::LimitExceeded { limit } => write!(f, "encoded data exceeds {limit} bytes"),
            Self::BudgetExceeded {
                resource,
                limit,
                required,
            } => {
                write!(
                    f,
                    "maintenance budget for {resource}: need at least {required}, limit {limit}"
                )
            }
            Self::SequenceExhausted => f.write_str("transaction sequence exhausted"),
            Self::UnsupportedPlatform => {
                f.write_str("persistent storage currently requires a Unix filesystem")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) | Self::CommitUncertain(error) | Self::MaintenanceUncertain(error) => {
                Some(error)
            }
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
