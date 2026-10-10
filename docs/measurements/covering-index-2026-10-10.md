# Covering indexes: resident game queries, 2026-10-10

The 10× target is exceeded for **larger resident immutable full-row area queries**
on this machine: approximately 13×/19×/24× versus prepared covering SQLite for
8/32/128 areas. The original two-area control is about 8× and one area about 6×.
This is not a general database, durable-write, cold-cache or whole-game speed claim.
Snapshot writes are slower; that cost is reported below.

## Machine, source and paired controls

Apple M5, 24 GiB RAM, macOS 27.0 (26A428), aarch64-apple-darwin,
Rust 1.89.0 (29483883e), Cargo release profile, bundled SQLite 3.53.2 through
pinned development-only rusqlite 0.40.2. All data is resident and volatile;
filesystem/device and persistence are outside these timings. One driver thread,
100,000 entities plus 100,000 items, complete four-u64 entity/two-u64 item fields,
64-row area/owner groups, 16 kinds. No SQL enters Skrin's engine or public API.

Before engine: `a31622db464a054f2fe49ef5bb1f23764cf2c0c2` from `git archive`.
Only the updated harness/bench declaration was copied into that archived checkout;
all archived engine source files were checked byte-for-byte against the commit.
Both engines used identical game-query, cursor, update and world harness sources.
After source hashes are in [after-source-sha256.json](covering-index-2026-10-10/after-source-sha256.json);
compiled executable hashes/paths are in [executables.json](covering-index-2026-10-10/raw/executables.json).
Before and after used different target directories. The explicit selected
executables came from Cargo's reported benchmark paths, excluding test harnesses.

Five fresh-process paired repetitions for **every** area span and workload.
Odd repetitions run before then after and Skrin before SQLite; even repetitions
reverse both engine order and span order and use SQLite-first. No own build/tests
ran during the final measurement cohort. Other workstation builds/processes,
thermal state, scheduler and caches were uncontrolled; all observations and tails
are retained, not a claim of laboratory isolation. 128 warmups and 2,000 samples
per varied query, 128 per hot cursor/update case. Per-query vector allocation,
full native field materialization and iterator/adapter work are inside timing;
setup, index building, snapshot conversion, prepared statement compilation and
independent result checks are outside. SQLite statements are reused with covering
indexes, checked seek plans, an ordered UNION ALL page seek without temporary
sorting, and the complete owner's primary-row seek for joins. Every full result
vector is checked against independent arithmetic. No count-only projection or
additional SQLite preparation cost is charged to the control.

The [raw directory](covering-index-2026-10-10/raw) contains all 50 query logs,
50 whole-process resource logs, ten cursor logs, ten update logs and binary
metadata. Query row totals match all modes/repetitions. The
[exploration directory](covering-index-2026-10-10/exploration) retains early probes,
32/64/128-leaf experiments, profiling output and an earlier complete paired cohort
before the final routing-update batching. Exploratory runs may overlap builds;
none is selected for the headline. The 128-entry leaf experiment was rejected
for worse read timing and larger copies. The current final tables use only `raw`.

## Full area results

Microseconds: median of five process p50s, with their full minimum–maximum range.
The ratio divides SQLite and snapshot medians; it is not a pooled-sample percentile.
Returned totals are full entities across 2,000 requests; partial trailing intervals
are included. Wider controls retain the original two-area test.

| Areas | Returned rows | Native before | Native after | Snapshot before | Snapshot after | SQLite after | SQLite/snapshot |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 127,968 | 5.750 [5.667–5.958] | 3.333 [3.292–3.375] | 4.167 [4.125–4.375] | 0.875 [0.792–0.917] | 5.000 [4.833–5.042] | 5.71× |
| 2 | 255,872 | 9.959 [9.875–10.334] | 5.584 [5.583–5.958] | 6.875 [6.833–7.458] | 1.166 [1.125–1.292] | 9.375 [9.291–9.500] | 8.04× |
| 8 | 1,021,440 | 34.750 [34.666–35.959] | 19.334 [18.792–19.625] | 21.459 [21.208–21.958] | 2.625 [2.542–2.708] | 35.042 [34.541–35.417] | 13.35× |
| 32 | 4,052,992 | 130.167 [129.458–133.125] | 69.958 [69.875–71.875] | 77.083 [76.541–79.334] | 7.000 [6.875–7.250] | 137.375 [134.208–139.666] | 19.62× |
| 128 | 15,699,104 | 514.208 [499.250–518.958] | 285.667 [276.458–289.500] | 294.584 [292.542–299.583] | 23.375 [22.250–24.333] | 560.167 [551.834–563.500] | 23.96× |

All varied workloads: median p50 / median p99 / largest individual sample across
all five runs, in microseconds. Owner-only joins are a diagnostic weaker access
path with the same full result; SQLite uses the stronger compound index.

| Areas | Workload | Before snapshot p50 | After native p50/p99/max | After snapshot p50/p99/max | After SQLite p50/p99/max |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 | bounded | 4.167 | 3.333 / 4.625 / 22.709 | 0.875 / 1.375 / 14.042 | 5.000 / 6.500 / 57.459 |
| 1 | filtered | 2.416 | 1.791 / 2.375 / 19.750 | 0.417 / 0.625 / 3.666 | 1.875 / 2.458 / 68.500 |
| 1 | page | 2.334 | 1.791 / 2.500 / 18.792 | 0.500 / 0.792 / 17.083 | 2.125 / 2.500 / 13.916 |
| 1 | owner_join | 3.208 | 2.292 / 3.583 / 25.000 | 0.750 / 1.250 / 6.500 | uses compound_join |
| 1 | compound_join | 1.292 | 0.791 / 1.167 / 18.917 | 0.667 / 1.208 / 8.000 | 0.750 / 1.250 / 19.916 |
| 2 | bounded | 6.875 | 5.584 / 7.375 / 37.375 | 1.166 / 1.750 / 36.333 | 9.375 / 11.542 / 42.041 |
| 2 | filtered | 2.292 | 1.791 / 2.292 / 18.542 | 0.417 / 0.584 / 5.834 | 1.833 / 2.208 / 20.167 |
| 2 | page | 2.542 | 2.000 / 2.584 / 32.583 | 0.500 / 0.750 / 7.792 | 2.334 / 2.917 / 21.875 |
| 2 | owner_join | 3.292 | 2.333 / 3.625 / 67.292 | 0.791 / 1.250 / 20.792 | uses compound_join |
| 2 | compound_join | 1.250 | 0.750 / 1.166 / 20.709 | 0.667 / 1.167 / 8.625 | 0.750 / 1.209 / 2.375 |
| 8 | bounded | 21.459 | 19.334 / 23.541 / 74.292 | 2.625 / 3.375 / 22.459 | 35.042 / 41.833 / 129.916 |
| 8 | filtered | 2.291 | 1.750 / 2.292 / 18.459 | 0.416 / 0.625 / 9.292 | 1.875 / 2.292 / 20.833 |
| 8 | page | 2.541 | 2.000 / 2.625 / 30.667 | 0.500 / 0.667 / 14.959 | 2.375 / 2.750 / 16.375 |
| 8 | owner_join | 3.125 | 2.291 / 3.625 / 34.583 | 0.750 / 1.208 / 15.084 | uses compound_join |
| 8 | compound_join | 1.209 | 0.792 / 1.208 / 27.333 | 0.667 / 1.083 / 8.417 | 0.750 / 1.250 / 11.959 |
| 32 | bounded | 77.083 | 69.958 / 84.083 / 416.541 | 7.000 / 9.250 / 51.291 | 137.375 / 154.459 / 1157.875 |
| 32 | filtered | 2.292 | 1.792 / 2.375 / 18.292 | 0.375 / 0.625 / 4.333 | 1.834 / 2.375 / 19.333 |
| 32 | page | 2.500 | 2.041 / 2.583 / 19.459 | 0.459 / 0.667 / 13.209 | 2.375 / 3.000 / 31.209 |
| 32 | owner_join | 3.208 | 2.292 / 3.500 / 17.833 | 0.709 / 1.125 / 4.542 | uses compound_join |
| 32 | compound_join | 1.250 | 0.750 / 1.125 / 13.417 | 0.708 / 1.208 / 9.458 | 0.750 / 1.292 / 78.708 |
| 128 | bounded | 294.584 | 285.667 / 350.417 / 910.959 | 23.375 / 35.208 / 110.041 | 560.167 / 679.834 / 1368.875 |
| 128 | filtered | 2.417 | 1.917 / 2.709 / 10.375 | 0.500 / 0.834 / 29.833 | 1.959 / 2.500 / 19.583 |
| 128 | page | 2.667 | 2.125 / 2.709 / 25.000 | 0.625 / 0.959 / 6.167 | 2.541 / 5.083 / 32.750 |
| 128 | owner_join | 3.333 | 2.458 / 3.750 / 21.208 | 0.833 / 1.542 / 7.166 | uses compound_join |
| 128 | compound_join | 1.167 | 0.792 / 1.208 / 6.875 | 0.709 / 1.541 / 7.458 | 0.750 / 1.292 / 14.500 |

The narrow compound join does not establish a 10× win; some repetitions favor
SQLite, and sub-microsecond comparisons include timer/allocator overhead and
scheduler noise. Individual Skrin maxima can exceed SQLite's. The read gains
come from covering immutable references, contiguous leaf traversal and removing
per-hit row-tree lookups. Native borrowed indexes now carry physical slots,
removing one map lookup per hit; they remain lock-based and do not reach the same
larger-query ratios. No persistence boundary was changed.

## Hot equal-key cursors

100,000 three-field entries in one group, 16 even-valued full rows after a fixed
front/middle/late position. These hot repeated cursors differ from varied areas.
All exact vectors match independent arithmetic; SQLite uses a prepared covering
ID seek. Prefix scanning remains a diagnostic, not the recommended page API.
Cells are five-run median p50 [minimum–maximum], microseconds.

| Depth | Mode | Before | After | After p99 / worst sample |
| --- | --- | ---: | ---: | ---: |
| front | native_prefix | 0.917 [0.875–0.958] | 0.667 [0.667–0.708] | 1.583 / 2.209 |
| front | native_seek | 0.875 [0.875–0.917] | 0.667 [0.667–0.708] | 0.875 / 8.834 |
| front | snapshot_prefix | 0.709 [0.708–0.750] | 0.250 [0.208–0.250] | 1.084 / 5.167 |
| front | snapshot_seek | 0.667 [0.667–0.708] | 0.250 [0.250–0.250] | 0.334 / 5.875 |
| front | sqlite_covering | 1.375 [1.292–1.375] | 1.334 [1.292–1.375] | 1.750 / 4.792 |
| middle | native_prefix | 2899.584 [2835.792–3042.750] | 1452.167 [1402.541–1570.208] | 1692.209 / 3126.708 |
| middle | native_seek | 0.875 [0.875–0.917] | 0.583 [0.542–0.583] | 0.584 / 0.667 |
| middle | snapshot_prefix | 1719.250 [1661.625–1779.958] | 46.833 [45.583–47.208] | 64.000 / 106.833 |
| middle | snapshot_seek | 0.667 [0.667–0.708] | 0.250 [0.250–0.250] | 0.292 / 0.375 |
| middle | sqlite_covering | 1.375 [1.333–1.416] | 1.375 [1.292–1.417] | 1.500 / 1.750 |
| late | native_prefix | 5807.000 [5654.458–6031.583] | 2919.166 [2822.833–2947.416] | 3526.541 / 6073.917 |
| late | native_seek | 0.917 [0.875–0.958] | 0.416 [0.375–0.417] | 0.417 / 0.500 |
| late | snapshot_prefix | 3386.875 [3282.958–3718.000] | 92.708 [90.125–95.250] | 119.458 / 310.125 |
| late | snapshot_seek | 0.625 [0.584–0.625] | 0.250 [0.250–0.250] | 0.292 / 0.292 |
| late | sqlite_covering | 1.334 [1.292–1.458] | 1.334 [1.333–1.375] | 1.500 / 1.709 |

## Write and memory costs

The update control applies complete atomic in-memory saves: 1/16/64 entity edits,
one item owner change and one saved operation. It includes transaction staging
and publication, retains the original full frame, checks every old/current row
and index, retries a saved operation and verifies sequence/idempotency. It uses
the existing production save path with independent field arithmetic. No SQLite
write or durable-write comparison is inferred. Covering references must advance
on **all** indexes, including unchanged keys, to prevent stale current rows.

| Entities/save | Mode | Before p50 [range] | After p50 [range] | p50 change | Before p99/worst | After p99/worst |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 1 | native | 1.042 [1.042–1.083] | 1.042 [1.041–1.042] | +0.0% | 2.250 / 19.750 | 2.042 / 19.333 |
| 1 | snapshot | 3.041 [2.958–3.125] | 4.041 [3.958–4.042] | +32.9% | 6.417 / 25.917 | 9.875 / 59.167 |
| 16 | native | 5.833 [5.792–6.208] | 5.791 [5.708–5.875] | -0.7% | 7.166 / 11.042 | 7.292 / 23.250 |
| 16 | snapshot | 8.542 [8.417–8.750] | 11.125 [11.000–11.458] | +30.2% | 15.291 / 28.792 | 19.958 / 45.292 |
| 64 | native | 21.583 [21.208–22.083] | 21.542 [21.416–21.750] | -0.2% | 27.000 / 71.250 | 26.375 / 31.250 |
| 64 | snapshot | 26.417 [26.125–27.000] | 33.583 [33.125–34.875] | +27.1% | 46.750 / 92.042 | 49.875 / 73.584 |

Snapshot conversion and conservative accounted current-root bytes, including
primary rows, routing nodes, covering leaf capacity/key buffers and row references.
Shared row heap is counted once within each coherent root, and whole roots remain
conservatively counted again across retained leases. These are not allocator/RSS
measurements; one retained old frame is verified, not a bound on all retention.

| Entities/save | Before conversion ms [range] | After conversion ms [range] | Before initial/current bytes | After initial/current bytes |
| --- | ---: | ---: | ---: | ---: |
| 1 | 20.992 [20.752–22.017] | 25.308 [24.675–26.728] | 54,400,000 / 54,419,456 | 45,325,000 / 45,344,456 |
| 16 | 19.945 [19.516–20.917] | 24.125 [23.880–25.599] | 54,400,000 / 54,419,456 | 45,325,000 / 45,344,456 |
| 64 | 18.998 [18.138–19.501] | 23.044 [22.123–23.619] | 54,400,000 / 54,419,456 | 45,325,000 / 45,344,456 |

Whole query process peak RSS from macOS `time -l`, bytes, median [range] of five
runs. Each process constructs native, immutable and SQLite data in sequence;
allocator retention and sample buffers are included. This is **not** isolated
Skrin-versus-SQLite RAM, nor the same quantity as accounted snapshot bytes.

| Areas | Before whole-process peak RSS | After whole-process peak RSS |
| --- | ---: | ---: |
| 1 | 128,188,416 [128,172,032–132,988,928] | 120,487,936 [120,422,400–125,239,296] |
| 2 | 128,303,104 [128,204,800–133,070,848] | 120,520,704 [120,422,400–125,239,296] |
| 8 | 128,548,864 [128,483,328–133,300,224] | 120,684,544 [120,668,160–125,485,056] |
| 32 | 128,974,848 [128,663,552–133,611,520] | 121,208,832 [120,881,152–125,878,272] |
| 128 | 129,138,688 [128,843,776–134,201,344] | 121,585,664 [121,372,672–126,468,096] |

## Reproduction and safety

Read [the methodology](../benchmarks.md) and use the checked-in
[`compare-query-engines.py`](../../scripts/compare-query-engines.py).
Archive the before commit into a new checkout, copy the current `game_queries.rs`,
`index_updates.rs` and bench manifest into it, and verify that all other engine
files still match that commit. Compile the identical harnesses with separate
`CARGO_TARGET_DIR`s using Rust 1.89.0 and `cargo bench -p skrin --locked --no-run
--bench game_queries --bench cursor_queries --bench index_updates`.
For each target create a separate selection directory containing symlinks only
to the three benchmark executables **reported by Cargo**. The driver intentionally
refuses directories with ambiguous executable matches and refuses an existing
output directory. Then run:

```sh
python3 scripts/compare-query-engines.py /tmp/before-selected /tmp/after-selected /tmp/new-measurements
```

Production changes are private native indexes: mutable `(logical ID, slot)`
postings; immutable at-most-64-entry sorted covering leaves behind a path-copy
routing tree; one copy per touched leaf per transaction with grouped unchanged-key
routing replacements; a safe inline traversal stack with dynamic overflow.
Empty leaves disappear and overflowing leaves split evenly. Sparse neighboring
leaves are not automatically merged; adversarial churn/retained frames can still
cost memory. No full database copy per commit, SQL API, new engine dependency,
unsafe code, disk-format/codec/fixture transition or weakened sync semantics.
Candidate rows/indexes/footprints still build before append/sync/publication;
failed I/O still poisons the handle. Old frames retain their exact row versions.

Verification includes independent randomized leaf/map ranges and retained roots,
copy bounds/untouched-leaf sharing, split boundaries, overflow/refusal, full-u64
cursors, mutable delete/reinsert slot identity and actual WAL recovery. A real
catalog regression checks pointer identity between covered rows and the primary
root across unchanged/changed index keys, delete/reinsert, old/current frames and
reopen. Existing corruption, schema, crash, uncertain-sync, maintenance and
storage-fault checks stay in place. Local and exact-head CI status are delivery
evidence separately from these macOS timings. No new physical power-loss,
concurrent writer latency, durable performance, cold cache, larger-than-RAM,
SwiftData speed or whole-game acceptance claim is established.

## Local verification of the measured engine

The final engine passed Rust 1.89.0 formatting, all-target Clippy with
`-D warnings`, documentation with `-D warnings`, full workspace debug and release
tests, MSRV/stable all-target checks with default/no-default features, and
no-default-feature documentation tests. Each full suite has 194 top-level
unit/integration tests plus 11 doc tests; subprocess helpers print additional
results in the logs. No failed/ignored top-level tests. Local Linux-only actual
ENOSPC/kernel OOM tests cannot run on this macOS host; the existing CI performs
them in both profiles without weakening or skipping those steps.

The accounts, snapshots, game-world, signed-model and game-query examples ran;
accounts/models/queries also verified persistent recovery. The native/SQLite
`api_journey` ran complete durable saves, error rollback and fresh-process reopen.
The snapshot-conversion benchmark ran against current code. All three performance
benchmarks ran above with independent full-state verification.
[Local check logs](covering-index-2026-10-10/local-checks) retain these outcomes.
CI timings and this macOS performance cohort are distinct evidence.
