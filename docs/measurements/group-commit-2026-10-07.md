# Independent durable group commit: local NVMe evidence

Twelve fresh-process runs compare the same indexed transfer workload with immediate synchronization and opt-in bounded group commit. Both use the same engine, codec, schema, frame format and successful `sync_all` boundary. This is a repeated local comparison supporting the opt-in feature, not device certification or a cross-engine claim.

## Reproduction and environment

- Source commit `b3d39a631e4e1576b31ce6f17ba91b1e1e2d7e69`, tree `d4e1a9230161fe75f52d44c8008ec3f2168ae31a`; tracked tree clean during measurement.
- Rust 1.89.0 (`29483883e`), optimized bench profile, thin LTO and one codegen unit. Compilation excluded. Linux allocation dependency remains enabled but these workloads do not request maintenance preallocation.
- Intel Core i5-1035G7, four cores/eight logical CPUs. Linux `7.2.5-3-omarchy`; identified Toshiba `KBG40ZNS256G` 238.5 GiB NVMe, encrypted `/dev/mapper/root`, Btrfs `@home`, `compress=zstd:3,ssd`. Scratch parent `/home/mikpe/.cache` is on that device, not tmpfs.
- Runs took place 18:24:59–18:26:45 UTC on 2026-10-07. Shared workstation, uncontrolled background work, CPU frequency, filesystem scheduling and device caches; no simultaneous local compilation or validation during the recorded runs. No power-cut experiment.
- Bench source SHA-256 `3facc288478c8bbcb10ec0b5647bb34b3df89f9884beaf28464de85c76221fd4`; executable SHA-256 `8dcb7b4bf4fb14c305487bd117cb561a4756049af1c198e3a468310f96ab52d4`.

[Machine/compiler/filesystem/run metadata](group-commit-2026-10-07.metadata.json), [parsed results](group-commit-2026-10-07.summary.json) and all twelve `group-commit-2026-10-07-run*-*.txt` files are checked in beside this report. Each raw file includes latency, waiting, group histogram, RSS, disk overlap, maintenance and independently verified reopen; no tail samples were removed.

```sh
cargo +1.89.0 bench -p skrin --bench group_commit --no-run --locked
# Invoke the built executable directly to exclude compilation from each run:
BENCH=/path/to/target/release/deps/group_commit-HASH
"$BENCH" /local/scratch immediate 1 500 10000 1000
"$BENCH" /local/scratch group     1 500 10000 1000
"$BENCH" /local/scratch immediate 8 200     0 1000
"$BENCH" /local/scratch group     8 200     0 1000
```

Repeat three times. Mode order was immediate/group in repeats 1 and 3 and group/immediate in repeat 2, at each load. Every invocation creates its own exclusive directory and cleans it only after complete success. Failed runs preserve their directory. Use the recorded source/compiler for reproduction; executable filenames vary.

## Workload and correctness

Each independent transaction debits account 1, credits account 2 and inserts a unique transfer operation ID. Account email uniqueness and ordered balance indexes participate in the same transaction. The donor starts with enough balance for every transfer. Producers wait for their own acknowledgment before the next request, with no callback replay or manual multi-transaction atomic batch.

Low load uses one producer, 500 transactions and 10 ms between acknowledged requests. Saturation uses eight producers, 200 transactions each and no pacing. Group mode uses capacity 64, maximum 16 requests and a 1 ms collection deadline; immediate mode synchronizes each transaction. The phase excludes initialization, reconciliation, checkpoint and reopen. Throughput uses actual phase wall time, including requested pacing and concurrent waiting.

Every run checks all operation IDs and amounts, exact balances, both account indexes, row count and sequence (502 at low load; 1602 at saturation). It then checkpoints, inventories file overlap, reclaims and independently reopens/reconciles in a fresh process. Native values/indexes stay lock-based; `retained_versions=0` is the current contract, not MVCC evidence.

## Transaction results

The table shows the median of each of the three **per-run** percentiles, not percentiles of pooled samples. Low-load rows have 500 observations per run; saturation rows have 1600. Throughput gives the range across all three runs.

| Load and mode | Transactions/s range | p50 ms | p95 ms | p99 ms | Sync groups per run |
| --- | ---: | ---: | ---: | ---: | ---: |
| One paced producer, immediate | 49.836–54.406 | 7.328 | 12.551 | 15.168 | 500 |
| One paced producer, group | 49.943–52.203 | 7.604 | 14.081 | 16.043 | 500 |
| Eight unpaced producers, immediate | 106.396–126.639 | 7.287 | 13.179 | 332.727 | 1600 |
| Eight unpaced producers, group | 815.984–915.146 | 7.261 | 14.079 | 17.230 | 200 |

At saturation every group had eight independent frames, reducing 1600 synchronizations to 200. The ratio of median run throughputs was about 7.14× on this workload/device. The low-load group could not combine requests and added collection/worker waiting; its median per-run call-to-callback p99 was 1.169 ms versus 0.000785 ms for immediate mode. Queueing therefore has an explicit low-load cost and remains opt-in.

Immediate saturation had long acquisition waits rather than a uniform eight-way latency distribution: per-run latency p99 was 611.741/188.896/332.727 ms and maxima were 7.317/9.217/13.229 s. Group p99 was 17.966/16.638/17.230 ms, with maxima 19.171/28.055/54.162 ms. The median per-run call-to-callback p99 was 183.420 ms immediate versus 1.224 ms grouped. These are observed contention/scheduler outcomes; they do not establish general fairness or tail bounds for arbitrary readers/callbacks. No extreme result was discarded.

## Memory, disk and maintenance

Whole-process HWM after maintenance was 3376 KiB in all low-load immediate runs and 3384–3452 KiB grouped. At saturation it was 3844–3940 KiB immediate and 3804–3824 KiB grouped. Independently reopened child HWM before reconciliation ranged 2888–3008 KiB at low load and 3096–3192 KiB at saturation. Samples, worker stacks, runtime, native rows/indexes and allocator slack are included; no process-memory cap or general RSS reduction is claimed.

All modes/repeats had identical logical and inode-allocation totals at each load:

| Load | Snapshot bytes | Before checkpoint logical / allocated | Published overlap and retained logical / allocated |
| --- | ---: | ---: | ---: |
| Low | 29,282 | 108,069 / 126,976 | 137,435 / 167,936 |
| Saturation | 93,082 | 344,569 / 364,544 | 437,735 / 466,944 |

This one-checkpoint workload retains active + previous, so reclamation removes no older generation and published/retained totals match. Overlap is sampled after publication and excludes the transient renamed manifest. Regular-file inode blocks exclude filesystem/directory/journal metadata, can include rounding and can double-count shared extents. They are not available quota or reserved headroom.

Checkpoint pauses were 49.870–56.574 ms at low load and 50.504–60.125 ms at saturation. Reclaim pauses were 5.869–15.374 ms and 5.305–10.086 ms respectively. Fresh-process verified snapshot open was 5.464–9.855 ms low load and 6.356–12.937 ms saturation, including mandatory recovery synchronization and excluding subsequent reconciliation. Cache policy was no advice; these are neither independently proven warm-cache nor device-cold samples.

The repeated equivalent-durability saturation benefit justifies offering bounded group commit while preserving immediate sync as the baseline. Snapshot readers, bounded pinned-version reclamation and their own measurements remain open in #7. Native-memory enforcement and dedicated device power-loss testing in #3/#5 remain separate gates; this comparison does not close them.
