# Ergonomics and measured advantages

The product target is an API that makes application database work easier than
SQL and SwiftData, with multiple measured advantages over SQLite and a small,
coherent public API. Neither subjective preference nor one fast microbenchmark
establishes that target. Keep the complete application paths executable and
compare equivalent work, failure contracts and costs.

## Current evidence and remaining work

| Requirement | Current evidence | Remaining work |
| --- | --- | --- |
| Declare ordinary application models once | `Record` derive and `catalog!` power accounts, game-world and signed/float/bool/optional character models; generated/manual persistent bytes are identical | Covered current model fields; custom codecs and explicit migrations remain deliberate boundaries |
| Query with native types and ordinary Rust composition | Typed index markers and native bounds/equality infer records and keys; lazy rows support predicates, projections, limits and same-frame joins | Avoid repeated application cursor code; measure cursor seeking rather than repeatedly filtering duplicate prefixes |
| Make writes and failure recovery understandable | Complete character model → typed query → atomic edit → WAL/checkpoint/backup reopen, plus duplicate/unique rollback, retries, retained frames and explicit migrations | Compare complete application declaration/query/save/recovery work with the external SQL/SwiftData controls |
| Beat SQLite at several useful operations | [Repeated game-world comparison](measurements/game-world-performance-2026-10-08.md): resident point/area reads and queued independent durable saves | Extend the comparison to filtered, bounded, paginated and joined game queries using a strong SQLite control and repeated process runs |
| Keep a high-quality public API | Manual traits remain available; no SQL, unsafe code, implicit migration or persistence downgrade; compile-fail and storage compatibility tests | Assess type errors, empty/full-u64 cases, poison, documentation and cross-platform/MSRV behavior on every new API |

The repeated comparison already records native point p50 2.59× faster and
snapshot area p50 2.44× faster on the specified resident workload. It also
records cases where SQLite wins: atomic-batch writes, much less RAM and smaller
post-maintenance storage under different retention policies. Those costs remain
part of the comparison. New declarations alone make no performance claim.

## Compare actual application work

Use model declaration, visible-entity selection, inventory/owner lookup, atomic
save and recovery as the common journeys. Compile/run the Skrin variants and
check exact results against independent application data. Count application
declarations and necessary query/transaction code only when the same work and
contracts are represented. Keep setup, metadata, index declarations, model
mapping and failure handling visible; hiding them in an adapter is not an
ergonomic improvement.

SwiftData's [`@Model`](https://developer.apple.com/documentation/swiftdata/model())
generates managed model conformance from a class. Its
[`FetchDescriptor`](https://developer.apple.com/documentation/swiftdata/fetchdescriptor)
describes typed predicate/sort/limit fetches and can prefetch relationships;
SwiftUI also has query integration. These are useful existing capabilities,
not evidence of shortcomings. Skrin currently offers ordinary Rust values and
composable closures, explicit transactions and retained immutable frames; it
does not offer SwiftUI observation or automatic relationship management.

SQLite's [prepared-statement interface](https://www.sqlite.org/cintro.html)
compiles statements, binds values, steps results and extracts columns, and
supports statement reuse. Benchmark/application comparisons must reuse prepared
statements and appropriate indexes rather than charging SQLite preparation
repeatedly when the application can avoid it. SQL remains confined to external
comparison adapters and can never become a Skrin API or engine dependency.

The complete ergonomics/performance target remains open until the missing paths
above have direct current evidence. Passing model tests alone does not finish it.
