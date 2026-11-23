# Rust pitfalls under prova

- First run compiles; the stall detector counts compiler CPU as activity,
  so long builds are not stalls. A linker waiting on a lock is.
- `cargo test` stops at the first failing test binary unless
  `--no-fail-fast` is given; prova still reports the first failure early.
- Tests that spawn servers must bind port 0 and read the port back;
  fixed ports make tests flaky when suites run in parallel.
- `#[ignore]` tests appear as skipped. Run them with `-- --ignored` as a
  separate suite, usually medium or large.
