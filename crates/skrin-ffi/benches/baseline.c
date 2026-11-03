#define _POSIX_C_SOURCE 200809L
#include "skrin.h"
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

enum { ITERATIONS = 1000, KEYS = 64 };
static skrin_error error;
static void check(skrin_status status) {
    if (status != SKRIN_OK) {
        fprintf(stderr, "C benchmark status %d: %s\n", status, error.message);
        exit(EXIT_FAILURE);
    }
}
static uint64_t nanos(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) exit(EXIT_FAILURE);
    return (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
}
static void verify(skrin_db *db) {
    skrin_stats stats;
    check(skrin_db_stats(db, &stats, &error));
    if (stats.rows != KEYS || stats.sequence != ITERATIONS) exit(EXIT_FAILURE);
    for (uint64_t key = 0; key < KEYS; ++key) {
        uint8_t out[8];
        size_t len = 0;
        check(skrin_db_get(db, key, out, sizeof(out), &len, &error));
        if (len != 8) exit(EXIT_FAILURE);
        uint64_t actual = 0;
        for (unsigned i = 0; i < 8; ++i) actual |= (uint64_t)out[i] << (8 * i);
        uint64_t expected = ITERATIONS - 1 - ((ITERATIONS - 1 - key) % KEYS);
        if (actual != expected) exit(EXIT_FAILURE);
    }
}
int main(int argc, char **argv) {
    if (argc != 1 && argc != 2) return EXIT_FAILURE;
    skrin_db *db = NULL;
    if (argc == 2) check(skrin_db_create((const uint8_t *)argv[1], strlen(argv[1]), &db, &error));
    else check(skrin_db_in_memory(&db, &error));
    uint64_t start = nanos();
    for (uint64_t index = 0; index < ITERATIONS; ++index) {
        skrin_write *write = NULL;
        uint8_t value[8];
        for (unsigned i = 0; i < 8; ++i) value[i] = (uint8_t)(index >> (8 * i));
        check(skrin_write_begin(db, &write, &error));
        check(skrin_write_put(write, index % KEYS, value, sizeof(value), &error));
        check(skrin_write_commit(&write, &error));
    }
    uint64_t elapsed = nanos() - start;
    verify(db); /* Exact complete values and sequence checked outside timing. */
    check(skrin_db_close(&db, &error));
    if (argc == 2) {
        check(skrin_db_open((const uint8_t *)argv[1], strlen(argv[1]), &db, &error));
        verify(db);
        check(skrin_db_close(&db, &error));
    }
    printf("C ABI %s: %d begin/put/commit operations, %.1f ns/op; all %d rows verified%s\n",
           argc == 2 ? "synced" : "volatile", ITERATIONS,
           (double)elapsed / ITERATIONS, KEYS, argc == 2 ? " after reopen" : "");
    return EXIT_SUCCESS;
}
