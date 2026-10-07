# Immutable read versions

`versioned` is an opt-in single-process read-version API. The ordinary
`Database` and `CatalogDatabase` remain the immediate-sync, borrowed-lock
baseline. Consume either handle with `into_snapshots(options, footprint)` to
move its native rows into shared immutable values without changing persistent
bytes, schema, committed sequence, directory ownership or WAL framing. The
one-time conversion traverses the resident rows and builds immutable trees;
it is not a full-table copy on ordinary commits.

```rust
use skrin::versioned::SnapshotOptions;

let db = baseline.into_snapshots(
    SnapshotOptions { max_snapshots: 32, max_pinned_bytes: 64 * 1024 * 1024 },
    footprint,
)?;
let old = db.snapshot()?;
db.write(|tx| tx.update::<Accounts>(1, |r| Ok(Account {
    email: r.email.clone(),
    balance: r.balance + 1,
})))?;
assert!(old.sequence()? < db.snapshot()?.sequence()?);
```

This excerpt uses the banking types and native footprint function from the
[executable example](../crates/skrin/examples/snapshots.rs). Run
`cargo run -p skrin --example snapshots --locked` for its memory lifecycle, or
supply a **new** directory path on Unix for group commit, checkpoint, backup,
offline migration and independent reopen. The example accounts for the native
enum and its String capacity and shows operation-ID retry after a dropped response.

## Visibility and transactions

A snapshot captures one synchronized sequence in a short publication/lease
critical section. It borrows native immutable rows without encoding or decoding
and holds no production state lock. A writer callback, pending append, WAL sync
or checkpoint does not block reading a captured root or capturing the previously
published root. Snapshot acquisition still uses short internal locks; it is not
lock-free and makes no real-time or scheduling guarantee.

Single-table snapshots support get, ordered iteration and primary-key ranges.
Catalog snapshots support typed get/scan and mandatory indexed equality/ranges.
Rows and **every** unique/non-unique index belong to the same root and sequence.
Updates, unique-key swaps and deletes publish together. An old view retains its
old rows and old postings, including rows deleted from the current version.

Writes use the existing serialized mutable state, staging, codec and final-view
constraint implementation. Path-copy AVL roots share unchanged nodes/native
values; only the touched search/rotation paths and index postings are rebuilt.
Index postings are individual `(index, byte_key, primary)` nodes: changing one
row does not copy an entire duplicate-key posting set. The original mutable
index structures remain for write constraint validation. This increases current
memory versus the baseline; all active rows and indexes still fit in RAM.

Candidate roots and memory assessments are prepared before append. Success and
new-root visibility require the same actual sync boundary as the baseline.
`write` uses one immediate sync per nonempty transaction. Empty or rolled-back
transactions do not advance sequence or synchronize. Reads inside a writer use
its preceding private writer state plus staging, including prior independent
transactions in a group; they do not capture a separate snapshot.

`into_group_commit(GroupCommitOptions)` enables the existing bounded independent
worker on either versioned handle. Each successful staged transaction remains a
separate WAL frame, order/uniqueness is checked per transaction, and one coherent
final root publishes after the group's shared sync. Successful responses follow
that boundary. Recovery after an uncertain group can reveal a complete prefix,
not necessarily all or none of the group. Dropped responses do not cancel work;
operation IDs are still required for ambiguous retry. See [group commit](group-commit.md).

## Reader admission and accounting

Both limits are required and nonzero. Snapshot capture returns `BudgetExceeded`
before admitting a lease when its count or bytes would exceed the configured
limit. Drop leases to restore capacity. Cloning a snapshot shares its existing
lease and does not count twice; independently capturing the same sequence creates
another lease and counts again. Writers continue advancing while old leases are
pinned. Unpinned intermediate roots drop as publication advances, so slow readers
cannot cause an unobserved growing chain of all intermediate versions.

`retention()` reports published sequence, live leases, distinct pinned sequences,
oldest pin, total pinned bytes and current root bytes without waiting for writer
I/O. Count/current sequence are observations, not a promise about a concurrent
next commit. `stats()` inspects synchronized production counters under its normal
locks and can wait for I/O.

Pinned bytes conservatively sum the **entire** immutable root of every live
lease, including a pinned current version. Shared rows/nodes/key buffers are
counted again for each lease; they are not subtracted through approximate pointer
sharing estimates. This fixed per-lease accounting bounds admitted old-root
retention and may refuse a capture earlier than exact unique allocation would.
Current root size is separately reported; an unpinned current table can grow
through application writes and is not capped by the reader budget.

The application's trusted, pure footprint function must report each native row's
inline size plus all owned allocation capacities and nested data. It must honor
immutable value semantics and cannot report codec bytes as a native-memory
bound. Assessment errors, inline-size undercounts and arithmetic overflows are
refused before append. Skrin adds immutable node sizes, index-key data, and Arc
control counters/alignment padding. Repeated index-key sharing is conservatively
counted per posting too.

**This is cooperative retention accounting, not a hard allocator or process RSS
limit.** Allocator metadata/rounding, publication/lease registry bookkeeping,
mutable active maps/indexes, writer candidate paths and staging, callback/result
captures, decoder allocations, thread stacks and unrelated application memory
are outside pinned-byte accounting. The separate lease-count limit bounds reader
bookkeeping count. Incorrect application assessment breaks the accounting
contract. Application records must have immutable value semantics: mutable state
behind a shared reference would invalidate both old views and their byte bounds.

## Failure, closing and maintenance

A real append/sync failure poisons the controller and every retained view. New
snapshot read calls, retention inspection and writes refuse with `Poisoned`.
A writer panic also propagates poison to existing leases. An application callback
returning an error rolls back that independent transaction; merely returning an
error named `Poisoned` does not create a storage failure. Already handed-out
immutable references/iterators cannot be revoked; reopen and reconcile an
operation ID after an uncertain outcome.

Checkpoint/backup/cleanup retain the same permanent directory LOCK and baseline
publication protocol. Maintenance is explicit and serialized with writers;
existing snapshots continue reading in-memory versions. A maintenance-publication
failure poisons snapshots too. Preparation or budget refusal leaves them usable.
Pins do not retain disk generations: active + previous cleanup may proceed even
when an old memory version remains pinned. No automatic maintenance is added.

A consistent backup is independently decoded and index-checked and returned as a
native baseline handle in a **new** directory. Old memory snapshots do not follow
its changes. `into_database()` returns the original native API for offline
migration only with exclusive controller/client ownership and no leases. Group
conversion first drains/joins admitted work. `Busy` consumes the requesting
client; other clients remain usable. If a sole controller is closed while pins
remain, those immutable views remain readable and do not keep directory LOCK;
they cannot be used to write or migrate the old version. Migrate only through a
native handle after releasing pins; schemas never change underneath a snapshot.

## Evidence and limits

The production WAL tests explore every byte cutoff across snapshot transactions,
short writes/ENOSPC and uncertain sync. Blocking the actual production sync proves
that row/index snapshots continue reading the preceding version until it finishes.
Independent reference maps cover retained updates/deletes, ranges and mandatory
index postings. Production namespace/content survival images exercise acknowledged
groups, checkpoint and reclamation with pins held. These are bounded fault models,
not hardware power-loss certification.

The `snapshots` benchmark compares identical synced indexed transfers and held
coherent reader work in `immediate`, `group`, `snapshot` and `group_snapshot`
modes. It reports writer/read distributions, throughput, groups/queue time,
process RSS, sampled accounted retention and checkpoint/fresh-process recovery.
Use an identified filesystem/device for performance evidence; `/tmp` or CI smoke
runs only prove execution. See [benchmark methodology](benchmarks.md).

[Three repeated low-load/saturated NVMe comparisons](measurements/snapshots-2026-10-07.md)
record reader availability, writer contention tails, unequal completed read counts,
process HWM and conservative pins. Snapshot reader p99 acquisition was roughly
33–35 µs in these workloads, with additional memory/sampling cost and variable
writer tails; this is not a general fairness or writer-performance guarantee.
