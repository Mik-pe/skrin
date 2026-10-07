# Maintenance and recovery: controlled local comparison

Recorded October 7, 2026. **These are warm-cache container measurements, not physical-SSD guarantees, sustainable production throughput, or a comparison with another database.**

## Reproduction and environment

- Before: engine at `1506fa13b6de135a32e6c874c57b6da7611f7ea7`. The new `storage_scale.rs` harness and its Cargo bench entry were copied unchanged onto that source before compilation.
- After: the implementation accompanying this document. SHA-256 of sorted `crates/skrin/src/**/*.rs` path/NUL/content/NUL concatenations: `8e3f77d863de0254ce65ea84f88fb0a4f4df797dada531dff403dc0f4f9337bc`.
- Both: Rust 1.89.0 (`29483883e`), edition 2024, optimized bench/release profile, thin LTO, one codegen unit, one client thread. No dependency or synchronization-contract change.
- Container-visible platform: Linux 6.18.44 x86_64, AMD EPYC 9V74 80-Core Processor; scratch `/tmp` on overlayfs. Physical backing storage, available dedicated RAM, CPU frequency and host/device caching are not identified or controlled. No CPU affinity or cache eviction.
- 108 encoded bytes per record (u64 plus length-prefixed 96-byte string), primary keys 0..N, 1,000 rows per synced seed transaction. Seven WAL replays; one backup; seven checkpoint/prune/reopen cycles, with full row verification outside timed phases.
- At 100,000 rows, three **interleaved before/after process pairs**; the table reports median of per-process medians (each replay/checkpoint/reopen median has seven samples). The 10,000/500,000-row checks are one process pair each, not equivalent statistical replication. Backup/seed have one sample per process.

```sh
cargo bench -p skrin --bench storage_scale -- /tmp 100000
cargo bench -p skrin --bench storage_scale -- /tmp 10000
cargo bench -p skrin --bench storage_scale -- /tmp 500000
```

The harness creates and removes only its own unique scratch directory. For the old engine, use a separate worktree at the before commit and copy only the benchmark source/Cargo bench entry. Keep toolchain, profile, dataset, batch sizes and scratch filesystem identical. Alternate processes; do not select the best run.

## 100,000 records

| Operation | Before (ms) | After (ms) | Before / after |
| --- | ---: | ---: | ---: |
| Seed synced batches | 61.774 | 33.824 | 1.83× |
| Complete WAL replay | 45.316 | 23.158 | 1.96× |
| Independent live backup | 122.959 | 34.308 | 3.58× |
| Checkpoint + decode verification | 127.486 | 26.678 | 4.78× |
| Snapshot reopen | 40.289 | 18.326 | 2.20× |

## Scale checks

| Rows | Checkpoint before / after (ms) | WAL replay before / after (ms) | Snapshot reopen before / after (ms) |
| ---: | ---: | ---: | ---: |
| 10,000 | 10.882 / 2.389 | 4.182 / 2.000 | 3.767 / 1.813 |
| 500,000 | 585.682 / 129.334 | 227.026 / 118.904 | 212.098 / 113.386 |

All recorded per-process min/median/max measurements, including seed and backup, are in [the CSV](maintenance-2026-10-07.csv). They are not latency-percentile claims for a concurrent service.

## What changed—and what did not

The implementation removes per-row production snapshot buffer flushes, reuses record/payload buffers, uses safe portable slicing-by-eight IEEE CRC-32, and verifies checkpoints with one decoded record at a time instead of another BTreeMap. The on-disk fixtures, validation policy, mandatory synchronization and success/publication ordering remain unchanged.

The process peak RSS from this **whole** harness is not a checkpoint-only memory measurement: a live backup necessarily owns a second table while the source still exists. At 100k rows the process high-water mark stayed around 38 MiB in both versions; no process-wide RSS reduction is claimed. A separate live-record-count regression proves that checkpoint verification keeps at most one additional decoded record alive and preserves the original rows.

Small-key in-memory operations are not the target of these changes. This comparison does not establish multi-table/index performance, reader/writer concurrency, cold-cache startup, real-device fsync latency, or long-duration stability. Run those workloads on identified hardware before making product or cross-engine claims.
