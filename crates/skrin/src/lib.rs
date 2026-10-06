//! Skrin: typed records, atomic transactions, and an explicit durable log.
//!
//! Implements one table per database with `u64` keys. Managed directories add
//! verified checkpoints, backups and explicit offline schema migrations.
//! Reads borrow native Rust values. Read guards block writers; they are not MVCC
//! snapshots. Persistent databases currently require a local Unix filesystem.
//! No SQL, network service, unsafe code, or external dependencies are involved.

#![forbid(unsafe_code)]

pub mod codec;
mod database;
mod directory;
mod error;
mod log;
mod snapshot;
#[cfg(test)]
mod test_support;

pub use codec::{Decoder, Encoder};
pub use database::{Database, ReadTransaction, Stats, WriteTransaction};
pub use directory::{Checkpoint, GenerationInfo, Migration, PruneReport};
pub use error::{Error, Result};

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
