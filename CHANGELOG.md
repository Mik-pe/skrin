# Changelog

## Native typed index access

- Add schema-owned `Index` markers and explicit ordered `IndexKey` codecs, sharing projection and query encoding for native integers, borrowed strings/bytes and custom keys.
- Add lazy `query(index, bounds)` and `matching(index, &key)` to borrowed catalogs and snapshots; infer record/key types and validate full definitions, encoded limits and malformed ranges without panics.
- Use typed declarations and queries throughout the game-world examples/benchmarks; retain raw/manual APIs, disk formats and atomic constraints, with generated/manual byte equivalence and production fault poison checks.

## Persistence projection identity lifetimes

- Retain open file/directory identities during bounded persistence observations, preventing recycled inode numbers from conflating removed objects with new files or directories.
- Add a real-filesystem replacement regression and retain all existing namespace, append-prefix and omitted-sync assertions; the production engine and formats are unchanged.

## Explicit-schema model declarations

- Add optional `Record` derive for concrete named structs using the existing u8/u32/u64/string/bytes codecs; require explicit schema IDs and versions, reject implicit field skips/defaults and unsupported state.
- Add `catalog!` to generate native row enum/table/codec/index dispatch from one declaration, retaining deterministic application projections and final-view constraints.
- Convert the executable accounts and game-world models without changing their schemas or storage bytes; retain manual codec/catalog traits and engine-only builds without procedural macro dependencies.

## Native game queries

- Add lazy typed `index_scan` to borrowed catalog reads and coherent immutable snapshots, composing with Rust filter/projection/limit operations.
- Keep collected `index_range` APIs and staged-write semantics; validate malformed range bounds consistently even when an index is empty.
- Add executable area/position queries, inventory kind filters, item/owner joins and complete ordering cursors, verified across retained frames, WAL reopen, checkpoint and backup.
- Exercise laziness, independent range ordering, full-u64/empty cases and poison refusal; add a checked resident query smoke benchmark and native/CI example runs.

## Direct immutable-root construction

- Build initial single-table/catalog snapshot AVL roots directly from ordered resident rows/postings, allocating each final node once without a full-size temporary vector.
- Preserve native rows, ordered indexes, cooperative accounting and subsequent path-copy updates; add retained-root and failed-assessment recovery checks plus a verified conversion benchmark.
- Remove a redundant return in the non-Linux reservation refusal so Rust 1.89 Clippy also passes on macOS.

## Compact derived-index postings

- Store one/two sorted primary IDs inline for each derived index key, promoting larger groups to a BTreeSet and demoting on shrink.
- Preserve ordered lookup/ranges, final-view uniqueness, full-u64 IDs and the existing codec/recovery/sync contract.

## Measured native game-world optimization

- Retain unchanged catalog primary/secondary entries after complete constraint validation.
- Coalesce immutable snapshot replacements so each shared ancestor is copied once in the replacement pass; retain original WAL/codecs/sync-before-publication.
- Use borrowed keys for native indexed equality and reserve result capacity.
- Strengthen SQLite statements and add direct arithmetic/atomic-batch controls plus bounded independent group-snapshot save pipelines.
- Record paired before/after CPU-path runs and 24 rotated physical-device runs, including faster independent pipelines/reads, SQLite atomic-batch wins, write tails, starved borrowed readers and RAM/maintenance tradeoffs.

## Native game-world persistence target

- Define a Rust-native, SQL-free direction around durable dirty-state saves, typed area/inventory retrieval and coherent frame reads.
- Permanently exclude SQL syntax, parsers, SQL query APIs and SQL compatibility layers in contributor instructions and product documentation.
- Add a runnable game-world save/backup/reopen example, atomic inventory rollback and retry checks, and an independent wrapping-workload reference model.
- Add a production-path native/snapshot benchmark and a pinned benchmark-only SQLite WAL/FULL adapter, reporting save/read/frame tails, RSS, maintenance/disk overlap and exact fresh-process WAL/snapshot recovery.
- Keep engine APIs, persistence semantics and existing format fixtures unchanged; document workload targets and platform/RAM/maintenance boundaries.

## Reproducible contributor verification

- Verify every baseline row and exact sequence after volatile/synced benchmark phases and fresh reopen, outside reported timing sections.
- Add a contributor guide for required checks, complete lifecycle examples, explicit bounded ENOSPC/OOM wrappers, benchmark selection and completed milestone evidence.

## Process-budget recovery and checkpoint overlap

- Exercise actual kernel OOM during production checkpoint verification in a private Linux cgroup; require bounded-worker death and independently verify acknowledged recovery, orphan reclamation and backup.
- Document trusted codec allocations, external application-process containment and uncertain outcomes without claiming a library allocator/RSS cap.
- Pause production catalog checkpoint verification to prove coherent old/current row/index snapshots remain readable while maintenance excludes a subsequent writer.

## Bounded immutable read versions

- Add opt-in coherent immutable row/index snapshots for single tables and catalogs, reusing immediate/shared sync boundaries and independent group framing.
- Bound reader admission by live leases and conservative full-root native-memory accounting; expose oldest pin, versions and bytes without claiming an allocator/RSS cap.
- Preserve explicit checkpoint/cleanup with old memory pins and independently decoded backup; require released pins and exclusive clients for native offline migration.
- Add retained-version/index reference tests, production append/sync/maintenance faults and survival images, an executable lifecycle, and equivalent durable held-reader benchmark modes.

## Bounded independent group commit

- Add opt-in persistent single-table/catalog queues with bounded admission and collection delay, separate transaction frames and shared synchronization before row/index visibility or successful responses.
- Preserve independent rollback, prefix recovery and uncertainty; do not replay callbacks or clone full tables. Expose receipts, dropped-response semantics and explicit drain for offline migration.
- Add production append/sync/persistence faults, catalog constraints/index checks, queue/cancellation/panic/publication regressions, a complete managed example and equivalent durable low-load/saturation benchmark.

## Linux/macOS flush contract review

- Review exact MSRV/current standard-library sources and platform contracts; document that reviewed macOS `sync_all` requests `F_FULLFSYNC` without weaker fallback.
- Require native macOS persistence/lifecycle checks on Rust 1.89 as well as stable, and record runner compiler identity. Preserve conditional device guarantees and all synchronization boundaries.

## Required Linux file-data reservation

- Add opt-in `reserve_file_data` maintenance policy using safe Linux allocation bindings for all new generation-file data; require allocation success without sparse/unsupported fallback.
- Preflight exact snapshot length with one additional encoding pass, keep conversion callbacks single-use, preserve logical formats/locks/sync/publication, and reject changed encoded sizes before publication.
- Test allocation refusal, physical blocks versus logical EOF, recovery images, single-table/catalog lifecycles and both policies under actual ENOSPC; expose the mode in executable examples/resource benchmarking.

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

Experimental API, typed single-table and schema-bound multi-table modes, borrowed lock-based readers or opt-in immutable row/index snapshots, and one writer. No encryption or replication. Persistence currently requires Unix. Publishing is disabled pending owner release decisions.
