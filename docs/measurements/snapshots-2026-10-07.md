# Held-reader snapshot comparison — 2026-10-07

Immutable snapshots substantially reduce reader waiting in this measured workload. This is an opt-in reader-concurrency result, not a general writer-throughput, fairness, RSS or latency guarantee. Immediate snapshot writes had materially worse saturated writer p99 than the borrowed immediate baseline; group/snapshot repeat 2 also had a worse writer tail and throughput than its paired group baseline. All tails and read counts are retained below and in the raw runs.

## Workload and identity

Source commit `75fc6181aae0f7f8390de32df2c1e815b32b8e79`, tree `6dd032498da8c9edb127f9780f1ac1b88682ad6a`. The worktree had no tracked changes during measurement. Benchmark source SHA-256 `2739cf981a6c33db5de8b9e6f336990f4582df6b4e7c55aeb3b7107425e79de8`; executable SHA-256 `eb865a98081020984f31f9400bba1a67b62883220befcce6d7cf27d274ece4df`.

Rust 1.89.0, x86_64 Linux 7.2.5-3-omarchy, Intel Core i5-1035G7 (4 cores/8 logical CPUs), Toshiba KBG40ZNS256G NVMe, Btrfs `compress=zstd:3,ssd` on encrypted `/dev/mapper/root` home subvolume. The existing parent was `/home/mikpe/.cache`. This is a shared workstation with uncontrolled background work, frequency and device caches. No local builds/tests ran during recording; no cache eviction or physical power-loss experiment was performed. The owner deferred the latter for this milestone.

UTC interval 2026-10-07T19:45:11.514758+00:00 to 2026-10-07T19:48:49.458582+00:00. Full compiler/kernel/CPU/device/filesystem identity, commands and exit status are in [metadata](snapshots-2026-10-07.metadata.json). All 24 fresh-process runs succeeded, reconciled every operation ID and native/index result, then verified a fresh-process reopen after checkpoint/reclaim.

All modes execute the same durable indexed transfer workload: decrement donor, increment recipient and insert a distinct operation ID in one transaction. Records, explicit codecs, constraints and sync requirements are the same. Concurrent admission can change physical slot/transaction order; application final state and total encoded lengths are identical. Immediate modes synchronize every independent transaction; group modes synchronize independent frames together before visible publication or success.

- Low load: one producer, 500 transfers, 10 ms between responses; one reader, 1 ms held view and 1 ms between reads.
- Saturation: eight producers × 200 unpaced transfers = 1,600; two readers each hold 1 ms with no between-read delay.
- Group options: queue 64, at most 16 collected requests, 1 ms maximum collection delay; snapshots: at most 32 leases and 512 MiB cooperatively accounted pinned bytes.
- Three repeats per mode/load. Mode order: immediate/snapshot/group/group_snapshot; then group_snapshot/group/snapshot/immediate; then snapshot/immediate/group_snapshot/group. Low load precedes saturation in each repeat.

Initial views are captured before the common barrier and held when timing starts. Their acquisition duration is included in reader distributions, barrier waiting is excluded. Each read verifies coherent account rows and mandatory unique/non-unique indexes, then holds that view. All reader leases release before maintenance, but versioned controllers retain their current roots during the timed checkpoint/reclaim. Native conversion/drain is measured separately. See [harness methodology](../benchmarks.md#synced-writes-with-held-read-views) and [source](../../crates/skrin/benches/snapshots.rs).

## Writer distributions

Values are medians of the three per-run statistics, not percentiles of pooled samples. Throughput includes producer delays; writer latency excludes the subsequent retention observation, while throughput includes that observation.

| Load / mode | Transfers/s | Writer p50 / p95 / p99 (ms) | Call→callback p99 (ms) | Sync groups per run |
| --- | ---: | ---: | ---: | ---: |
| low / immediate | 54.745 | 7.158 / 13.073 / 14.382 | 0.037 | 500 |
| low / group | 48.472 | 10.099 / 14.835 / 17.475 | 2.295 | 500 |
| low / snapshot | 54.316 | 7.757 / 12.518 / 14.315 | 0.002 | 500 |
| low / group_snapshot | 49.622 | 9.150 / 13.517 / 15.665 | 1.161 | 500 |
| saturation / immediate | 104.764 | 7.903 / 13.495 / 111.898 | 0.042 | 1600 |
| saturation / group | 756.983 | 10.229 / 14.549 / 17.196 | 6.624 | 200 |
| saturation / snapshot | 124.725 | 7.110 / 12.449 / 1255.494 | 1249.497 | 1600 |
| saturation / group_snapshot | 860.588 | 9.126 / 13.724 / 19.102 | 1.200 | 200 |

At low load all groups contain one frame. At saturation every group contains exactly eight frames in every group/group_snapshot run: 200 successful sync groups versus 1,600 immediate syncs. Immediate snapshot saturation p99 was 1,255.494 / 1,647.050 / 1,033.052 ms, versus 111.898 / 232.882 / 24.257 ms for immediate borrowed reads. Its writer contention tail remains a limitation; adding snapshots does not make the serialized writer fair.

Group_snapshot saturation throughput was 860.588 / 535.714 / 882.101 transfers/s, versus group 756.983 / 666.559 / 829.750. Its p99 was 13.997 / 106.426 / 19.102 ms, versus group 16.169 / 19.547 / 17.196. These variable results do not justify a uniform writer-tail or throughput claim. The low-load group collection cost remains visible.

## Reader waiting and completed work

| Load / mode | Reads/s | Read samples (repeats 1 / 2 / 3) | Acquire p50 / p95 / p99 (µs) | Acquire+verify p99 (µs) |
| --- | ---: | ---: | ---: | ---: |
| low / immediate | 273.399 | 2497 / 2497 / 2497 | 0.781 / 9318.908 / 12311.903 | 12314.562 |
| low / group | 290.346 | 2995 / 2992 / 2995 | 0.422 / 8712.541 / 11641.211 | 11645.466 |
| low / snapshot | 455.820 | 4083 / 4196 / 4267 | 0.864 / 30.203 / 34.284 | 42.942 |
| low / group_snapshot | 456.722 | 4361 / 4602 / 4640 | 0.823 / 29.889 / 33.216 | 42.283 |
| saturation / immediate | 0.321 | 6 / 4 / 4 | 0.469 / 15270027.028 / 15270027.028 | 15270030.403 |
| saturation / group | 312.728 | 661 / 681 / 693 | 6204.311 / 11997.332 / 14215.697 | 14221.215 |
| saturation / snapshot | 1837.973 | 24319 / 23578 / 22975 | 0.560 / 11.455 / 34.011 | 41.076 |
| saturation / group_snapshot | 1842.488 | 3426 / 5469 / 3342 | 0.457 / 12.321 / 34.946 | 40.500 |

The saturated borrowed immediate reader has only 4–6 observations, including initial views and reads delayed until writers finish. Its reported p99 is therefore an empirical maximum of very few samples, not a reliable steady-state population estimate. Observed waits were 12.451–18.014 seconds. Snapshots perform the same per-read task but complete far more reads during the identical writer workload; do not describe these as equal completed read counts. Snapshot reads can return the preceding synchronized version during pending writer I/O, while borrowed acquisition waits for that writer. Both held views remain coherent and immutable.

This reader availability/freshness tradeoff, backed by blocked-production-sync tests, justifies the optional version machinery for workloads that need concurrent coherent reads. The original immediate and borrowed group APIs remain reproducible baselines. No default durability or reader behavior is changed.

## Process memory and retained versions

| Load / mode | Process HWM (KiB, repeats 1 / 2 / 3) | Sampled max pinned bytes (same order) | Sampled max pinned versions |
| --- | ---: | ---: | ---: |
| low / immediate | 3924 / 3912 / 3928 | 0 / 0 / 0 | 0 / 0 / 0 |
| low / group | 4000 / 4000 / 4000 | 0 / 0 / 0 | 0 / 0 / 0 |
| low / snapshot | 4580 / 4428 / 4452 | 68804 / 68804 / 68668 | 1 / 1 / 1 |
| low / group_snapshot | 4464 / 4488 / 4500 | 68804 / 68804 / 68668 | 1 / 1 / 1 |
| saturation / immediate | 4272 / 4264 / 4296 | 0 / 0 / 0 | 0 / 0 / 0 |
| saturation / group | 4412 / 4460 / 4416 | 0 / 0 / 0 | 0 / 0 / 0 |
| saturation / snapshot | 9068 / 8988 / 8872 | 436536 / 436672 / 436536 | 2 / 2 / 2 |
| saturation / group_snapshot | 5328 / 5696 / 5292 | 436808 / 435720 / 434632 | 2 / 2 / 2 |

Versioned current-root accounting finishes at 68,804 bytes (low) / 218,404 bytes (saturation), with zero pins/retained pinned versions and no oldest pin after reader release. The sampled oldest pin is sequence 2; leases peak at one/two for the configured one/two readers. Accounted bytes sum complete roots per lease, deliberately double-count sharing, and exclude allocator/process overhead. Sampling is not a continuous peak-retention profiler.

Process HWM is the maximum reported workload/maintenance/conversion VmHWM, includes benchmark samples, threads, native active state/indexes, immutable roots, verification buffers and allocator slack, and cannot be attributed solely to the engine. Snapshot modes collect thousands more ReadSample/retention observations; immediate snapshot saturation collects roughly 23k–24k versus only 4–6 in the borrowed baseline. The higher HWM therefore does not isolate the cost of version trees, and is not evidence of a process-memory reduction or cap. Native rows and assessment functions remain trusted.

## Maintenance, disk and independent recovery

All modes had the same measured regular-file footprints for a given load:

| Load | Before logical / inode allocated | After publish logical / inode allocated | After reclaim logical / inode allocated | Snapshot bytes |
| --- | ---: | ---: | ---: | ---: |
| Low | 108,069 / 126,976 | 137,435 / 167,936 | 137,435 / 167,936 | 29,282 |
| Saturation | 344,569 / 364,544 | 437,735 / 466,944 | 437,735 / 466,944 | 93,082 |

One checkpoint retains active + previous, so this interval has no obsolete generation to delete; staged and retained totals coincide. These are sampled after publication and exclude transient temporary-manifest overlap. Counts sum regular files and per-inode st_blocks × 512; directory/filesystem/journal metadata is excluded, shared extents may be counted again and allocation rounds to blocks.

| Load / mode | Checkpoint ms (1 / 2 / 3) | Fresh-process open ms (1 / 2 / 3) |
| --- | ---: | ---: |
| low / immediate | 52.925 / 55.333 / 57.853 | 9.440 / 10.106 / 5.989 |
| low / group | 49.914 / 62.062 / 72.541 | 10.192 / 9.065 / 6.131 |
| low / snapshot | 50.452 / 55.808 / 60.981 | 9.961 / 5.856 / 10.300 |
| low / group_snapshot | 48.255 / 57.943 / 54.063 | 5.321 / 8.883 / 5.659 |
| saturation / immediate | 53.978 / 61.330 / 54.707 | 5.885 / 7.241 / 6.638 |
| saturation / group | 54.779 / 56.735 / 49.405 | 8.835 / 6.333 / 6.523 |
| saturation / snapshot | 348.843 / 68.508 / 50.917 | 6.165 / 14.510 / 7.603 |
| saturation / group_snapshot | 60.436 / 56.709 / 50.358 | 11.829 / 10.328 / 6.007 |

Reclaim spans 5.104–11.276 ms low / 5.464–9.287 ms saturation. Native conversion/drain is 0–0.336 ms low / 0–0.447 ms saturation, outside workload and maintenance. Fresh child VmHWM after open/before complete row reconciliation is 2,996–3,128 KiB low / 3,268–3,328 KiB saturation. Open includes production recovery synchronization and excludes the later exact row/index verification; no filesystem/device-cold or warm-cache claim is made. The 348.843 ms checkpoint and other stalls remain in the report.

## Reproduce and inspect raw evidence

Check out the source commit above and build once:

```sh
cargo +1.89.0 bench -p skrin --bench snapshots --no-run --locked
cargo +1.89.0 bench -p skrin --bench snapshots --locked -- /home/mikpe/.cache snapshot 1 500 10000 1000 1 1000 1000
cargo +1.89.0 bench -p skrin --bench snapshots --locked -- /home/mikpe/.cache group_snapshot 8 200 0 1000 2 1000 0
```

Run the compiled executable directly in the recorded rotated orders for three repeats of all four modes/load settings. Every argv, result and wall duration appears in metadata. Each successful invocation removes only its own uniquely created scratch root; failure preserves it. Raw files are named `snapshots-2026-10-07-runN-LOAD-MODE.txt`. [Parsed per-run/median summary](snapshots-2026-10-07.summary.json) keeps original metrics, all four process-HWM samples and disk accounting. This local comparison is not device certification, an untrusted-codec allocator limit or a cross-engine/SpacetimeDB result.
