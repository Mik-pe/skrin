# Working on Skrin

Skrin is an embedded, typed Rust database, not a SQL server or a wrapper around another database. Keep the public API small and executable; do not add placeholder crates or APIs for speculative features.

Read `README.md`, `docs/architecture.md`, `docs/durability.md`, `docs/file-format.md`, and `docs/managed-storage.md` before changing the engine.

## Invariants

- No success or visible publication before the configured persistence boundary. Never silently downgrade sync semantics to improve a benchmark.
- A failed write/sync has an uncertain commit outcome and poisons the handle. A corrupt complete frame is not a torn tail.
- Do not mutate files when refusing a schema/version/corruption error. Creation never overwrites an existing file.
- Persist explicit codecs, not native Rust memory layouts. Version format changes deliberately; preserve the golden fixture unless the format transition is intentional and documented.
- All row/index changes must share a transaction. Do not introduce full-database copies per commit or an unmeasured blanket `unsafe` optimization.
- Test storage faults beneath the production implementation rather than building a mock database that can pass while the real engine fails.
- Document current limitations, especially lock-based readers, explicit maintenance, headroom and retained/orphaned files. Keep the permanent directory LOCK across generation replacement. Sync a new generation and its parent before publishing CURRENT. Never recover by picking the newest filename. Do not advertise indexes, MVCC or automatic maintenance before they exist.

## Verification and delivery

Run format, Clippy (`-D warnings`), tests, docs, all-target checks, and the example. Compile/run the benchmark when changing its API or hot paths. CI includes Rust 1.89/MSRV and stable; formatting is pinned to 1.89. Missing local tooling means using CI as evidence, not claiming unexecuted checks passed.

Use focused commits and an isolated PR. Check for overlapping work before starting. Do not weaken tests/checks, bypass merge gates, enable auto-merge, or claim performance improvements without like-for-like measurements. Keep dependencies purposeful and do not publish or select a license on behalf of the owner.
