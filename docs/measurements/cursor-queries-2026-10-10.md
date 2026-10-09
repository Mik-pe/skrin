# Resident cursor pages, 2026-10-10

`query_after(index, bounds, (&key, primary_key))` removes application prefix
filtering and seeks directly into the native equal-key posting or snapshot
posting tree. This report compares identical filtered 16-row pages with the old
prefix traversal and a strong external SQLite covering-index seek.

## Source and environment

- Source: parent `b998e64e5b2495f7bc51eaaf6431f37ed1b7cd46` plus this cursor change;
  [`cursor_queries.rs`](../../crates/skrin/benches/cursor_queries.rs).
- Apple M5, aarch64 macOS 27.0 build 26A428, Rust 1.89.0
  `29483883eed69d5fb4db01964cdf2af4d86e9cb2`, release thin LTO/one codegen unit.
- rusqlite 0.40.2, bundled SQLite 3.53.2. Existing development-only dependency;
  no SQLite interface or SQL syntax is added to Skrin.
- Five sequential fresh processes, 100,000 rows in one equal-key group, 128
  timed samples per position/mode, eight warmups per position. SQLite runs first
  in rounds 2/4 and last in 1/3/5. Skrin's prefix mode precedes its seek mode.
- Both databases are in memory with no concurrent writer. Each result owns the
  same 16 `(id, group, value)` tuples after filtering even values; all samples
  compare exact rows against independent arithmetic outside timing. Seed/build,
  snapshot conversion, preparation, plan inspection and verification are untimed.

SQLite reuses one prepared statement with a covering `(group_id, id, value)`
index. Every run records `SEARCH entries USING COVERING INDEX entries_group
(group_id=? AND id>?)`; it does not repeatedly prepare, scan a prefix or sort a
full result. The original bounds for Skrin are `0..=0`; SQLite's `group_id=0`
is equivalent. See [methodology](../benchmarks.md#resident-cursor-pages).

## Results

These are medians of the five process-level p50/p99 values, in microseconds.
Ranges show all five process p50s, including adverse repeats.

| Mode | Position | p50 median | p50 range | p99 median |
| --- | --- | ---: | ---: | ---: |
| Native prefix filter | Front | 0.917 | 0.875–2.542 | 1.875 |
| Native prefix filter | Middle | 2880.334 | 2857.958–2969.167 | 3248.459 |
| Native prefix filter | Late | 5783.875 | 5658.375–6055.791 | 8712.333 |
| Native direct seek | Front | 0.875 | 0.875–0.917 | 0.959 |
| Native direct seek | Middle | 0.875 | 0.875–0.875 | 0.959 |
| Native direct seek | Late | 0.917 | 0.916–0.917 | 1.125 |
| Snapshot prefix filter | Front | 0.750 | 0.708–0.750 | 1.792 |
| Snapshot prefix filter | Middle | 1688.291 | 1681.417–1751.959 | 1833.834 |
| Snapshot prefix filter | Late | 3508.208 | 3342.208–3757.125 | 4309.000 |
| Snapshot direct seek | Front | 0.834 | 0.667–1.500 | 4.500 |
| Snapshot direct seek | Middle | 0.709 | 0.666–1.541 | 4.125 |
| Snapshot direct seek | Late | 1.375 | 0.666–1.458 | 1.500 |
| SQLite covering seek | Front | 1.375 | 1.333–1.500 | 2.042 |
| SQLite covering seek | Middle | 1.416 | 1.334–1.458 | 2.333 |
| SQLite covering seek | Late | 1.375 | 1.375–1.458 | 1.542 |

On this repeated hot-page workload, native direct-seek p50 is 1.50–1.62×
faster than the covering SQLite control across positions. Snapshot p50 is
1.65×/2.00× faster at front/middle and equal at late positions; adverse snapshot
repeats overlap or exceed SQLite timings. This does not establish a uniform
snapshot advantage. The prefix controls demonstrate why continuation must seek
inside a large duplicate group: middle/late traversal grows with preceding rows.
The new operation returns the same exact results without that prefix work.

## Existing-query regression control

The unchanged `game_queries` workload selects two varied indexed areas, filters
even x positions and returns 16 full entities. Five old/new process pairs at
100,000 rows/2,000 samples alternate execution order. The old primary checkout
is exactly `b998e64`; the new worktree includes this cursor implementation.

| Existing mode | Old p50 median/range (µs) | New p50 median/range (µs) |
| --- | ---: | ---: |
| Native area query | 3.500 / 3.375–3.666 | 3.458 / 3.375–3.708 |
| Snapshot area query | 2.792 / 2.667–2.834 | 2.750 / 2.667–3.750 |

Median differences are small and do not establish an area-query speedup.
One new snapshot repeat reaches 3.750 µs versus the old maximum 2.834 µs;
that adverse repeat is retained. There is no consistent median regression in
these short controls, but they do not prove absence of a tail/regression effect.
All old/new results were independently verified too.

Both checkouts and benchmarks were rebuilt into separate empty target
folders. An earlier shared-target attempt reused an old library artifact and
failed compilation of the new query example in release; its area controls were
unconfirmed and have been replaced in full. Only the isolated builds/runs above
are reported, including all adverse repeats. The release verification also uses
the new isolated target directory.

## Reproduce and inspect

```sh
cargo +1.89.0 bench -p skrin --bench cursor_queries --locked -- 100000 128
cargo +1.89.0 bench -p skrin --bench cursor_queries --locked -- 100000 128 --sqlite-first
```

Raw full comparison runs: [1](cursor-queries-2026-10-10-run1.txt),
[2](cursor-queries-2026-10-10-run2.txt), [3](cursor-queries-2026-10-10-run3.txt),
[4](cursor-queries-2026-10-10-run4.txt), [5](cursor-queries-2026-10-10-run5.txt).
Area controls are `cursor-queries-2026-10-10-{old,new}-area{1..5}.txt` beside
this report. No run was dropped.

The same cursor repeats after warmup. Sub-microsecond timings include timer
overhead/quantization; the 128-sample p99s are modest tail evidence. This is not
a random game-frame, durability, concurrency, cold-cache, memory/storage cost
or whole-application comparison. SQLite batch-write/RAM/storage advantages in
the existing broader reports remain. Varied bounded/filtered/joined queries and
the complete SQL/SwiftData application ergonomics comparison are still open.
