#include "skrin.h"
#include <cstddef>

static_assert(sizeof(skrin_error) == 260, "error ABI");
static_assert(sizeof(skrin_stats) == 40, "stats ABI");
static_assert(offsetof(skrin_stats, persistent) == 32, "stats field ABI");
static_assert(sizeof(skrin_reclaim_report) == 32, "reclaim ABI");

int main() {
    skrin_db *db = nullptr;
    skrin_error error{};
    if (skrin_c_abi_version() != SKRIN_C_ABI_VERSION) return 1;
    if (skrin_db_in_memory(&db, &error) != SKRIN_OK) return 1;
    return skrin_db_close(&db, &error) == SKRIN_OK && db == nullptr ? 0 : 1;
}
