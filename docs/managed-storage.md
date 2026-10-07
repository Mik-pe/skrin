# Managed storage: checkpoints, backup and schema evolution

The managed backend is selected explicitly with `Database::create_dir` / `open_dir`. Original `create` / `open` standalone files keep their unchanged v1 contract. Both backends use the same typed table, transaction encoder and WAL recovery engine.

## Ownership and layout

```text
accounts.skrin/
  LOCK                        permanent inode, never replaced or removed
  CURRENT                     bounded, checksummed active manifest
  g0000000000000001/
    OWNER                     recognized generation marker
    snapshot                  immutable, sealed row state
    wal                       append-only format-2 segment
  g0000000000000002/
    OWNER
    snapshot
    wal
```

Generation numbers are hexadecimal, nonzero and monotonically allocated. They are not transaction sequences. A stage left by a failed operation is skipped rather than overwritten. After 1,024 allocation collisions, maintenance refuses to proceed until an operator inspects the directory.

A permanent `LOCK` file contains `SKRLOCK1` and holds the same OS lock owner across all WAL replacements. It is acquired before reading `CURRENT`. Do not rename, unlink, replace or externally modify an open database directory, its lock, or any active files. The entire directory namespace is engine-owned; keep user files elsewhere. Paths are canonicalized so changing the process working directory cannot redirect later maintenance.

The old WAL also remains locked while a replacement is prepared. Ordinary writes still stage only their touched rows. Checkpoint and migration work is serialized; there are no hidden workers.

## Publication protocol

For a checkpoint at committed sequence S:

1. Acquire the table's exclusive lock while retaining the permanent directory lock.
2. Create a fresh generation directory, never overwriting a staged directory.
3. Write and sync its owner marker and a complete, checksummed snapshot.
4. Create and sync an empty WAL whose prelude binds it to this generation and S.
5. Read and decode the snapshot independently; reject incomplete/invalid content.
6. Sync the generation directory, then **sync its parent directory**. The generation's name must already be durable before a manifest may refer to it.
7. Create and sync a new, checksummed temporary manifest.
8. Rename it over `CURRENT`, then sync the database directory again.
9. Install the new WAL under the still-held locks, drop the previous WAL handle, and return success.

The preceding generation is retained. A checkpoint creates no application transaction: sequence S is unchanged, and the next write is S+1.

Before step 8, a reported error leaves the existing handle usable and the old `CURRENT` authoritative. From the rename attempt onward, an error is conservatively `MaintenanceUncertain`: poison the handle and reopen. The new generation may already be selected. A panic while holding the exclusive lock also requires reopening.

File and directory synchronization must be honored by the local Unix filesystem/device. These tests do not model loss of previously synced bytes or prove hardware-flush behavior on every platform.

## Recovery

Open the stable lock; validate the bounded `CURRENT` manifest and its schema; validate its owner marker; verify/decode the exact referenced snapshot; then replay only its matching WAL segment starting at S+1. No complete snapshot or manifest is repairable by truncation. Only the shared WAL engine may repair an incomplete final transaction frame.

A missing/invalid `CURRENT`, missing active files, cross-generation snapshot/WAL swap, incompatible schema or complete corruption is a refusal. Recovery does not pick the largest generation number or roll back to the previous generation. Recovered files/directories are synced before returning a usable handle, including after an earlier uncertain publication.

Failed preparations can leave unreferenced generation directories and temporary manifests. They are never automatically selected. Complete recognized generations become eligible for pruning after a later successful checkpoint; malformed or unknown content is left for explicit inspection. There is no automatic deletion during recovery. Explicit `reclaim()` can remove recognized abandoned generations and complete validated temporary manifests, including newer unselected generations. Corrupt/partial ownership metadata stays untouched for operator investigation; see [bounded maintenance](maintenance.md).

## Retention and headroom

`checkpoint()` rotates to a new snapshot and WAL. `prune()` is an explicit separate operation that retains the active and immediately previous generations. Both must be called by the application; without checkpoints the active WAL still grows. `checkpoint_if_needed(policy, options)` supplies explicit WAL-byte/commit thresholds, and `reclaim()` extends cleanup to recognized abandoned work without running inside commits.

Pruning only examines exact reserved generation directory names older than the active generation. Nonempty candidates require the correct owner marker and only recognized regular files (`OWNER`, `snapshot`, `wal`). Unknown content is skipped. It removes named files, never recursively deletes a directory tree, and leaves active/previous generations untouched. Empty reserved directories left by interrupted final cleanup can be removed on retry. Cleanup errors do not poison the active database. The returned report records removal counts, bytes and skips.

With successful periodic maintenance, history retention is bounded to the active and previous generation's WAL intervals, not all historical transactions. Pruning is not a backup policy. It may remove the older schema's generation after subsequent checkpoints.

Provide room for the current data, retained generation and the complete replacement. Explicit encoded-file/record/row budgets are available through `MaintenanceOptions`; the limits do not constrain arbitrary application allocations or guarantee physical free space. Optional Linux `reserve_file_data` requires each replacement file's data allocation before writing, with an exact snapshot preflight; filesystem metadata and subsequent commits remain outside it. ENOSPC before publication leaves the old generation selected; a failure during publication requires reopen/reconciliation. Never delete the only good copy merely to force a checkpoint through.

Snapshot writing is streaming and bounded per record, not limited to one transaction payload. Checkpoint/migration verification uses the same real decoder as recovery, discarding each verification row before reading the next one. Checkpoints retain the original native table. Backups retain the decoded table because the returned independent handle owns it. See [resource limits and buffers](maintenance.md#streaming-verification-and-buffers).

## Backup and restore

`backup_to(NEW_PATH)` holds a consistent source read guard, builds an independent managed directory and decodes its snapshot before returning the backup handle. It accepts memory databases, v1 files and managed directories. Writers wait until it finishes. It never overwrites an existing destination, and a failed creation may leave a partial new directory that is never silently adopted on retry.

A backup preserves committed sequence, schema and migration history, but begins its own generation lineage. Subsequent source writes do not affect it. Drop the returned backup handle, then use `open_dir` with its recorded schema to verify restoration independently. A pre-migration backup remains in the old schema. Keep separately stored copies for actual disaster recovery; two directories on one failed disk are not independent hardware protection.

To import a v1 file: open it with the original `Record`, call `backup_to` on a new path, and verify reopening that directory. The original file is not rewritten or removed. Select the new path explicitly in the application.

## Offline migrations

`migrate::<NewRecord>(id, closure)` consumes the old handle. Both schemas are concrete Rust `Record` types; old bytes were decoded by the old codec. The closure receives `(primary_key, owned_old_value)` and returns a complete new value. It can rename/split fields or add defaults without `Clone`.

The table ID and primary keys are preserved. The new schema version must increase. Migration IDs contain 1..128 ASCII letters, digits, `.`, `_` or `-`, must be unique within history, and the bounded history allows 128 transitions. A transition records its source/destination versions and preserved committed sequence. History is checked for continuity, duplicate IDs and impossible sequences at open.

All conversion, encoding and independent decoding happens before the same generation publication protocol. A conversion error leaves the source generation active, although the consumed handle is closed. A publication error may leave either schema active; reopen with explicit types and use a `SchemaMismatch` result to determine which schema the valid manifest selected. Never retry an external side effect from a migration closure.

## Binary formats

All integers are little-endian. CRC is the same IEEE CRC-32 used by WAL v1: error detection, not authentication. Snapshot/manifest/segment fixtures are generated independently using Python `struct` and `zlib.crc32` and compared byte-for-byte in tests.

### CURRENT v1

`SKRDIR01` (8 bytes), generation `u64`, previous generation `u64`, table ID `u64`, schema version `u32`, checkpoint sequence `u64`, snapshot row count `u64`, migration count `u32`; then migration entries; then CRC-32 of all preceding bytes. With no migrations the file is 60 bytes; the entire file is capped at 64 KiB.

Each migration entry is a `u32` length-prefixed UTF-8 ID, source version `u32`, destination version `u32`, and committed sequence `u64`. Generation must be nonzero; previous must be smaller; schema/history must agree.

### Snapshot v1

A 48-byte header: `SKRSNP01` (8), generation `u64`, table ID `u64`, schema version `u32`, sequence `u64`, row count `u64`, header CRC `u32`. Every header field must match the active manifest.

Exactly that many rows follow, in strictly increasing primary-key order. A row is key `u64`, encoded length `u32`, encoded bytes, and a CRC `u32` covering key, length and payload. Encoded rows remain capped at 8 MiB. No trailing bytes, duplicate keys, partial rows or codec leftovers are accepted. The complete snapshot may exceed 16 MiB.

### WAL segment v2

The original 32-byte WAL header uses storage format 2 (reserved bits still zero). A 24-byte prelude follows: `SEG2` (4), generation `u64`, snapshot sequence `u64`, and CRC `u32`. Both identifiers must match the manifest/snapshot. An empty segment is 56 bytes.

The rest uses unchanged v1 transaction framing, with its first sequence equal to snapshot sequence + 1. Standalone `Database::open` rejects format 2 even if the segment is empty; internal WALs are not independent databases.

### OWNER v1

`SKROWN01` (8), table ID `u64`, generation `u64`, and CRC `u32`: 28 bytes. This recognizes engine-owned generation contents for validation/cleanup; it is not cryptographic identity or access control.
