# C API: byte records and explicit transactions

`skrin-ffi` builds `libskrin_ffi.a`, with its public interface in
[`skrin.h`](../crates/skrin-ffi/include/skrin.h). ABI v1 exposes a single table
of u64 keys and opaque byte values, managed directories, atomic writes,
checkpoint/reclamation and verified backups. The implementation uses Skrin's
production engine. It has no SQL interface or second storage engine.

This is an experimental, implemented C interface, not an automatic export of
Rust generics. Applications own their payload codecs and schema evolution.
Typed Rust catalogs, secondary indexes, snapshots, group commit, arbitrary
user-provided record callbacks and schema generators are not exposed by ABI v1.

## Build and link

```sh
cargo +1.89.0 build -p skrin-ffi --release --locked
cargo +1.89.0 rustc -p skrin-ffi --release --locked -- --print=native-static-libs
```

Link `target/release/libskrin_ffi.a` after your C object files, followed by the
native libraries printed by the second command. Those libraries and their order
are target-specific; do not assume a universal list. The archive includes the
Rust dependencies, but system-library dependencies can remain dynamic. Build the
archive and consumer for matching OS, architecture, C ABI and runtime target.
This is static linkage of Skrin, not a promise of a fully static executable.

The complete native build/test driver handles these link flags and compiles
with strict C11/C++11 warnings:

```sh
scripts/test-c-api.sh
scripts/test-c-api.sh --release
SKRIN_C_SANITIZE=1 CC=clang CXX=clang++ scripts/test-c-api.sh
```

It runs a C++ header/link probe, the [C accounts example](../crates/skrin-ffi/examples/accounts.c)
in memory and on disk, native C regressions, and the verified C benchmark in
volatile and synced modes. `CC`, `CXX`, `SKRIN_TEST_TOOLCHAIN` and
`CARGO_TARGET_DIR` select the tools/output location. The script is for native
Linux/macOS builds and uses a disposable temporary directory. It caches the
actual compiler's native link flags beside that target/profile, including the
compiler version and host, for Cargo builds that replay no compiler notes.

The C ABI is compiled/tested through Rust on all existing CI targets; native C
and C++ linkage is additionally tested on Linux/macOS with MSRV and stable.
Windows persistence remains unsupported. No Windows C linker run is claimed.
Sanitizer mode instruments the C/C++ callers, not the Rust library itself.

## Use from C

All handles start as NULL. `create` requires a new directory under an existing,
durably created parent; `open` requires an existing database. They are separate
operations and never overwrite/adopt an existing directory on creation failure.
Use `skrin_db_in_memory` for explicitly volatile storage.

```c
skrin_error error;
skrin_db *db = NULL;
skrin_write *write = NULL;
skrin_status status = skrin_db_in_memory(&db, &error);
if (status != SKRIN_OK) { /* handle error.code / error.message */ }
status = skrin_write_begin(db, &write, &error);
if (status == SKRIN_OK) {
    const uint8_t value[] = {'h', 'i', 0, 255};
    status = skrin_write_insert(write, 42, value, sizeof(value), &error);
    if (status == SKRIN_OK) status = skrin_write_commit(&write, &error);
    /* Always safe after a completed commit, or to discard prior staging. */
    skrin_write_abort(&write, NULL);
}
skrin_db_close(&db, NULL);
```

The executable example stores two C account records using an application-owned,
explicit little-endian codec, saves both sides of a transfer in one transaction,
and verifies complete payloads after WAL, checkpoint and independent backup
reopen. Its UTF-8 name includes an embedded NUL. A C struct's memory layout is
never its persisted representation. Payloads may also contain arbitrary binary
data; no normalization, string termination or UTF-8 conversion is applied.

## Buffer and handle ownership

No Rust allocation crosses the ABI for the caller to free. Put/insert copy
inputs during the call. Reads copy into caller-owned output buffers and always
take an explicit byte capacity and required output length.
Output byte buffers and scalar/struct outputs may start uninitialized, as normal
C stack storage. The ABI initializes outputs with raw writes before creating
Rust references; only handle slots require prior initialization to NULL.

For a nonempty value, `get(db, key, NULL, 0, &len, &error)` returns
`SKRIN_BUFFER_TOO_SMALL` and the required length. Retry with enough space.
`BUFFER_TOO_SMALL` never partially copies. An empty stored value is `SKRIN_OK`
with length zero; an absent key is `SKRIN_NOT_FOUND` with length zero. Other
failures also leave the length zero. Committed data may change between two
separate calls, so a size probe can need another retry. A write transaction
provides a stable base and sees its own earlier changes.

Only the library creates/destroys opaque handles. Output handle slots must point
to initialized NULL pointers; a nonempty slot is refused without overwriting it.
Close, abort and commit clear their input slots. Commit consumes a valid write
handle on **every** result, including a clean size refusal or uncertain outcome;
abort and close accept already NULL handles. Copied pointer aliases become
invalid when their owner is consumed. A live transaction retains its database
and permanent directory lock even after the originating database handle closes.

Database operations may share a live handle across threads. Closing it must be
externally synchronized with all uses of that handle. A write handle requires
external serialization of all its calls, but may move between threads. All
foreign pointers must have the validity, alignment, size and nonoverlap stated
in the header; NULL checks cannot validate dangling pointers or foreign races.
Paths are nonempty UTF-8 bytes without NULs; opaque record values have neither
restriction. Error outputs are optional per-call objects, never shared globals
or thread-local borrowed messages. Their bounded, NUL-terminated UTF-8 text is
diagnostic only; branch on the numeric status.

## Transactions and persistence

One admission lease excludes other C writers and maintenance while a transaction
is live. Competing begin/maintenance calls return `SKRIN_BUSY`. Point reads of
the committed database remain available during staging. Each read holds only a
short production read guard and returns a copy. There is no exposed MVCC view.

The safe adapter owns a BTreeMap of only touched keys. At commit it moves those
values into one production write transaction; there is no full-database copy,
self-referential Rust guard, lifetime transmute or callback into foreign code.
Only that engine transaction appends/syncs/publishes. Insert refuses duplicates;
put explicitly replaces; remove reports existence. Statement errors leave prior
staging intact, matching the manual Rust API. The caller must deliberately commit
or abort. Empty/coalesced no-op commits do not advance the sequence.

A successful put is only staging. A successful **persistent commit** follows
the same `append -> sync -> publish` boundary as Rust writes. The byte-value limit
is 8 MiB minus the codec's four-byte length prefix and is checked before copying
input even in memory mode. Persistent transaction payloads remain capped at
16 MiB, including operation framing. Volatile mode skips transaction encoding
and disk I/O, as the engine does. Staged/native data must fit in process memory;
there is no allocator/RSS reservation.

`SKRIN_COMMIT_UNCERTAIN` and `SKRIN_MAINTENANCE_UNCERTAIN` preserve the engine's
uncertain-outcome contract: close **all** owners, reopen and reconcile using an
application operation ID. Never assume a failed commit rolled back and blindly
retry a non-idempotent operation. New operations refuse poisoned state. Unwinding
operation panics poison the adapter too and never unwind through C. Abort/close
remain available. OOM/process aborts and invalid foreign-memory accesses are
outside panic recovery.

`checkpoint`, `reclaim` and `backup` are explicit. Reclaim preserves active and
previous generations and reports skipped unknown contents. Backups are new,
independently decoded managed directories. Provide replacement/retained-file
headroom; no automatic maintenance or retention worker exists. See
[durability](durability.md) and [managed storage](managed-storage.md).

## Adapter schema and compatibility

ABI v1 uses the existing engine formats unchanged. Its record schema is
`{ table_id: 0x534b524342595431, version: 1 }` (`SKRCBYT1`). Each record codec
is a u32 little-endian payload length followed by exact opaque bytes; for
`00 c3 a9` its independent fixture is `03 00 00 00 00 c3 a9`. That codec and
identity are application storage contracts, separate from the C ABI version.

`skrin_db_open` accepts this adapter's managed directories only. Another Rust
record/catalog schema is refused before tail repair. Engine formats, complete
corruption and missing manifests retain their existing refusal policies. No
existing database is implicitly converted, and existing format fixtures remain
unchanged. A future incompatible adapter codec requires an explicit version and
migration decision. Payload schema versions are owned by the C application;
the engine cannot inspect opaque payload semantics on its behalf.

## Evidence and limits

Rust ABI tests cover ownership, byte buffers, staging/committed visibility,
threaded writer admission, output validation, schema/corruption refusal without
mutation and panic/error handling. Native C tests verify the actual header,
layouts, exported symbols, duplicate/empty semantics, record/transaction limits,
transaction ownership of the directory lock and exact recovery. The POSIX test
sets `RLIMIT_FSIZE` to force a real partial production WAL append, checks
`COMMIT_UNCERTAIN`/poison and restores the limit before reopening the torn tail.
This is a real kernel failure test, not a mock database or power-loss certificate.

The C benchmark checks all 64 final rows and the exact 1,000-commit sequence
outside timing, including persistent reopen. It measures this adapter's
begin/put/commit path only. Its volatile and synced numbers represent different
guarantees, and local tmpfs/CI timings are not physical-device or cross-engine
performance claims. The library remains experimental and unpublished.
