# Varied resident game queries — 2026-10-10

Five fresh processes of the current `game_queries` harness, each with 100,000
entities, 100,000 items and 2,000 requests per workload. All full returned rows
match independent arithmetic. This extends the earlier hot-cursor controls to
varied bounded/filtered/page/join requests; it does not supersede the historical
cursor source or repeat logs.

## Source and machine

Base engine commit `d0e99ebebcab840892d80d3ae05c154434d99162` plus the paired
index codec, tests and replacement query harness accompanying this report.
Apple M5, Mac17,2, 24 GiB RAM, macOS 27.0 (26A428), aarch64 Rust 1.89.0
(29483883e, LLVM 20.1.7), SQLite 3.53.2 through bundled development-only
rusqlite 0.40.2. Cargo bench inherits the workspace release profile: thin LTO,
one codegen unit, no additional target CPU flags. Separate
checkout target directory: `/tmp/skrin-cursor-isolated.pQZ6b8/new`.
No simultaneous Skrin tests/builds during these timed runs. This is a shared
workstation; scheduler/device interference is uncontrolled and every tail is kept.

Commands:

```sh
rustup run 1.89.0 cargo bench --target-dir /tmp/skrin-cursor-isolated.pQZ6b8/new \
  -p skrin --bench game_queries --locked -- 100000 2000
# Repeat in five fresh processes; --sqlite-first in runs 2 and 4.
```

The exact optimized benchmark executable reported by Cargo was invoked directly
for each repeat, avoiding a shared target or wildcard selection of old executables.
The separate 64/67/1,000-row functional controls passed; partial last groups and
empty results are checked. Statement preparation, seed/index construction,
snapshot conversion, 128 warmups and result verification are outside timing.
Every query allocates its complete result vector within timing. See the
[complete access-path methodology](../benchmarks.md#varied-resident-game-queries).
SQLite's recorded plans require covering searches, cursor ID seeks and no
full table scan/temporary sort; statements are reused. Its page uses an ordered
UNION ALL of equal-area and subsequent-area seeks. Its join fetches the owner
by primary key and items by `(owner,kind,id)`. Both Skrin joins cache that same
constant owner once per nonempty query.

## All repeat aggregates

Microseconds. Medians are medians of five per-process statistics; ranges retain
all five processes. The maximum is the worst individual sample in all processes.

| Mode | Workload | p50 median [range] | p99 median [range] | Worst sample |
| --- | --- | ---: | ---: | ---: |
| native | bounded | 9.958 [9.875–10.083] | 13.209 [12.000–15.584] | 30.791 |
| native | filtered | 3.042 [3.000–3.166] | 3.875 [3.792–4.542] | 45.084 |
| native | page | 3.375 [3.375–3.500] | 4.208 [4.125–4.458] | 22.250 |
| native | owner_join | 4.250 [4.167–4.375] | 6.708 [6.500–6.875] | 23.917 |
| native | compound_join | 1.125 [1.083–1.167] | 1.750 [1.666–1.875] | 20.000 |
| snapshot | bounded | 6.958 [6.708–7.041] | 9.041 [8.792–9.292] | 46.000 |
| snapshot | filtered | 2.167 [2.125–2.375] | 2.958 [2.709–3.291] | 19.166 |
| snapshot | page | 2.542 [2.459–2.708] | 3.459 [3.208–3.583] | 18.250 |
| snapshot | owner_join | 3.167 [3.125–3.208] | 4.834 [4.708–5.000] | 26.125 |
| snapshot | compound_join | 1.250 [1.208–1.333] | 2.083 [1.917–2.375] | 11.833 |
| sqlite_covering | bounded | 9.166 [9.042–9.375] | 11.292 [9.958–11.667] | 31.709 |
| sqlite_covering | filtered | 1.833 [1.792–1.875] | 2.167 [2.042–5.208] | 11.625 |
| sqlite_covering | page | 2.292 [2.250–2.334] | 2.667 [2.500–2.959] | 21.208 |
| sqlite_covering | compound_join | 0.708 [0.708–0.750] | 1.125 [1.042–1.209] | 10.584 |

## Conclusions and costs

For the full two-area result, snapshot p50 is **1.32× faster** than SQLite
(6.958 versus 9.166 µs); all five snapshot medians are lower than all five
SQLite medians. Baseline native p50 is slower than SQLite (9.958 µs).

SQLite wins filtered/page/compound-join p50 against both Skrin representations.
The compound key improves Skrin's own join access path: the native
owner-only/compound p50 ratio is **3.78×** (4.250 versus 1.125 µs), snapshot **2.53×**
(3.167 versus 1.250 µs). It remains slower than SQLite's equivalent compound
join (0.708 µs). These are query-access-path comparisons in one three-index
catalog, not whole-engine before/after claims; maintaining an extra index has
write/memory costs that this resident harness does not measure.

Result counts per process are identical in every comparable mode: bounded
255,872; filtered 32,000; page 31,984; joined 4,874. Owner groups hold up to 64
items with sixteen kinds; many joined results contain fewer than the limit.
Cursor/key ordering uses this deterministic dataset. Arbitrary application
relationships, ownership errors and uncorrelated primary/index order have
separate correctness tests and are not performance coverage here.

No durable-write, cold-cache, concurrent-writer, RAM/storage or whole-game
advantage is established. All rows/indexes fit RAM, baseline guards block
writers, snapshots cost resident memory, and persistence remains experimental
and Unix-only. The earlier durable write/storage/memory comparison retains its
reported SQLite advantages. All outliers, including the 45–46 µs Skrin samples,
remain in the aggregates and raw logs.

## Raw evidence and source identity

- `crates/skrin/benches/game_queries.rs` SHA-256: `fdd05b135c2cae2bb3984e2f30505a27882d3884b222d4b8b4f78cdf191ee07a`.
- `crates/skrin/src/typed_index.rs` SHA-256: `7f313e8e1c24804c18ccab558613a8c15274559a589c7ffcb70ad2e761f02a1d`.
- [run-1.txt](varied-queries-2026-10-10/run-1.txt).
- [run-2.txt](varied-queries-2026-10-10/run-2.txt).
- [run-3.txt](varied-queries-2026-10-10/run-3.txt).
- [run-4.txt](varied-queries-2026-10-10/run-4.txt).
- [run-5.txt](varied-queries-2026-10-10/run-5.txt).
