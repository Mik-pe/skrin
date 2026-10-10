# Architecture

## Ownership and representation

`Database<R: Record>` owns one typed table. Its committed records are a `BTreeMap<u64, R>`, not serialized byte buffers. Borrowed point reads and ordered scans avoid decoding and record cloning. `Record` does not require `Clone`.

The key is external to the record. `Record::SCHEMA` identifies the logical table and application schema version. Schema identity is not derived from a Rust type name, field name, `TypeId`, or memory layout. Reusing an identity for an incompatible codec is an application bug; Skrin cannot infer semantic compatibility from a struct.

`Encoder`/`Decoder` provide bounded, little-endian primitives and length-prefixed bytes/UTF-8. The application's field order and representation are part of its versioned contract. Field-ID macros are not implemented. A schema mismatch is a safe refusal, not an implicit migration. Managed databases support explicit old-to-new `Record` transformations through `migrate`; see [managed storage](managed-storage.md).

Records must have immutable value semantics. `Send + Sync` is insufficient to prohibit atomics, mutexes, shared external mutable state, or panicking destructors; these are excluded by the `Record` contract. No implementation can make arbitrary application code's external side effects transactional.

## Transactions and concurrency

A single `RwLock` owns the committed table, sequence, WAL and failure state. Read guards borrow from that state. Multiple read guards can coexist, but writers wait for all of them; a writer excludes all other transactions throughout staging, encoding and sync. This supplies a consistent read view but is deliberately **not MVCC**.

A writer stages only touched keys in a separate `BTreeMap<u64, Option<R>>`. `Some` means insert/replace and `None` means delete. Reads inside the writer first consult staging. Repeated writes to a key coalesce; inserting then deleting a previously absent key is a no-op. Dropping staging changes no committed data.

Commit encodes the delta, appends one frame, syncs, and then applies the delta under the same exclusive guard. Untouched records are never copied. The publication step may allocate tree nodes; an allocation abort or application destructor panic after sync can leave a committed transaction whose caller never received success. Reopening determines the disk state.

The closure API commits only after `Ok` and returns the closure value only after commit. Propagating an error rolls back all staging. With the manual API a statement error leaves earlier staging intact, so callers must deliberately commit or drop it. Nested transactions can deadlock, including reacquiring a read lock while a writer is queued. Keep guards short and never carry them across an async suspension.

## Storage

`log.rs` owns framing, bounds, checksums and replay; `file_storage.rs` owns the locked file backend. A small internal `Storage` trait wraps read/write/seek/size/truncate/sync. Tests inject faults here, so they execute the real codec, transaction and recovery code. There is one dynamic dispatch per I/O operation, not per in-memory row read.

For the standalone v1 backend, the file is both the WAL and the OS lock target. It must not be renamed, unlinked, replaced or externally modified while open. Unix locks attach to an open file/inode, not a permanent path identity. The managed backend instead holds a permanent directory `LOCK` owner across all generation replacement. Reusing the standalone lock protocol while renaming its WAL would be incorrect.

The file wrapper explicitly unlocks on owner-process drop, including failed initialization/recovery paths. Merely closing one descriptor can leave the lock held by a descriptor temporarily inherited during another thread's fork/exec. A process-ID guard prevents cleanup in a forked child from explicitly unlocking the parent's live database. Do not use an inherited database handle in a child before exec; open a fresh database only after the owner has released it. Regression tests model the shared descriptor lifetime deterministically using `File::try_clone` and exercise process tests repeatedly in release mode. See the standard library's [lock lifetime contract](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock).

`create` uses atomic create-new semantics, writes and syncs the header, then syncs the parent directory. `open` takes the lock before validation/replay. Complete frames are replayed in sequence, an incomplete final frame can be truncated, and the resulting file is synced before state is exposed. Data and primary index must fit in memory. A standalone file replays all historical committed frames; a managed directory decodes its active snapshot and replays only its bound WAL suffix.

## Why this starting point

A native map and serialized writer are a measurable correctness baseline, not the final performance architecture. This avoids hiding whole-database cloning or a second database engine beneath an ergonomic API. The optional `skrin-derive` procedural macro implements concrete named-struct `Record` codecs using the existing encoder primitives. `catalog!` generates native enum/table dispatch without changing engine representation or persistence. Typed index markers and `IndexKey` codecs infer native query/record types and
share projection/query encoding without changing stored schemas. Both retain explicit schema identities; see [models](models.md).

Managed checkpoints, explicit retention, backups and offline migrations are implemented in `directory.rs`, `snapshot.rs` and `maintenance.rs`. See [the publication protocol](managed-storage.md) and [remaining release gates](roadmap.md). Streaming verification, encoded-resource budgets, conservative inventory/reclamation and a production-path persistence projection are implemented; see [maintenance](maintenance.md). Remaining hardware/resource certification is tracked explicitly before production use. Schema-bound multi-table transactions and atomic derived indexes are implemented in `catalog.rs`; see [the catalog contract](catalog.md). The original single-table representation remains the baseline. [Independent group commit](group-commit.md) now holds the same exclusive row/index guards across separate frame appends and one shared sync; the original immediate-sync path remains the baseline. [Immutable read versions](snapshots.md) add coherent index versioning and bounded cooperative reader retention while retaining this baseline. The baseline benchmark is not evidence for a SpacetimeDB comparison.

## Opt-in immutable read versions

`versioned` consumes a native baseline once, moving rows into Arcs and building
immutable primary AVL roots and bounded covering index leaves from ordered
sources. Each final routing node is allocated once with logarithmic construction
stack space. Ordinary updates copy changed
paths. The original baseline representation remains
available. Versioned writers still use the production mutable rows, codec, WAL
and index final-view validator under one exclusive writer boundary. Changed
row/index paths are prepared before append. The complete final index projection
is validated before unchanged primary addresses and secondary keys are filtered
from the mutation delta. Existing row replacements are sorted and copy each
shared AVL ancestor once in the replacement pass, preserving tree shape and untouched
subtrees; inserts/deletes still use balanced tree operations. No full database
copy occurs. A short separate publication lock
swaps one coherent immutable root only after immediate/shared sync. Captured
read versions never hold the production row/index locks across I/O.

Mutable catalog postings retain `(primary ID, physical slot)`, so an index scan
does one row-map lookup per hit without repeating the primary-address lookup.
Immutable catalog indexes keep up to 64 ordered `(index, byte key, primary ID,
Arc row)` entries in each contiguous leaf behind a path-copy AVL routing tree.
Scans seek once and then borrow directly from these covering row references.
Every row replacement refreshes all its covering references even when the
index keys are unchanged. A transaction coalesces changes by original leaf,
copies at most 64 existing references from each touched leaf once, and shares
untouched leaves. Large equal-key groups are split across leaves. Removed empty
leaves disappear; sparse final leaf buffers shrink, without copying the whole
index. This trades some write work for faster resident reads; it does not change
stored codecs, schema identities or the persistence boundary. Traversal stacks
hold 32 pointers inline and safely spill deeper paths to a vector.

Lease admission bounds live reader count and conservative full-root-per-lease
bytes, including shared nodes/rows counted again. A trusted pure application
footprint assessment adds native owned capacities to engine node/key accounting;
it is not a hard allocator/RSS cap. Oldest pin, pinned versions and current/
pinned bytes are observable. Unpinned intermediate roots are freed rather than
retained in an ever-growing version chain. Pins retain memory, not generation
files, so explicit checkpoint/reclaim and permanent LOCK ordering are unchanged.
Returning to native offline migration requires exclusive client ownership and
no pins. Real writer/maintenance uncertainty and writer panics propagate poison
to retained view read calls. See [snapshots](snapshots.md) for the complete contract.
