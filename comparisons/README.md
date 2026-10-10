# Complete game application controls

These are runnable external comparisons, not Skrin engine dependencies or a SQL
product API. The common journey uses two character and two item records:

1. Declare name, signed x/y, Float health, Bool alive, optional unsigned team,
   plus item owner/kind and explicit primary IDs.
2. Insert all records in one save.
3. Select visible living characters in `-64 <= x < 64`, `y >= -16`, ordered by
   x then ID, limit eight. Join one owner/kind inventory, ordered by item ID,
   limit four; verify every returned field.
4. Atomically move character 7 to x=128, set health=0/alive=false and move item
   101 from owner 7 to owner 9. The other character/item must stay unchanged.
5. Stage changes to character 9 and item 103, reject that edit and verify the
   complete prior state and index queries.
6. Close the process; a fresh process must verify all full rows, counts, filtered
   selection and joined inventory against the expected final fixture.

The small fixture proves application contracts, not query scaling. Larger varied
and hot-cursor workloads have separate prepared, covering SQLite controls and
complete result verification; see [methodology](../docs/benchmarks.md).

## Skrin and SQLite

```sh
rustup run 1.89.0 cargo bench -p skrin --bench api_journey --locked
```

The [source](../crates/skrin/benches/api_journey.rs) contains both complete
applications, including native schema IDs/indexes and SQL definitions, column
mapping, prepared queries, transaction saves and error handling. It spawns
separate create/verify child processes for each engine and removes only its
new fixture directory after all checks succeed. Persistence requires Unix.
For inspecting retained data, run the optimized executable reported by Cargo:

```sh
/path/to/api_journey create native /existing-parent/NEW-native-directory
/path/to/api_journey verify native /existing-parent/NEW-native-directory
/path/to/api_journey create sqlite /existing-parent/NEW-sqlite-file
/path/to/api_journey verify sqlite /existing-parent/NEW-sqlite-file
```

## SwiftData on macOS

Use Xcode with SwiftData `#Index` support (macOS 15+ SDK/runtime). The recorded
local run uses Xcode 27.0, Swift 6.4 and macOS 27.0. Compile the
[complete standalone application](swiftdata_game.swift), then create and verify
in separate processes:

```sh
swift_run_dir=$(mktemp -d /tmp/skrin-swiftdata-comparison.XXXXXX)
xcrun swiftc -parse-as-library -module-cache-path "$swift_run_dir/cache" \
  comparisons/swiftdata_game.swift -o "$swift_run_dir/game"
"$swift_run_dir/game" create "$swift_run_dir/game.store"
"$swift_run_dir/game" verify "$swift_run_dir/game.store"
```

The script leaves the owned fixture/store/cache directory for inspection. Use a
new path; `create` refuses an existing store. It disables autosave and CloudKit,
uses explicit indexes and saves, and calls `rollback()` after the intentionally
throwing transaction. The indexed predicates, sorting, limits and model/owner
fetches are visible in the source. No unavailable relationship/SwiftUI capability
is treated as a framework defect. The control intentionally uses scalar owner
IDs in all three engines. It does not compare implicit migration, observation,
automatic relationships, hardware sync behavior or end-to-end game performance.

[The report](../docs/measurements/api-journeys-2026-10-10.md) records what ran,
framework versions, every repeat and both advantages and costs. Choose an API
for its actual application contracts; a subjective preference is not a universal
ranking of languages/frameworks.
