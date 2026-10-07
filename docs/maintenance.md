# Bounded, caller-driven maintenance

Managed databases now have streaming checkpoint verification, explicit encoded-resource budgets, read-only inventory, conservative orphan reclamation, and threshold-driven checkpoints. These APIs do not create a background worker or change ordinary commit acknowledgement semantics.

Run the [complete maintenance example](../crates/skrin/examples/maintenance.rs) with a **new** directory:

```sh
cargo run -p skrin --example maintenance -- /tmp/skrin-maintenance-demo
```

## Limits and preflight

`checkpoint_with_options`, `backup_to_with_options` and `migrate_with_options` accept `MaintenanceOptions`. Their existing convenience methods use default limits: the 8 MiB encoded-record format cap, no extra row cap and no extra total-file-byte cap.

| Option | Enforced boundary |
| --- | --- |
| `max_new_file_bytes` | Combined logical lengths of the new snapshot, OWNER, WAL prelude and CURRENT manifest; also LOCK for a newly created backup |
| `max_record_bytes` | Encoded length of one row, checked by the encoder before extending its buffer; cannot exceed the format's 8 MiB cap |
| `max_rows` | Resulting snapshot row count, checked before staging/conversion |

The new-file limit is checked before each affected snapshot write, with remaining metadata already accounted for. This includes migration-history bytes. A limit rejection precedes publication: an existing source generation remains valid. A partially prepared stage can remain for explicit inspection/reclamation. Record-size errors use `LimitExceeded`; row/file budget errors use `BudgetExceeded` with the resource, limit and minimum required at the rejected step.

These are **encoded-size/count limits, not a filesystem or process-memory reservation**. They do not limit allocator capacity/slack, user codec allocations, native tables/indexes, filesystem metadata, block rounding, snapshots outside the new generation, old files or other processes' writes. ENOSPC and I/O failures still need normal handling. No racy free-space measurement is interpreted as a promise that publication will succeed.

`estimate_checkpoint(options)` traverses the current rows using the real encoder, without mutating storage. It returns the exact snapshot/new-file lengths, row count, committed sequence and largest encoded record for that view. Estimation is optional for ordinary one-pass checkpoints; required file-data reservation adds its own exact snapshot preflight under the maintenance lock. Because another transaction may run after an explicit estimate's read guard ends, pass the limits to the actual checkpoint as well.

```rust
let options = MaintenanceOptions {
    max_new_file_bytes: 64 * 1024 * 1024,
    max_record_bytes: 64 * 1024,
    max_rows: 100_000,
    ..Default::default()
};
let estimate = db.estimate_checkpoint(options)?;
let checkpoint = db.checkpoint_with_options(options)?;
```

## Streaming verification and buffers

Snapshot writing reuses an encoder and batches writes through a 64 KiB buffer. Snapshot verification/reopen share **one parser** that verifies framing, ordering, schema identity, every CRC, codec consumption and EOF. A visitor decides whether a decoded row is retained.

A checkpoint drops each verification row before decoding the next one and keeps the original native table. It neither builds a second BTreeMap nor replaces every original row with a decoded copy. A regression test checks both the maximum number of live verification records and native row-address stability.

Offline migration consumes old rows and builds the destination table. Its verification also keeps only one additional decoded row alive. The destination table and arbitrary conversion allocations are still application-sized. A returned backup handle, by contrast, must own its independently decoded table: it necessarily adds that resident data while the source exists. Neither API claims a process-wide RSS cap.

Reusable bounded record/payload buffers and a portable slicing-by-eight IEEE CRC-32 path reduce repeated allocation/checksum work. Those buffer/CRC optimizations introduce no `unsafe`, intrinsics, dependencies, format change or synchronization downgrade. The later Linux allocation policy has the separate dependency described below. Existing independently encoded format fixtures remain unchanged.

## Thresholds without hidden commit work

`checkpoint_if_needed(policy, options)` evaluates the thresholds and publishes under the same exclusive state lock. Either enabled threshold triggers a checkpoint, but only after a new committed transaction. No-op/rolled-back transactions do not advance the sequence. Repeated calls against an unchanged snapshot do no maintenance. `None` disables a threshold; zero requests a checkpoint on the next call after a commit.

```rust
let policy = CheckpointPolicy {
    wal_bytes: Some(64 * 1024 * 1024),
    commits: Some(10_000),
};
// Call from your application's maintenance cadence, not inside a transaction.
if let Some(checkpoint) = db.checkpoint_if_needed(policy, options)? {
    let cleanup = db.reclaim()?;
}
```

The application controls the pause. Readers/writers are blocked while maintenance runs. Cleanup is deliberately a separate call: a cleanup error cannot turn an already successful checkpoint or write into an apparent rollback. Keep independent backups; retained generations are not a disaster-recovery policy.

## Required file-data reservation

On Linux, set `MaintenanceOptions::reserve_file_data` to `true` for checkpoint, backup or migration. It defaults to false. The engine requires `fallocate(FALLOC_FL_KEEP_SIZE)` through safe `rustix` bindings for the new snapshot, OWNER, WAL prelude and CURRENT manifest, plus LOCK for a new backup. Each file's complete data range is allocated before writing it. Logical lengths and codecs stay unchanged; preallocation never creates trailing zero bytes in the snapshot format. The permanent directory owner and new WAL locks remain held during allocation.

```rust
let options = MaintenanceOptions {
    reserve_file_data: true,
    ..Default::default()
};
db.checkpoint_with_options(options)?;
```

Required mode performs one extra snapshot encoding pass to determine its full data extent before the first snapshot write. It keeps one encoder buffer and adds no native table copy. Migration conversion callbacks still run once per row. Actual encoding must produce the preflight's size; growth/shrink is a codec refusal before publication. Codecs must remain deterministic even when an explicit estimate/preflight calls them more than once.

Allocation failure, including ENOSPC or unsupported filesystem operations, is a preparation error with no fallback to sparse length extension or unreserved writes. The existing source generation stays selected and usable. Backups can leave a partial create-only destination; migrations consume their old handle and require reopening it. Non-Linux platforms refuse the required option before destination/stage creation. The flag is an operation policy, not a persisted format/schema change or an allocation guarantee for later WAL commits.

The [Linux allocation contract](https://man7.org/linux/man-pages/man2/fallocate.2.html) covers the requested file-data range; block rounding can allocate more. It does not reserve other files, retained generations, filesystem/directory/journal metadata, quotas for other work or application RAM. Additional files are allocated as their preparation starts. This is no global filesystem transaction or promise that publication cannot fail. File/directory syncs and uncertain-outcome handling remain mandatory. Use a filesystem whose allocation/flush behavior meets the application's requirements; no device power-loss certification follows from preallocation. [The safe binding](https://docs.rs/rustix/1.1.5/rustix/fs/fn.fallocate.html) is the sole new Linux dependency purpose.

Tests check real allocated blocks with unchanged logical EOF, each failed required allocation (including unsupported operation), acknowledged crash images, exact encoded sizes, single-table/catalog backup/migration and once-only conversion. The bounded-tmpfs wrapper executes both allocation policies against real ENOSPC. Unknown stages retain the inspection/reclamation contract below.

## Inventory and safe reclamation

`storage_inventory()` scans immediate entries while holding the stable directory owner and a read guard. It validates CURRENT against the owning handle and performs no explicit writes, repair, sync or deletion. It classifies metadata, active/retained generations, obsolete generations, unpublished future generations, valid temporary manifests, and unknown contents. Paths are relative and sorted.

Reported bytes are **observed regular-file lengths**, not filesystem allocation/free space. Reserved generation directories are scanned one level deep. Unknown directory trees and symlinks are not followed. Inventory is metadata inspection, not full snapshot/WAL corruption verification.

`prune()` keeps its original scope: recognized old generations, retaining active + previous. `reclaim()` additionally handles recognized unpublished generations newer than CURRENT and complete temporary manifests. Both are explicit, serialized and restartable. Cleanup never chooses a new active generation.

A nonempty generation is removable only with the exact checksummed OWNER identity and only recognized regular children (`snapshot`, `wal`, `OWNER`). Data files are removed and directory-synced before removing OWNER. An empty reserved generation directory can be removed on retry. Temporary manifests must be bounded, complete, checksummed, table-matching and consistent with their canonical filename. Active/previous identities are protected.

**Unknown, malformed or partially written ownership markers/manifests are preserved**, as are symlinks, special files and directories containing unexpected children. They need operator investigation rather than a destructive guess. Consequently reclamation bounds recognized abandoned work, not arbitrary externally added/corrupt content. All namespace changes by other programs remain unsupported; regular-file checks are not a security boundary against a concurrent adversary.

Cleanup errors do not poison the active generation. A report is returned only on full success; after an error, re-inventory/retry rather than assuming nothing was removed. CURRENT mismatch/corruption is refused without cleanup or WAL repair.

### Operator workflow for unknown or partially owned stages

An unknown entry is preserved evidence, not a cleanup candidate. A failed OWNER write can leave a reserved-name directory with an empty/partial marker; repeated `reclaim()` calls intentionally cannot prove ownership from that name. Use this workflow:

1. Restore headroom using only files whose ownership and disposability are established independently of Skrin. Preserve CURRENT, LOCK, active/previous generations and every unknown entry. Encoded maintenance limits are not free-space reservations.
2. For an uncertain/poisoned handle, stop its work, drop every handle and reopen the same root with its recorded application schema. Recovery follows CURRENT and may repair an incomplete WAL tail. If byte-for-byte forensic preservation is needed, copy the complete closed root to separate storage before attempting recovery. Missing/invalid CURRENT, incompatible schema and complete corruption remain refusals; preserve the refused root for diagnosis instead of constructing a replacement manifest or selecting a filename.
3. On a usable handle, save `generation_info()` and `storage_inventory()` output. Inventory is read-only under the permanent owner lock; record the relative Unknown paths and active/previous identities. Run `reclaim()` for recognized candidates, then inventory again. A skipped partial owner is expected and may still occupy space.
4. If the selected data is valid but unknown contents need offline investigation, call `backup_to_with_options` with a **new** destination on storage with sufficient encoded headroom and application memory. Open the backup independently and reconcile its recorded sequence, rows and indexes with the application. Coordinate writers if comparing against a specific source sequence. Backup blocks writers during its consistent capture but also holds a second decoded table; budget for that native memory separately.
5. The application can reopen that verified independent directory while retaining the original closed root for investigation. Do not replace/rename files or roots while any source handle exists. Quarantine is an operator storage policy, not an engine repair or automatic deletion path. Never manufacture OWNER bytes just to make unknown contents reclaimable; disposal requires independent ownership/data-retention evidence.

The original/quarantined root still consumes disk. This workflow gives a usable independently verified destination and preserves unknown data; it does not impose an automatic bound on arbitrary externally added, corrupt or unclaimed files. An application needs an explicit quarantine/headroom policy.

## Persistence-order evidence

The test-only persistence projection observes production writes/truncates, file-syncs, directory-syncs and manifest renames. File contents and namespace/inode identities are persisted separately. For bounded workloads it enumerates every subset of the current unsynchronized namespace differences, including an independently surviving CURRENT rename. Those alternatives are crossed with all observed unsynced file writes lost, all surviving, or one file's writes surviving; existing synchronized WAL bytes also get every possible subsequent append prefix. New files use observed write prefixes, including test-only partial snapshot-record flushes. **Actual `open_dir` recovery** then reads each image.

Tests record acknowledgments only after successful production writes and publication only after successful maintenance. Recovery must retain every acknowledged transaction and completed generation publication; earlier images permit unacknowledged transactions to survive. They cover complete old-or-new schema selection, coherent multi-table rows/indexes, and restartable cleanup with active/previous retention. Eight negative controls independently omit snapshot, new WAL, appended WAL, OWNER, manifest, generation-directory and both parent-directory sync guarantees from the projection and must detect a violation. Production synchronization still executes in these controls.

Explicit `StorageFull` injection additionally exercises append byte cutoffs, append sync and every sampled checkpoint/migration boundary. Preparation errors must keep the old CURRENT and usable source; uncertain publication must poison the handle and reopen only a complete old/new generation. These are ENOSPC-equivalent injected errors, not measurements on an exhausted filesystem.

`scripts/test-storage-full.sh` separately mounts an exclusive 4 MiB tmpfs in a private Linux mount namespace and fills it until the kernel returns real ENOSPC. The production managed/WAL paths must refuse checkpoint preparation without changing CURRENT, poison a failed large append, recover the previous row/sequence after headroom is restored, preserve the partial ownership stage through reclamation, and create/reopen an independently decoded backup. The test refuses ordinary filesystems, nonempty mounts and capacities outside 1–8 MiB. Normal `cargo test` reports this environment-specific test as ignored; the wrapper runs it explicitly and Linux/MSRV CI runs both debug and `--release` as a required step. The CI-only `--privileged-namespace` mode uses passwordless sudo to create the isolated mount; local default uses an unprivileged user namespace. Neither fills the caller's filesystem or changes its mounts. This is real filesystem-error evidence, not physical-device persistence or power-loss certification.

The projection complements existing partial-write, sync-failure, corruption and subprocess-exit tests. It limits current namespace deltas to twelve, observed files to 1 MiB and exhaustive append suffixes to 4 KiB. It does not enumerate every new-file byte prefix, combinations of partial contents across multiple files, historical write reorderings, sectors, device caches, kernel bugs or all filesystems. macOS hardware-flush review and identified real-device long-duration/power-cut measurements remain release work.

## Schema-bound catalogs

`catalog::CatalogDatabase` exposes the same encoded budgets, exact checkpoint preflight, caller-driven thresholds, read-only inventory and migration history as the single-table backend. It also enforces independently decoded index constraints/projections before publication. The catalog descriptor participates in snapshot row/record/file accounting, while catalog stats and checkpoint reports exclude it from application-row counts. See [catalog maintenance](catalog.md#recovery-maintenance-and-migrations). Native/index allocations and physical filesystem reservations remain outside these encoded budgets.
