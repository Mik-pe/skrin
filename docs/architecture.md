# Architecture: first executable slice

## Ownership and representation

`Database<R: Record>` owns one typed table. Its committed records are a `BTreeMap<u64, R>`, not serialized byte buffers. Borrowed point reads and ordered scans avoid decoding and record cloning. `Record` does not require `Clone`.

The key is external to the record. `Record::SCHEMA` identifies the logical table and application schema version. Schema identity is not derived from a Rust type name, field name, `TypeId`, or memory layout. Reusing an identity for an incompatible codec is an application bug; Skrin cannot infer semantic compatibility from a struct.

`Encoder`/`Decoder` provide bounded, little-endian primitives and length-prefixed bytes/UTF-8. The application's field order and representation are part of its versioned contract. Field-ID macros and old-to-new transformations are not implemented yet. A schema mismatch is a safe refusal, not a migration.

Records must have immutable value semantics. `Send + Sync` is insufficient to prohibit atomics, mutexes, shared external mutable state, or panicking destructors; these are excluded by the `Record` contract. No implementation can make arbitrary application code's external side effects transactional.

## Transactions and concurrency

A single `RwLock` owns the committed table, sequence, WAL and failure state. Read guards borrow from that state. Multiple read guards can coexist, but writers wait for all of them; a writer excludes all other transactions throughout staging, encoding and sync. This supplies a consistent read view but is deliberately **not MVCC**.

A writer stages only touched keys in a separate `BTreeMap<u64, Option<R>>`. `Some` means insert/replace and `None` means delete. Reads inside the writer first consult staging. Repeated writes to a key coalesce; inserting then deleting a previously absent key is a no-op. Dropping staging changes no committed data.

Commit encodes the delta, appends one frame, syncs, and then applies the delta under the same exclusive guard. Untouched records are never copied. The publication step may allocate tree nodes; an allocation abort or application destructor panic after sync can leave a committed transaction whose caller never received success. Reopening determines the disk state.

The closure API commits only after `Ok` and returns the closure value only after commit. Propagating an error rolls back all staging. With the manual API a statement error leaves earlier staging intact, so callers must deliberately commit or drop it. Nested transactions can deadlock, including reacquiring a read lock while a writer is queued. Keep guards short and never carry them across an async suspension.

## Storage

`log.rs` owns framing, bounds, checksums, replay and the file backend. A small internal `Storage` trait wraps read/write/seek/size/truncate/sync. Tests inject faults here, so they execute the real codec, transaction and recovery code. There is one dynamic dispatch per I/O operation, not per in-memory row read.

The file is both the WAL and the OS lock target. It must not be renamed, unlinked, replaced or externally modified while open. Unix locks attach to an open file/inode, not a permanent path identity. A future generation/checkpoint design must first introduce a stable lock owner that survives generation replacement; reusing the current lock protocol while renaming the WAL would be incorrect.

`create` uses atomic create-new semantics, writes and syncs the header, then syncs the parent directory. `open` takes the lock before validation/replay. Complete frames are replayed in sequence, an incomplete final frame can be truncated, and the resulting file is synced before state is exposed. Data and primary index must fit in memory; the log is streamed during recovery, but all historical committed frames must still be processed.

## Why this starting point

A native map and serialized writer are a measurable correctness baseline, not the final performance architecture. This avoids hiding whole-database cloning or a second database engine beneath an ergonomic API. No derive crate exists yet because there is no implemented derive behavior to host.

The next storage milestone is checkpoints, bounded log retention and offline migrations with a crash-safe generation handoff. Next come multi-table schemas and atomic secondary indexes. Read versions and group commit must be designed with index versioning, reader retention, sync ordering and measured contention in mind. The baseline benchmark is not evidence for a SpacetimeDB comparison.
