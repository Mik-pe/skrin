# Skrin

A small, Rust-native embedded database. Typed values in memory, explicit transactions, and a checksummed log on disk. No SQL or database server.

**Status: experimental first milestone.** This is a working storage-engine foundation, not a production-ready database or a claim to outperform SpacetimeDB. In particular, the WAL currently grows until you retire the file: checkpoints and migrations are the next storage milestone.

## Run it

Requires Rust 1.89 or newer. The crate has no external dependencies and is not published to crates.io.

```sh
cargo test --workspace --locked
cargo run -p skrin --example accounts
```

The example above uses volatile memory. On Linux/macOS, give it a **new** file path to commit to disk, close, and verify the data by reopening:

```sh
cargo run -p skrin --example accounts -- /tmp/accounts-demo.skrin
```

An existing file is never overwritten. Use another new path on subsequent runs. The parent directory must already exist.

## API

Define an ordinary Rust value and implement `Record` with a stable schema identity and explicit codec. See the complete, executable [accounts example](crates/skrin/examples/accounts.rs).

```rust
let db = Database::<Account>::create("accounts.skrin")?;

db.write(|tx| {
    tx.insert(42, Account { name: "Alice".into(), balance: 100 })?;
    tx.insert(7, Account { name: "Bob".into(), balance: 100 })?;
    Ok(())
})?; // Persistent commits sync before success.

{
    let read = db.read()?;
    let account: &Account = read.get(42).unwrap();
    println!("{}: {}", account.name, account.balance);
} // Release read guards before writing.
```

`create` is create-only; `open` is open-only. `in_memory` explicitly opts out of persistence and does not invoke the codec. Manual `begin_write`/`commit` is also available; dropping a write transaction discards staging.

## What works now

| Capability | First milestone |
| --- | --- |
| Native typed records, `u64` primary keys | Implemented; no decode/clone on reads |
| Atomic writes, duplicate detection, read-your-writes | Implemented, including multi-row batches |
| Ordered iteration and primary-key ranges | Implemented with `BTreeMap` |
| Persistent commits and recovery | Versioned/checksummed WAL, sync-before-publication |
| Exclusive file ownership | OS file lock held for the handle lifetime |
| Schema protection | Wrong table/schema/format is rejected before recovery repair |
| Failure handling | Incomplete final frames may be discarded; complete corruption is an error |
| Inspection | Row count, sequence, log bytes, recovered suffix bytes |
| Portability | In-memory: Linux/macOS/Windows CI; file backend: Unix, tested on Linux/macOS |

## Important boundaries

This version has **one typed table per database**. Read guards block writers, and write transactions block readers. There is no MVCC, group commit, secondary index, multi-table transaction, derive macro, migration executor, checkpoint, replication, encryption, or live-backup API yet. Data must fit in memory. An encoded record is limited to 8 MiB and a transaction payload to 16 MiB.

Do not nest transactions, hold them across `await`, or perform external side effects inside a transaction closure. Records must have immutable value semantics: interior mutation through shared references bypasses persistence. A Rust API and advisory file locks are not an access-control boundary against another program with file permissions.

`CommitUncertain` means the transaction **may** be present after reopening, even though the caller received an error. The handle is then poisoned, including reads. Do not blindly retry a non-idempotent operation.

See [durability](docs/durability.md) before storing anything important. The current test suite is not power-loss certification; filesystem and hardware sync guarantees matter.

## Measure, then optimize

```sh
cargo bench -p skrin --bench baseline
cargo bench -p skrin --bench baseline -- --durable /tmp
```

The benchmark separates a plain `BTreeMap` baseline, volatile database operations, and synced transactions. Persistent measurements include per-transaction p50/p99 and distinguish transactions/s from rows/s. The directory must exist; only the benchmark's newly created file is removed. Results from shared CI runners are smoke tests, not publishable performance evidence.

## Development

```sh
cargo +1.89.0 fmt --all -- --check
cargo +1.89.0 clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
```

The tests inject short writes and read/sync/truncation failures into the real WAL implementation. They cover every truncation position and every single-bit corruption in a sample log, an independently encoded format fixture, a deterministic reference model, concurrent updates, OS locks across processes, and process exit without destructors.

Read [architecture](docs/architecture.md), [file format](docs/file-format.md), and [benchmark methodology](docs/benchmarks.md). Publishing remains disabled; choosing the project's license and release compatibility policy is an owner decision before distribution.
