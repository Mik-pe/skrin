//! Skrin: typed records, atomic transactions, and an explicit durable log.
//!
//! `Database` implements one table; `catalog` adds schema-bound tables and atomic
//! derived indexes with `u64` keys per table. Managed directories add
//! verified checkpoints, backups and explicit offline schema migrations.
//! Reads borrow native Rust values. Baseline read guards block writers; opt-in
//! `versioned` handles provide bounded immutable row/index snapshots. Persistent databases currently require a local Unix filesystem.
//! No SQL, network service or local unsafe code is involved. Linux file-data
//! reservations use safe syscall bindings.

#![forbid(unsafe_code)]

extern crate self as skrin;

pub mod catalog;
pub mod codec;
mod database;
mod directory;
#[cfg(test)]
mod edit_tests;
mod error;
pub mod group_commit;
mod log;
mod maintenance_options;
#[cfg(all(test, unix))]
mod persistence_model;
mod postings;
mod reservation;
mod schema_macros;
mod snapshot;
#[cfg(test)]
mod test_support;
mod typed_index;
mod version_tree;
pub mod versioned;

pub use codec::{Decoder, Encoder};
pub use database::{Database, ReadTransaction, Stats, WriteTransaction};
pub use directory::{
    Checkpoint, GenerationInfo, Migration, PruneReport, ReclaimReport, StorageEntry,
    StorageEntryKind, StorageInventory,
};
pub use error::{Error, Result};
pub use maintenance_options::{CheckpointPolicy, MaintenanceEstimate, MaintenanceOptions};

/// Generate a fixed codec for a named struct with an explicit schema.
///
/// Fields are persisted in declaration order. Fixed-width signed/unsigned
/// integers, `bool`, `f32`/`f64`, `String`, `Vec<u8>` and `Option` of supported types use
/// explicit encoder methods. Integers/float bits are little endian; strings and
/// bytes have a u32 length; bool/Option use strict 0/1 tags. Reordering, adding, removing or
/// changing the meaning of fields requires a schema version and migration.
/// Other field types, enums and generic records need a manual `Record` impl.
/// No field may be skipped or silently defaulted. `Clone` is not required.
///
/// ```
/// #[derive(Debug, PartialEq, Eq, skrin::Record)]
/// #[skrin(table_id = 7, version = 1)]
/// struct Player { name: String, score: u64 }
/// let db = skrin::Database::<Player>::in_memory();
/// db.write(|tx| tx.insert(1, Player { name: "Ada".into(), score: 10 }))?;
/// assert_eq!(db.read()?.get(1).unwrap().score, 10);
/// # Ok::<(), skrin::Error>(())
/// ```
///
/// Dependency aliases use `#[skrin(crate = alias)]`. Disable the default
/// `derive` feature to keep the procedural macro dependencies out of builds.
///
/// ```compile_fail
/// #[derive(skrin::Record)]
/// struct MissingIdentity { score: u64 }
/// ```
/// ```compile_fail
/// #[derive(skrin::Record)]
/// #[skrin(table_id = 7, version = 1)]
/// struct NativeWidth { score: usize }
/// ```
/// ```compile_fail
/// #[derive(skrin::Record)]
/// #[skrin(table_id = 7, version = 1)]
/// struct LostState { #[skrin(skip)] score: u64 }
/// ```
#[cfg(feature = "derive")]
pub use skrin_derive::Record;

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
