# Direction and release gates

Skrin is a Rust-native embedded database, not a SQL server or a wrapper around a different database. Keep the native typed API, explicit durability and measurable overhead.

## Delivered foundations

Typed native records, stable schema codecs, atomic staged writes, borrowed reads, ranges, checksummed WAL/recovery and process ownership are implemented. Managed directories add sealed checkpoints, explicit active/previous retention, independently decoded backup/restore and named offline migrations. The executable lifecycle and fault tests are part of the deliverable, not future API sketches.

## Next: strengthen the operational boundary

Complete the resource/retention policy before treating the engine as production-ready: snapshot/RAM/disk budgeting, explicit orphan inspection/cleanup, a filesystem persistence model (including reordered/lost unsynced directory operations), macOS flush semantics, and reproducible long-running maintenance measurements. Preserve existing format fixtures and keep the legacy import path non-destructive.

## Then: multi-table schemas and atomic indexes

Design a schema-bound typed transaction spanning multiple tables and their unique/secondary indexes. Table and index definitions must be present at open and validated as part of schema identity. Do not add an optional index registration step that lets writers accidentally bypass uniqueness after restart.

Keep one durable transaction envelope for row and index changes; validate uniqueness on the final staged view, including key swaps. Rebuild derived indexes from validated rows, compare against an independent reference model, and cover rollback/recovery/migration with multiple tables. Start with one executable two-table example and narrow APIs, not placeholder crates.

## Later: concurrency justified by measurements

Use the current lock-based engine as the correctness/performance baseline. Explore snapshot readers and group commit only after index/transaction semantics are settled. Reader version retention, low-load latency, backpressure and sync-before-publication remain first-class constraints. No normal commit should clone the complete database merely to claim MVCC.

A future fast mode may have a different durability contract, but it must be explicit in the API and benchmarks. No SQL, replication or network service is required for this roadmap.

## Release gates

- Owner-selected license and distribution policy; publishing is intentionally disabled today.
- Documented API/format compatibility policy and reviewed migrations for changes to stored formats.
- Independent backup/restore exercises, capacity/headroom guidance and failures tested below production I/O.
- Reproducible workload definitions, hardware/compiler/filesystem metadata, latency distributions and maintenance cost.
- No unsupported production-safety or SpacetimeDB comparison claims.

A green CI is necessary, not proof of general power-loss safety or product maturity. Current persistent storage is Unix-only; Windows memory-mode CI is not Windows durable-storage support.
