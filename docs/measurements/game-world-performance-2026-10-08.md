# Native game-world optimization and SQLite comparison

Skrin demonstrates an advantage on resident reads and a bounded pipeline of
**independent durable saves**. It does not beat every SQLite configuration.
The optimized SQLite atomic-batch control remains faster at writing, and uses
much less RAM. These are local workload results, not a general database ranking.

## Changes and reproducibility

The production catalog now validates every final primary/unique/secondary
projection, then omits unchanged primary addresses and secondary postings from
index mutations. Snapshot row replacements copy each shared AVL ancestor once
in the replacement pass instead of deleting/reinserting rows individually. Native
indexed equality uses a borrowed key and reserves the result capacity. There
is no new public API, unsafe optimization, format transition or sync downgrade.
Existing golden fixtures and real storage-fault tests remain applicable.

The benchmark also strengthens SQLite: statements are held through the whole
transaction, only changed columns are assigned, and `sqlite_bulk` updates
positions/revisions directly over primary-key intervals. `sqlite_batch` uses
that optimized path and commits eight requests atomically under FULL. SQL
remains confined to the external benchmark adapter; Skrin never supports it.

- Before: `fd4c486a330eed20b54bbdafd0cf70c0444faf72`; after:
  `66544f664ba10ab83e87fc1278b6a9353624331b`. Both use the same updated harness.
- Rust 1.89.0, optimized thin LTO, one codegen unit; bundled SQLite 3.53.2.
- Intel i5-1035G7, four cores/eight threads, 7.3 GiB RAM, Linux
  7.2.5-3-omarchy, powersave governor. Toshiba KBG40ZNS256G NVMe,
  encrypted Btrfs `@home`, `compress=zstd:3,ssd`.
- Physical campaign: 2026-10-08 07:18:13–07:20:46 UTC. Existing scratch parent
  `/home/mikpe/.cache`. Three repeats, rotating eight mode/window combinations
  by three positions each repeat. No local compilation/validation during runs.
- Shared workstation. Another repository's Actions runner performed Java
  builds/tests during the CPU campaign and was observed around the physical
  campaign. Background work, frequency, filesystem and caches were uncontrolled.
  Every result is retained; no outlier rejection. No power-cut experiment.

[Metadata and exact commands](game-world-performance-2026-10-08/metadata.json)
record source/executable SHA-256, compiler, device/filesystem, order, timestamps
and physical-run process samples. [Parsed results](game-world-performance-2026-10-08/summary.json)
and all raw `nvme-*`, `cpu-paired-*` and nine earlier `cpu-before-*` pilot files
are included. Pilots are separate from paired medians, not discarded evidence.
Run `python3 docs/measurements/game-world-performance-2026-10-08/summarize.py`
to verify complete recovery markers/group totals and regenerate the summary.
The recorded campaign script documents the actual machine-specific invocation.

Build `cargo +1.89.0 bench -p skrin --bench game_world --no-run --locked` at each
source revision and save the two executables separately. Invoke them directly,
excluding compilation, using [the CLI/methodology](../benchmarks.md#game-world).
For the physical campaign, run these configurations three times in rotating
order, replacing `/local/scratch` with an existing directory on the target SSD:

```sh
BENCH=/path/to/optimized/game_world-HASH
"$BENCH" /local/scratch native         100000 128 64 1
"$BENCH" /local/scratch snapshot       100000 128 64 1
"$BENCH" /local/scratch group_snapshot 100000 128 64 1
"$BENCH" /local/scratch sqlite         100000 128 64 1
"$BENCH" /local/scratch sqlite_bulk    100000 128 64 1
"$BENCH" /local/scratch group_snapshot 100000 128 64 8
"$BENCH" /local/scratch sqlite_bulk    100000 128 64 8
"$BENCH" /local/scratch sqlite_batch   100000 128 64 8
```

Each run has 100k entities, 100k items, mandatory area/owner indexes and 128
saves. Each save updates 64 entity positions/revisions, moves one item and
inserts its complete retry request. Each run verifies every row/index/operation
and opens fresh verification children before and after checkpoint/reclaim.
Scratch directories are retained. SQLite uses WAL/FULL, checked settings,
explicit maintenance, mmap off and a 64 MiB page-cache limit per connection.
[Synchronous FULL in WAL](https://www.sqlite.org/wal.html) syncs each committed
transaction; NORMAL is not used as a substitute.

## Production CPU cost, separated from physical durability

Three before/after pairs ran the same executable CLI on `/dev/shm`, 100k
entities/items, 4,096 saves and 64 updates/save, window 1. Mode order was
snapshot/native/SQLite bulk; revision order before/after, after/before,
before/after. Persistent production codecs/WAL/validators/sync calls still run,
but tmpfs provides no physical power-loss durability. These quantify an
end-to-end CPU/scheduling/storage-path proxy, not isolated process CPU cycles.

| Mode/revision | Saves/s across repeats | Median saves/s | Median per-run save p99 µs |
| --- | --- | ---: | ---: |
| Native before | 10,038 / 9,613 / 644 | 9,613 | 150.109 |
| Native after | 13,984 / 13,880 / 14,307 | 13,984 | 114.200 |
| Snapshot before | 2,018 / 2,230 / 1,846 | 2,018 | 696.395 |
| Snapshot after | 9,870 / 8,884 / 8,842 | 8,884 | 163.942 |
| SQLite bulk before | 20,477 / 16,190 / 975 | 16,190 | 167.066 |
| SQLite bulk after | 23,062 / 14,849 / 12,665 | 14,849 | 165.676 |

Ratios of median run throughput are **4.40× snapshot** and **1.45× native**.
The unchanged SQLite control varied substantially, including a slow third
before run; this limits precision of attributing end-to-end gains on this
shared workstation. Snapshot throughput nevertheless improved in every pair.
SQLite bulk still has higher median throughput than either Skrin mode here.
Snapshot saved RSS was 142,500–142,624 KiB before and 125,864–125,916 KiB after,
including allocator retention; this does not establish a lower steady root size.

## Equivalent durable independent saves and the atomic-batch control

Window 1 waits after every request. Window 8 makes eight requests ready together
and measures readiness-to-consumed-ack latency. Skrin admits eight independent
transactions and publishes/acknowledges after shared sync; SQLite independent
mode processes the same ready window serially with FULL per commit. Its driver
incurs no extra worker-thread/connection-contention penalty. SQLite atomic-batch
mode instead commits all eight in one transaction, with all-or-none failure
semantics. Skrin can recover an independently committed prefix after uncertainty.
An application accepting atomic batches should consider that control.

Percentiles below are medians of three per-run percentiles, never pooled; the
last column is the **worst maximum** across all repeats. Each run has 128 request
latencies; window-8 grouped/batched runs have only 16 distinct sync boundaries.

| Mode/window | Saves/s, repeats 1 / 2 / 3 | Median saves/s | p50 ms | p99 ms | Worst max ms |
| --- | --- | ---: | ---: | ---: | ---: |
| Native / 1 | 105.181 / 127.652 / 112.573 | 112.573 | 8.221 | 15.914 | 39.277 |
| Snapshot / 1 | 162.826 / 121.682 / 125.306 | 125.306 | 6.525 | 13.015 | 60.387 |
| Group snapshot / 1 | 139.666 / 112.252 / 92.512 | 112.252 | 7.656 | 15.683 | 18.914 |
| SQLite typed / 1 | 162.056 / 126.882 / 106.408 | 126.882 | 7.459 | 12.866 | 33.609 |
| SQLite bulk / 1 | 94.013 / 121.772 / 118.160 | 118.160 | 7.804 | 15.125 | 25.065 |
| Group snapshot / 8 | 716.785 / 831.924 / 897.885 | **831.924** | 8.660 | 13.989 | 16.031 |
| SQLite bulk / 8 | 105.902 / 120.110 / 113.846 | **113.846** | 39.886 | 75.166 | 84.343 |
| SQLite atomic batch / 8 | 850.976 / 952.685 / 928.785 | **928.785** | 7.259 | 14.932 | 17.012 |

Skrin's independent pipeline is **7.31×** faster by median throughput than
SQLite's optimized independent pipeline; each individual repeat wins
6.77×/6.93×/7.89×. Actual histograms show 16 groups of eight independent Skrin
frames versus 128 SQLite independent commits. This advantage comes from
amortizing synchronization while preserving independent transaction outcomes,
not abandoning durability. Against SQLite atomic batches (also 16 syncs), Skrin
has 10.4% lower median throughput. No clear general win exists for one-at-a-time
saves. Group mode at window 1 cannot amortize sync and adds collection waiting.

## Resident reads and coherent frames

Read values are fully materialized native integers, with 4,000 samples per
operation per run. Values/indexes are warmed and the world fits in RAM.
Table cells show median per-run p50 / p99 µs; SQLite bulk window 1 is the control.

| Operation | Native | Snapshot | SQLite |
| --- | ---: | ---: | ---: |
| Full entity point | 0.646 / 1.233 | 0.661 / 1.300 | 1.672 / 2.289 |
| Full area, up to 64 entities | 9.301 / 12.075 | 7.476 / 10.803 | 18.264 / 22.967 |
| Initial one-item inventory | 1.373 / 2.174 | 1.755 / 3.098 | 2.221 / 3.134 |

Native point p50 is **2.59×** faster; snapshot area p50 is **2.44×** faster.
Inventory tails are closer. The warmed integer-row workload and different RAM
representations are essential qualifications, not a cold-disk/read ranking.

Synthetic frames capture one coherent version, do 64 point reads plus area,
inventory and saved-prefix checks at 60 Hz. Native readers again starved: one
frame per run blocked 1.003–1.217 s, none completed during writes. Snapshot
window 1 completed 48/64/62 overlapping frames; maximum work was
0.100/0.142/0.102 ms, versus SQLite bulk's 82/64/65 frames and
0.324/0.397/0.352 ms. Group window 8 completed only 11/10/9 frames, maximum work
0.036/0.086/0.056 ms; SQLite atomic batch had 10/9/9 frames and
0.265/0.206/0.253 ms. These short phases have sparse tails, not steady-state
real-time guarantees. No snapshot/SQLite frame exceeded 16.67 ms; sampled
start lateness stayed below 0.256 ms. Group last observed prefixes were
112/120/112; final verification saw all 128. Grouping changes publication
freshness, and snapshots can read the preceding synced version during writes.

## Costs and remaining limits

After these 128 saves, RSS was 64,216–64,248 KiB native, 106,400–106,700 KiB
snapshot/group and 12,196–12,424 KiB SQLite. Snapshot worlds cost about **104 MiB
versus SQLite's 12 MiB**, despite their faster reads. Entire data/indexes remain
resident; cooperative pin accounting is not an allocator/RSS cap.

Skrin logical files were 11,552,280 bytes before checkpoint and 23,162,004 after
reclaim, retaining active + previous. SQLite independent modes were 28,526,504
before and 5,627,904 after TRUNCATE; atomic batch was 26,676,624 before. Different
retention contracts prevent a like-for-like post-maintenance size claim.
Skrin checkpoints ranged 99.738–837.764 ms and reclaim 5.271–12.432 ms;
SQLite checkpoint/TRUNCATE ranged 38.554–75.986 ms. Explicit serialized
maintenance/headroom remains an application responsibility.

Fresh children exactly verified WAL recovery and snapshot recovery in every
run. Skrin eagerly decoded/rebuilt rows/indexes (physical-run open roughly
143–188 ms); SQLite opened lazily (8.3–9.3 ms before checkpoint, below 0.5 ms
after), with subsequent complete verification outside those timers. This is
not an equivalent recovery-speed comparison, device-cold cache test, sustained
maintenance test or hardware failure certification. Windows persistence and
large-world/RAM costs remain separate work.
