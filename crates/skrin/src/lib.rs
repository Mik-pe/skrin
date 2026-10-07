//! Skrin: typed records, atomic transactions, and an explicit durable log.
//!
//! `Database` implements one table; `catalog` adds schema-bound tables and atomic
//! derived indexes with `u64` keys per table. Managed directories add
//! verified checkpoints, backups and explicit offline schema migrations.
//! Reads borrow native Rust values. Read guards block writers; they are not MVCC
//! snapshots. Persistent databases currently require a local Unix filesystem.
//! No SQL, network service or local unsafe code is involved. Linux file-data
//! reservations use safe syscall bindings.

#![forbid(unsafe_code)]

#[cfg(test)]
extern crate self as skrin;

pub mod catalog;
pub mod codec;
mod database;
mod directory;
mod error;
mod log;
mod maintenance_options;
#[cfg(all(test, unix))]
mod persistence_model;
mod reservation;
mod snapshot;
#[cfg(test)]
mod test_support;

pub use codec::{Decoder, Encoder};
pub use database::{Database, ReadTransaction, Stats, WriteTransaction};
pub use directory::{
    Checkpoint, GenerationInfo, Migration, PruneReport, ReclaimReport, StorageEntry,
    StorageEntryKind, StorageInventory,
};
pub use error::{Error, Result};
pub use maintenance_options::{CheckpointPolicy, MaintenanceEstimate, MaintenanceOptions};

/// Stable application identity, independent of the storage format version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schema {
    /// An application-assigned identity. Keep it stable across Rust refactors.
    pub table_id: u64,
    /// Increase this when the encoded representation or its meaning changes.
    pub version: u32,
}

/// A value stored in a typed table. The primary key is supplied separately.
///
/// Codecs must be deterministic and round-trip all persisted state. Use stable
/// field encodings, never Rust memory layouts. Decoding must honor the schema
/// version; Skrin verifies that identity before calling the codec.
///
/// Records must have value semantics: no mutation through shared references,
/// shared mutable external state, or panicking destructors. Rust's `Sync` bound
/// alone cannot enforce this semantic contract. Keys and all persisted changes
/// must go through a write transaction.
pub trait Record: Sized + Send + Sync + 'static {
    /// Persistent table identity and application schema version.
    const SCHEMA: Schema;

    /// Encode this record using a stable, application-owned representation.
    fn encode(&self, encoder: &mut Encoder) -> Result<()>;

    /// Decode one record. Skrin rejects trailing bytes after this returns.
    fn decode(decoder: &mut Decoder<'_>) -> Result<Self>;
}
