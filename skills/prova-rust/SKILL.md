---
name: prova-rust
description: prova setup details for Rust projects (cargo test, cargo-nextest). Use together with prova-adopt or prova-test in a Rust codebase.
---

# prova for Rust

- Best: cargo-nextest 0.9.66+ with its libtest JSON stream (level
  `native`: every test live, with its duration and output). `prova adopt`
  proposes it when it finds nextest:

  ```toml
  [suite.default]
  run = "NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run --no-fail-fast --message-format libtest-json --message-format-version 0.1"
  select = "NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run --no-fail-fast --message-format libtest-json --message-format-version 0.1 {filter}"
  ```

  The format is experimental upstream; `--message-format-version 0.1` pins
  it. nextest writes that JSON only when a test binary ends, so prova also
  reads nextest's own status lines (`PASS`, `FAIL`, `SLOW`) as they print:
  the first failure arrives while the run goes on, and nextest's `SLOW`
  line makes `prova wait` return on a hung test. Nightly `cargo test -- -Z unstable-options --format json` is read the
  same way.
- nextest does not run doctests. For a crate with a library, `prova adopt`
  adds `cargo test --doc` after nextest in the same suite, so a failing
  doctest fails the run.
- nextest runs each test in its own process and counts exit code 0 as a
  pass: a test that calls `std::process::exit(0)` before its assertions
  passes there. Plain `cargo test` catches it (the harness never prints its
  summary); keep a `cargo test` suite (`make test` often is one) and run it
  before reporting the suite green.
- `cargo test` output is recognized live (level `recognized output`):
  `test path::name ... ok|FAILED|ignored`. Nothing to configure.
- `cargo test --quiet` (often inside `make test`) prints only failures:
  prova then cannot record passing tests, so flaky detection is blind.
  `prova adopt` proposes running `cargo test` directly as the default suite
  and keeps `make test` as a separate suite.
- Plain `cargo test` reports no per-test durations; `prova sizes` and slow
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
  attempts, which feeds prova's flaky detection.
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

- Benchmarks for `prova exp`: criterion output is read directly
  (`name.time_ns`); otherwise print `{"metric": ..., "value": ...}`.

See `references/pitfalls.md` for common problems.
