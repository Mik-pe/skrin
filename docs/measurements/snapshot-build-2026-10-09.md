# Direct immutable-root construction: resident conversion

Initial snapshot conversion previously inserted sorted resident rows/postings
one at a time into immutable AVL trees, copying paths and dropping temporary
roots repeatedly. The new private builder consumes the existing ordered sources
in-order and allocates each final node once. It uses logarithmic recursion space
and no full-size temporary vector. Ordinary writes still copy changed paths;
native Arc mapping and catalog slot lookups remain part of conversion.

## Method and evidence

Baseline engine: `71a781b8dc97c8f7ab1cd9266ea47a780f251b87`.
Candidate engine: `3bf8fc0e7d1b8c5665f43c38a4f5dfc720f5381e`.
The identical `snapshot_build` harness was built against an isolated archive of
baseline main and the candidate. Both used Rust 1.89.0, optimized bench profile,
thin LTO and one codegen unit. Candidate package artifacts were explicitly
cleaned before rebuilding to prevent shared-target reuse across source trees.
Both binary hashes, harness/source hashes and commands are retained in
[metadata](snapshot-build-2026-10-09/metadata.json), alongside the verbose
[baseline](snapshot-build-2026-10-09/before-build.txt) and
[candidate](snapshot-build-2026-10-09/after-build.txt) build logs.

Local Apple M5 host, model Mac17,2, 24 GiB RAM, macOS 27.0 (26A428), arm64.
Five process pairs at each of 1k/10k/100k rows alternate before/after,
after/before, before/after, after/before, before/after. Each process measures
both a single table and a game-world catalog with equal entity/item counts and
area/owner postings. All 30 raw process outputs are retained in the evidence
directory; no samples were discarded. No Skrin compilation or validation ran
during the campaign. Unrelated host/runner activity, caches, clocks and scheduling
were uncontrolled.

This is explicitly in-memory conversion of already resident data. Timing
includes `into_snapshots`, native Arc mapping and footprint assessment; it
excludes seeding and verification. There is no filesystem/device or sync work
in this timed operation. Every run verifies initial rows, scans and all catalog
indexes, old-root retention, subsequent changes and catalog operation-ID retry.
The separate production tests and durable workload cover persistence.

Regenerate [all parsed samples and summary](snapshot-build-2026-10-09/summary.json):

```sh
python3 docs/measurements/snapshot-build-2026-10-09/summarize.py
```

Reproduce with separately saved before/after binaries of the identical harness:

```sh
rustup run 1.89.0 cargo bench -p skrin --bench snapshot_build --locked --no-run
/path/to/before-or-after 1000
/path/to/before-or-after 10000
/path/to/before-or-after 100000
```

## Results

Times are medians of five separate processes. Parentheses give the full
minimum–maximum range. Catalog row counts below are **per table**: 100k means
100k entities + 100k items + 200k postings.

| Mode | Rows | Before ms (range) | After ms (range) | Median ratio |
| --- | ---: | ---: | ---: | ---: |
| Single | 1,000 | 0.203 (0.199–0.226) | 0.041 (0.038–0.061) | 4.97× |
| Single | 10,000 | 2.680 (2.666–3.037) | 0.323 (0.314–0.326) | 8.31× |
| Single | 100,000 | 34.464 (33.800–35.775) | 3.015 (2.946–3.167) | 11.43× |
| Catalog | 1,000 | 1.030 (1.009–1.096) | 0.228 (0.207–0.279) | 4.51× |
| Catalog | 10,000 | 13.157 (12.913–13.866) | 2.063 (1.965–2.204) | 6.38× |
| Catalog | 100,000 | 166.194 (165.027–168.747) | 21.590 (21.053–22.072) | 7.70× |

Every candidate conversion was faster than its paired baseline in this local
campaign. Current-root cooperative accounting is identical: 104 bytes per
single-table row and 544 bytes per entity/item pair in this schema. At 100k,
that is 10,400,000 and 54,400,000 bytes respectively. These values are **not**
allocator/RSS measurements; no resident-memory reduction is established here.

## Correctness and limits

Rust 1.89 local format, all-target Clippy (`-D warnings`), debug and release
workspace tests (145 each), docs (`-D warnings`) and all-target checks passed.
Stable all-target checks passed. Accounts/snapshot/game-world memory examples
and the complete persistent accounts, maintenance, banking, group-commit,
snapshot, game-world and lifecycle examples passed on macOS. The durable
`game_world` benchmark passed native/snapshot/group-snapshot modes with 67 rows,
73 saves, batch 13, window 8, including exact fresh-process WAL recovery and
post-checkpoint recovery. These smoke timings are not performance evidence.

New tree tests compare ordered ranges, weights, balance and retained roots with
independent maps across construction sizes and subsequent mutations; non-Clone
values and partial-build assessment failures release all constructed values.
Production-path single-table/catalog regressions check failed initial footprint
assessment leaves WAL bytes and sync count unchanged, then reopen/verify rows
and indexes. Existing format fixtures, append/sync failure tests, publication
and survival projections remain in place. Linux actual ENOSPC/OOM tests require
their CI environment and were not run locally on macOS.

Tree shape changes, so this campaign establishes a resident-conversion benefit,
not a general read/write latency or durable startup claim. Five samples are not
a tail-latency guarantee. Codecs, public APIs, persisted formats and sync-before-
publication remain unchanged. Storage is still Unix-only; rows/indexes remain
resident, snapshot accounting remains cooperative, and maintenance is explicit
with retained generations and headroom requirements.
