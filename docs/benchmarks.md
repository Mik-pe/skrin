# Benchmark methodology

Run the optimized baseline with `cargo bench -p skrin --bench baseline`. Add `-- --durable /path/to/existing/local/directory` for actual synced transactions. The benchmark creates a unique new file using create-only semantics and removes only that owned file after closing it.

## What is measured

The deterministic in-memory workload uses 10,000 eight-byte records and a multiplicative key permutation. It reports a plain `BTreeMap` lookup baseline, Skrin lookups under one reused read guard, lookups acquiring one guard each, individual volatile write transactions, and volatile batches. The plain map does not provide transactions or durability; it shows overhead, not an equivalent product.

Persistent tests run 200 transactions each at batch sizes 1 and 100, with each transaction ending in a successful sync. They report transactions/s, rows/s, and measured p50/p99 **transaction** latency. Batching amortizes sync cost across rows but changes the atomic unit. It is not group commit across independent transactions. A final warm-cache reopen reports WAL replay time; it is not a cold-disk startup test.

The tests use a single thread, small fixed records and hot keys. Memory measurements do not include encoding or persistence. The in-memory map is warmed during initial population; no filesystem cache eviction is attempted. Latency collection has timing overhead and 200 samples is only a smoke baseline, not a robust tail-latency study.

## Comparisons and reporting

Do not publish shared CI-runner output as a performance claim. Record the exact commit, compiler/profile, CPU, RAM, OS, filesystem, device, data size, payload size, thread count, transaction/batch size and sync policy. Repeat runs, report distributions rather than the best sample, and separate row operations from committed transactions.

Before comparing with redb, SQLite, SpacetimeDB or another engine, match durability and transaction semantics. Compare embedded storage paths with embedded storage paths, not a local pointer lookup with a network/backend benchmark. Include realistic record sizes, multiple dataset sizes, allocation/memory usage and recovery work.

Skrin implements checkpoints and WAL rotation. The managed and scale harnesses below exercise maintenance, but short warm-cache runs do not establish sustained real-device throughput. Multi-table workloads, secondary indexes, concurrent readers/writers and controlled cold-cache recovery are follow-on benchmarks, not hidden assumptions in these results.

## Managed maintenance

Run `cargo bench -p skrin --bench maintenance -- /tmp` with an existing scratch parent. The benchmark creates its own fresh directory, arms cleanup only after successful creation, and removes only that owned directory after closing the database. A failed create can leave an unowned partial directory for inspection.

Eight rounds each run 200 synced transactions of 100 row updates, checkpoint, prune, close and reopen. Output includes checkpoint/prune pause, warm-cache reopen time, WAL shrinkage, snapshot bytes and before/peak/after disk bytes. Rows and sequence are checked after every reopen. The retained previous WAL is included in disk totals.

These are smoke measurements on the actual selected filesystem, not proof of hardware persistence or a cross-engine comparison. Record physical storage, mount type, available RAM and cache state before publishing results. Do not describe container or shared-runner numbers as SSD guarantees.

## Scale and before/after evidence

`cargo bench -p skrin --bench storage_scale -- /tmp 100000` accepts 1..=1,000,000 rows. It uses 108-byte encoded records, 1,000-row synced seed transactions, seven complete WAL replays, one independent backup, and seven checkpoint/prune/reopen cycles. Every reopened row is checked outside the timed regions. Output reports per-process sample medians/min/max; it does not mislabel seven samples as a robust p99 estimate.

[The recorded comparison](measurements/maintenance-2026-10-07.md) includes identical-harness runs against the pre-change engine, three interleaved process pairs at 100k rows, larger/smaller workload checks, visible environment details, and the limitations of overlayfs/warm-cache measurements. The complete harness includes a live backup copy, so its process peak RSS is not a checkpoint-only memory measurement.
