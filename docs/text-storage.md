# Text and application storage

Skrin stores ordinary Rust `String` fields: Markdown, JSON text, source code,
multiline comments, empty strings and embedded NULs. It preserves every UTF-8
byte without normalization, case folding, trimming or line-ending translation.
`"é"` and `"e\u{301}"`, and `"a"` and `"A"`, remain distinct values and unique
index keys. Invalid UTF-8 is refused; use `Vec<u8>`/the byte codec for binary data.

The [task store example](../crates/skrin/examples/task_store.rs) exercises a
task workflow: tasks with titles, Markdown specifications and status,
linked comments, a unique string request ID and atomic status/comment updates.
Its [models](../crates/skrin/examples/support/tasks.rs) use derived codecs and
`catalog!`, with `String` fields and explicit schema versions.

## String identity and queries

Primary IDs remain caller-assigned u64s. A unique `key: str` index provides exact
string identity, without hashing or potential hash collisions:

```rust
ByRequest: Tasks {
    id: 1, version: 1, unique: true, key: str => |task| &task.request_id
}
```

Use `read.matching(ByRequest, request_id)` to borrow the native task. Declare
non-unique string indexes when repeated status/title values are valid. Both
borrowed catalog reads and immutable snapshots support:

```rust
let page = read.prefix(ByTitle, "Lagring/")?
    .take(20)
    .map(|(id, task)| (id, task.title.as_str()))
    .collect::<Vec<_>>();
```

`prefix` accepts indexes with `Key = str`. It seeks one UTF-8 byte interval and
yields lazily in index-key then primary-key order. No table scan or result vector
is needed. An empty prefix selects all indexed strings, including empty keys.
Punctuation (`%`, `_`, quotes, backslashes) is literal data. The upper endpoint
is a byte sentinel, which need not be valid UTF-8; strings containing the highest
Unicode scalar followed by more text are correctly included. Input strings can
be dropped immediately after iterator construction. Oversized prefixes are
refused even on empty indexes.

`query` accepts borrowed string bounds; `query_after` accepts a `(&str, u64)`
continuation position. See [native queries](queries.md). Retain one snapshot for
coherent pages/joins. Rust predicates such as `contains` may examine every
candidate; no full-text index or locale collation is supplied.

## Read your writes with native keys

Native and snapshot catalog writers both provide `matching(index, &key)` and
`query(index, bounds)`, inferring record/key types as on reads. These return
**collected vectors of borrowed rows**, ordered by index then primary key,
merging the selected committed interval and the transaction's staged delta.
`IndexRecord<C, I>` names an index's record type for generic code. Invalid bounds,
mismatched definitions, key codec errors and size limits are refused before
traversal, including on empty tables.

Check a request ID inside the serialized writer and stage related changes
together. Earlier inserts, updates and removals appear immediately in typed
queries. Temporary unique-key duplicates during a swap are visible; final
uniqueness is checked before WAL append. Propagate any error to roll back the
whole closure. The example atomically changes status and adds a completion
comment with a unique operation ID, which identifies retries independently of
other comments. An uncertain
commit still requires reopen/reconciliation. Group requests also see preceding
successful transactions. External effects belong outside transaction closures.

## Run the lifecycle

```sh
cargo run -p skrin --example task_store --locked
# Unix: choose a NEW path under an existing parent; PATH.backup must also be new.
cargo run -p skrin --example task_store --locked -- /tmp/new-task-store
```

Persistent mode verifies original text after WAL recovery, checkpoint, independent
backup and snapshot recovery. Both modes check retained/current views, string
request lookup and atomic task/comment state. This is an executable application
pattern. An integration must map every model, relationship
and ID allocation rule, version codecs/indexes, import into a new destination and
verify behavior before selecting it. There are no automatic foreign keys or
cascading deletes. Skrin never accepts SQL syntax or offers SQL compatibility.

## Limits and costs

The **complete encoded record**, including all fields/length prefixes, must fit
in 8 MiB. An isolated string has a four-byte length prefix. The complete durable
transaction payload must fit in 16 MiB. Oversized commits are refused before
append and leave the handle usable. Memory-only writes deliberately skip record
encoding; verify encoded limits with persistent storage.

All rows/indexes remain in RAM. Index only needed access paths: each indexed
string adds owned key bytes. Large specifications/comments need not be indexed.
`edit` clones that row's strings; `update` builds a replacement without requiring
`Clone`. Retained snapshots keep old rows/keys within cooperative reader limits.
Borrowed guards block writers and must not cross `await`. Explicit maintenance,
retained generations and headroom remain application responsibilities. Persistent
storage is Unix-only and experimental. These APIs change no storage format,
schema, sync boundary or recovery policy. See [durability](durability.md).

The design follows established ideas: schema-bound typed tables as in
[redb](https://docs.rs/redb/latest/redb/struct.TableDefinition.html), and bounded
ordered prefix traversal as discussed in
[RocksDB](https://github.com/meta/rocksdb/wiki/Prefix-Seek). Skrin uses its existing
resident indexes and engine; neither database is an engine dependency.

## Verification and measurement

`tests/text_store.rs` covers exact identity, atomic duplicate refusal, staged
swaps, grouped retries, long-text persistence, limits and backup recovery.
`tests/typed_indexes.rs` compares prefixes with independently sorted
`str::starts_with` results across scalar boundaries, NULs, empty/duplicate keys
and retained versions. `tests/queries.rs` counts actual row borrows to prove
lazy `take`. Production append/sync fault tests require prefix reads to refuse
after poison.

`cargo bench -p skrin --bench text_queries --locked -- 10000` verifies and times
bounded title-prefix queries and an equivalent scan/filter/sort control in native
and snapshot modes. Full records are checked against an independent dataset
outside timing. This is a volatile, warm resident smoke workload with fixed
prefix-then-scan order, not a cross-engine or durable performance claim.
