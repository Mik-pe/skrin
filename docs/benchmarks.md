# Benchmark methodology

Run the optimized baseline with `cargo bench -p skrin --bench baseline`. Add `-- --durable /path/to/existing/local/directory` for actual synced transactions. The benchmark creates a unique new file using create-only semantics and removes only that owned file after closing it.

## What is measured

The deterministic in-memory workload uses 10,000 eight-byte records and a multiplicative key permutation. It reports a plain `BTreeMap` lookup baseline, Skrin lookups under one reused read guard, lookups acquiring one guard each, individual volatile write transactions, and volatile batches. The plain map does not provide transactions or durability; it shows overhead, not an equivalent product.

Persistent tests run 200 transactions each at batch sizes 1 and 100, with each transaction ending in a successful sync. They report transactions/s, rows/s, and measured p50/p99 **transaction** latency. Batching amortizes sync cost across rows but changes the atomic unit. It is not group commit across independent transactions. A final warm-cache reopen reports WAL replay time; it is not a cold-disk startup test.

The tests use a single thread, small fixed records and hot keys. Memory measurements do not include encoding or persistence. The in-memory map is warmed during initial population; no filesystem cache eviction is attempted. Latency collection has timing overhead and 200 samples is only a smoke baseline, not a robust tail-latency study.

## Comparisons and reporting

Do not publish shared CI-runner output as a performance claim. Record the exact commit, compiler/profile, CPU, RAM, OS, filesystem, device, data size, payload size, thread count, transaction/batch size and sync policy. Repeat runs, report distributions rather than the best sample, and separate row operations from committed transactions.

Before comparing with redb, SQLite, SpacetimeDB or another engine, match durability and transaction semantics. Compare embedded storage paths with embedded storage paths, not a local pointer lookup with a network/backend benchmark. Include realistic record sizes, multiple dataset sizes, allocation/memory usage and recovery work.

Skrin implements checkpoints and WAL rotation. The managed and scale harnesses below exercise maintenance, but short warm-cache runs do not establish sustained real-device throughput. The [catalog workload](catalog.md#resources-and-evidence) measures typed multi-table transactions and secondary-index overhead separately. The resource harness adds repeated churn and advisory file-cache eviction. Concurrent readers/writers and device-cold recovery remain follow-on benchmarks.

## Managed maintenance

Run `cargo bench -p skrin --bench maintenance -- /tmp` with an existing scratch parent. The benchmark creates its own fresh directory, arms cleanup only after successful creation, and removes only that owned directory after closing the database. A failed create can leave an unowned partial directory for inspection.

Eight rounds each run 200 synced transactions of 100 row updates, checkpoint, prune, close and reopen. Output includes checkpoint/prune pause, warm-cache reopen time, WAL shrinkage, snapshot bytes and before/peak/after disk bytes. Rows and sequence are checked after every reopen. The retained previous WAL is included in disk totals.

These are smoke measurements on the actual selected filesystem, not proof of hardware persistence or a cross-engine comparison. Record physical storage, mount type, available RAM and cache state before publishing results. Do not describe container or shared-runner numbers as SSD guarantees.

## Scale and before/after evidence

`cargo bench -p skrin --bench storage_scale -- /tmp 100000` accepts 1..=1,000,000 rows. It uses 108-byte encoded records, 1,000-row synced seed transactions, seven complete WAL replays, one independent backup, and seven checkpoint/prune/reopen cycles. Every reopened row is checked outside the timed regions. Output reports per-process sample medians/min/max; it does not mislabel seven samples as a robust p99 estimate.

[The recorded comparison](measurements/maintenance-2026-10-07.md) includes identical-harness runs against the pre-change engine, three interleaved process pairs at 100k rows, larger/smaller workload checks, visible environment details, and the limitations of overlayfs/warm-cache measurements. The complete harness includes a live backup copy, so its process peak RSS is not a checkpoint-only memory measurement.

## Catalog transactions and indexes

`cargo bench -p skrin --bench catalog -- /local/scratch/parent 10000` measures equivalent synced two-row transactions with plain records, schema-bound rows and mandatory indexes, then a separate three-row transfer workload. Modes `plain`, `unindexed`, `indexed` permit separate-process Linux RSS measurements. [The recorded NVMe runs](measurements/catalog-2026-10-07.md) report repeated latency distributions and exact workload differences. They do not establish a speedup or saturation/cold-cache/power-loss result.

## Sustained resources and cache state

```sh
cargo +1.89.0 bench -p skrin --bench resources -- /local/scratch/parent 100000 16 512 1000 --advisory-evict
```

Arguments specify existing scratch parent, rows, rounds, payload bytes and rows per transaction. Limits are 10..1M rows, 1..10k rounds, 1..4096 payload bytes and 1..1000 rows per transaction; they are workload controls, not process-memory or disk reservations. Each process exclusively creates/removes its own directory. The harness seeds deterministic pseudorandom byte payloads, checkpoints, then updates 90% of rows and deletes/reinserts 10% under alternate keys in every round. Every commit uses `sync_all`. Every row, payload and sequence is verified outside timed commit/open regions, without a second resident reference table or live backup.

Each round measures transaction p50/p95/p99 and transaction rate (commits divided by summed timed commit durations), blocking verified checkpoint and reclamation pauses, and disk totals before checkpoint, after publication/before reclamation, and with active/previous retention. The staging total captures complete old/new generation overlap; it excludes a transient manifest already renamed during publication. Logical lengths and the sum of regular-file `st_blocks * 512` are reported separately. The latter includes per-inode block accounting/rounding; compressed/reflink extents may be shared, filesystem/directory metadata is excluded, and it is not unique device occupancy or a reservation.

Linux `/proc/self/status` reports sampled RSS and whole-process VmHWM. The workload process includes allocator retention, row regeneration and recovery, with one live database at a time. Each reopen runs in a fresh child, which reports its own RSS/high-water mark before application verification; child memory is not added to the parent high-water mark. Linux documents these proc RSS counters as approximate; this is observable process memory, not allocator accounting or an enforced native-memory budget. Other platforms report unavailable counters rather than zeros.

There are three fresh-process reopen samples for each of snapshot-plus-WAL and post-checkpoint snapshot, first warm and then optionally advisory-evicted. Timed `open_dir` includes mandatory recovery synchronization and excludes process startup and subsequent application verification. Keep the first warm sample: it can include a slower first recovery sync than later samples.

`--advisory-evict` requires Linux and Python3. With all database handles closed, the helper requests `os.posix_fadvise(..., POSIX_FADV_DONTNEED)` on only the benchmark's regular files before **each** advised sample. It never drops global caches. Reopen additionally reports the delta of Linux `/proc/self/io` `read_bytes` during open, to distinguish actual filesystem reads from cached reads. Nonzero counters support successful file-cache eviction for that sample; they do not imply an empty device cache or hardware power-loss durability. Treat unsupported/failed advice as an error, and do not relabel ordinary warm opens as cold.

References: [Python file-cache advice](https://docs.python.org/3/library/os.html#os.posix_fadvise) and [Linux proc memory/I/O counters](https://www.kernel.org/doc/html/latest/filesystems/proc.html). These measurements leave hardware-flush review, real power cuts, allocator budgets, physical-space reservation and long-duration production stability as separate gates.
