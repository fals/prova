# 0031: A quarantine needs one test-runner call; a test keeps its place; a hang in a container is named in the first wait

Status: Accepted (2026-11-25). Amends [0030](0030-nothing-after-the-summary-and-named-selections.md) §1 to §4, [0029](0029-quarantine-accounts-for-every-error-and-hangs-in-the-first-wait.md) §1 and §3, and [0027](0027-quarantine-and-slow-tests.md) §4. §1 superseded by [0034](0034-a-quarantine-never-turns-a-failed-run-green.md): a quarantine never turns a failed run green.

## Context

Usability round 17 (five fixture agents, three through Docker wrappers, and
about 75 adversarial probes on v0.1.8):

1. 0030 accepted a quarantined failure when nothing but the runner's wrap-up
   followed the tests' summary. Every other way a command can do more than
   run its tests still passed: a step before the tests that fails with no
   error word (`go vet`, black, prettier, golangci-lint, `gofmt -l`, ruff's
   `Found 1 error.`), a step between two runner calls, a step the failure
   skipped (`&&`, make stopping, Vitest not checking coverage thresholds
   once a test failed, goleak in a `TestMain` checking only after a pass).
   Matching lines cannot tell these apart from a quarantined failure: each
   round found another.
2. Tests of different places with one name were one test: two crates'
   `tests::flaky_clock`, two directories' `tests/test_calc.py::test_flaky`
   (pytest names tests from its rootdir). A real failure of one was shown as
   the quarantined other, `diff` called it "known flaky", and history, flaky
   rates, sizes and slowest tests merged them. `quarantine add --in` worked
   only for Rust.
3. Selections: a nested Vitest name, a pytest node id and a Go subtest that
   does not exist failed or passed wrongly; a filter inside a wrapper's
   quoted argument was split and expanded twice.
4. A cancel killed another container that mounted the run's directory; a
   hang behind a container client was not named within the first wait,
   since a stall is called there only after 60 s.

## Decision

1. **A quarantine sets aside only the failures of one test-runner call.**
   The suite's command qualifies when, after environment assignments and
   wrappers that hand the rest to the runner (a script given the command as
   its argument, `sh -c`/`bash -c`, `docker`/`podman` `run`/`exec`,
   `docker compose run`/`exec`, `npx`, `pnpm`/`yarn`/`bun` exec,
   `uv run`/`poetry run`, `npm test` and `npm run <script>` read through
   `package.json`), what remains is one call of a known runner: `go test`,
   `gotestsum`, `cargo test`, `cargo nextest run`, `pytest` (`python -m
   pytest`), `vitest`, `jest`, `mvn`/`mvnw` with `test` as its only
   lifecycle goal, `gradle`/`gradlew test`, `dotnet test`; and nothing joins
   it to another command (`;`, `&&`, `||`, `|`, `&`, a newline, `$(` or a
   backtick). Two cases where the runner itself skips a check once a test
   fails also fail it: Vitest with coverage on (`--coverage`, or a
   coverage `thresholds` setting in its config), and a Go package of a
   quarantined test that defines `TestMain`. Otherwise the run fails as if
   nothing were quarantined, names the quarantined failure, says why, and
   suggests a suite that calls the runner directly. 0030 §1 still holds for
   what the runner prints. The runner's own records of the quarantined test
   (a JSON event of it, Maven's `[INFO] BUILD FAILURE`) are its own.
   A quarantined test that failed every time in a `--repeat` is named:
   quarantine is for flaky tests, not broken ones.
2. **A test keeps its place.** Tests with one name in different places
   (Rust test binaries, Go packages, pytest files, Vitest files) are
   different tests. A run reports each of them (`FAIL tests::flaky (geo)`),
   and `history`, `flaky`, `slowest`, `sizes` and `diff` keep them apart and
   show the place where names collide. `quarantine add --in` and `history
   --in` take the place in every framework, and `quarantine list` shows it.
   pytest ids are read from prova's working directory, not pytest's rootdir.
   A test recorded under a bare name and under its qualified name is one
   test. A live line names a test as the summary will.
3. **A selection runs what it names, as the framework names it.** A nested
   Vitest name, a pytest node id (`tests/x.py::t`, `::` inside a parameter)
   and a Go subtest select and verify that test; a parent that ran without
   the named subtest is no pass. "Outside the selection" lists only tests
   that ran and that the filter did not name in the framework's own form.
   A `{filter}` inside a quoted argument of a wrapper is quoted for the
   shell inside the wrapper, so it arrives whole and unexpanded.
4. **Containers.** A cancel kills a container created after the run started
   whose mounts include every directory the client's own `-v`/`--volume`
   arguments name, and whose command matches the client's when it gave one;
   containers created before the run are never candidates. A run through a
   container client waits at least 65 s before `--until first-failure`
   returns running, so a stall (60 s there) is named in the first wait. The
   list of tests that never reported follows the last finished run's order,
   counts tests it skipped, and is whole in `status --json`. A report or
   stream an inject recipe asked for that never appears is a warning that
   names the path, and says when a container client may not see it.

## Consequences

- A quarantine no longer helps a suite run through `make`, a shell script
  or a chain of steps: such a run fails while the quarantined test fails.
  The fix is a suite that runs the test runner alone, next to the full one.
- Ids of pytest tests run from a rootdir below prova's working directory
  change once; their history starts anew.
- A Go package with `TestMain` cannot have a quarantined failure set aside.
- `--until first-failure` through a container client can take 65 s.
