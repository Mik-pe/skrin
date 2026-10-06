# Durability and failure contract

## Persistent versus volatile

`Database::create` and `Database::open` use a file backend on Unix. A nonempty persistent commit follows:

1. Validate the application closure and encode all changed records within size limits.
2. Append a complete transaction frame with sequence and checksums.
3. Call `File::sync_all` successfully.
4. Publish all changes under the exclusive state lock and return success.

Read guards cannot observe staging or an unsynced commit. Empty transactions neither append nor sync. `Database::in_memory` deliberately skips encoding and persistence; do not compare its numbers to a durable database and call that a speedup.

The guarantee is conditional on the OS, local filesystem and storage device honoring synchronization. Parent directories must already exist durably; the caller is responsible for any newly created ancestors. Skrin syncs its immediate parent on create/open. Network filesystems and external file modifications are unsupported. Non-Unix persistence currently returns `UnsupportedPlatform` before creating/opening a file; in-memory mode still works there.

**This is not universal power-loss certification.** In particular, the current macOS backend uses standard `sync_all`; no stronger platform-specific hardware-flush contract is implemented. Hardware that lies about flush completion, disappearing media, catastrophic corruption and loss of already-synced bytes are outside what this WAL can repair. See the standard library's [file synchronization and locking contracts](https://doc.rust-lang.org/std/fs/struct.File.html).

## Errors and uncertain commits

| Situation | Effect and caller action |
| --- | --- |
| Duplicate key / propagated closure error | No commit; drop staging; handle remains usable |
| Record codec or size-limit error | Detected before append; handle remains usable |
| Write or sync error during commit | `CommitUncertain`; all reads and writes on the handle are refused |
| Panic while holding the write lock | Lock is poisoned; close and reopen |
| Process exits after a successful commit | Replay should restore the transaction, subject to the sync contract |
| Complete frame exists but its caller saw no success | Recovery may include it; this is a valid uncertain outcome |
| Read/sync/truncate failure during recovery | Opening fails; no recovered database handle is returned |
| Schema/format mismatch or complete corruption | Opening fails without automatic repair or truncation |

After an uncertain commit, drop all owners of the handle, reopen, and reconcile using an application-level operation/idempotency key. Blindly repeating a transfer or counter increment can apply it twice. A database transaction cannot undo a sent email, network request, or mutation behind a shared record reference.

An unsuccessful create may leave an incomplete new file. It is never silently overwritten or treated as an empty database. Initialization failure needs explicit inspection/removal by the application, not automatic destructive recovery.

## Recovery policy

The file header must be complete, checksummed and compatible before any row decoding or tail repair. Each frame's bounded length and sequence are protected by a header checksum; complete payloads also have checksums and a validated trailer. All operations in a frame are decoded before applying any of them.

A final prefix of a valid frame can be discarded. A partial header must still match the available transaction-magic prefix. A complete header with a bad checksum/length/sequence is corruption, not permission to truncate. A complete payload with a bad checksum or an available trailer prefix that disagrees with the expected trailer is also corruption, even at EOF. Arbitrary trailing garbage is not silently ignored.

Recovery first validates all available complete frames. Only then can it truncate an incomplete final frame and sync the file. It syncs even when no truncation was necessary: a valid final frame can have come from a previous uncertain commit still sitting in the OS cache. The discarded byte count is available through `stats()`.

Checksums detect accidental damage, not malicious modifications. No log-only protocol can prove that an entire valid suffix was not externally removed without another trusted durable reference. Keep independent backups of important data.

## Current operational limits

The log is append-only and **has no checkpoint/rotation yet**. Disk space and recovery time grow with history, including overwritten/deleted records. This is a production blocker, not background maintenance that already exists.

There is no live-backup API. For experiments, fully close the database and copy the closed file; verify the copy by reopening it independently. Do not copy/rename/replace the live WAL and expect the lock or consistency contract to survive.

## Evidence and its limits

Tests exercise every byte truncation boundary and single-bit corruption in a sample two-transaction log, injected short writes and read/sync/truncate failures, uncertain sync outcomes, malformed but correctly checksummed operations, a fixed external-format fixture and a deterministic reference model. Integration tests use actual files, cross-process locks and process exit without destructors.

These tests are valuable but not a full filesystem simulator: they do not exhaustively model torn sectors, write reordering, device caches, kernel/filesystem bugs, or real power cuts. A future durability certification requires those additional tests and platform-specific flush review.
