# Ordinary values, explicit persistent schemas

Skrin models are ordinary Rust structs. The optional `Record` derive removes
codec boilerplate while keeping table identity, schema version and disk bytes
explicit. `catalog!` removes repetitive enum/table/codec dispatch. Both generate
implementations of the existing public traits; they add no query language,
object-tracking runtime or automatic migration.

## Declare a model

```rust
#[derive(Debug, skrin::Record)]
#[skrin(table_id = 1, version = 1)]
pub struct Entity {
    pub area: u64,
    pub x: u64,
    pub y: u64,
    pub revision: u64,
}
```

The schema attributes are mandatory integer literals. IDs never derive from
type or field names. Fields encode in declaration order with these fixed codecs:

| Field type | Encoded representation |
| --- | --- |
| `u8`, `u16`, `u32`, `u64`, `u128` | Fixed-width 1/2/4/8/16 little-endian bytes |
| `i8`, `i16`, `i32`, `i64`, `i128` | Fixed-width 1/2/4/8/16 little-endian two's-complement bytes |
| `bool` | One byte: 0 for false, 1 for true; other tags are errors |
| `f32`, `f64` | Exact IEEE-754 binary32/binary64 bits, little endian |
| `Option<T>` | 0 for None; 1 plus the supported T codec for Some; other tags are errors |
| `String` | u32 little-endian byte length, followed by validated UTF-8 |
| `Vec<u8>` | u32 little-endian byte length, followed by raw bytes |

Standard qualified `std`/`alloc` string/vector paths, `std`/`core` primitive
paths and `std::option::Option`/`core::option::Option` are accepted too. Nested
options preserve distinct states: None is `00`, Some(None) is `01 00` and
Some(Some(value)) starts `01 01`. Missing/truncated values never decode to a
default. `usize`/`isize` have no platform-independent width and remain unsupported.
There is no native struct layout, pointer or padding in storage. Limits and decoder
validation are the same production `Encoder`/`Decoder` operations as manual
codecs. Records need not implement `Clone`. Enums, generics, references, aliases
to unsupported spellings and other field types require a manual `Record` impl.
No field may use `#[skrin(skip)]` or receive an implicit decode default.

Float codecs preserve signed zero, infinities and NaN payloads exactly. They do
not enforce finite positions or valid health ranges; those are application
validation rules. Float index keys have no implicit encoding/collation. Use a
custom `IndexKey` when you have explicitly chosen NaN/zero/equality/order
semantics. Signed integer index keys instead flip the high sign bit of their
big-endian two's-complement bytes, giving natural numeric order across zero.
Bool index keys use 0/1. Record codecs remain little endian.

The [character model example](../crates/skrin/examples/game_models.rs) declares
signed x/y coordinates, float health, an alive flag and optional team once. It
queries signed chunk bounds, stages a complete character replacement with both
indexes, then verifies WAL/checkpoint/backup reopen:

```sh
cargo run -p skrin --example game_models --locked
# Unix, new directory under an existing parent:
cargo run -p skrin --example game_models --locked -- /local/scratch/new-characters
```

Field order and meaning are part of the schema contract. Reordering, adding or
removing fields, conditional fields across builds, or changing meaning requires
a deliberate schema version and old-to-new migration. The derive cannot infer
semantic compatibility or enforce immutable value semantics. An explicit manual
codec can preserve an old encoding during source refactors.

Derive is included by default. `default-features = false` removes its compiler
dependencies from an engine build; manual `Record` and `catalog!` still work.
With a renamed dependency, use `#[skrin(crate = your_alias)]` and
`#[derive(your_alias::Record)]`.

## Declare a catalog once

With the model above:

```rust
skrin::catalog! {
    pub World, #[derive(Debug)] Row {
        schema: (9000, 1),
        tables: { Entities: Entity },
        indexes: {
            Area: Entities {
                id: 1, version: 1, unique: false,
                key: u64 => |row| row.area
            }
        }
    }
}
```

This generates `World: Catalog`, enum `Row` with `Entities(Entity)`, marker
`Entities: Table<World>`, and marker `Area: Index<World>`. All generated items use the
declaration's visibility. Optional attributes before `Row` apply to that enum;
`Debug` is opt-in and requires the record types to implement it. Record types can
be named with paths. Empty `indexes: {}` and one-table catalogs are supported.

Declare tables and indexes in strictly increasing ID order. The engine refuses
duplicates, unsorted definitions or undeclared index ownership before creating
storage. Table schemas come from their actual record types. Each typed projection
receives its table's inferred record. Its expression
produces the declared key, which uses the same `IndexKey` codec during projection
and query. Borrow a string/blob with `key: str => |row| &row.name` or
`key: [u8] => |row| &row.payload`; the expression can also propagate an error
with `?`. Wrong table names, projection values and query key types fail
compilation. Codec failures and uniqueness violations use the existing
transaction error contract.

The built-in u64 key codec uses fixed-width big-endian bytes to preserve numeric
index order. All fixed-width unsigned keys work likewise; signed/bool keys are
described above; str/[u8] keys preserve raw byte order.
Index ID, version, uniqueness and projection remain persistent schema decisions;
change them through a catalog migration. Projections must be pure, deterministic
and immutable. This is the same contract as implementing `Catalog` manually.

Then insert and query real values:

```rust
let db = skrin::catalog::CatalogDatabase::<World>::in_memory()?;
db.write(|tx| tx.insert::<Entities>(42, Entity {
    area: 3, x: 100, y: 50, revision: 0,
}))?;
let read = db.read()?;
let visible: Vec<_> = read.matching(Area, &3)?
    .filter(|(_, entity)| entity.x >= 30)
    .take(32)
    .map(|(id, entity)| (id, entity.x, entity.y))
    .collect();
```

The original raw form remains available for existing catalogs:
`key: |row: &Entity| Ok(row.area.to_be_bytes().to_vec())` generates a u64 ID
constant, used with `index_scan`/`lookup`. A declaration uses either all typed
or all raw indexes. Manual catalogs can implement `Index<C>` with the matching
full `IndexDefinition`, delegating their projection to `Index::project` and
sharing the same key codec. Custom/composite key types implement `IndexKey` with
an explicit canonical, order-preserving encoding. Changing that encoding or
projection meaning requires an index version and catalog migration.

## Executable compatibility evidence

The [accounts example](../crates/skrin/examples/accounts.rs) and complete
[three-table game schema](../crates/skrin/examples/support/world.rs) now use
these declarations. Existing game saves, query pages, snapshot frames, backup,
checkpoint and reopen checks exercise them without changing their application
code or persistent schema. Run:

```sh
cargo run -p skrin --example accounts --locked
cargo run -p skrin --example game_queries --locked
```

[`tests/models.rs`](../crates/skrin/tests/models.rs) compares independent codec
bytes, rejects every truncation/invalid UTF-8/length overflow, checks encoder
limits and final-view uniqueness with old/current indexes, and compares complete
WAL/snapshot/manifest bytes with a handwritten catalog. Each implementation opens
the other's data, including checkpoint and independent backup. No fixture,
storage version or persistence boundary changes. Compile-fail docs and macro
parser tests cover missing/duplicate identity, unsupported state and field skips.

[`tests/model_values.rs`](../crates/skrin/tests/model_values.rs) compares a new
application-model fixture generated independently using Python `struct` and
integer byte conversion. It covers every truncation, all noncanonical bool/
optional tags, distinct nested optional states, encoder limits and character
recovery with typed signed/bool queries. Codec tests cover integer extremes,
NaN/infinity/subnormal/zero bits and production WAL refusal before torn-tail
repair for correctly checksummed invalid tags. The existing storage fixtures and
format versions are unchanged; adding fields to an existing model still requires
a schema version and migration.
