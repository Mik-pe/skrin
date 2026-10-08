# Verify a Skrin change

Run these commands from the repository root. Formatting is pinned to Rust 1.89; CI tests both 1.89 and stable on Linux/macOS, plus Windows memory mode. Read the architecture, durability, format and managed-storage contracts before changing the engine. Use an isolated focused PR, inspect overlapping work, and let the exact final commit pass every required check before normal merge.

## Required local checks

```sh
cargo +1.89.0 fmt --all -- --check
cargo +1.89.0 clippy --workspace --all-targets --locked -- -D warnings
cargo +1.89.0 test --workspace --locked
cargo +1.89.0 test --workspace --release --locked
RUSTDOCFLAGS='-D warnings' cargo +1.89.0 doc --workspace --no-deps --locked
cargo +1.89.0 check --workspace --all-targets --locked
cargo +1.89.0 run -p skrin --example accounts --locked
cargo +1.89.0 run -p skrin --example snapshots --locked
cargo +1.89.0 run -p skrin --example game_world --locked
```

If local tooling/platform support is missing, record that fact and use the corresponding CI job as evidence. An unexecuted command is not a passing check. Windows tests do not establish persistent-storage support.

## Persistent lifecycle examples

On Linux/macOS, create an exclusive scratch parent and pass **new child paths**. Reusing a database path can exercise reopen rather than the complete creation lifecycle; a backup destination must be absent.

```sh
scratch=$(mktemp -d)
cargo +1.89.0 run -p skrin --example accounts --locked -- "$scratch/accounts.skrin"
cargo +1.89.0 run -p skrin --example lifecycle --locked -- "$scratch/lifecycle" "$scratch/backup"
cargo +1.89.0 run -p skrin --example maintenance --locked -- "$scratch/maintenance"
cargo +1.89.0 run -p skrin --example banking --locked -- "$scratch/banking"
cargo +1.89.0 run -p skrin --example group_commit --locked -- "$scratch/groups"
cargo +1.89.0 run -p skrin --example snapshots --locked -- "$scratch/snapshots"
cargo +1.89.0 run -p skrin --example game_world --locked -- "$scratch/world"
```

These examples verify reopen, independent backup, migration, operation-ID retry and reader admission where applicable. Inspect or remove the test-owned scratch parent after the commands finish. A temporary/container filesystem run is correctness evidence, not identified physical-device performance.

## Real resource failures

Normal `cargo test` reports three Linux environment-specific entries as ignored. Run their wrappers explicitly; worker/recovery entrypoints require ordered setup and are not standalone checks.

```sh
scripts/test-storage-full.sh
scripts/test-storage-full.sh --release
scripts/test-memory-budget.sh
scripts/test-memory-budget.sh --release
```

The storage-full wrapper mounts and exhausts its own bounded tmpfs in a private namespace. It requires unprivileged user namespaces locally; CI uses `--privileged-namespace` through passwordless sudo. It does not fill the caller's filesystem.

The memory wrapper requires systemd and a cgroup v2 memory controller. Its private worker verifies a 64 MiB/no-swap limit before deliberately excessive decoder allocation; the parent requires actual kernel OOM and independently verifies recovery. Local default uses the user manager; CI uses `--privileged-manager` to launch the worker as the invoking user through the system manager. A missing controller, setup error or timeout fails, rather than silently skipping enforcement. Failed memory-test evidence is retained at the printed path.

See [resource limits and failure handling](maintenance.md). Neither wrapper proves device-cache, torn-sector or physical power-loss safety. Physical power-loss testing is owner-deferred for the current software milestone because no disposable hardware is available.

## Relevant benchmarks

Compile and run the affected benchmark when changing an API or hot path. The harnesses include correctness checks; the baseline checks exact rows/sequences after volatile phases, synced phases and reopen outside the timed sections. [CI](../.github/workflows/ci.yml) contains bounded smoke invocations, including low-load/saturation group and reader comparisons. Use the existing scratch parent argument required by each persistent harness.

| Harness | Work to validate |
| --- | --- |
| `baseline` | Native map, volatile database and immediate synced transaction baseline |
| `maintenance` | Checkpoint/prune and independently verified reopen |
| `storage_scale` | Larger verified row workloads |
| `catalog` | Multi-table transfers and atomic unique/non-unique indexes |
| `resources` | Sustained update/delete/insert, process memory, disk overlap and cache observations |
| `group_commit` | Equivalent durable immediate/group writes, queue/latency/group size |
| `snapshots` | Four durable write/read modes, held readers, retention and maintenance/recovery |
| `game_world` | Typed area/inventory reads, atomic dirty-state saves, coherent synthetic frames and exact fresh-process recovery, with optimized SQLite independent/batch controls and windows 1/8 |

Use [benchmark methodology](benchmarks.md) and the checked-in raw measurements. Keep source/compiler/device/filesystem, workload and synchronization comparable; report tails and adverse repeats, not just a best throughput number. Advisory file-cache eviction does not control device caches, and cooperative pinned-byte accounting is not RSS or an allocator cap.

## Where the milestone evidence lives

| Completed software milestone | Executable evidence and contract |
| --- | --- |
| [#3: storage](https://github.com/Mik-pe/skrin/issues/3) | [Managed integration tests](../crates/skrin/tests/managed.rs), [production boundary/survival tests](../crates/skrin/src/directory_tests.rs), [managed protocol](managed-storage.md) and [sustained resource measurements](measurements/resources-2026-10-07.md) |
| [#5: maintenance hardening](https://github.com/Mik-pe/skrin/issues/5) | [Budget/inventory tests](../crates/skrin/tests/maintenance.rs), [real ENOSPC](../crates/skrin/tests/storage_full.rs), [real OOM recovery](../crates/skrin/tests/memory_budget.rs), [maintenance contract](maintenance.md) and [platform flush review](flush-contract-review.md) |
| [#6: atomic catalog](https://github.com/Mik-pe/skrin/issues/6) | [Catalog reference/integration tests](../crates/skrin/tests/catalog.rs), [production catalog faults](../crates/skrin/src/catalog_tests.rs), [catalog contract](catalog.md) and [recorded workload](measurements/catalog-2026-10-07.md) |
| [#7: measured concurrency](https://github.com/Mik-pe/skrin/issues/7) | [Group faults/acknowledgments](../crates/skrin/src/group_commit_tests.rs), [single-table versions](../crates/skrin/src/versioned_tests.rs), [catalog versions/checkpoint overlap](../crates/skrin/src/versioned_catalog_tests.rs), [group measurements](measurements/group-commit-2026-10-07.md) and [held-reader measurements](measurements/snapshots-2026-10-07.md) |

Issue closure records software delivery, not production certification. Keep experimental status, disabled publishing, explicit codec/schema compatibility, conditional synchronization guarantees and the [remaining documented boundaries](roadmap.md). Do not choose a license, weaken checks, enable auto-merge or bypass merge gates to deliver a change.
