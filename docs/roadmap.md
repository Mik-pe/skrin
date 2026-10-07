# Direction and release gates

Skrin is a Rust-native embedded database, not a SQL server or a wrapper around a different database. Keep the native typed API, explicit durability and measurable overhead.

## Delivered foundations

Typed native records, stable schema codecs, atomic staged writes, borrowed reads, ranges, checksummed WAL/recovery and process ownership are implemented. Managed directories add sealed checkpoints, explicit active/previous retention, independently decoded backup/restore and named offline migrations. The executable lifecycle and fault tests are part of the deliverable, not future API sketches.

## Next: strengthen the operational boundary

Delivered in the next maintenance milestone: single-row streaming snapshot verification, bounded encoded record/new-file bytes and row count, read-only estimates/inventory, conservative recognized-orphan reclamation, caller-driven checkpoint thresholds, and a persistence projection that separates file contents and namespace survival. Bounded tests enumerate unsynced namespace subsets and WAL append prefixes, track acknowledgments/publications, exercise ENOSPC-equivalent errors, and detect eight omitted sync guarantees. The same verified scale workload measures the actual before/after engine changes.

Real kernel ENOSPC is additionally tested in an isolated bounded Linux tmpfs; [the operator workflow](maintenance.md#operator-workflow-for-unknown-or-partially-owned-stages) preserves malformed/unclaimed stages while making an independently verified destination. [Repeated NVMe resource measurements](measurements/resources-2026-10-07.md) report process RSS, retained/staging file overlap and observed cache state. Required Linux file-data allocation is implemented for new maintenance files; it refuses unsupported allocation and does not reserve filesystem metadata or future WAL growth. Unknown contents still require an application quarantine/headroom policy.

The application-memory boundary now has [explicit process-containment guidance and a real kernel-OOM recovery regression](maintenance.md#application-memory-and-process-containment), while codecs remain trusted and library budgets remain cooperative/encoded. Remaining production-certification work includes a library-wide allocator budget if required by an application, filesystem-metadata/global-quota reservation where supported, exhaustive device/filesystem fault coverage and platform/device hardware-flush certification beyond the [reviewed Linux/macOS standard-library requests](flush-contract-review.md). Physical power-loss testing is explicitly deferred by the owner because no disposable hardware is available; it does not block the current software milestone. These limitations remain visible and publishing remains disabled. Preserve existing format fixtures and keep the legacy import path non-destructive.

## Delivered: multi-table schemas and atomic indexes

`catalog::CatalogDatabase<C>` provides a schema-owned persistent catalog, native enum rows, typed table markers, atomic multi-table transactions, final-view uniqueness and derived ordered indexes. Snapshot and every WAL transaction's index validation precede tail repair. Checkpoints, backups, explicit legacy import and real old/new-codec catalog migrations execute the same managed-generation protocol. See [the catalog contract and executable banking example](catalog.md).

The original `Database<R>` remains the lock-based single-table baseline. This milestone adds no MVCC, concurrent writer, SQL or background maintenance. Index/row memory and maintenance headroom still require operational review.

## Delivered: concurrency justified by measurements

Use the current lock-based engine as the correctness/performance baseline. Explore snapshot readers and group commit only after index/transaction semantics are settled. [Bounded independent group commit](group-commit.md) is implemented with a maximum collection delay and explicit queue refusal, preserving shared-sync-before-publication; [repeated equivalent-durability NVMe measurements](measurements/group-commit-2026-10-07.md) report its saturation benefit and low-load waiting cost. [Immutable row/index snapshots](snapshots.md) are now opt-in, with full-root reader accounting, explicit admission backpressure and observed oldest pins. The current table, arbitrary application allocations and allocator overhead remain outside the cooperative pin budget. Ordinary commits copy changed tree paths, not the complete database. Compare held-reader throughput and latency, read freshness, current/retained memory and maintenance/recovery before selecting the additional version machinery.

[Repeated held-reader measurements](measurements/snapshots-2026-10-07.md) preserve the borrowed-reader baseline and report read admission, write tails, retention, memory and checkpoint/recovery costs for all four immediate/group and borrowed/snapshot modes. Snapshots reduce observed reader admission waits while a writer holds the native locks; writer throughput/tails are not uniformly improved. Production verification is also paused in a concurrent regression to prove coherent old/current row and index reads proceed during a serialized checkpoint, while a subsequent writer remains excluded until maintenance completes.

A future fast mode may have a different durability contract, but it must be explicit in the API and benchmarks. No SQL, replication or network service is required for this roadmap.

## Release gates

- Owner-selected license and distribution policy; publishing is intentionally disabled today.
- Documented API/format compatibility policy and reviewed migrations for changes to stored formats.
- Independent backup/restore exercises, capacity/headroom guidance and failures tested below production I/O.
- Reproducible workload definitions, hardware/compiler/filesystem metadata, latency distributions and maintenance cost.
- No unsupported production-safety or SpacetimeDB comparison claims.

A green CI is necessary, not proof of general power-loss safety or product maturity. Current persistent storage is Unix-only; Windows memory-mode CI is not Windows durable-storage support.
