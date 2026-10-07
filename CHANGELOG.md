# Changelog

## Real storage-full regression and unknown-stage operation

- Fill a private 4 MiB Linux tmpfs to exercise actual kernel ENOSPC beneath production checkpoint/WAL code; require clean preparation refusal, poisoned append, selected recovery and a verified independent backup.
- Preserve the partial ownership stage through reclamation, document the operator/quarantine headroom workflow, and run the explicit bounded-filesystem wrapper in Linux/MSRV CI.

## Sustained resource measurement

- Add a verified update/delete/insert maintenance benchmark with Linux process RSS, logical/per-inode allocated disk overlap, checkpoint/reclaim pauses and independent-process warm/advisory-evicted recovery.
- Preserve mandatory syncs, report observed read bytes and cache/accounting limits, and exercise the complete harness in CI.

## Unsynced survival and storage-full evidence

- Expand the test-only production persistence projection with unsynced namespace subsets, observed file-write survival, exhaustive bounded WAL append prefixes and explicit acknowledgment/publication tracking.
- Detect eight independently omitted file/directory sync guarantees; recover coherent catalog rows/indexes through checkpoints, migrations and restartable retained-generation cleanup.
- Exercise `StorageFull` append, sync and maintenance failures, preserving clean preparation refusal versus poisoned uncertain outcomes; document the bounded model's limits.

## Catalog maintenance controls

- Expose encoded checkpoint/backup/migration budgets, exact preflight, explicit checkpoint thresholds, inventory and generation history to catalog applications.
- Preserve decoded projection validation under the threshold/publication lock; keep source/index state usable after preparation refusal.
- Document descriptor accounting and exercise bounded catalog maintenance in the banking lifecycle.

## Schema-bound tables and atomic indexes

- Add a catalog-owned native row enum, typed table markers and persisted table/index definitions.
- Commit heterogeneous row and unique/non-unique index changes under one WAL sync/publication boundary; validate final-view swaps and staged queries without whole-database commit copies.
- Rebuild/validate indexes at snapshot and every WAL transaction before tail repair; reject changed catalogs and complete constraint corruption without mutation.
- Integrate verified checkpoints, backups, real V2 catalog migration and explicit non-destructive legacy-table import.
- Add independent catalog codec fixture, row/index reference model, production I/O faults, persistence images, process exits, executable banking example and equivalent-durability latency/RSS benchmark.

## Bounded maintenance and recovery performance

- Stream checkpoint/migration verification without a second native table; preserve original checkpoint rows.
- Reuse record/WAL/snapshot buffers, batch snapshot writes, and accelerate unchanged IEEE CRC-32 in safe Rust.
- Add maintenance byte/record/row budgets, exact encoded preflight and explicit checkpoint thresholds.
- Add read-only inventory and conservative, restartable reclamation of recognized obsolete/orphan generations and temporary manifests.
- Refuse symlinked/special managed metadata and active storage files.
- Model persistence ordering at production sync/rename sites and test a missing-parent-sync negative control.
- Add capacity, ownership, retention, policy, CRC-reference and streaming-memory regressions; executable maintenance and scale benchmarks.


## Unreleased — experimental

### Added

- Typed records with explicit schema codecs, atomic staged transactions, borrowed reads, primary-key ranges and checked updates.
- Original standalone v1 WAL backend with bounded framing, checksums, sync-before-publication, conservative recovery and exclusive ownership.
- Managed directory backend with a permanent lock, authoritative manifest, sealed verified checkpoints and explicit active/previous generation retention.
- Consistent backup/import/restore and named offline schema migrations with preserved sequence and history.
- Publication-boundary I/O faults and subprocess exits, snapshot/manifest corruption checks, independent format fixtures and a complete lifecycle example.
- Maintenance benchmarks for checkpoint pauses, disk footprint and warm-cache reopen.

### Boundaries

Experimental API, typed single-table and schema-bound multi-table modes, lock-based readers and one writer. No MVCC, group commit, encryption or replication. Persistence currently requires Unix. Publishing is disabled pending owner release decisions.
