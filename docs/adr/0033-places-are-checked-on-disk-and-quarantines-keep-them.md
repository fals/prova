# 0033: A test's directory is checked on disk; a quarantine is per name and place; a pass in a failed repeat is kept

Status: Accepted (2026-11-25). Amends [0031](0031-quarantine-needs-one-runner-call-and-tests-keep-their-place.md) §2 and [0029](0029-quarantine-accounts-for-every-error-and-hangs-in-the-first-wait.md) §2.

## Context

Follow-up to round 17, on the five places where 0031 §2 still let two tests
share one id or lost a result:

1. 0031 read pytest ids "from prova's working directory, not pytest's
   rootdir" and added the rootdir's path when its `rootdir:` header lay
   below it. Real pytest prints terminal ids relative to the directory it
   runs in (`cwd_relative_nodeid`), not to its rootdir: `pytest -v
   sub2/tests` from the root with a `sub2/pytest.ini` printed
   `sub2/tests/x.py`, and prova named it `sub2.sub2.tests.x`. `-q` prints no
   `rootdir:` at all, so `cd sub2 && pytest -q tests` lost `sub2`.
2. Vitest names files from its root; run from a package directory two
   packages' `src/a.test.ts` were one file.
3. Plain `cargo test` prints `Running tests/it.rs (target/debug/deps/it-<hash>)`
   with no crate: two crates' `tests/it.rs` shared a name.
4. A quarantine was one row per name: `quarantine add t --in geo` replaced
   the one made `--in calc`.
5. A Go `-count=N` run whose test failed first and passed later recorded
   the pass as the failure standing: no pass in the history, no flaky label.
   The other order (pass, then fail) was already flaky evidence.

## Decision

1. **The directory pytest ran in is the one that holds the id's file.** In
   this order: a `cd <dir>` in the command whose target holds the file; the
   working directory; the header's rootdir; the one directory below the
   working directory (four levels) that holds it. With nothing on disk to
   check (a container), the rootdir's path below the working directory, as
   before. When more than one directory could be it, the test's place is
   **unknown**: the run says so, the test is kept apart in history, and no
   quarantine covers it (a quarantine is for a test known to be that one).
   A JUnit result that names no live test but is the tail of exactly one
   live name is that test.
2. **Vitest's root is its `RUN` header's path**, below the working
   directory when it is; its path is put before every file (text reporters,
   the stream, the file-failed-to-load entry). JUnit results are matched by
   the tail.
3. **A cargo test binary is named with its crate**, as nextest names it
   (`calc::it`): the dependency file cargo writes next to the binary names
   its source, and the source's `Cargo.toml` the package. Unit-test binaries
   already carry the crate. Without the file (a container, a moved target
   directory) the bare name stays.
4. **A quarantine is one per name and place.** `add --in <place>` replaces
   only that place's; `remove` takes `--in` and refuses to guess between
   places. No schema change: the `file` column of 0013 is the place.
5. **A pass inside a failed repeat is recorded.** The attempt of a later
   `-count` iteration that passed carries `passed_in_iteration` (event
   `test_finished`, column of migration 0014): the outcome stays the failure
   standing, so a failure in any iteration still fails the test (0029 §2),
   and history reads either order as a pass and a failure on one code:
   flaky.

## Consequences

- Ids of pytest tests run from a directory other than the working one, and
  of Vitest tests run from a package directory, change once; their history
  starts anew.
- A directory-ambiguous pytest run is named and set apart instead of
  guessed; the cost is a note and a split history for that rare layout.
- `schemas/event.v1.json` gains one optional boolean; old events read as
  `false`.
