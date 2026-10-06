# Changelog

## Unreleased — experimental

### Added

- Typed records with explicit schema codecs, atomic staged transactions, borrowed reads, primary-key ranges and checked updates.
- Original standalone v1 WAL backend with bounded framing, checksums, sync-before-publication, conservative recovery and exclusive ownership.
- Managed directory backend with a permanent lock, authoritative manifest, sealed verified checkpoints and explicit active/previous generation retention.
- Consistent backup/import/restore and named offline schema migrations with preserved sequence and history.
- Publication-boundary I/O faults and subprocess exits, snapshot/manifest corruption checks, independent format fixtures and a complete lifecycle example.
- Maintenance benchmarks for checkpoint pauses, disk footprint and warm-cache reopen.

### Boundaries

Experimental API, one in-memory typed table, lock-based readers and one writer. No secondary indexes, multi-table schemas, MVCC, group commit, encryption or replication. Persistence currently requires Unix. Publishing is disabled pending owner release decisions.
