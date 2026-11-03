#define _POSIX_C_SOURCE 200809L
#include "skrin.h"
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>

static skrin_error error;
static void require(int condition, unsigned line) {
    if (!condition) {
        fprintf(stderr, "C ABI check failed at line %u: status %d, %s\n",
                line, error.code, error.message);
        exit(EXIT_FAILURE);
    }
}
#define CHECK(condition) require((condition), __LINE__)
#define STATUS(call, expected) do { \
    skrin_status actual_status = (call); \
    CHECK(actual_status == (expected)); \
    CHECK(error.code == actual_status); \
    CHECK(error.message[SKRIN_ERROR_MESSAGE_CAPACITY - 1] == 0); \
} while (0)

static void child_path(char *out, size_t capacity, const char *root, const char *leaf) {
    int len = snprintf(out, capacity, "%s/%s", root, leaf);
    CHECK(len > 0 && (size_t)len < capacity);
}

static void memory_contract(void) {
    skrin_db *db = NULL;
    skrin_write *write = NULL, *other = NULL;
    uint8_t out[16];
    const uint8_t value[] = {0, 255, 0xc3, 0xa5, 0xf0, 0x9f, 0xa6, 0x80};
    size_t len = 99;
    CHECK(skrin_c_abi_version() == SKRIN_C_ABI_VERSION);
    CHECK(sizeof(skrin_stats) == 40);
    CHECK(sizeof(skrin_reclaim_report) == 32);
    CHECK(sizeof(skrin_error) == 260);
    STATUS(skrin_db_in_memory(&db, &error), SKRIN_OK);
    STATUS(skrin_write_begin(db, &write, &error), SKRIN_OK);
    STATUS(skrin_write_begin(db, &other, &error), SKRIN_BUSY);
    CHECK(other == NULL);
    STATUS(skrin_db_checkpoint(db, &error), SKRIN_BUSY);
    STATUS(skrin_write_insert(write, UINT64_MAX, value, sizeof(value), &error), SKRIN_OK);
    STATUS(skrin_write_insert(write, UINT64_MAX, value, sizeof(value), &error), SKRIN_DUPLICATE_KEY);
    STATUS(skrin_db_get(db, UINT64_MAX, out, sizeof(out), &len, &error), SKRIN_NOT_FOUND);
    CHECK(len == 0);
    memset(out, 99, sizeof(out));
    STATUS(skrin_write_get(write, UINT64_MAX, out, 1, &len, &error), SKRIN_BUFFER_TOO_SMALL);
    CHECK(len == sizeof(value) && out[0] == 99);
    STATUS(skrin_write_get(write, UINT64_MAX, out, sizeof(out), &len, &error), SKRIN_OK);
    CHECK(len == sizeof(value) && memcmp(out, value, len) == 0);
    STATUS(skrin_write_put(write, 0, NULL, 0, &error), SKRIN_OK);
    STATUS(skrin_write_put(write, 8, NULL, 1, &error), SKRIN_INVALID_ARGUMENT);
    STATUS(skrin_write_commit(&write, &error), SKRIN_OK);
    CHECK(write == NULL);
    STATUS(skrin_write_abort(&write, &error), SKRIN_OK);
    STATUS(skrin_write_commit(&write, &error), SKRIN_INVALID_ARGUMENT);
    STATUS(skrin_db_get(db, 0, NULL, 0, &len, &error), SKRIN_OK);
    CHECK(len == 0);
    STATUS(skrin_db_get(db, UINT64_MAX, NULL, 0, &len, &error), SKRIN_BUFFER_TOO_SMALL);
    CHECK(len == sizeof(value));
    STATUS(skrin_db_get(db, UINT64_MAX, out, sizeof(out), &len, &error), SKRIN_OK);
    CHECK(memcmp(out, value, len) == 0);
    STATUS(skrin_write_begin(db, &write, &error), SKRIN_OK);
    uint32_t removed = 0;
    STATUS(skrin_write_remove(write, UINT64_MAX, &removed, &error), SKRIN_OK);
    CHECK(removed == 1);
    STATUS(skrin_write_get(write, UINT64_MAX, out, sizeof(out), &len, &error), SKRIN_NOT_FOUND);
    STATUS(skrin_write_abort(&write, &error), SKRIN_OK);
    STATUS(skrin_db_get(db, UINT64_MAX, out, sizeof(out), &len, &error), SKRIN_OK);
    STATUS(skrin_db_close(&db, &error), SKRIN_OK);
    STATUS(skrin_db_close(&db, &error), SKRIN_OK);
}

static void limits_and_owner(const char *root) {
    char path[4096];
    child_path(path, sizeof(path), root, "limits");
    skrin_db *db = NULL, *another = NULL;
    skrin_write *write = NULL;
    skrin_stats before, after;
    STATUS(skrin_db_create((const uint8_t *)path, strlen(path), &db, &error), SKRIN_OK);
    STATUS(skrin_db_create((const uint8_t *)path, strlen(path), &another, &error), SKRIN_IO_ERROR);
    CHECK(another == NULL);
    STATUS(skrin_db_stats(db, &before, &error), SKRIN_OK);
    CHECK(before.persistent == 1 && before.reserved == 0);
    STATUS(skrin_write_begin(db, &write, &error), SKRIN_OK);
    size_t max = skrin_max_value_size();
    uint8_t *value = malloc(max + 1);
    CHECK(value != NULL);
    memset(value, 42, max + 1);
    STATUS(skrin_write_put(write, 1, value, max + 1, &error), SKRIN_LIMIT_EXCEEDED);
    STATUS(skrin_write_put(write, 1, value, max, &error), SKRIN_OK);
    STATUS(skrin_write_put(write, 2, value, max, &error), SKRIN_OK);
    STATUS(skrin_write_commit(&write, &error), SKRIN_LIMIT_EXCEEDED);
    CHECK(write == NULL);
    STATUS(skrin_db_stats(db, &after, &error), SKRIN_OK);
    CHECK(after.rows == before.rows && after.sequence == before.sequence && after.wal_bytes == before.wal_bytes);
    STATUS(skrin_write_begin(db, &write, &error), SKRIN_OK);
    STATUS(skrin_write_put(write, 1, value, max, &error), SKRIN_OK);
    free(value); /* Input bytes have already been copied. */
    STATUS(skrin_db_close(&db, &error), SKRIN_OK);
    STATUS(skrin_db_open((const uint8_t *)path, strlen(path), &another, &error), SKRIN_BUSY);
    STATUS(skrin_write_commit(&write, &error), SKRIN_OK);
    STATUS(skrin_db_open((const uint8_t *)path, strlen(path), &db, &error), SKRIN_OK);
    size_t len = 0;
    STATUS(skrin_db_get(db, 1, NULL, 0, &len, &error), SKRIN_BUFFER_TOO_SMALL);
    CHECK(len == max);
    value = malloc(max);
    CHECK(value != NULL);
    STATUS(skrin_db_get(db, 1, value, max, &len, &error), SKRIN_OK);
    for (size_t i = 0; i < len; ++i) CHECK(value[i] == 42);
    free(value);
    STATUS(skrin_db_close(&db, &error), SKRIN_OK);
}

/* An actual kernel write failure beneath the production WAL. The process is
 * single-threaded; restore its size limit/signal disposition before recovery.
 * This tests error handling, not hardware power-loss behavior. */
static void uncertain_write(const char *root) {
    char path[4096];
    child_path(path, sizeof(path), root, "uncertain");
    skrin_db *db = NULL;
    skrin_write *write = NULL;
    STATUS(skrin_db_create((const uint8_t *)path, strlen(path), &db, &error), SKRIN_OK);
    STATUS(skrin_write_begin(db, &write, &error), SKRIN_OK);
    const uint8_t seed[] = {7, 0, 255};
    STATUS(skrin_write_put(write, 7, seed, sizeof(seed), &error), SKRIN_OK);
    STATUS(skrin_write_commit(&write, &error), SKRIN_OK);
    skrin_stats before;
    STATUS(skrin_db_stats(db, &before, &error), SKRIN_OK);
    struct rlimit old_limit, limit;
    struct sigaction old_action, ignore;
    memset(&ignore, 0, sizeof(ignore));
    ignore.sa_handler = SIG_IGN;
    CHECK(sigemptyset(&ignore.sa_mask) == 0);
    CHECK(getrlimit(RLIMIT_FSIZE, &old_limit) == 0);
    limit = old_limit;
    limit.rlim_cur = (rlim_t)before.wal_bytes + 8;
    CHECK(sigaction(SIGXFSZ, &ignore, &old_action) == 0);
    CHECK(setrlimit(RLIMIT_FSIZE, &limit) == 0);
    uint8_t payload[4096];
    memset(payload, 33, sizeof(payload));
    STATUS(skrin_write_begin(db, &write, &error), SKRIN_OK);
    STATUS(skrin_write_put(write, 7, payload, sizeof(payload), &error), SKRIN_OK);
    STATUS(skrin_write_put(write, 8, payload, sizeof(payload), &error), SKRIN_OK);
    STATUS(skrin_write_commit(&write, &error), SKRIN_COMMIT_UNCERTAIN);
    CHECK(write == NULL);
    size_t len = 1;
    STATUS(skrin_db_get(db, 7, payload, sizeof(payload), &len, &error), SKRIN_POISONED);
    CHECK(len == 0);
    STATUS(skrin_write_begin(db, &write, &error), SKRIN_POISONED);
    CHECK(write == NULL);
    STATUS(skrin_db_checkpoint(db, &error), SKRIN_POISONED);
    CHECK(setrlimit(RLIMIT_FSIZE, &old_limit) == 0);
    CHECK(sigaction(SIGXFSZ, &old_action, NULL) == 0);
    STATUS(skrin_db_close(&db, &error), SKRIN_OK);
    STATUS(skrin_db_open((const uint8_t *)path, strlen(path), &db, &error), SKRIN_OK);
    STATUS(skrin_db_get(db, 7, payload, sizeof(payload), &len, &error), SKRIN_OK);
    CHECK(len == sizeof(seed) && memcmp(payload, seed, len) == 0);
    STATUS(skrin_db_get(db, 8, payload, sizeof(payload), &len, &error), SKRIN_NOT_FOUND);
    skrin_stats after;
    STATUS(skrin_db_stats(db, &after, &error), SKRIN_OK);
    CHECK(after.sequence == before.sequence && after.rows == before.rows);
    CHECK(after.recovered_tail_bytes > 0);
    STATUS(skrin_db_close(&db, &error), SKRIN_OK);
}

int main(int argc, char **argv) {
    CHECK(argc == 2);
    memory_contract();
    limits_and_owner(argv[1]);
    uncertain_write(argv[1]);
    puts("Verified C ABI buffers, staging, limits, ownership and real uncertain-write recovery");
    return EXIT_SUCCESS;
}
