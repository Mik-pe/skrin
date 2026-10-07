# Bounded independent group commit

`Database::into_group_commit` and `CatalogDatabase::into_group_commit` consume a persistent handle and start a bounded worker. The original immediate-sync APIs remain available as the reference baseline. Both modes require successful `sync_all` before reader visibility or successful acknowledgment; group commit shares that boundary across independent WAL frames.

```rust
use skrin::group_commit::GroupCommitOptions;
let group = db.into_group_commit(GroupCommitOptions::default())?;
let response = group.submit(|tx| {
    tx.insert(1, MyRecord { value: 42 })?;
    Ok("inserted")
})?;
let receipt = response.wait()?;
assert!(receipt.synchronized_sequence >= receipt.sequence);
```

Run `cargo run -p skrin --example group_commit -- NEW_DIRECTORY` for the full indexed banking example, including a dropped response, operation-ID retry, independent backup, retained ownership, worker drain and offline V2 migration. Persistence remains Unix-only; volatile databases refuse group commit rather than becoming a durability comparison.

## Ordering and publication

Admission order is the queue's mutex order. Concurrent callers have no stronger wall-clock ordering guarantee. The worker collects at most `max_transactions` requests, acquires the existing exclusive state guard (and the catalog's index guard), then runs callbacks once in order. Each callback stages only its own touched rows, reads its own changes and the preceding successful group view, and gets its own final-view index/uniqueness validation. A closure/codec/constraint error drops that transaction's staging and does not undo other independent requests.

Each successful nonempty transaction receives its own next sequence and original checksummed WAL frame. The worker appends frames separately, applying their private native/index deltas while retaining both exclusive guards. No other reader, writer or maintenance operation can access that intermediate state. One successful shared WAL synchronization precedes guard release and every successful response, including a no-op that read a preceding private transaction. There is no per-commit table copy, new stored format, callback replay or weaker durability option.

A `CommitReceipt` reports the closure value, its individual sequence, the last synchronized sequence, nonempty frames in its group, and admission-to-callback queue time. A reader can see later completed groups by the time it acquires a guard. Clean failed or empty transactions do not consume a sequence; an all-empty group needs no WAL sync.

Readers remain borrowed lock-based guards and can block the worker. This feature adds no MVCC, retained versions, online migration or automatic maintenance.

## Bounds and low-load delay

Defaults are 64 queued requests, at most 16 requests per group and a 1 ms collection delay. A full queue returns `QueueFull` without admission or callback execution. The application chooses retry/backpressure and retains responsibility for operation IDs; the engine does not rerun arbitrary closures.

The collection deadline is the first request's admission time plus `max_delay`. It is never reset by subsequent arrivals. A backlog whose deadline has already elapsed proceeds without another intentional batching delay. The allowed delay is 0–1 s and group size 1–1024. Queue capacity must be nonzero.

These limits bound request counts, not process memory captured by callbacks or their return values. The active group is additional to queued requests. Each frame retains the existing 16 MiB encoded-transaction cap; frames are encoded/appended one at a time. Native tables/indexes, arbitrary codec/callback allocations and blocked readers remain application resource responsibilities. Collection delay does not bound prior queue work, callback execution, reader locks, scheduler overshoot or synchronization latency; it is not an end-to-end deadline.

Callbacks must remain short, free of external side effects and free of nested database calls. Waiting for another submitted request inside a callback can deadlock the worker, just as nested transactions can deadlock the baseline.

## Failures, dropped responses and shutdown

| Outcome | Contract |
| --- | --- |
| Queue full | Not admitted, callback not invoked |
| Closure/codec/final uniqueness refusal | This transaction stages no committed delta or frame; other independent requests continue |
| Append fails | Handle is poisoned; already appended/prepared successful requests receive `CommitUncertain`; later callbacks are not invoked and receive `Poisoned` |
| Shared sync fails | Every successful request in that group has an uncertain outcome; handle is poisoned, queue stops and later callbacks are refused |
| Callback/codec/destructor panic before guard release | Owned guards poison; undelivered responses are uncertain; the worker stops and queued callbacks are refused |
| Response dropped | Execution continues; absence of acknowledgment is not evidence of absence |
| Last client dropped | Close admission, drain admitted requests and join the worker, including its final sync |

An uncertain group's independent frames may recover as a complete prefix. They are not one atomic batch: a later failed transaction does not roll back an earlier complete frame. No request is automatically retried. Preserve the original operation ID, close all clients, reopen CURRENT with the recorded schema and reconcile the exact operation contents before retrying. The banking example's duplicate operation ID rolls back staged balance changes; it also checks the stored transfer's contents.

`into_database` requires no other clients. It closes admission, drains and joins, then returns the original baseline handle for explicit offline migration or maintenance options. With other clients it returns `Busy` and consumes only that caller's client; a poisoned engine is refused and released. Dropping a client captured by the worker itself avoids self-joining; it closes admission and the worker finishes its remaining requests.

## Checkpoints, backup and cleanup

The group handle exposes serialized checkpoint, independently verified backup, read-only storage inventory and conservative reclamation. These operations compete for the same existing guards and permanent directory ownership. A capture can happen between complete groups; it cannot select an unsynchronized intermediate row/index state. Maintenance is not an admission barrier: a queued request may execute before or after a concurrent checkpoint. For an exact final sequence, stop/drain the worker with exclusive client ownership first.

All baseline generation, backup, migration, encoded-budget and opt-in file-data reservation contracts still apply. Source schema/format refusals, stable LOCK, active/previous retention and unknown-stage protection are unchanged.

## Evidence and scope

Tests inject every byte cutoff across two independent frames beneath the real WAL, plus shared sync/ENOSPC failures, and reopen coherent prefixes. Catalog variants verify balances, operation IDs and all derived indexes. A blocked production storage synchronization proves that neither a read nor successful response escapes early. Queue-full, dropped-response, no-op/error independence, panic, bounded low-load collection and shutdown/drain tests exercise the worker. Production persistence images retain grouped acknowledgments through checkpoint publication.

The `group_commit` benchmark runs the same independent indexed transfers and synchronization contract in fresh immediate/group processes. It reports end-to-end latency and call-to-callback waiting p50/p95/p99, admission waiting for groups, throughput, exact shared-sync group sizes, process memory, staging/retained file overlap, checkpoint/reclaim pauses and independently verified reopen. Low load and saturation are separate workloads. Reopen has no cache eviction advice; no device-cold claim follows. RSS includes benchmark samples and native data; inode allocation excludes directories/filesystem metadata and may double-count shared extents.

[Recorded repeated local NVMe results](measurements/group-commit-2026-10-07.md) preserve the saturation benefit, low-load delay and contention tails. Snapshot readers, coherent version reclamation/backpressure and their measurements remain separate work in issue #7. This implementation keeps the lock-based reader baseline and does not claim that the entire concurrency issue is complete.

The optional [versioned handles](snapshots.md) reuse this same queue, independent
transaction framing and synchronization boundary. They publish one coherent
immutable row/index root after shared sync; old snapshots do not hold writer
locks. The original group APIs above retain their borrowed lock-based reads.
