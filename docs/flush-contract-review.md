# Linux and macOS synchronization review

Reviewed 2026-10-07 for the Rust 1.89 MSRV and Rust 1.98.1/1.99.0 implementations. This is a source/contract review and native CI evidence, not a drive power-loss experiment. Skrin delegates persistence to `File::sync_all` and checks its result before publication or acknowledgment.

## What the standard library actually requests

The public [`File::sync_all` contract](https://doc.rust-lang.org/std/fs/struct.File.html#method.sync_all) attempts to synchronize file contents and metadata. Its platform implementation matters. The reviewed [`Rust 1.89 Unix source`](https://github.com/rust-lang/rust/blob/1.89.0/library/std/src/sys/fs/unix.rs#L1129), [`1.98.1 source`](https://github.com/rust-lang/rust/blob/1.98.1/library/std/src/sys/fs/unix.rs) and [`1.99.0 source`](https://github.com/rust-lang/rust/blob/1.99.0/library/std/src/sys/fs/unix.rs) implement the call as follows:

| Target | Requested operation | Error behavior |
| --- | --- | --- |
| Linux | `fsync(fd)` | Retry interruption; propagate other failures |
| Apple targets, including macOS | `fcntl(fd, F_FULLFSYNC)` | Retry interruption; propagate other failures; no fallback to weaker `fsync` in these versions |

The earlier description that Skrin had no implemented macOS hardware-flush request was incomplete: it has no custom wrapper, but the reviewed standard library already requests `F_FULLFSYNC`. A successful call still does not certify a filesystem/device against power loss. A different future standard library implementation needs review before relying on the same platform interpretation.

The [Linux `fsync` contract](https://man7.org/linux/man-pages/man2/fsync.2.html) includes associated file metadata and device-cache flushing when supported; it separately requires synchronizing the containing directory for a new or renamed entry. It can report delayed write failures, including ENOSPC. Skrin therefore treats commit synchronization failures as uncertain, rather than undoing the acknowledgment boundary or retrying with a weaker operation.

[Apple's `fsync` documentation](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fsync.2.html) distinguishes host-to-drive completion from flushing a drive's buffered data. Its [`F_FULLFSYNC` documentation](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fcntl.2.html) requests the latter and warns that not all drives honor it. Apple's [current storage guidance](https://developer.apple.com/documentation/xcode/reducing-disk-writes) also describes full synchronization as best effort. Ordering-only barriers are not substituted for the configured acknowledgment boundary.

## Production call sites and publication ordering

| Path | Boundary before successful exposure |
| --- | --- |
| WAL creation and commit | `LockedFile::sync` delegates to `sync_all`; append success follows it |
| WAL recovery | Validate all complete frames, optionally truncate an incomplete tail, synchronize even an intact suffix, then return the handle |
| Snapshot | Flush the writer, synchronize the complete snapshot, independently decode and validate it before installing CURRENT |
| OWNER, LOCK and temporary CURRENT | Write complete bytes and synchronize the file |
| Generation publication | Synchronize new files, generation directory and root entry; synchronize temporary CURRENT; rename it; synchronize the root before returning publication success |
| Cleanup | Synchronize recognized child removals before removing their ownership marker/directory; preserve active/previous and unknown contents |
| Immediate parent on create/open | Synchronize its directory; callers must already have made any ancestors durable |

See [managed storage](managed-storage.md) for the complete protocol. Directory handles also use `sync_all`; an unsupported or failed directory operation propagates an error. There is no directory-sync bypass for macOS and no fallback to filename-based recovery. The production fault seam exercises file/directory synchronization failures beneath this protocol; the survival projection models successful synchronization, rather than pretending to implement either operating system's storage stack.

## Native verification and remaining scope

CI executes persistent recovery, process locks, corruption/refusal tests and all managed examples on native macOS. Both Rust 1.89 and stable are required there, so the platform contract is exercised at the MSRV as well as the current toolchain. Linux runs the same managed paths at both toolchains, plus release and real bounded-filesystem ENOSPC checks. Windows remains memory-only and rejects persistence before opening/creating storage.

Native CI establishes executable operation support and checked failure/publication behavior on its runner filesystem. It does not observe drive cache completion, torn sectors or survival after an actual power cut. Dedicated, identified disposable devices and filesystem-specific experiments are still necessary for such certification. The current public promise remains conditional on successful OS/filesystem/device synchronization, with no universal power-loss claim.

Reviewed whole-source SHA-256 values for `library/std/src/sys/fs/unix.rs`:

- Rust 1.89.0: `9d71a9e27e8b09934c3e313c8d49427a94a7e0ac0c94147b8f54a94231245813`.
- Rust 1.98.1: `b8a72cfd2af073ee9eaa5acddc0b6c95a2f6d9e69cbdad18ba1ee4ae2fa7b800`.
- Rust 1.99.0: `61f4ef3a4b679a38182e76634f96e551ef0b21b06c5695f36187f557d1efc232`.
