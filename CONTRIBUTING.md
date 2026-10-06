# Contributing to Skrin

Read [AGENTS.md](AGENTS.md), [architecture](docs/architecture.md), [durability](docs/durability.md) and [managed storage](docs/managed-storage.md) before editing persistence or transactions.

Use Rust 1.89 or newer. CI tests the minimum version and stable on Linux, plus stable on macOS/Windows. Persistent storage is currently Unix-only. There are no external crate dependencies.

```sh
cargo +1.89.0 fmt --all -- --check
cargo +1.89.0 clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test --workspace --release --locked
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked
```

Run the accounts and lifecycle examples and both benchmarks after changing relevant APIs. Use new scratch paths, never real user databases. Include a failing regression test with a fix. Persistence work must test failure boundaries in the real implementation and preserve the independent format fixtures unless the transition is intentional and documented.

Open focused pull requests and inspect the actual head's checks before merging. Keep roadmap ideas distinct from delivered capabilities. Do not introduce broad `unsafe`, disable checks, silently relax sync ordering, publish the crate or choose a project license as incidental cleanup.

Bug reports should include the commit, platform/filesystem, minimal sanitized reproduction, error and whether any operation had returned success. Do not upload private databases, credentials or personal records.
