# Complete game journeys and current query controls — 2026-10-10

The native Rust, external SQLite and standalone SwiftData applications all
complete the same declaration → seed → indexed selection/join → atomic save →
rejected edit/rollback → fresh-process reopen journey. Every persisted model
field, row count, filtered selection and full joined result is checked. This
closes the previously missing executable application comparison. It is a
functional ergonomics comparison; no SwiftData speed or hardware-durability
comparison is claimed.

## Exact sources and environment

Base `ca02ab7fca821792d680bdd35e35090fdeee7185` plus the `edit` convenience,
production-path regressions and complete application controls accompanying this
report. All new API operations remain in the existing staging/validation/codec/
WAL/sync/publication path. Existing stored formats and golden fixtures remain
unchanged. Native and external SQL use two ordinary structs and a separate
catalog identity 14000, version 1; no existing application schema is silently
changed. SQL stays in a development-only adapter, never a Skrin dependency/API.

Apple M5, Mac17,2, 24 GiB RAM, macOS 27.0 (26A428). Rust 1.89.0
(29483883e, LLVM 20.1.7), aarch64, thin LTO/one codegen unit, no extra rustflags;
SQLite 3.53.2, bundled through pinned development-only rusqlite 0.40.2.
SwiftData control: Xcode 27.0 (27A266a), Swift 6.4
(swiftlang-6.4.0.34.1), macOS 27.0 SDK/runtime, normal executable compiler
settings. Future CI runs print their own SDK/compiler/runtime versions.

Commands:

```sh
rustup run 1.89.0 cargo bench --target-dir /tmp/skrin-cursor-isolated.pQZ6b8/new \
  -p skrin --bench api_journey --locked
# SwiftData: compile and run create/verify in separate processes as documented:
# comparisons/README.md
```

The native/SQLite control verifies both processes and removes its own successful
fixture directory. SwiftData leaves its generated store/cache in a new owned
scratch directory. All models contain public deterministic fixture data.
[The complete procedure](../../comparisons/README.md) describes the model,
ordering, limits, save and rejected edit; all setup/mapping/transaction code is
visible in the [Rust control](../../crates/skrin/benches/api_journey.rs) and
[Swift control](../../comparisons/swiftdata_game.swift).

## Practical API differences

| Application work | Skrin | SQL control | SwiftData control |
| --- | --- | --- | --- |
| Declare persisted fields | Ordinary struct + Record derive; explicit stable schema and one catalog declaration | Ordinary Rust struct plus CREATE TABLE definitions, bind lists and column extraction | @Model classes, unique IDs and explicit indexes; native typed fields |
| Indexed filtered/ordered selection | Index marker infers row/key, native range, ordinary filter/take | Prepared SELECT, parameter binding and result mapping | Typed FetchDescriptor/#Predicate, key-path sorting and fetch limit |
| Owner/kind join | matching(OwnerKind, &(owner, kind)) + typed owner get on the same view | Prepared JOIN and column mapping | Typed item fetch + typed owner fetch; scalar IDs are the common model here |
| Change character and item | One write closure, two edit-by-ID callbacks; index changes automatic | Transaction, two UPDATE statements, explicit commit | Fetch two managed models, mutate within context.transaction |
| Reject staged application changes | Propagated closure error drops the whole staging set; a caught callback error drops only that edited clone | rusqlite transaction DropBehavior rolls back by default | This control explicitly calls context.rollback after the throwing transaction |
| Stable retained frame | Optional immutable borrowed row/index snapshot, explicit lease budget | SQLite has transaction snapshots; not a new capability claim for Skrin | This control retains managed model instances; SwiftUI/relationship features are outside this scalar-data journey |

The concrete ergonomics improvements are shared field definitions without SQL
column mapping, inferred indexed query types/order, ordinary language iterator
composition, direct field edits by typed table/ID, and a fresh rollback scope for
every write closure. `edit` eliminates the complete-record reconstruction that
previously made simple field changes verbose. It calls Clone on the selected
record once; no other records are cloned. It cannot return a mutable reference
to committed or staged data. The non-Clone `update` path remains available.
These are executable API properties in the target Rust game workflows, not a
claim that every developer/language/framework prefers the same syntax.

SwiftData also offers typed predicates and model declarations. Its direct
managed setters and SwiftUI/relationship integration can be preferable for a
Swift UI application; Skrin does not provide those integrations. Adding a
catalog declaration is deliberate explicit schema work, not zero setup. Clone
can copy large owned fields; use update for controlled replacement construction.

In this identified local SwiftData run, the intentionally throwing transaction
left `hasChanges=true` before explicit rollback. Rollback and fresh-process
verification restored both rejected records to their committed values. This is
observed SDK/runtime behavior, not a universal framework rule. Apple documents
[transaction](https://developer.apple.com/documentation/swiftdata/modelcontext/transaction(block:))
and [rollback](https://developer.apple.com/documentation/swiftdata/modelcontext/rollback()).
The SQLite control also propagates an error through a Rust closure and uses
[rusqlite's documented default rollback on transaction drop](https://docs.rs/rusqlite/0.40.2/rusqlite/struct.Transaction.html).
No engine is charged for an unnecessary manual save/rollback path.

## Current-source query measurements

Both existing query harnesses were recompiled against this API source in their
isolated target directory before timing. Five fresh processes per harness;
SQLite-first in repeats 2 and 4, Skrin-first in 1/3/5. No concurrent Skrin
build/tests during timings. Workstation scheduling/thermal/cache interference
is uncontrolled. Every returned field matches independent arithmetic outside
query timing. Full result-vector creation is inside timing; preparation, seed,
index creation, snapshot conversion and warmups are outside timing. The
[methodology](../benchmarks.md) retains strong reused prepared/covering SQLite
controls and validated seek plans.

For varied requests: 100,000 entities + 100,000 items, 2,000 timed requests per
workload and 128 warmups. For cursor pages: 100,000 three-field rows in one equal
key group, 128 samples plus eight warmups at each front/middle/late position.
That cursor repeats and stays hot; it is not a varied world-query advantage.
A first cursor invocation had an incorrect executable filename and did not run;
its corrected invocation and all five complete measurements are retained.
No timing result was discarded.

Microseconds. Median/ranges below aggregate all five per-process statistics;
worst is the maximum individual sample across all five processes.

### Varied queries

| Mode | Workload | p50 median [range] | p99 median [range] | Worst |
| --- | --- | ---: | ---: | ---: |
| native | bounded | 9.416 [9.375–9.500] | 13.083 [12.791–13.917] | 27.959 |
| native | filtered | 2.875 [2.833–2.916] | 3.625 [3.583–3.916] | 19.250 |
| native | page | 3.208 [3.167–3.291] | 4.125 [3.917–4.541] | 16.333 |
| native | owner_join | 4.042 [4.000–4.125] | 6.500 [6.125–6.625] | 23.583 |
| native | compound_join | 1.084 [1.042–1.125] | 1.625 [1.583–1.750] | 8.125 |
| snapshot | bounded | 6.583 [6.334–6.667] | 9.334 [8.209–10.000] | 45.958 |
| snapshot | filtered | 2.166 [2.083–2.167] | 2.875 [2.708–3.000] | 22.042 |
| snapshot | page | 2.417 [2.416–2.459] | 3.250 [3.167–3.458] | 20.458 |
| snapshot | owner_join | 3.042 [2.959–3.166] | 4.583 [4.417–5.458] | 17.625 |
| snapshot | compound_join | 1.167 [1.166–1.250] | 2.000 [1.875–2.167] | 8.667 |
| sqlite_covering | bounded | 8.709 [8.708–8.792] | 11.625 [11.500–14.666] | 32.417 |
| sqlite_covering | filtered | 1.709 [1.708–1.791] | 2.291 [1.917–2.292] | 17.917 |
| sqlite_covering | page | 2.208 [2.167–2.250] | 2.584 [2.500–2.958] | 17.375 |
| sqlite_covering | compound_join | 0.667 [0.666–0.708] | 1.042 [1.000–1.083] | 9.459 |

### Hot cursor pages

| Mode | Position | p50 median [range] | p99 median [range] | Worst |
| --- | --- | ---: | ---: | ---: |
| native_prefix | front | 0.875 [0.833–1.125] | 1.875 [1.542–2.083] | 2.417 |
| native_prefix | middle | 2632.833 [2625.292–2682.208] | 2740.083 [2718.667–2860.500] | 2887.625 |
| native_prefix | late | 5317.541 [5311.458–5409.375] | 5526.125 [5410.209–5747.917] | 5896.833 |
| native_seek | front | 0.833 [0.792–0.833] | 0.875 [0.834–0.959] | 7.417 |
| native_seek | middle | 0.792 [0.792–0.833] | 0.875 [0.834–1.042] | 1.042 |
| native_seek | late | 0.833 [0.833–0.875] | 0.875 [0.875–1.542] | 7.541 |
| snapshot_prefix | front | 0.667 [0.667–0.750] | 1.875 [1.750–2.500] | 2.583 |
| snapshot_prefix | middle | 1629.250 [1548.708–1655.125] | 1770.708 [1708.750–1781.541] | 1882.209 |
| snapshot_prefix | late | 3173.459 [3121.667–3266.750] | 3295.042 [3212.208–3476.833] | 4012.958 |
| snapshot_seek | front | 0.625 [0.584–0.792] | 0.750 [0.625–0.917] | 4.083 |
| snapshot_seek | middle | 0.625 [0.625–0.792] | 0.667 [0.667–0.833] | 0.834 |
| snapshot_seek | late | 0.542 [0.542–0.708] | 0.584 [0.584–0.750] | 0.750 |
| sqlite_covering | front | 1.250 [1.209–1.250] | 1.375 [1.292–1.417] | 3.958 |
| sqlite_covering | middle | 1.292 [1.250–1.333] | 1.416 [1.334–1.792] | 2.000 |
| sqlite_covering | late | 1.250 [1.250–1.333] | 1.334 [1.334–1.458] | 1.500 |

### Advantages and costs

The current-source repeated controls establish two distinct useful query
advantages over optimized SQLite: **snapshot full two-area selection is 1.32×
faster at p50** (6.583 versus 8.709 µs); **native hot cursor pages are 1.50–1.63×
faster at p50** (0.833/0.792/0.833 versus 1.250/1.292/1.250 µs). All native
cursor and snapshot bounded per-process medians are lower than every matching
SQLite median. Current snapshot cursor p50 also wins (0.625/0.625/0.542 µs),
but the historical report has adverse snapshot repeats; no universal improvement
from edit is claimed. These fresh controls confirm advantages on current API
source; adding edit did not deliberately change these read paths.

SQLite wins all varied filtered/page/compound-join median controls. Baseline
native bounded reads also remain slower. Native maximum hot-cursor samples
still exceed SQLite maximum samples in front/late cases, despite faster median
and per-process p99; this is not a blanket tail-latency advantage. The snapshot
bounded maximum reaches 45.958 µs versus SQLite 32.417 µs. All adverse samples
remain visible.

The earlier [game-world report](game-world-performance-2026-10-08.md) separately
records point/area reads and queued independent durable save advantages, plus
SQLite wins at atomic batches, RAM and post-maintenance disk footprint. Those
costs are not disproved by these resident controls. This report does not claim
cold-cache, concurrent-writer, RAM/storage, SwiftData-speed or whole-game speed
advantages. Every Skrin row/index remains resident, baseline readers block
writers, and immutable leases consume explicitly accounted memory. Persistence
remains Unix-only/experimental with explicit maintenance, retention and headroom.
Neither common application control certifies physical power-loss behavior.

## API quality and reproduction evidence

The new edit regressions cover all four writers: complete/caught callback errors,
missing keys, current staging, unique final-view rollback, old/current immutable
indexes, codec refusal with identical WAL bytes/sync counts, every short-write
and ENOSPC append prefix, uncertain sync, poison and actual WAL recovery. A
1,000-row test counts Clone invocations: one selected record per edit, zero for
missing records, no full-database row copies. Positive docs and compile-fail
docs cover successful field edits, the Clone boundary and mutable-reference
escape refusal. Existing non-Clone fixtures and all format/recovery assertions
are preserved. MSRV/stable, warnings-as-errors and platform CI remain required.

Raw functional and repeat evidence:

- [cursor-1.txt](api-journeys-2026-10-10/cursor-1.txt).
- [cursor-2.txt](api-journeys-2026-10-10/cursor-2.txt).
- [cursor-3.txt](api-journeys-2026-10-10/cursor-3.txt).
- [cursor-4.txt](api-journeys-2026-10-10/cursor-4.txt).
- [cursor-5.txt](api-journeys-2026-10-10/cursor-5.txt).
- [native-sqlite.txt](api-journeys-2026-10-10/native-sqlite.txt).
- [query-1.txt](api-journeys-2026-10-10/query-1.txt).
- [query-2.txt](api-journeys-2026-10-10/query-2.txt).
- [query-3.txt](api-journeys-2026-10-10/query-3.txt).
- [query-4.txt](api-journeys-2026-10-10/query-4.txt).
- [query-5.txt](api-journeys-2026-10-10/query-5.txt).
- [swiftdata-create.txt](api-journeys-2026-10-10/swiftdata-create.txt).
- [swiftdata-verify.txt](api-journeys-2026-10-10/swiftdata-verify.txt).

Source SHA-256 values (including the API implementation used by the controls):

- `crates/skrin/benches/api_journey.rs`: `1d01bbbc317ac4f53bbd2a736f547469e1551d51aea9197554a6ce53eabf0e05`.
- `comparisons/swiftdata_game.swift`: `73ba073fc386503077c7688c276cfb1cefcc56e4edecb817d39b750ed924f8fb`.
- `crates/skrin/benches/game_queries.rs`: `fdd05b135c2cae2bb3984e2f30505a27882d3884b222d4b8b4f78cdf191ee07a`.
- `crates/skrin/benches/cursor_queries.rs`: `6fa5eb9359edd50e46a8dd878546fc47e76adea9c0f9a238dcda9ab2f182856b`.
- `crates/skrin/src/database.rs`: `3048d9a76d1dfb40fec6606eb9046e94c452a8bfe9df5aa470f73f05c86a55cf`.
- `crates/skrin/src/catalog.rs`: `d3f1fc72978a53f5459ab61bb5ab30645978b62f526d1f352e041b813085af03`.
- `crates/skrin/src/versioned.rs`: `62e89d941b98faa8367508e3a575636fb04fba2a6f8e23abe5f28a604f481e86`.
- `crates/skrin/src/versioned_catalog.rs`: `23004216381752043651be98823176e1c05aa3df237315727d1abe35fe36b68d`.
