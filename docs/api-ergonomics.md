# Ergonomics and measured advantages

Skrin's target is a small native API that makes Rust game database work easier
than direct SQL and the corresponding SwiftData workflow, with several measured
advantages over SQLite. The complete application controls and current-source
query repeats now provide direct evidence for the target paths below. A
subjective preference or a faster microbenchmark alone is insufficient.

## Verified application requirements

| Requirement | Executable evidence | Deliberate boundary/cost |
| --- | --- | --- |
| Declare ordinary application models once | Record derive and catalog! power accounts, signed/float/bool/optional character models and the complete two-table control; generated/manual persistent bytes are identical | Explicit schema IDs/versions and migrations; unsupported/custom fields use manual codecs |
| Native typed indexed queries and composition | Markers infer table/key; query/matching/query_after compose Rust predicates/projections/limits; tuples select owner/kind without an application byte codec | Application selects indexes; all rows/indexes fit RAM; no query optimizer or implicit relationships |
| Convenient field changes with clear rollback | edit-by-ID callbacks in all four writers; complete native/SQL/SwiftData save/error/reopen controls verify every field and join | One selected record Clone per edit; update remains non-Clone; caught statement errors preserve earlier staging |
| Multiple useful SQLite advantages | Current-source five-process controls: snapshot full area selection and native hot cursor pages; earlier point/area and queued independent-save reports provide separate workload evidence | SQLite wins varied filtered/page/join medians, atomic batches, RAM and disk cost; no universal speed claim |
| High-quality public API | Current staging, missing rows, unique final-view rollback, retained frames, clone-count tests, compile-fail type/lifetime checks, production WAL faults and golden compatibility fixtures | MSRV/stable/platform CI, warnings-as-errors and documented persistence/reader/maintenance limits remain required |

[The complete application report](measurements/api-journeys-2026-10-10.md)
contains exact sources, compiler/runtime versions, functional logs, all timing
repeats and observed failure handling. The [comparison guide](../comparisons/README.md)
makes native, prepared SQL and standalone SwiftData model → query → join → save →
error/rollback → fresh-process reopen paths independently runnable. Setup,
metadata, indexes, result mapping and failure handling stay visible.

## The practical ergonomics improvement

In the common journey, native queries reuse the declared index's row type and
ordering, plus ordinary language ranges and iterators. The application needs no
SQL column extraction or predicate descriptor/key-path sort construction.
SwiftData also offers typed predicates and model declarations; those capabilities
are represented in the control.

For field edits, a typed ID selects the value inside one write scope:

```rust
db.write(|tx| {
    tx.edit::<Characters>(7, |player| {
        player.x = 128;
        player.health = 0.0;
        player.alive = false;
        Ok(())
    })?;
    tx.edit::<Items>(101, |item| {
        item.owner = 9;
        Ok(())
    })
})?;
```

This actual save in [api_journey.rs](../crates/skrin/benches/api_journey.rs)
replaces the previous full-record reconstruction for simple changes. It avoids
separate model fetches before field assignment and gives every native write a
fresh staging scope. Propagating an error rolls back all rows/indexes. A rejected
edit callback discards its private cloned value, even when the caller catches
the error. Successful outer writes still wait for the configured persistence
boundary; storage uncertainty poisons the handle. These are concrete advantages
for the target Rust game paths, not a universal ranking of languages/frameworks.

Clone is explicit and can copy large owned fields. Records without Clone use
update; it still accepts a complete replacement from a borrowed current value.
A successful edit stages a replacement even with unchanged fields. There is no
implicit dirty tracking. Mutable callback references cannot escape. Retained
immutable row/index frames remain coherent across committed edits.

SwiftData's direct managed setters, observation and automatic relationships can
be preferable for Swift UI applications; Skrin has no SwiftUI integration. The
comparison intentionally uses scalar owner IDs in every engine. Its explicit
context rollback is verified on the named SDK/runtime, not asserted as a rule
for all framework versions. SQLite through rusqlite also has default transaction
rollback on drop; the control uses it. No artificial failure-handling boilerplate
is charged to those alternatives.

## Measured advantages and limits

The [current-source report](measurements/api-journeys-2026-10-10.md) records
snapshot full two-area p50 **1.32× faster** than a reused prepared covering
SQLite query, and native hot cursor pages **1.50–1.63× faster** than a covering
SQLite ID seek. Their complete result vectors match independent arithmetic in
all five fresh process repeats. The same report preserves SQLite's wins in all
varied filtered/page/compound-join controls and higher individual Skrin tails.
Adding edit is an ergonomics change, not a claimed read-path optimization.

[The earlier game-world comparison](measurements/game-world-performance-2026-10-08.md)
records point/area reads and queued independent durable saves, plus SQLite wins
at atomic batches, RAM and smaller post-maintenance storage under different
retention policies. [The earlier cursor report](measurements/cursor-queries-2026-10-10.md)
retains adverse snapshot repeats. Later controls do not erase those results.
No SwiftData speed, cold-cache or whole-game performance advantage is established.

The API/application target has executable evidence for these ordinary Rust game
journeys; production certification and feature parity with every SQL/ORM/UI
framework remain outside it. Persistence is Unix-only and experimental, all
rows/indexes remain resident, baseline read guards block writers, and snapshot
leases/maintenance/retention/headroom are explicit application responsibilities.
Read [models](models.md), [queries](queries.md), [durability](durability.md) and
[managed storage](managed-storage.md) for the complete contracts.
