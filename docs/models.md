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
| `u8` | One byte |
| `u32`, `u64` | 4/8 little-endian bytes |
| `String` | u32 little-endian byte length, followed by validated UTF-8 |
| `Vec<u8>` | u32 little-endian byte length, followed by raw bytes |

Standard qualified `std`/`alloc` string/vector paths are accepted too. There is
no native struct layout, pointer or padding in storage. Limits and decoder
validation are the same production `Encoder`/`Decoder` operations as manual
codecs. Records need not implement `Clone`. Enums, generics, references, aliases
to unsupported spellings and other field types require a manual `Record` impl.
No field may use `#[skrin(skip)]` or receive an implicit decode default.

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
            AREA_INDEX: Entities {
                id: 1, version: 1, unique: false,
                key: |row: &Entity| Ok(row.area.to_be_bytes().to_vec())
            }
        }
    }
}
```

This generates `World: Catalog`, enum `Row` with `Entities(Entity)`, marker
`Entities: Table<World>`, and constant `AREA_INDEX`. All generated items use the
declaration's visibility. Optional attributes before `Row` apply to that enum;
`Debug` is opt-in and requires the record types to implement it. Record types can
be named with paths. Empty `indexes: {}` and one-table catalogs are supported.

Declare tables and indexes in strictly increasing ID order. The engine refuses
duplicates, unsorted definitions or undeclared index ownership before creating
storage. Table schemas come from their actual record types. Each projection
receives its table's typed record and returns `Result<Vec<u8>>`. Wrong table
names or projection parameter types fail compilation; codec failures and
uniqueness violations use the existing transaction error contract.

The key above uses big-endian bytes to preserve numeric index order. Its index
ID, version, uniqueness and projection remain persistent schema decisions;
change them through a catalog migration. Projections must be pure, deterministic
and immutable. This is the same contract as implementing `Catalog` manually.

Then insert and query real values:

```rust
let db = skrin::catalog::CatalogDatabase::<World>::in_memory()?;
db.write(|tx| tx.insert::<Entities>(42, Entity {
    area: 3, x: 100, y: 50, revision: 0,
}))?;
let read = db.read()?;
let area = 3_u64.to_be_bytes().to_vec();
let visible: Vec<_> = read.index_scan::<Entities>(AREA_INDEX, area.clone()..=area)?
    .filter(|(_, entity)| entity.x >= 30)
    .take(32)
    .map(|(id, entity)| (id, entity.x, entity.y))
    .collect();
```

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
