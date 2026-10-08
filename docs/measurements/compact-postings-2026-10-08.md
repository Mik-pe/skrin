# Compact derived-index postings: memory evidence

The derived index now stores one or two sorted primary IDs inline. Three or
more use a BTreeSet, returning to inline storage on shrink. This removes a
heap tree node for unique keys and common small ownership groups, while large
shared-key groups retain logarithmic insertion/removal. It is private engine
representation, with no new API, SQL support, dependency, persisted layout,
codec or synchronization change. Immutable snapshot posting trees are unchanged.

## Method and retained evidence

Baseline main is `e1e2f0f4ed88de9919a88b816e3862d4cf0a30cd`; candidate engine is
`eb686c9` (full ID in metadata). The baseline executable was compiled at
`66544f6`, whose engine/benchmark are identical to merged main. Both binaries
use Rust 1.89.0, thin LTO, one codegen unit and the same game-world harness.
[Metadata](compact-postings-2026-10-08/metadata.json) includes binary hashes,
source, compiler, host, filesystem, device, commands, order and process samples.

Same i5-1035G7 workstation, 7.3 GiB RAM, encrypted Btrfs on Toshiba KBG40ZNS256G
NVMe as [the earlier comparison](game-world-performance-2026-10-08.md). Clocks,
caches and background user/automation work were uncontrolled. No local
compilation/validation occurred during measurement. Every run is retained;
there is no outlier rejection or power-cut certification.

Three before/after pairs per configuration use 100k entities plus 100k items,
64 position updates + one indexed item move + complete retry request per save.
Revision order alternates before/after, after/before, before/after:

- Native and snapshot: `/dev/shm`, 512 saves, window 1. Production codecs,
  validation, WAL and sync run, but tmpfs is not physical durability or isolated
  CPU cycles.
- Group snapshot: `/home/mikpe/.cache`, 128 saves, window 8, independently
  durable frames with one shared synchronization before publication/acknowledgment.

The short physical results were adverse, so **six additional alternating pairs**
used 1,024 saves/window 8 on the same NVMe, starting after/before. Their
[metadata](compact-postings-2026-10-08/confirmation-metadata.json) explicitly
records the follow-up purpose and commands. Each run contained 128 groups of
eight independent transactions. Both campaigns remain separate in the report;
longer runs do not replace or erase short results.

[All 30 raw runs and parsed values](compact-postings-2026-10-08/summary.json)
are included beside the metadata. Regenerate with
`python3 docs/measurements/compact-postings-2026-10-08/summarize.py`; incomplete
runs, missing fresh-process verification or inconsistent group counts are errors.
Reproduce by building the same harness at each engine version, saving executables
separately and running these configurations with the same arguments/order:

```sh
/path/to/before-or-after /dev/shm native         100000 512  64 1
/path/to/before-or-after /dev/shm snapshot       100000 512  64 1
/path/to/before-or-after /local/ssd group_snapshot 100000 128 64 8
/path/to/before-or-after /local/ssd group_snapshot 100000 1024 64 8
```

## Stable memory reduction

Whole-process post-save RSS medians, including allocator retention, adapters,
threads, sample buffers, native rows/indexes and immutable roots:

| Configuration | Before KiB | After KiB | Reduction |
| --- | ---: | ---: | ---: |
| Native, 512 saves | 64,364 | 54,568 | **15.2%** |
| Snapshot, 512 saves | 110,644 | 100,848 | **8.9%** |
| Group snapshot, 128 saves | 106,588 | 96,720 | **9.3%** |
| Group snapshot, 1,024 saves | 116,980 | 107,118 | **8.4%** |

The absolute reduction is about 9.6 MiB in every configuration, consistent with
removing 100k small owner-posting heap nodes. Across initial repeats native
before was 64,228–64,372 KiB and after 54,468–54,592 KiB. Snapshot before was
110,612–110,700 KiB and after 100,840–100,860 KiB. This is a narrow resident-world
result: worlds dominated by large shared-key groups will save fewer allocations.
All rows/indexes still fit in RAM; cooperative pin accounting remains distinct
from RSS, and immutable posting roots still store each indexed row.

## Latency results and limits

All percentiles below are medians of per-run percentiles; maxima are worst
observations across every run in that configuration. No new write-speed claim
is made. The local memory result is substantially more consistent than timing.

| Configuration/revision | Median saves/s | Save p99 ms | Worst save ms |
| --- | ---: | ---: | ---: |
| Native tmpfs before | 18,465 | 0.087 | 0.204 |
| Native tmpfs after | 19,278 | 0.093 | 0.169 |
| Snapshot tmpfs before | 12,137 | 0.131 | 0.256 |
| Snapshot tmpfs after | 12,428 | 0.154 | 0.251 |
| NVMe 128 before | 733 | 18.787 | 19.794 |
| NVMe 128 after | 511 | 18.764 | 26.894 |
| NVMe 1,024 before | 561 | 21.868 | 69.320 |
| NVMe 1,024 after | 598 | 22.808 | **527.167** |

The first physical campaign has 30.3% lower candidate median throughput. Longer
pairs alternated wins/losses: before throughput 869/780/608/498/484/514, after
625/919/966/571/325/455 saves/s. The candidate's 527 ms worst acknowledgment is
an adverse observation, not a discarded sample. These uncontrolled measurements
neither establish a durable speedup nor rule out a timing regression. Equal
persisted bytes/group counts do not imply equal device/scheduler latency.

Resident point/area/inventory reads still sample 4,000 warmed requests per run.
Native tmpfs median inventory p50 was 1.551→1.278 µs, but entity point p50 was
0.696→0.768 µs. Snapshot point p50 was 0.732→0.677 µs and area p50
7.508→7.862 µs. There is no universal read improvement. In longer physical runs,
maximum coherent snapshot frame work was 0.265 ms before / 0.223 ms after;
this remains a synthetic frame workload, not a real-time guarantee.

## Correctness, compatibility and delivery

Every run checks all fields, row counts, saved operations and both indexes,
and freshly reopens before and after checkpoint/reclaim. All **15 paired
persisted directory images are byte-for-byte identical**, recorded in
[per-file hashes](compact-postings-2026-10-08/byte-images.json). Four additional
[cross-version reopen checks](compact-postings-2026-10-08/cross-version-reopen.txt)
load old output with the new engine and new output with the old engine. Wrapped
ranges, repeated owner moves and partial final windows pass in all Skrin modes.
These checks supplement unchanged independent format fixtures and real
production-storage fault tests, rather than substituting a mock database.

Local Rust 1.89 format/Clippy (`-D warnings`), debug/release workspace tests,
docs (`-D warnings`), all-target checks and examples passed; stable workspace
tests/all-target checks passed. A set-reference test exercises promotion,
demotion, duplicates, missing removals, full-u64 keys and randomized ordering.
CI and existing merge gates remain configured normally. Maintenance stays
explicit/serialized, with permanent LOCK, retained generations and headroom
requirements unchanged. This iteration targets memory; dedicated controlled
latency measurements remain necessary for a stronger timing conclusion.
