# Direction: durable native state for games

Skrin's product target is fast, embedded, typed state for Rust applications,
starting with a game world. Persist changed data, retrieve native values and
publish coherent transactions with predictable application costs. SQL syntax,
a query planner, replication and a network service are outside this milestone.
The application's Rust schema and operations define the API. A comparator does
not define Skrin's architecture.

## Executable first workload

```sh
cargo +1.89.0 run -p skrin --example game_world --locked
# On Unix, use a NEW path under an existing parent:
cargo +1.89.0 run -p skrin --example game_world --locked -- /local/scratch/new-world
```

The application catalog has Entities, Items and Saves. Entity positions use
integer millimetres, with an area and revision. An area index retrieves the
entities in one area; an owner index retrieves an inventory. A save moves a
bounded range of entities, transfers one item to another entity and records
the complete operation ID/request in one transaction. Missing ownership or a
recipient refuses the whole operation, including staged positions. This is an
application-defined consistency rule, not a built-in foreign-key feature.

The example reads one coherent frame snapshot while a separate worker saves.
The old frame preserves its positions and inventory; a new frame sees both
changes together after successful synchronization. The worker's successful
result acknowledges the save. Enqueueing/spawning work is not an acknowledgment.
Retrying the identical request is a no-op, and reusing its ID with different
contents is refused. Persistent runs additionally checkpoint, reclaim, make
a verified independent backup and reopen both copies. Existing destinations
are never overwritten. In-memory runs are explicitly volatile.

The deterministic save generator is a reproducible example workload, not an
ECS, arbitrary gameplay API, world-streaming system or geometric spatial index.
The simulation can retain its active data in its ECS and submit dirty values to
Skrin. A renderer can use the preceding saved snapshot while the next save is
pending; gameplay requiring immediate unsaved changes reads the simulation's
own state. Persistence and simulation freshness must be chosen deliberately.

## What fast means here

The first representative dataset is 100,000 entities plus 100,000 items. Each
save updates 64 entities, moves an item and stores its operation ID. Measure:

| Work | Acceptance/evidence sought |
| --- | --- |
| Point and area/owner lookup | Correct typed results; include view acquisition, materialization and release |
| Durable save | Same success-after-sync boundary; p50/p95/p99/max and updates per second |
| Frame work while saving | Target p99 below 1 ms for 64 lookups plus area/inventory reads, within a 16.67 ms frame budget; report actual sample count and stalls |
| Storage lifecycle | Exact rows, indexes and operations in a fresh process before and after checkpoint/reclaim |
| Resource cost | Process RSS/high-water observations, logical disk overlap and explicit reader retention limits |

These are workload targets, not existing guarantees or a real-time promise.
The synthetic frame includes coherence checks but no rendering, physics or
complete engine scheduler. Repeated physical-device runs determine whether the
targets hold. Long frames and adverse repeats stay in the report.

SQLite is one external yardstick for the equivalent application operation. It
uses prepared statements, corresponding indexes, application-side typed
get/modify/replace, WAL/FULL synchronization and explicit checkpoints. Skrin's
engine/public API does not depend on SQLite: the adapter is benchmark-only via
a pinned development dependency. Same useful work and durability matter more
than matching implementation details. See [methodology](benchmarks.md#game-world).

## Optimization order

1. Establish the real-device baseline with this workload and a smaller wrapped
   dataset. Separate resident read/CPU costs from save synchronization and
   recovery/maintenance costs.
2. Profile the largest measured costs: changed-row encoding, allocation,
   checksum work, index traversal and immutable publication. Try representations
   suited to entity IDs and area batches when evidence supports them. Preserve
   useful ordered/range operations; do not select a tree or hash table merely
   because another database uses it.
3. Measure bounded dirty-row batches and independently queued group commit.
   Reduce synchronization per useful changed value while preserving the
   configured acknowledgment boundary. The first harness has one producer;
   it does not claim a group-commit benefit.
4. Measure a held frame version during saves and maintenance, including memory
   retention and read freshness. Optimize background scheduling and bounded
   work where measured frame tails justify it.
5. Implement and verify Windows persistence before calling Skrin broadly usable
   for desktop games. Codec ergonomics and larger-than-RAM area loading follow
   concrete application needs, rather than speculative public APIs.

## Current operational boundaries

All active rows and indexes fit in RAM; there is one serialized writer.
Baseline borrowed guards block writers. Immutable snapshots have short internal
locks and conservative lease admission limits, not a process-memory cap or a
real-time scheduling guarantee. The example uses four leases and 512 MiB of
cooperative pin accounting; current rows, allocator overhead and application
memory are separate.

Maintenance remains explicit and serialized. Retained previous generations,
replacement headroom and recognized abandoned files require application policy.
Unknown contents are preserved. Operation-ID rows grow until the application
defines a safe retry-retention policy; deleting them casually defeats retries.
Persistent storage is Unix-only. The engine remains experimental, with the
existing [durability](durability.md) and [managed-storage](managed-storage.md)
failure contract, explicit codecs and unchanged format fixtures.
