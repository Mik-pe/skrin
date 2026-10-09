# Native typed queries for a game world

Skrin queries use Rust table markers, declared indexes and ordinary iterators.
`CatalogRead` and immutable `CatalogSnapshot` both provide `query(index, bounds)`
and `matching(index, &key)`. The schema-owned marker infers the table, record and
key types; queries cannot mix indexes from different catalogs or pass a string
to a numeric index. Both borrow native rows, visit key then primary-key order,
and yield lazily. No row decoding, row cloning or full result vector
is required by the engine. The application chooses its index and key encoding.
SQL syntax, parsers and compatibility APIs remain permanently excluded.

## Choose an access path

| Operation | Native access path |
| --- | --- |
| Entity by ID | `get::<Entities>(id)` |
| Entities in one/more areas | `query(Area, start..=end)` |
| Items owned by one entity | `matching(Owner, &owner)` |
| Continue an indexed page | `query_after(Area, start..=end, (&last_area, last_id))` |
| Additional predicates/projection/limit | Rust `filter`, `map`, `take`, then optionally `collect` |
| Item plus its owner | Item scan and `get::<Entities>(item.owner)` on the same read view |
| Explicit table scan | `scan::<T>()`, ordered by primary key |

`lookup` and `index_range` still collect borrowed rows into vectors. Write
transactions retain their existing staged `lookup`/`index_range` semantics,
including earlier staged changes and temporary duplicates before final unique
validation. Lazy scans are read-view operations.

The game schema encodes area/owner index keys as big-endian u64 bytes, so numeric
order matches lexicographic order. Record codecs remain explicit little-endian
fields. Custom sorting/collation comes from a versioned application projection;
changing a projection requires the catalog's existing migration protocol.

## Native keys and bounds

Declare `Area: Entities { id: 1, version: 1, unique: false,
key: u64 => |row| row.area }` in `catalog!`. It generates `Area: Index<World>`;
the same big-endian key codec serves both persisted projections and query bounds.
Built-in keys are all fixed-width signed/unsigned integers, bool, str and [u8].
Signed keys preserve numeric order across negative/positive values; float keys
require an explicitly chosen application codec/collation. Match strings with
`read.matching(ByName, "Ada")`; strings use case-sensitive UTF-8 byte order.
For unsized keys Rust's standard borrowed bound pairs work:
`read.query(ByName, (Included("Ada"), Excluded("Zoe")))` (import
`std::ops::Bound::{Included, Excluded}`). Numeric keys use ordinary ranges,
including `..`, `..=end`, `start..` and `start..=end`. Full-u64 keys need no casts
or sentinel value. Empty valid ranges yield no rows. Iterators borrow only their
read view; input keys and bounds can be dropped immediately after construction.

Typed calls validate the full declared index definition and encoded key size.
Reversed bounds and equal excluded endpoints return `InvalidOperation`, including
empty indexes. Key codec errors propagate before traversal. Custom key codecs
must preserve intended ordering and canonical equality. Their semantics and
versions are application contracts; the engine cannot infer an unversioned
change. Existing raw `index_scan::<T>` remains available and retains its
BTreeMap-style panic for invalid byte bounds. No disk format changes.

## Executable queries

```sh
cargo +1.89.0 run -p skrin --example game_queries --locked
# On Unix, supply a NEW path under an existing parent:
cargo +1.89.0 run -p skrin --example game_queries --locked -- /local/scratch/new-queries
```

The [complete example](../crates/skrin/examples/game_queries.rs) and
[application query functions](../crates/skrin/examples/support/world_queries.rs)
use the existing World catalog. Area selection, position predicates and a
result limit compose directly:

```rust
let visible = frame
    .query(Area, start..=end)?
    .filter(|(_, row)| query.x.contains(&row.x) && query.y.contains(&row.y))
    .take(limit)
    .map(|(key, row)| (key, *row))
    .collect::<Vec<_>>();
```

The example pages visible entities seven at a time, filters inventory by item
kind and joins each item to its typed owner in the same frame. It saves atomic
position/inventory changes, verifies old/current query results and operation-ID
retry, then verifies the queries after WAL reopen, checkpoint, independent backup
and snapshot reopen. Volatile mode performs no persistence. Existing persistent
paths and backup destinations are never overwritten.

## Stable pagination and coherence

An area's application cursor contains **both `(area, primary_key)`**, matching
the full index ordering. `query_after(Area, original_bounds, (&area, primary_key))`
seeks strictly after that position; the cursor need not be an existing row.
Original bounds still apply: a cursor before them starts normally and a cursor
after them returns no rows. Invalid original bounds and bad cursor codecs remain
errors, including empty indexes. Input cursor keys can be dropped immediately
after constructing the iterator, including borrowed str/blob keys.

The borrowed baseline seeks into a large equal-key `BTreeSet` posting; the
immutable snapshot seeks its `(index, byte key, primary key)` tree. Neither
repeatedly traverses a preceding equal-key prefix. Apply the same Rust predicates
and `take` to the suffix as to the initial `query` page. Primary IDs alone are
insufficient when area assignments do not follow primary order. Inventory within
one owner uses `query_after(Owner, owner..=owner, (&owner, last_item_id))`.
Exclusive comparison supports full-u64 values without `cursor + 1` overflow.

Keep the same snapshot and predicates for every page and both sides of a join.
Concurrent saves may move rows between index keys; a new snapshot starts a new
query view and is not a continuation of the previous version. Retained versions
remain subject to lease admission and cooperative byte accounting. Borrowed
baseline guards instead block writes/maintenance for their complete lifetime.
Do not nest operations or hold baseline guards across `await`.

## Cost and current boundaries

The index interval chooses candidates. `filter` evaluates application predicates;
`take` bounds returned rows and allows traversal to stop once enough qualify.
A sparse predicate can still examine the complete selected interval. Arbitrary
sorting after collecting uses application memory/work proportional to candidates.
Skrin supplies no query optimizer, geometric spatial index, automatic join planner
or ECS component engine. The example's area buckets are application-defined;
position filtering is an ordinary Rust predicate. An orphan inventory owner is
an application consistency error, not a built-in foreign-key constraint.

Iterator creation validates table/index ownership and range bounds, including
empty indexes, and checks snapshot poison. Typed calls also check definition
version/uniqueness and key type/codec; their malformed ranges return errors.
Already returned immutable iterators
and references cannot be revoked by a later storage failure. New query calls
refuse after uncertainty; close/reopen and reconcile as for other reads/writes.
All rows and indexes remain resident in RAM. Persistence is Unix-only and
experimental; explicit maintenance, retained generations and headroom remain
application responsibilities.

## Verification

[Typed-index regressions](../crates/skrin/tests/typed_indexes.rs) cover numeric
bounds, borrowed UTF-8/blob keys, mismatched definitions, codec errors and size
limits, cursor/bounds intersections and absent cursor positions in both views.
[Query regressions](../crates/skrin/tests/queries.rs) compare indexed
bounds/order
with independently sorted rows, cover empty and full-u64 ranges, wrong table/index
refusal, and instrument actual typed borrowing to prove early `take` is lazy,
including zero limits and late pages in a large equal-key posting. Retained
cursor views remain coherent after removal/movement of current rows.
[Game regressions](../crates/skrin/tests/game_queries.rs) verify filtered pages,
inventory kinds/joins, retained/current frames and a cursor across area ordering
that differs from primary order. Production short-write/ENOSPC and uncertain-sync
tests additionally require new lazy snapshot query calls to refuse after poison.

`cargo bench -p skrin --bench game_queries --locked -- 1000` runs 2,000 resident
queries in native/snapshot modes: two indexed areas, an even-position predicate,
projection and a 16-row limit. Every result is checked against the workload model
outside timing. This is a functional in-memory smoke benchmark with per-query
timings, not durable-storage evidence or a comparative performance claim.

`cargo bench -p skrin --bench cursor_queries --locked -- 100000 128` compares
direct seeks and the former prefix-filter continuation with a reused prepared
SQLite covering-index seek. All filtered pages have identical independently
checked rows. The [five-process report](measurements/cursor-queries-2026-10-10.md)
records hot resident pages and an existing-area regression control; it does not
claim durable, cold-cache or whole-application performance. See
[methodology](benchmarks.md#resident-cursor-pages) for scope and execution order.
