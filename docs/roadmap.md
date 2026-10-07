# Direction and release gates

Skrin is a Rust-native embedded database, not a SQL server or a wrapper around a different database. Keep the native typed API, explicit durability and measurable overhead.

## Delivered foundations

Typed native records, stable schema codecs, atomic staged writes, borrowed reads, ranges, checksummed WAL/recovery and process ownership are implemented. Managed directories add sealed checkpoints, explicit active/previous retention, independently decoded backup/restore and named offline migrations. The executable lifecycle and fault tests are part of the deliverable, not future API sketches.

## Next: strengthen the operational boundary

Delivered in the next maintenance milestone: single-row streaming snapshot verification, bounded encoded record/new-file bytes and row count, read-only estimates/inventory, conservative recognized-orphan reclamation, caller-driven checkpoint thresholds, and a persistence projection that separates file contents and namespace survival. Bounded tests enumerate unsynced namespace subsets and WAL append prefixes, track acknowledgments/publications, exercise ENOSPC-equivalent errors, and detect eight omitted sync guarantees. The same verified scale workload measures the actual before/after engine changes.

Real kernel ENOSPC is additionally tested in an isolated bounded Linux tmpfs; [the operator workflow](maintenance.md#operator-workflow-for-unknown-or-partially-owned-stages) preserves malformed/unclaimed stages while making an independently verified destination. [Repeated NVMe resource measurements](measurements/resources-2026-10-07.md) report process RSS, retained/staging file overlap and observed cache state. Unknown contents still require an application quarantine/headroom policy.

Remaining release work: allocator/application-memory accounting and enforced native-memory budgets, true filesystem reservation policies where supported, exhaustive device/filesystem fault coverage, macOS hardware-flush review, and identified real-device long-duration/power-loss measurements. The current encoded budgets and projection are not substitutes for those guarantees. Preserve existing format fixtures and keep the legacy import path non-destructive.

## Delivered: multi-table schemas and atomic indexes

`catalog::CatalogDatabase<C>` provides a schema-owned persistent catalog, native enum rows, typed table markers, atomic multi-table transactions, final-view uniqueness and derived ordered indexes. Snapshot and every WAL transaction's index validation precede tail repair. Checkpoints, backups, explicit legacy import and real old/new-codec catalog migrations execute the same managed-generation protocol. See [the catalog contract and executable banking example](catalog.md).

The original `Database<R>` remains the lock-based single-table baseline. This milestone adds no MVCC, concurrent writer, SQL or background maintenance. Index/row memory and maintenance headroom still require operational review.

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
