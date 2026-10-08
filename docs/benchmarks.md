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

Arguments specify existing scratch parent, rows, rounds, payload bytes and rows per transaction. Optional `--reserve-file-data` requires Linux data preallocation for every checkpoint; output records the policy, and the extra snapshot encoding preflight is included in checkpoint timing. Limits are 10..1M rows, 1..10k rounds, 1..4096 payload bytes and 1..1000 rows per transaction; they are workload controls, not process-memory or disk reservations. Each process exclusively creates/removes its own directory. The harness seeds deterministic pseudorandom byte payloads, checkpoints, then updates 90% of rows and deletes/reinserts 10% under alternate keys in every round. Every commit uses `sync_all`. Every row, payload and sequence is verified outside timed commit/open regions, without a second resident reference table or live backup.

Each round measures transaction p50/p95/p99 and transaction rate (commits divided by summed timed commit durations), blocking verified checkpoint and reclamation pauses, and disk totals before checkpoint, after publication/before reclamation, and with active/previous retention. The staging total captures complete old/new generation overlap; it excludes a transient manifest already renamed during publication. Logical lengths and the sum of regular-file `st_blocks * 512` are reported separately. The latter includes per-inode block accounting/rounding; compressed/reflink extents may be shared, filesystem/directory metadata is excluded, and it is not unique device occupancy or a reservation.

Linux `/proc/self/status` reports sampled RSS and whole-process VmHWM. The workload process includes allocator retention, row regeneration and recovery, with one live database at a time. Each reopen runs in a fresh child, which reports its own RSS/high-water mark before application verification; child memory is not added to the parent high-water mark. Linux documents these proc RSS counters as approximate; this is observable process memory, not allocator accounting or an enforced native-memory budget. Other platforms report unavailable counters rather than zeros.

There are three fresh-process reopen samples for each of snapshot-plus-WAL and post-checkpoint snapshot, first without cache advice and then optionally advisory-evicted. Timed `open_dir` includes mandatory recovery synchronization and excludes process startup and subsequent application verification. Keep the first sample: it can include a slower first recovery sync than later samples. A lack of deliberate eviction does not guarantee warm pages on a shared workstation: output reports the observed zero/nonzero read-byte state separately. The [recorded runs](measurements/resources-2026-10-07.md) preserve their earlier `warm` phase names and distinguish verified zero-read samples from naturally evicted ones.

`--advisory-evict` requires Linux and Python3. With all database handles closed, the helper requests `os.posix_fadvise(..., POSIX_FADV_DONTNEED)` on only the benchmark's regular files before **each** advised sample. It never drops global caches. Reopen additionally reports the delta of Linux `/proc/self/io` `read_bytes` during open, to distinguish actual filesystem reads from cached reads. Nonzero counters support successful file-cache eviction for that sample; they do not imply an empty device cache or hardware power-loss durability. Treat unsupported/failed advice as an error, and do not relabel ordinary warm opens as cold.

References: [Python file-cache advice](https://docs.python.org/3/library/os.html#os.posix_fadvise) and [Linux proc memory/I/O counters](https://www.kernel.org/doc/html/latest/filesystems/proc.html). These measurements leave hardware-flush review, real power cuts, allocator budgets, physical-space reservation and long-duration production stability as separate gates.

## Independent group commit

`cargo bench -p skrin --bench group_commit -- EXISTING_DIRECTORY immediate|group PRODUCERS TRANSACTIONS_PER_PRODUCER INTER_REQUEST_US GROUP_DELAY_US` runs independent indexed transfers with the same codec/schema, synchronization boundary and verified final data. Each producer waits for its own acknowledgment before its next request. Use fresh process invocations for each mode/load, rotate mode order and keep workload/compiler/filesystem identical. Compilation, initialization, validation and maintenance are outside the transaction phase.

The harness reports client call-to-result latency and call-to-callback waiting p50/p95/p99, group admission-to-callback waiting, wall-clock throughput (including requested pacing), synchronization groups and their size histogram. Immediate mode's queue time is unavailable as a separate admission metric; both modes' call-to-callback metric includes acquisition/queue/collection waiting. Capture parent RSS/HWM and independent child reopen RSS before row reconciliation. They are whole-process observations, including benchmark samples and resident tables, not allocator caps. `retained_versions=0` records the current lock-based reader contract.

Checkpoint/reclaim pause and before/staging/retained logical and inode-allocation bytes follow the verified transaction phase. Fresh-process reopen has no cache advice and includes mandatory recovery synchronization. This does not establish cold-device performance. Directory/filesystem metadata and shared-extent accounting are outside the disk totals. Low load (one paced producer) and saturation (multiple unpaced producers) must be reported separately; grouping can add low-load latency while reducing synchronization count at saturation. See [the API/failure contract](group-commit.md) and [the recorded repeated NVMe comparison](measurements/group-commit-2026-10-07.md).

## Synced writes with held read views

```sh
cargo bench -p skrin --bench snapshots -- /local/scratch snapshot 1 500 10000 1000 1 1000 1000
cargo bench -p skrin --bench snapshots -- /local/scratch group_snapshot 8 200 0 1000 2 1000 0
```

The four modes `immediate`, `group`, `snapshot`, `group_snapshot` use identical
synced indexed transfers, producer counts and held coherent reader work. CLI
arguments are existing parent, mode, producers, transactions per producer,
inter-request microseconds, group collection microseconds, readers, held-view
microseconds, then between-read microseconds. Read checks use both account
rows and mandatory unique/non-unique indexes at one committed sequence. Every
operation ID and transfer is reconciled after the workload and in a fresh child
process after checkpoint/reclaim. No callback replay or sync downgrade is used.

Each reader captures an initial view before the timing barrier; its acquisition
time is reported, but barrier waiting is excluded. All modes start with those
same views held. Readers capture/verify, hold for the configured duration, then
release before their next interval. Producer throughput/latencies cover all
commits plus configured inter-request delays, exclude final reader shutdown and
maintenance, and include no implicit replay. The harness reports writer/read
p50/p95/p99/max, groups/queue time, observed RSS/HWM and sampled oldest pin,
current bytes, pinned versions/whole-root bytes. Sampling at reader verification
and writer acknowledgment is not a continuous peak-retention profiler. Reader
sample count/rate is reported because concurrency modes can complete different
amounts of read work during the same writer workload. A one-million-sample cap
per reader fails the run rather than silently dropping samples.

Both modes preserve acknowledged-write durability while old snapshot freshness
differs from borrowed reads that wait for the writer. Held views are immutable
in both modes. Current snapshot state has extra immutable row/posting trees and
Arc allocations; RSS comparisons include this overhead, threads and samples.
Native row accounting is cooperative and excludes allocator/global-process
overhead. All reader leases are released before the timed maintenance stage,
which reports checkpoint/reclaim, regular-file logical/inode-allocated disk
bytes and fresh-process unadvised recovery. Maintenance runs through the current controller while its published immutable
root still exists; conversion/drain back to the native baseline is reported
separately and excluded from workload/maintenance timings. Neither filesystem
nor device cache is declared cold. Use repeated paired runs on the same identified device; keep
tails and read rates when judging whether the added version machinery is useful.

[Recorded 24-run NVMe comparison](measurements/snapshots-2026-10-07.md) preserves
all low-load/saturated writer/read tails and sample counts, including sparse
starved borrowed-reader observations and worse snapshot writer contention tails.

## Game world

The [application workload and product targets](game-world.md) prioritize native
state persistence and retrieval. SQL is confined to a benchmark adapter through
`rusqlite = 0.40.2` with bundled SQLite 3.53.2, cached statements and checked
unsigned conversions. This is a development dependency; it adds no SQLite
runtime dependency or SQL API to Skrin. Building development targets now needs
a C compiler for the bundled comparator.

```sh
cargo +1.89.0 bench -p skrin --bench game_world --no-run --locked
cargo +1.89.0 bench -p skrin --bench game_world --locked -- /local/scratch native   100000 128 64
cargo +1.89.0 bench -p skrin --bench game_world --locked -- /local/scratch snapshot 100000 128 64
cargo +1.89.0 bench -p skrin --bench game_world --locked -- /local/scratch sqlite   100000 128 64
```

Run the built executable in separate fresh processes, rotating native/snapshot/
SQLite order across at least three repeats. Also smoke `67 73 13`: it wraps
entity ranges and moves the same item again. Arguments require an existing
scratch parent, mode, 2..1M entities, 1..1M saves and a batch of 1..min(rows,1024).
Each run creates an exclusive directory and **retains it even on success**.
Output identifies the exact path; inspect/clean only these test-owned directories.
A `--verify DB MODE ROWS SAVES BATCH` child invocation checks an existing run.

Every entity has area/x/y/revision (four u64s), and every item has owner/kind
(two u64s). Each save changes `BATCH` successive wrapping entity positions,
moves one item to the next owner and inserts a complete operation ID/request
(five u64s). Entities are grouped in areas of 64. Seed commits insert 256
entities and 256 items each, outside timing. Both engines have area and owner
indexes and use the same application-side typed get/modify/replace operations.
Skrin uses its production catalog codec/WAL/index validator, with no special
benchmark storage path. SQLite uses integer primary keys, normal rowid tables,
corresponding non-unique indexes and prepared statements. All generated values
fit SQLite's signed integer range; this is not a full-u64 compatibility claim.

SQLite settings are WAL, synchronous FULL, fullfsync/checkpoint_fullfsync ON,
a 64 MiB page cache per connection, mmap disabled and wal_autocheckpoint=0.
Checkpoint-on-close is also disabled so the pre-maintenance child actually
reopens the WAL. WAL/FULL and explicit maintenance settings are checked rather
than assumed. Statements are cached and both readers warm their query paths
before sampling. On macOS, fullfsync is necessary to request the stronger flush
boundary used by Skrin's standard-library sync_all. OS/device behavior remains
conditional for both. See SQLite's primary documentation for
[synchronization](https://www.sqlite.org/pragma.html#pragma_synchronous),
[WAL checkpoints](https://www.sqlite.org/wal.html) and
[checkpoint-on-close](https://www.sqlite.org/c3ref/c_dbconfig_defensive.html#sqlitedbconfignockptonclose).

Read phases each sample 4,000 point, area and inventory operations. Each operation
acquires/releases a coherent view and materializes identical native integer
values; isolated SQLite SELECTs use their implicit statement read transaction,
avoiding unnecessary explicit BEGIN/ROLLBACK. Multi-query frames use an explicit
read transaction. Indexed results include full rows ordered by primary key. Area results
have up to 64 rows, inventories initially one. These are resident/warmed reads;
Skrin holds its entire typed world/indexes in RAM and SQLite has a warmed page
cache. Do not describe this as equal memory cost or a cold-storage comparison.
Timings include Instant overhead, enum dispatch and adapter/materialization costs.

The save phase has **one unpaced writer** making independent immediate synced
transactions. One reader starts with it at a barrier and requests synthetic
frame work at 60 Hz: one coherent view, 64 point lookups, one full area read,
one inventory read, and a saved-operation watermark. Every returned value is
checked against that view's exact acknowledged save prefix. Skrin derives the
watermark from its committed sequence; SQLite reads MAX(operation ID)+1. This
small observability difference is included in frame costs. Frame work includes
coherence assertions but excludes real rendering/physics. Start lateness is
reported separately from work duration; missed periods are skipped. Frame count,
frames completed while the writer is active, over-budget work and last observed
save are reported. Fast writer phases or starved readers can have very few frame
samples: their p99 may just be the maximum, not a steady-state latency estimate.
Snapshots may observe the preceding synchronized state during pending writes.
This measures availability/freshness as well as speed; it is not a real-time bound.

Report durable save percentiles/max, wall-time saves/s and changed entities/s.
Retry/no-op and mismatched-operation rejection checks are outside timing. The
complete state is checked: all fields in all rows, every save request, counts,
sequence/watermark and both indexes. An independent iterative integration model
validates the closed-form reference across wrapping ranges/repeated transfers.
Fresh children verify exact state **before and after** serialized maintenance,
including pre-checkpoint WAL replay. Opening timings exclude subsequent full
verification, use no cache eviction/advice, and are not device-cold timings.
Post-save reopening for maintenance rebuilds native/index state and, in snapshot
mode, immutable roots; it is outside save/checkpoint timing. Skrin eagerly decodes/rebuilds all data/indexes on
open; SQLite loads pages lazily and full verification happens outside that timer.
These are different amounts of work, not a recovery-speed ranking. Snapshot leases are
released before maintenance. Checkpoint and reclaim are separate timings;
SQLite checkpoint(TRUNCATE) includes WAL reclamation and its reclaim step is a
no-op. Skrin retains its previous generation. These maintenance contracts differ;
report them rather than treating their pauses/disk bytes as interchangeable.

Linux RSS/high-water samples are whole-process observations including adapters,
threads, sample buffers, native rows/indexes and allocator retention. The binary
contains both engines even in single-mode runs. Children report memory before
full verification. Disk samples sum logical regular-file bytes only, including
SQLite WAL/shared-memory files and Skrin's retained generations; metadata,
physical allocation and quotas are excluded. The workload stores narrow fixed
records and a finite save history; it does not establish behavior for large
assets, unlimited operation-ID retention or worlds larger than RAM.

[The nine-run local NVMe baseline](measurements/game-world-2026-10-08.md) retains
all adverse save tails, starved native frames and memory/lifecycle costs alongside
resident lookup and snapshot frame results; no general durable speedup is claimed.
