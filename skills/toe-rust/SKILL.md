---
name: toe-rust
description: toe setup details for Rust projects (cargo test, cargo-nextest). Use together with toe-adopt or toe-test in a Rust codebase.
---

# toe for Rust

- Best: cargo-nextest 0.9.66+ with its libtest JSON stream (level
  `native`: every test live, with its duration and output). `toe adopt`
  proposes it when it finds nextest:

  ```toml
  [suite.default]
  run = "NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run --no-fail-fast --message-format libtest-json --message-format-version 0.1"
  select = "NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run --no-fail-fast --message-format libtest-json --message-format-version 0.1 {filter}"
  ```

  The format is experimental upstream; `--message-format-version 0.1` pins
  it. nextest writes that JSON only when a test binary ends, so toe also
  reads nextest's own status lines (`PASS`, `FAIL`, `SLOW`) as they print:
  the first failure arrives while the run goes on, and nextest's `SLOW`
  line makes `toe wait` return on a hung test. Nightly `cargo test -- -Z unstable-options --format json` is read the
  same way.
- `cargo test` output is recognized live (level `recognized output`):
  `test path::name ... ok|FAILED|ignored`. Nothing to configure.
- `cargo test --quiet` (often inside `make test`) prints only failures:
  toe then cannot record passing tests, so flaky detection is blind.
  `toe adopt` proposes running `cargo test` directly as the default suite
  and keeps `make test` as a separate suite.
- Plain `cargo test` reports no per-test durations; `toe sizes` and slow
  budgets need nextest's JUnit report (below).
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

- Benchmarks for `toe exp`: criterion output is read directly
  (`name.time_ns`); otherwise print `{"metric": ..., "value": ...}`.

See `references/pitfalls.md` for common problems.
