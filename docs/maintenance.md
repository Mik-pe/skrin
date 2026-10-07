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

`estimate_checkpoint(options)` traverses the current rows using the real encoder, without mutating storage. It returns the exact snapshot/new-file lengths, row count, committed sequence and largest encoded record for that view. Estimation is optional: ordinary checkpoints do **not** serialize twice for a preflight. Because another transaction may run after the estimate's read guard ends, pass the limits to the actual checkpoint as well.

```rust
let options = MaintenanceOptions {
    max_new_file_bytes: 64 * 1024 * 1024,
    max_record_bytes: 64 * 1024,
    max_rows: 100_000,
};
let estimate = db.estimate_checkpoint(options)?;
let checkpoint = db.checkpoint_with_options(options)?;
```

## Streaming verification and buffers

Snapshot writing reuses an encoder and batches writes through a 64 KiB buffer. Snapshot verification/reopen share **one parser** that verifies framing, ordering, schema identity, every CRC, codec consumption and EOF. A visitor decides whether a decoded row is retained.

A checkpoint drops each verification row before decoding the next one and keeps the original native table. It neither builds a second BTreeMap nor replaces every original row with a decoded copy. A regression test checks both the maximum number of live verification records and native row-address stability.

Offline migration consumes old rows and builds the destination table. Its verification also keeps only one additional decoded row alive. The destination table and arbitrary conversion allocations are still application-sized. A returned backup handle, by contrast, must own its independently decoded table: it necessarily adds that resident data while the source exists. Neither API claims a process-wide RSS cap.

Reusable bounded record/payload buffers and a portable slicing-by-eight IEEE CRC-32 path reduce repeated allocation/checksum work. No `unsafe`, intrinsics, new dependencies, format change or synchronization downgrade is involved. Existing independently encoded format fixtures remain unchanged.

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

## Inventory and safe reclamation

`storage_inventory()` scans immediate entries while holding the stable directory owner and a read guard. It validates CURRENT against the owning handle and performs no explicit writes, repair, sync or deletion. It classifies metadata, active/retained generations, obsolete generations, unpublished future generations, valid temporary manifests, and unknown contents. Paths are relative and sorted.

Reported bytes are **observed regular-file lengths**, not filesystem allocation/free space. Reserved generation directories are scanned one level deep. Unknown directory trees and symlinks are not followed. Inventory is metadata inspection, not full snapshot/WAL corruption verification.

`prune()` keeps its original scope: recognized old generations, retaining active + previous. `reclaim()` additionally handles recognized unpublished generations newer than CURRENT and complete temporary manifests. Both are explicit, serialized and restartable. Cleanup never chooses a new active generation.

A nonempty generation is removable only with the exact checksummed OWNER identity and only recognized regular children (`snapshot`, `wal`, `OWNER`). Data files are removed and directory-synced before removing OWNER. An empty reserved generation directory can be removed on retry. Temporary manifests must be bounded, complete, checksummed, table-matching and consistent with their canonical filename. Active/previous identities are protected.

**Unknown, malformed or partially written ownership markers/manifests are preserved**, as are symlinks, special files and directories containing unexpected children. They need operator investigation rather than a destructive guess. Consequently reclamation bounds recognized abandoned work, not arbitrary externally added/corrupt content. All namespace changes by other programs remain unsupported; regular-file checks are not a security boundary against a concurrent adversary.

Cleanup errors do not poison the active generation. A report is returned only on full success; after an error, re-inventory/retry rather than assuming nothing was removed. CURRENT mismatch/corruption is refused without cleanup or WAL repair.

## Persistence-order evidence

The test-only persistence projection observes the production file-sync, directory-sync and manifest-rename sites. File contents and namespace/inode identities are persisted separately. It materializes crash images at production boundaries by discarding unsynced namespace changes, and also explores an unsynced CURRENT rename surviving independently of other directory entries. **Actual `open_dir` recovery** then reads each image.

Tests cover acknowledged writes across checkpoint/migration, complete old-or-new schema selection, and restartable cleanup with active/previous retention. A negative control deliberately omits the pre-publication parent sync from the projection and must detect a manifest referring to an absent generation. This proves the test can catch that ordering regression; it does not bypass or weaken production synchronization.

The projection complements existing partial-write, sync-failure, corruption and subprocess-exit tests. It is not an exhaustive model of sectors, device caches, every possible unsynced file write, kernel bugs or all filesystems. macOS hardware-flush review and identified real-device long-duration/power-cut measurements remain release work.
