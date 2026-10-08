# Typed game-world persistence on a shared local NVMe

Nine fresh-process runs establish a game workload baseline, not a new engine
optimization or a general claim that Skrin is faster than SQLite. Typed resident
point/area reads and snapshot frame availability are promising; save tails,
native memory, maintenance and eager loading remain substantial costs.

## Workload and identity

- Source commit `459701085a601ac518d475f407d7b4680826bcd5`, tree
  `0ef7e94257c3b2d9d3a2fe3db65bd6657063d0fc`; source tree clean during measurement.
- Rust 1.89.0, optimized bench profile, thin LTO, one codegen unit. Bundled
  SQLite 3.53.2 through pinned development-only rusqlite 0.40.2.
- Intel Core i5-1035G7 (4 cores/8 logical CPUs), Linux 7.2.5-3-omarchy,
  7.3 GiB visible RAM, Toshiba KBG40ZNS256G 238.5 GiB NVMe. Scratch under
  `/home/mikpe/.cache`, encrypted `/dev/mapper/root`, Btrfs `@home`,
  `compress=zstd:3,ssd`; not tmpfs.
- UTC interval 2026-10-08 06:52:19–06:54:14. Shared workstation, uncontrolled
  frequency/background work, filesystem scheduling, swap and device caches.
  A local Actions runner compiling another repository was observed during this
  campaign. No separately issued Skrin build/test commands overlapped the runs.
  No outlier was discarded, no cache advice/eviction or power-cut experiment.
- 100,000 Entities and 100,000 Items, area/owner indexes, 128 independent synced
  saves of 64 entity updates + one inventory move + one operation-ID record.
  One unpaced writer and one synthetic frame reader at 60 Hz; corresponding
  SQLite indexes, cached statements, application-side typed read/modify/replace,
  WAL/FULL, fullfsync ON and caller-driven checkpoints.
- Three repeats in order native/snapshot/SQLite; SQLite/native/snapshot;
  snapshot/SQLite/native. Every run succeeded and checked exact values, counts,
  every operation and both indexes before/after maintenance in fresh children.

[Metadata, exact commands and source/executable hashes](game-world-2026-10-08.metadata.json),
[per-run values and median summary](game-world-2026-10-08.summary.json) and all
nine `game-world-2026-10-08-runN-MODE.txt` files are retained beside this report.
The [methodology](../benchmarks.md#game-world) describes view acquisition,
materialization, reference checks and the different maintenance contracts.

```sh
cargo +1.89.0 bench -p skrin --bench game_world --no-run --locked
# Run the resulting executable separately; rotate mode order for three repeats:
/path/to/game_world-HASH /local/scratch native   100000 128 64
/path/to/game_world-HASH /local/scratch snapshot 100000 128 64
/path/to/game_world-HASH /local/scratch sqlite   100000 128 64
```

All generated scratch directories are retained even on success. Their paths are
in raw output. The smaller `67 73 13` workload also passed in all three modes,
including range wrapping and repeated item transfers; those smoke timings are
not part of this campaign. An independent iterative integration model validates
the reference formula rather than trusting a self-round-trip.

## Resident read costs

Each cell is the median of three **per-process** percentiles, not a percentile
of pooled samples. Each phase has 4,000 samples per process. Point reads retrieve
all entity fields; areas return up to 64 full entities, inventory initially one
item. Native values include materialization, view acquisition/release and timer
overhead. SQLite's isolated SELECTs use implicit read transactions; multi-query
frames use explicit transactions. No unnecessary BEGIN/ROLLBACK is added to a
single SELECT.

| Mode | Point p50 / p99 (µs) | Area p50 / p99 (µs) | Inventory p50 / p99 (µs) |
| --- | ---: | ---: | ---: |
| Skrin native | 0.689 / 1.476 | 9.991 / 43.563 | 1.721 / 2.806 |
| Skrin snapshots | 0.834 / 1.689 | 8.268 / 13.936 | 2.363 / 4.460 |
| SQLite | 2.148 / 4.009 | 21.150 / 52.365 | 2.589 / 3.915 |

This supports measuring Skrin's native access path further. It does not show a
uniform win: snapshot inventory p99 is worse than SQLite here. Both engines are
warmed, but Skrin holds native rows/indexes fully resident while SQLite caches
compact pages. The memory cost is materially different. There is no disk-cold,
large-asset, arbitrary-query or larger-than-RAM comparison.

## Durable saves and frame work

Per-process save tails are preserved to expose the adverse runs. Frame work
includes 64 point reads, an area, an inventory, a coherent-save watermark and
reference checks, not rendering/physics. Start lateness is separate.

| Mode / repeat | Saves/s | Save p50 / p99 / max (ms) | Frame samples | Frame work p99 (ms) |
| --- | ---: | ---: | ---: | ---: |
| Native 1 | 47.079 | 5.685 / 440.370 / 722.233 | 1 | 2718.830 |
| Native 2 | 98.120 | 10.439 / 15.731 / 15.825 | 1 | 1304.544 |
| Native 3 | 29.289 | 9.505 / 176.421 / 1704.625 | 1 | 4370.340 |
| Snapshot 1 | 58.225 | 8.527 / 56.714 / 938.425 | 132 | 0.124 |
| Snapshot 2 | 94.319 | 10.379 / 25.644 / 32.786 | 82 | 0.077 |
| Snapshot 3 | 121.489 | 6.559 / 13.650 / 15.299 | 64 | 0.079 |
| SQLite 1 | 7.908 | 11.433 / 2044.723 / 2231.802 | 972 | 0.302 |
| SQLite 2 | 100.078 | 9.814 / 16.057 / 17.700 | 77 | 0.311 |
| SQLite 3 | 125.441 | 6.175 / 14.196 / 15.239 | 62 | 0.248 |

Native readers completed no frames while the writer remained active in any run;
each sole sample waited until all saves finished. Their p99 is therefore one
empirical blocked observation, not a steady-state population estimate. This
baseline is unsuitable for this unpaced writer/frame access pattern.

All 132/82/64 snapshot frames and all 972/77/62 SQLite frames completed while
their writers were active. Both meet the workload's frame-work p99 target below
1 ms in all repeats. Snapshot maximum work was 0.136/0.077/0.079 ms; SQLite's
first run had a 2.097 ms maximum. Neither had work over 16.67 ms. Snapshot start
lateness reached 1.416 ms and SQLite 10.734 ms in the first runs; these are not
real-time guarantees. Snapshot readers last observed saves 126/127/127 and
SQLite 127/126/127 rather than the final 128; the last commits finished after
their last scheduled frame. Snapshot capture can also return the preceding
synchronized version during pending writer I/O, as documented by the API.

Save throughput/tails vary too much to establish a durable speed advantage.
SQLite is slightly faster in repeat 3 and its repeat 1 is severely slower;
native/snapshot adverse tails also remain. No cause for an individual stall is
inferred from these timings. Profile CPU work separately and repeat under quieter
conditions before choosing a write-path optimization. Every successful save
retains sync-before-publication/acknowledgment; no relaxed mode was compared.

## Memory, disk and lifecycle

Whole-parent saved-state RSS ranges are 64,100–64,272 KiB for native Skrin,
107,880–107,924 KiB for snapshots and 12,216–12,276 KiB for SQLite. These include
threads, adapters, sample buffers, allocator retention and cached state; they
are not exact engine allocation accounting. SQLite has a 64 MiB page-cache
setting per connection but does not fill it with this narrow dataset. The
snapshot availability benefit comes with substantial native-memory cost.

| Logical regular files | Native/snapshot Skrin (bytes) | SQLite (bytes) |
| --- | ---: | ---: |
| Before checkpoint | 11,552,280 | 29,218,664 |
| Published overlap / after reclamation | 23,162,004 | 5,627,904 |

All repeats have the same byte totals. Skrin retains active + previous
generations; with one checkpoint there is nothing old enough to reclaim.
SQLite checkpoint(TRUNCATE) reclaims its WAL and its separate reclaim step is
a no-op. The compact SQLite page format differs from Skrin's explicit u64
record codec. Disk counts exclude metadata, physical allocation and quota;
different retention contracts prevent interpreting these as equal capacity.

Checkpoint pauses range 153.254–425.991 ms native, 179.463–2224.261 ms snapshot
and 56.324–3094.372 ms SQLite. Fresh-process pre-checkpoint opens range
227.619–445.620 ms native, 189.583–1029.432 ms snapshot-origin storage and
10.500–1839.371 ms SQLite. Post-checkpoint opens range 169.541–340.498 ms native,
160.152–178.686 ms snapshot-origin storage and 0.406–0.438 ms SQLite.

Skrin eagerly decodes the entire world and rebuilds indexes when opening;
SQLite opens a connection with lazy data/index loading. Both then verify all
contents outside the open timer. Snapshot-origin children use the native open
path, so they do not time immutable-root construction. These open costs are
different amounts of work and are not a recovery-speed ranking. The parent's
reopen/root conversion before maintenance is also outside checkpoint timing.

The next optimization priorities are measured save/flush tails, per-row/index
memory, immutable publication and startup/maintenance work. These results make
snapshots useful for this frame pattern, while preserving the permanent SQL-free
product direction and current RAM/platform/maintenance boundaries.
