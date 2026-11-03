#ifndef SKRIN_H
#define SKRIN_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define SKRIN_C_ABI_VERSION 1u
#define SKRIN_ERROR_MESSAGE_CAPACITY 256u

typedef int32_t skrin_status;
enum {
    SKRIN_OK = 0,
    SKRIN_NOT_FOUND = 1,
    SKRIN_INVALID_ARGUMENT = 2,
    SKRIN_BUFFER_TOO_SMALL = 3,
    SKRIN_BUSY = 4,
    SKRIN_DUPLICATE_KEY = 5,
    SKRIN_LIMIT_EXCEEDED = 6,
    SKRIN_IO_ERROR = 7,
    SKRIN_COMMIT_UNCERTAIN = 8,
    SKRIN_MAINTENANCE_UNCERTAIN = 9,
    SKRIN_POISONED = 10,
    SKRIN_CORRUPT = 11,
    SKRIN_UNSUPPORTED_FORMAT = 12,
    SKRIN_SCHEMA_MISMATCH = 13,
    SKRIN_UNSUPPORTED_PLATFORM = 14,
    SKRIN_PANIC = 15,
    SKRIN_ENGINE_ERROR = 16
};

typedef struct skrin_db skrin_db;
typedef struct skrin_write skrin_write;

/* Optional per-call output, never thread-local. The message is truncated UTF-8,
 * always NUL terminated, and diagnostic only; branch on the returned status. */
typedef struct skrin_error {
    skrin_status code;
    char message[SKRIN_ERROR_MESSAGE_CAPACITY];
} skrin_error;

typedef struct skrin_stats {
    uint64_t rows;
    uint64_t sequence;
    uint64_t wal_bytes;
    uint64_t recovered_tail_bytes;
    uint32_t persistent;
    uint32_t reserved;
} skrin_stats;

typedef struct skrin_reclaim_report {
    uint64_t generations_removed;
    uint64_t temporary_manifests_removed;
    uint64_t bytes_removed;
    uint64_t skipped;
} skrin_reclaim_report;

/* ABI v1 stores one table of u64 keys and opaque byte values. The adapter has
 * a fixed, versioned codec; applications own their payload codecs and versions.
 * It does not open arbitrary Rust Record/catalog databases. No SQL is accepted.
 * All pointers must refer to valid, aligned, live memory of the documented size.
 * Input/output regions and handles must not overlap. NULL data is allowed only
 * with zero length/capacity. Inputs are borrowed only for the call, never freed.
 * Output buffers and scalar/struct outputs may be uninitialized. Output handle
 * slots must instead be initialized to NULL as required below.
 * Returned bytes are copied into caller-owned buffers; no allocator is shared.
 * Every function containing an error parameter accepts NULL there.
 * Unwinding Rust panics are contained; process aborts/OOM cannot be recovered.
 * A database involved in a caught operation panic is poisoned.
 */
uint32_t skrin_c_abi_version(void);
size_t skrin_max_value_size(void);

/* out_db must point to NULL; it remains NULL on failure. UTF-8 paths are passed
 * with byte lengths and must be nonempty and contain no embedded NULs. Create
 * never overwrites; open never creates. Parents must already exist durably.
 * Persistent storage is currently Unix-only. */
skrin_status skrin_db_in_memory(skrin_db **out_db, skrin_error *error);
skrin_status skrin_db_create(const uint8_t *path, size_t path_len,
                            skrin_db **out_db, skrin_error *error);
skrin_status skrin_db_open(const uint8_t *path, size_t path_len,
                          skrin_db **out_db, skrin_error *error);

/* Close consumes and clears *db; an already NULL *db is allowed. Database
 * operations may run concurrently, but close must not race any use of that
 * handle. A live write handle retains its database, including its file lock,
 * and remains usable after this database handle is closed. */
skrin_status skrin_db_close(skrin_db **db, skrin_error *error);

/* Point reads see committed data. Required out_len is zero on errors other than
 * BUFFER_TOO_SMALL, which reports the required capacity without copying bytes.
 * NULL/0 is a size probe. Empty values return OK with length zero, absent keys
 * return NOT_FOUND. Concurrent commits may change the size between two calls;
 * retry BUFFER_TOO_SMALL or read through a write handle for a stable base view.
 * No Rust reference or lock escapes a call. */
skrin_status skrin_db_get(const skrin_db *db, uint64_t key,
                         uint8_t *out, size_t capacity, size_t *out_len,
                         skrin_error *error);
skrin_status skrin_db_stats(const skrin_db *db, skrin_stats *out,
                           skrin_error *error);

/* One active writer per database; begin and maintenance return BUSY while it is
 * active. Committed point reads remain available during staging. out_write must
 * point to NULL. This owned transaction stages only touched keys and sees its
 * own changes on top of a committed base that cannot change until it ends.
 * Calls on the same write handle must be externally serialized. A statement
 * error leaves earlier staging intact: deliberately commit it or abort.
 */
skrin_status skrin_write_begin(const skrin_db *db, skrin_write **out_write,
                              skrin_error *error);
skrin_status skrin_write_get(const skrin_write *write, uint64_t key,
                            uint8_t *out, size_t capacity, size_t *out_len,
                            skrin_error *error);
skrin_status skrin_write_insert(skrin_write *write, uint64_t key,
                               const uint8_t *value, size_t value_len,
                               skrin_error *error);
skrin_status skrin_write_put(skrin_write *write, uint64_t key,
                            const uint8_t *value, size_t value_len,
                            skrin_error *error);
skrin_status skrin_write_remove(skrin_write *write, uint64_t key,
                               uint32_t *out_removed, skrin_error *error);

/* Commit ALWAYS consumes and clears a valid *write, including on failure.
 * Persistent success follows the engine's append -> sync -> publish boundary.
 * A COMMIT_UNCERTAIN/MAINTENANCE_UNCERTAIN outcome may have committed: the
 * database is poisoned; close all owners, reopen and reconcile operation IDs.
 * Clean validation/size refusal leaves the database usable. Empty commits do
 * not advance the sequence. Abort consumes/clears without publishing anything;
 * aborting an already NULL *write is allowed, including after a failed commit.
 */
skrin_status skrin_write_commit(skrin_write **write, skrin_error *error);
skrin_status skrin_write_abort(skrin_write **write, skrin_error *error);

/* Explicit managed maintenance. Checkpoint retains the previous generation;
 * reclaim preserves active + previous and unknown contents (reported skipped).
 * Memory checkpoint/reclaim are invalid operations. Backup creates and verifies
 * an independent new managed directory; it never overwrites its destination.
 * Provide headroom for retained/replacement generations. */
skrin_status skrin_db_checkpoint(const skrin_db *db, skrin_error *error);
skrin_status skrin_db_reclaim(const skrin_db *db, skrin_reclaim_report *out,
                             skrin_error *error);
skrin_status skrin_db_backup(const skrin_db *db, const uint8_t *path,
                            size_t path_len, skrin_error *error);

#ifdef __cplusplus
}
#endif
#endif
