# 0030: A quarantine sets nothing aside after the tests' summary; a named selection must run its test

Status: Accepted (2026-11-24). Amends [0029](0029-quarantine-accounts-for-every-error-and-hangs-in-the-first-wait.md) §1 and §4, and [0027](0027-quarantine-and-slow-tests.md) §2.

## Context

Usability round 16 (five fixture agents, two through Docker wrappers, and
about 95 adversarial probes on v0.1.7):

1. 0029 accounted for error-looking lines by vocabulary. Tools that print
   no error word passed behind a quarantined failure: ruff, flake8, go vet,
   shellcheck, a smoke step, `coverage report --fail-under`, a Jest
   coverage threshold; pytest-cov's gate printed inside the quarantined
   test's FAILURES section was absorbed by it. In the other direction, real
   outputs were refused: gotestsum's `✖  . (1.5s)`, VSTest's failure block,
   Vitest's `FAIL file > describe > test` block, Maven's `BUILD FAILURE`.
2. Two crates' `tests::flaky` share one name: a quarantine of one hid the
   other's failure.
3. `--select tests.test_q::test_flaky` became pytest's `-k test_flaky`,
   which ran only `test_flaky_slow`, and the run passed; a package-qualified
   Go selection ran another package's test of the same name.
4. Through a container client, a restore inside the container stalled the
   run at 20 s, and a cancel left an unnamed container running.

## Decision

1. **Nothing after the tests' summary.** A quarantined failure is set aside
   only if the output after the test runner's last summary line (pytest's
   banner, libtest's `test result:`, Jest/Vitest counts, gotestsum's `DONE`,
   Go's package lines, Maven's `Tests run:`/`BUILD`, dotnet's counts,
   nextest's `Summary`) holds nothing but the command's own wrap-up of the
   failed test step (make's line for the suite's own target, npm's lifecycle
   lines, cargo's and nextest's wrap-up, Maven's help lines). Anything else
   is another step whose result cannot be told apart: the run fails, and
   says so. Coverage gates are never accounted for. A crash-like line is
   accounted for only inside the quarantined test's own section or output,
   never by its name alone. A runner's count of failed files above the files
   of quarantined failures, or two failed cargo targets, is another failure.
   Real outputs of each supported framework are kept as a corpus
   (`crates/prova-cli/tests/fixtures/quarantine/`): with one quarantined
   failure each passes, with any other failure added each fails.
2. **A quarantine names its file when the name is shared.** `quarantine
   add` refuses a test name held by tests of several files (binaries,
   modules) unless `--in <file>` picks one; only that one is set aside.
   File-level entries (`(file)`, `(package)`, `(suite)`) cannot be
   quarantined.
3. **A selection that names a test in full must run it.** When `--select`
   gives a full name (`a::b`), the run fails unless that test, one of its
   subtests or one of its parameterized cases ran; tests the filter matched
   besides it are named.
4. **Containers.** With a container client, process inactivity alone is no
   stall before 60 s, and the stall says the tests run in a container. A
   cancel kills the run's container when exactly one container created
   during the run mounts its directory; otherwise it names the candidates.

## Consequences

- A quarantine refuses any run whose command does more after its tests than
  wrap up their failure; a project that lints after testing gets a pass only
  when the tests pass, or by running the lint as its own suite.
- A wrapper's or reporter's footer that prova has not been taught refuses a
  quarantine until it is added to the corpus: the safe side.
- An explicit `--stall-after` under 60 s does not apply to a run through a
  container client.
