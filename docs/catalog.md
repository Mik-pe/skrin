# Schema-bound tables and atomic indexes

`Database<R>` remains the small single-table baseline. `catalog::CatalogDatabase<C>` adds an executable, schema-bound multi-table engine using the same WAL, permanent directory lock, snapshots and CURRENT publication protocol. Both retain serialized writers and lock-based borrowed reads. There is no MVCC, SQL, query optimizer or optional index registration.

## Define everything before open

Implement `Catalog` with a native `Row` enum, a distinct stable catalog `SCHEMA`, strictly ID-sorted `TABLES` and `INDEXES`, explicit row codecs and deterministic index projections. Implement one `Table<C>` marker per real `Record` type to wrap/borrow its enum variant. The catalog validates marker record identity against the declared table schema whenever that table is accessed.

Table IDs, index IDs, versions, index ownership and uniqueness are persisted exactly in a mandatory descriptor. Missing or changed definitions are refused even when the catalog version has accidentally not been increased. Versions identify codec/projection semantics; Skrin cannot inspect the meaning of application code. Increase the table/index version when its encoding or meaning changes, increase the catalog version for the transition, and perform an explicit migration. Rows, projections and codecs must obey immutable value semantics and deterministic round trips.

The [banking definitions](../crates/skrin/examples/support/banking.rs) are a complete two-table schema: `Account`, `Transfer`, a unique email index and a non-unique balance index. Index key bytes sort lexicographically. Email uses UTF-8 bytes; balance uses big-endian u64 bytes to preserve numeric order. The ordinary record codec still uses little-endian integers. No automatic collation, nullable/multivalued projection or query optimizer is supplied.

## One transaction owns every change

```rust
// Accounts, Transfers and Banking are the executable example's table markers/schema.
let db = CatalogDatabase::<Banking>::create_dir("new-banking-directory")?;
db.write(|tx| {
    tx.update::<Accounts>(1, |r| Ok(Account {
        email: r.email.clone(), balance: r.balance - 25,
    }))?;
    tx.update::<Accounts>(2, |r| Ok(Account {
        email: r.email.clone(), balance: r.balance + 25,
    }))?;
    tx.insert::<Transfers>(42, Transfer { from: 1, to: 2, amount: 25 })
})?;
```

The actual example checks overflow/insufficient funds and rejects transfers to the same account. A duplicate operation ID rolls back both balances when its error is propagated out of the closure. After uncertain synchronization, reopen and look up the transfer ID before deciding whether to retry. External side effects never belong in the closure.

`insert`, `put`, `update`, `remove`, `get`, indexed `lookup` and `index_range` all see staging. Row statement errors leave that statement unapplied; propagating the error rolls back the closure. Uniqueness is checked on the **final** staged view before WAL encoding/append. Two rows may temporarily share an email during a swap, and staged lookups expose that temporary view; the final commit must satisfy every unique index. Deleting a row releases its index keys; deleting/reinserting and repeated replacements coalesce correctly.

An outer catalog lock covers the native engine lock and derived indexes. The write path prepares only touched primary/index entries, encodes one coalesced WAL frame across all affected tables, syncs it, publishes rows and applies its already-validated index delta before unlocking or returning success. Untouched rows/indexes are not cloned. Application panic or post-sync allocation failure can leave an acknowledged-on-disk transaction without a response; close/reopen as described in [durability](durability.md).

Reads borrow native enum variants. `get::<Table>` is a primary lookup;
`scan::<Table>` is an explicit full scan in logical primary-key order.
`index_scan::<Table>` visits an index interval lazily in byte-key then primary-key
order, composing with Rust `filter`/`map`/`take`. `lookup` and `index_range`
materialize borrowed result vectors. Read guards block writers and maintenance.
Do not nest operations or hold a guard across `await`. See [typed game queries](queries.md)
for filtering, projection, coherent joins and snapshot pagination.

## Recovery, maintenance and migrations

Derived indexes are **rebuilt**, not stored as separately writable files. Recovery validates snapshot uniqueness/logical keys, then each complete WAL transaction's final index delta. These checks finish before any incomplete tail is truncated or recovered state is synced/exposed. A complete checksummed frame with a uniqueness violation is corruption, including if a later transaction might remove it. Missing/changed descriptors are refusals. CURRENT stays authoritative; no filename-based recovery is introduced.

`checkpoint`, `reclaim`, `backup_to` and `migrate` use the existing managed-generation implementation. Checkpoint also checks that independently decoded rows preserve their logical identities and index projections. Backup/migration independently validate decoded destination constraints **before CURRENT publication**. The permanent LOCK stays held across every generation change. Cleanup retains active and previous generations and preserves unknown/orphan ownership metadata according to the existing [maintenance contract](maintenance.md).

Catalog maintenance also exposes `checkpoint_with_options`, `checkpoint_if_needed`, `estimate_checkpoint`, `backup_to_with_options`, `migrate_with_options`, `storage_inventory` and `generation_info`. These use the same production budgets, ownership checks and publication locks as the single-table engine. There is no hidden maintenance in ordinary writes.

```rust
let options = MaintenanceOptions {
    max_new_file_bytes: 64 * 1024 * 1024,
    max_record_bytes: 64 * 1024,
    max_rows: 100_001,
    ..Default::default()
};
let estimate = db.estimate_checkpoint(options)?;
if let Some(checkpoint) = db.checkpoint_if_needed(
    CheckpointPolicy { wal_bytes: None, commits: Some(10_000) }, options,
)? {
    let cleanup = db.reclaim()?;
}
let inventory = db.storage_inventory()?;
```

The mandatory descriptor counts as **one stored snapshot row** for `MaintenanceOptions.max_rows` and `MaintenanceEstimate.rows`. Its encoded record and envelope count toward record/file-byte limits too; a very small record limit may reject metadata before reaching user rows. In contrast, catalog `Stats.rows` and `Checkpoint.rows` count application rows only. Estimates measure current encoded contents exactly and do not reserve disk space or include native/index allocator usage. Limits are enforced again during execution, including migration to a potentially larger destination codec. Budget refusal leaves CURRENT unchanged before publication; migration consumes its source handle even on refusal. A failed preparation may leave recognized stages for inspection/reclamation. Default methods retain the original default options.

Migration consumes the old handle, accepts concrete old/new native enums and preserves table IDs, logical u64 keys, committed sequence and named catalog migration history. The example's [real V2 schema](../crates/skrin/examples/support/banking_v2.rs) adds an encoded `active` flag with an explicit default while retaining Transfer records and rebuilding both indexes. Conversion/constraint failures preserve the old CURRENT. Publication uncertainty requires reopening with the actual selected catalog. A migrated descriptor can add tables/indexes; existing rows cannot silently change table identity or be decoded with the new codec before conversion.

```sh
cargo run -p skrin --example banking --locked
cargo run -p skrin --example banking --locked -- /path/to/NEW-directory
```

Persistent mode demonstrates a transfer, duplicate-ID rollback, checkpoint/reopen and real V1-to-V2 catalog migration with old-code refusal. Existing paths are never overwritten.

## Explicit legacy import and representation

There is no implicit reinterpretation of existing v1 single-table files or managed snapshots. Use `CatalogDatabase::<C>::import_table::<Table>(&source, NEW_PATH, copy)` with a source `Database<Table::Record>`. The source codec is the real old codec; a consistent read guard excludes source writers; the explicit copy callback constructs destination values without requiring `Clone`. This creates a new catalog lineage, retains the source committed sequence, validates native/decoded destination constraints before publication and leaves all source bytes unchanged. Source single-table migration history is not catalog migration history. Other tables begin empty; no cross-source atomic import is promised.

The underlying standalone v1 and managed v1/v2 framing remains unchanged, including all existing golden fixtures. The deliberately versioned **catalog row codec** is new:

- Every encoded stored record starts with envelope version `u8 = 1` and kind `u8`.
- Physical slot zero is mandatory metadata, kind 0: a `u32` length-prefixed descriptor. That descriptor contains a length-prefixed `SKRCAT01`, table count `u32`, each table's `(u64 ID, u32 version)`, index count `u32`, each index's `(u64 ID, u64 table ID, u32 version, u8 unique)` in declared ID order.
- Other slots contain kind 1: logical table ID `u64`, logical primary key `u64`, then that table's explicit record codec bytes. Logical keys use their full u64 range independently in each table. Internal physical slots have no application meaning and are preserved across checkpoint/migration.
- The schema in the WAL/snapshot/manifest header is the catalog's distinct identity/version. Do not reuse a legacy table identity to claim compatibility. There is no new storage-format interpretation on ordinary open.

[The independent catalog fixture](../crates/skrin/tests/fixtures/catalog-v1.hex) freezes metadata plus a full-u64-key Account envelope, generated with Python `struct`. Existing frame checksums/bounds also cover these bytes. The 8 MiB record limit includes the envelope; the 16 MiB atomic transaction payload limit still applies across all touched tables.

## Resources and evidence

Data, the logical-to-physical primary map and every derived index live in RAM. Each row contributes one posting to every declared index. One/two-row posting
sets store sorted row IDs inline; larger sets use a BTreeSet and return to inline
storage when they shrink to two. This avoids a separate heap tree node for the
common unique/small non-unique case without making large shared-key updates
linear. Snapshot recovery builds those indexes before exposure. This derived
in-memory representation changes no persisted codec or index definition;
immutable snapshot posting trees still contain one entry per indexed row. Maintenance is explicit and serialized. Offline import/backup/migration can temporarily hold source/destination native tables and validation index maps; encoded budgets do not reserve process memory. Linux `reserve_file_data` is available for checkpoint/backup/migration, with the same data-only allocation and extra snapshot preflight contract. The internal descriptor occupies one storage row; public catalog stats/checkpoint row counts exclude it.

Tests cover an independent row/index model, final-view swaps and staged ranges, full-u64 keys, explicit legacy import, restart across WAL/checkpoints, independent backups, genuine V2 migration, descriptor mismatch and complete constraint corruption without tail repair, every short WAL write, uncertain sync, checkpoint I/O faults, production persistence images and subprocess exits during checkpoint/migration. These extend the existing engine's fault seam; they are not device power-loss certification.

Run the [equivalent-durability benchmark](../crates/skrin/benches/catalog.rs) on an existing directory on the intended filesystem:

```sh
cargo bench -p skrin --bench catalog --locked -- /path/to/scratch-parent 10000
```

It compares plain two-row synced transactions, catalog transactions without indexes, catalog transactions with two indexes, and two-table transfers with operation IDs. Each phase verifies results and reports 1,000 sequential latency samples. The transfer phase writes an additional row and is a separate workload. Modes `plain`, `unindexed`, `indexed` allow per-process RSS measurement. These are low-load measurements; no saturation, concurrency or cross-engine speed claim follows from them.

[Recorded local NVMe latency and RSS](measurements/catalog-2026-10-07.md) include repeated process runs, exact compiler/source metadata and the limits of a sync-dominated workstation workload.

[Compact posting before/after measurements](measurements/compact-postings-2026-10-08.md)
record reduced resident memory alongside mixed read/write timing and adverse
acknowledgment tails. No persisted index representation changes.
