# Benchmark methodology

Run the optimized baseline with `cargo bench -p skrin --bench baseline`. Add `-- --durable /path/to/existing/local/directory` for actual synced transactions. The benchmark creates a unique new file using create-only semantics and removes only that owned file after closing it.

## What is measured

The deterministic in-memory workload uses 10,000 eight-byte records and a multiplicative key permutation. It reports a plain `BTreeMap` lookup baseline, Skrin lookups under one reused read guard, lookups acquiring one guard each, individual volatile write transactions, and volatile batches. The plain map does not provide transactions or durability; it shows overhead, not an equivalent product.

Persistent tests run 200 transactions each at batch sizes 1 and 100, with each transaction ending in a successful sync. They report transactions/s, rows/s, and measured p50/p99 **transaction** latency. Batching amortizes sync cost across rows but changes the atomic unit. It is not group commit across independent transactions. A final warm-cache reopen reports WAL replay time; it is not a cold-disk startup test.

The tests use a single thread, small fixed records and hot keys. Memory measurements do not include encoding or persistence. The in-memory map is warmed during initial population; no filesystem cache eviction is attempted. Latency collection has timing overhead and 200 samples is only a smoke baseline, not a robust tail-latency study.

## Comparisons and reporting

Do not publish shared CI-runner output as a performance claim. Record the exact commit, compiler/profile, CPU, RAM, OS, filesystem, device, data size, payload size, thread count, transaction/batch size and sync policy. Repeat runs, report distributions rather than the best sample, and separate row operations from committed transactions.

Before comparing with redb, SQLite, SpacetimeDB or another engine, match durability and transaction semantics. Compare embedded storage paths with embedded storage paths, not a local pointer lookup with a network/backend benchmark. Include realistic record sizes, multiple dataset sizes, allocation/memory usage and recovery work.

Skrin has no checkpoint or log rotation yet, so the current harness cannot establish long-running steady-state maintenance costs. Implement those before claiming sustainable throughput. Multi-table workloads, secondary indexes, concurrent readers/writers and controlled cold-cache recovery are follow-on benchmarks, not hidden assumptions in these results.
