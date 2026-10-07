# Sustained maintenance resources on local NVMe

Recorded October 7, 2026. Three independent verified processes ran 100,000 rows through sixteen update/delete/insert and checkpoint/reclaim cycles each. **This is a single-client storage baseline, not a speedup, saturation, device-cold, enforced-budget or power-loss claim.**

## Source, environment and reproduction

- Engine: main `e88a3ed8b9282a2d3893a5a1ef675be700c7a2df` (PR #11). Engine source SHA-256 `a04b57f2bcde66db8d2719912a82a7e2117fb0938252e9e7a82dda93efd829c8`, computed over sorted `crates/skrin/src/*.rs` path/NUL/content/NUL concatenations.
- Recorded harness: [source at `ef3f8a36`](https://github.com/Mik-pe/skrin/blob/ef3f8a36c6a4ac9c2dd4140166c5c76293bdde0a/crates/skrin/benches/resources.rs), SHA-256 `311e517010763bbbe6a42f17e29b1b83dfc182c7ee89bf5bd53b6bab77cf7b62`. The final harness changes the unadvised `warm` phase labels to `no_advice` and adds an observed cache-state field; the workload and timing remain the same. Original outputs are preserved.
- Rust 1.89.0 (`29483883e`), optimized bench profile, thin LTO, one codegen unit; no external Rust dependencies. Compile time excluded.
- Intel Core i5-1035G7, four cores/eight logical CPUs; 7,521 MiB RAM visible, about 4,548 MiB available before measurement, swap present. Linux `7.2.5-3-omarchy` x86_64.
- Scratch in the local user's `.cache`, `/dev/mapper/root` btrfs subvolume `@home`, `compress=zstd:3,ssd`. Backing NVMe: `KBG40ZNS256G TOSHIBA MEMORY`, 238.5 GiB. `/tmp` is tmpfs on this machine and was not used.
- Shared workstation, no CPU affinity/frequency controls, exclusive access, reboot or device-cache flush. Other applications and natural page eviction are possible. Engine sync semantics are the conditional [Unix durability contract](../durability.md).

```sh
cargo +1.89.0 bench -p skrin --bench resources --no-run --locked
cargo +1.89.0 bench -p skrin --bench resources --locked -- /local/btrfs/scratch 100000 16 512 1000 --advisory-evict
```

Run the built executable directly for three independent processes to exclude compiler work. Each process creates/removes only its own fresh directory under the existing scratch parent. [Metadata](resources-2026-10-07.metadata.json), [run 1](resources-2026-10-07.run1.txt), [run 2](resources-2026-10-07.run2.txt), [run 3](resources-2026-10-07.run3.txt) and [per-process summaries](resources-2026-10-07.summary.json) retain all recorded samples, including stalls.

## Workload

Rows have a u64 version plus a length-prefixed 512-byte deterministic pseudorandom payload: 524 encoded record bytes and a 54,000,048-byte snapshot. Each seed/round uses 100 transactions of 1,000 rows, ending in mandatory `sync_all`. Every round updates 90% of rows and deletes/reinserts 10% under alternate keys in the same transactions. All expected keys, versions, payloads, row counts and acknowledged sequences are checked outside timed commit/open regions.

Before checkpoint and after checkpoint/reclaim, three unadvised and three advisory-evicted recovery samples each run in fresh child processes. Linux/Python3 `POSIX_FADV_DONTNEED` applies only to this benchmark's closed, synced regular files before each advised sample. The kernel's `read_bytes` delta during `open_dir` is retained; process startup and application verification are outside the open timer. Device caches are uncontrolled.

Each process finishes at sequence 1,700: 100 seed plus 1,600 churn commits. Checkpoints preserve the sequence. The three sixteen-round workloads take 112.648, 107.468 and 208.531 seconds, totaling 428.647 seconds after the seed checkpoint. This repeated baseline does not certify long-duration production stability. One live database exists at a time in the workload process; there is no second resident backup/reference table. The child can coexist with allocator-retained parent memory, so parent VmHWM is not total simultaneous system memory.

## Results

The commit table is the **median across three processes of each process's median of sixteen round statistics**. Every round percentile has 100 transaction samples; these are not pooled 4,800-transaction percentiles. Transaction rate excludes maintenance, reopen and verification.

| 1,000-row synced churn transaction | Median process/round statistic |
| --- | ---: |
| p50 | 21.774 ms |
| p95 | 29.024 ms |
| p99 | 37.302 ms |
| Transaction rate | 45.295 tx/s |

The same median-of-process-medians procedure gives 1,070.436 ms for verified blocking checkpoint and 39.295 ms for reclamation. Across all 48 rounds, checkpoint spans 405.734–9,706.421 ms and reclamation 26.141–110.161 ms. One round's transaction p99 reaches 5,186.622 ms. The third process is substantially slower; no cause is inferred from these measurements and no outlier is discarded.

The recovery table reports the median of three per-process medians. Each process has 48 samples per requested cache policy/storage state. The zero-read column filters only unadvised samples whose observed `read_bytes` delta is zero; the unfiltered column preserves every unadvised sample.

| Recovery state | No advice, all samples (ms) | No advice, zero-read samples (ms) | Advisory-evicted (ms) |
| --- | ---: | ---: | ---: |
| Snapshot + one round's WAL | 132.135 | 131.664 | 158.534 |
| Post-checkpoint snapshot + empty WAL | 73.273 | 72.667 | 82.459 |

There are 43/46/43 zero-read snapshot+WAL samples per process and 48/48/46 zero-read snapshot samples. Other unadvised samples incurred natural cache misses; calling all of them warm would be incorrect. Advised snapshot+WAL opens report 107,798,528–109,486,080 read bytes, and advised snapshot opens 54,001,664 bytes. This supports file-cache eviction and real filesystem reads for those samples, not an empty device cache. The largest unadvised snapshot open is 8,059.946 ms. Recovery includes mandatory synchronization, so the measurements are not pure decode/CPU timings.

## Memory and retained disk

Linux parent VmHWM is 64,168 / 64,192 / 64,128 KiB (about 62.6–62.7 MiB). This includes allocator retention, row regeneration, encoding/verification buffers and subsequent reopen, with one live table at a time. Fresh recovery children's RSS/high-water samples are retained separately before application verification. These approximate proc counters are process observations, not exact native/index allocation accounting or enforced memory limits.

| Regular-file accounting | Bytes | Relative to one sealed snapshot |
| --- | ---: | ---: |
| Logical old/new overlap after checkpoint, before reclaim (rounds 2–16) | 269,588,464 | 4.992× |
| Logical active/previous retention after reclaim (all rounds) | 161,794,332 | 2.996× |
| Sum of allocated `st_blocks * 512` at old/new overlap (rounds 2–16) | 269,623,296 | — |
| Sum of allocated `st_blocks * 512` after reclaim (all rounds) | 161,820,672 | — |

The first round has a smaller staging total because the immediately previous generation begins with only the seed history. In later rounds, two snapshots plus retained previous/current WAL intervals overlap with the replacement snapshot. After reclamation the previous generation still needs its WAL: retention is about three snapshot lengths, not two. The post-reclaim totals remain identical throughout all sixteen rounds in all three processes; each inventory reports no recognized reclaimable entries.

These totals include regular engine files, exclude directory/filesystem metadata, and capture the full old/new generation overlap after CURRENT publication rather than every transient manifest byte. `st_blocks` is per-inode allocation, including block rounding; compressed/reflink extents can be shared. It is not unique physical-device occupancy, quota headroom or guaranteed reserved space. Arbitrary unknown files and malformed ownership stages are not part of this controlled workload and remain protected by the reclamation contract.

[Methodology and primary counter/advice references](../benchmarks.md#sustained-resources-and-cache-state) explain the accounting limits. Issues #3/#5 remain open for native-memory enforcement, physical reservations, malformed-stage operator handling, platform/hardware flush review and real power cuts. Snapshot readers/group commit in #7 remain separate implementation and measurement work.
