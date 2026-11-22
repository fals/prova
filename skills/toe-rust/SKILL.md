---
name: toe-rust
description: toe setup details for Rust projects (cargo test, cargo-nextest). Use together with toe-adopt or toe-test in a Rust codebase.
---

# toe for Rust

- `cargo test` output is recognized live (level `recognized output`):
  `test path::name ... ok|FAILED|ignored`. Nothing to configure.
- Single test: `select = "cargo test -- {filter}"` (substring match on the
  test path). Add `--exact` only if names are fully qualified.
- cargo-nextest: JUnit output is ingested automatically when enabled in
  `.config/nextest.toml`:

  ```toml
  [profile.default.junit]
  path = "junit.xml"
  ```

  It lands in `target/nextest/default/junit.xml` and includes retries as
  attempts, which feeds toe's flaky detection.
- Sizes: unit tests next to the code are small; `tests/*.rs` that spawn
  processes or touch the filesystem are medium. Split suites accordingly:

  ```toml
  [suite.small]
  run = "cargo test --lib --bins"
  select = "cargo test --lib --bins -- {filter}"
  size = "small"

  [suite.medium]
  run = "cargo test --tests"
  size = "medium"
  ```

- Benchmarks for `toe exp`: print `{"metric": ..., "value": ...}` from a
  small binary, or wrap `hyperfine --export-json` output.

See `references/pitfalls.md` for common problems.
