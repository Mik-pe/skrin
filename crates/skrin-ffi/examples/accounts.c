#include "skrin.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Application codec v1: tag u8, name length u32 LE, UTF-8 bytes, balance u64 LE.
 * No C struct layout or native-width integer is stored. */
static size_t encode_account(uint8_t *out, const uint8_t *name,
                             uint32_t name_len, uint64_t balance) {
    out[0] = 1;
    for (unsigned i = 0; i < 4; ++i) out[1 + i] = (uint8_t)(name_len >> (8 * i));
    memcpy(out + 5, name, name_len);
    for (unsigned i = 0; i < 8; ++i) out[5 + name_len + i] = (uint8_t)(balance >> (8 * i));
    return 13u + name_len;
}

static void check(skrin_status status, const skrin_error *error) {
    if (status != SKRIN_OK) {
        fprintf(stderr, "Skrin status %d: %s\n", status, error->message);
        /* COMMIT_UNCERTAIN/MAINTENANCE_UNCERTAIN require close, reopen and
         * reconciliation. Exiting here reports failure without an unsafe retry. */
        exit(EXIT_FAILURE);
    }
}

static const uint8_t alice[] = {'A', 'l', 'i', 'c', 'e', 0, 0xc3, 0xa5};
static const uint8_t bob[] = {'B', 'o', 'b'};

static void verify(skrin_db *db) {
    uint8_t expected[64], actual[64];
    size_t actual_len = 0;
    skrin_error error;
    size_t len = encode_account(expected, alice, sizeof(alice), 90);
    check(skrin_db_get(db, 42, actual, sizeof(actual), &actual_len, &error), &error);
    if (actual_len != len || memcmp(actual, expected, len) != 0) exit(EXIT_FAILURE);
    len = encode_account(expected, bob, sizeof(bob), 110);
    check(skrin_db_get(db, 7, actual, sizeof(actual), &actual_len, &error), &error);
    if (actual_len != len || memcmp(actual, expected, len) != 0) exit(EXIT_FAILURE);
    skrin_stats stats;
    check(skrin_db_stats(db, &stats, &error), &error);
    if (stats.rows != 2 || stats.sequence != 2 || stats.reserved != 0) exit(EXIT_FAILURE);
}

int main(int argc, char **argv) {
    if (argc != 1 && argc != 3) {
        fprintf(stderr, "usage: %s [NEW_DATABASE_DIRECTORY NEW_BACKUP_DIRECTORY]\n", argv[0]);
        return EXIT_FAILURE;
    }
    if (skrin_c_abi_version() != SKRIN_C_ABI_VERSION) return EXIT_FAILURE;
    skrin_error error;
    skrin_db *db = NULL;
    if (argc == 3) {
        check(skrin_db_create((const uint8_t *)argv[1], strlen(argv[1]), &db, &error), &error);
    } else {
        check(skrin_db_in_memory(&db, &error), &error);
    }
    skrin_write *write = NULL;
    uint8_t value[64], readback[64];
    size_t len = 0, readback_len = 0;
    check(skrin_write_begin(db, &write, &error), &error);
    len = encode_account(value, alice, sizeof(alice), 100);
    check(skrin_write_insert(write, 42, value, len, &error), &error);
    len = encode_account(value, bob, sizeof(bob), 100);
    check(skrin_write_insert(write, 7, value, len, &error), &error);
    check(skrin_write_commit(&write, &error), &error);

    /* Read your writes and atomically save both sides of a transfer. */
    check(skrin_write_begin(db, &write, &error), &error);
    len = encode_account(value, alice, sizeof(alice), 90);
    check(skrin_write_put(write, 42, value, len, &error), &error);
    check(skrin_write_get(write, 42, readback, sizeof(readback), &readback_len, &error), &error);
    if (readback_len != len || memcmp(value, readback, len) != 0) return EXIT_FAILURE;
    len = encode_account(value, bob, sizeof(bob), 110);
    check(skrin_write_put(write, 7, value, len, &error), &error);
    check(skrin_write_commit(&write, &error), &error);
    verify(db);
    check(skrin_db_close(&db, &error), &error);

    if (argc == 3) {
        check(skrin_db_open((const uint8_t *)argv[1], strlen(argv[1]), &db, &error), &error);
        verify(db); /* WAL recovery before checkpoint. */
        check(skrin_db_checkpoint(db, &error), &error);
        check(skrin_db_backup(db, (const uint8_t *)argv[2], strlen(argv[2]), &error), &error);
        skrin_reclaim_report report;
        check(skrin_db_reclaim(db, &report, &error), &error);
        check(skrin_db_close(&db, &error), &error);
        for (int i = 1; i <= 2; ++i) {
            check(skrin_db_open((const uint8_t *)argv[i], strlen(argv[i]), &db, &error), &error);
            verify(db);
            check(skrin_db_close(&db, &error), &error);
        }
        puts("Verified C transactions, exact bytes, WAL, checkpoint and backup recovery");
    } else {
        puts("Verified C transactions and exact bytes in memory");
    }
    return EXIT_SUCCESS;
}
