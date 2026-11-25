# 0029: A quarantine accounts for every error line; repeats are not retries; a hang is named in the first wait

Status: Accepted (2026-11-24). Amends [0028](0028-quarantine-sets-aside-only-what-it-explains.md) §1 and §6, [0024](0024-sandbox-retries-and-abba.md) (what a retry is), and [0004](0004-liveness-and-fail-fast.md) (stall default). §1 superseded by [0034](0034-a-quarantine-never-turns-a-failed-run-green.md): a quarantine never turns a failed run green.

## Context

Usability round 15 (five fixture agents, two of them through a Docker
`./dev` wrapper, and about 85 adversarial probes on v0.1.6):

1. 0028's guard matched a short list of crash lines. A coverage gate, a type
   checker, a linter or another make target after the tests, an interrupted
   pytest session, a run killed by a signal (exit 137), a crash of
   `test_queue` next to a quarantined `test_q`, Vitest's unhandled errors and
   a panic in a Go package's own output all passed behind one quarantined
   failure. In the other direction, nextest's own `error: test run failed`
   line made every quarantined pass under nextest fail.
2. `go test -count=N` iterations were treated as retries, where the last
   attempt decides: a test that failed in one iteration and passed in the
   next was a pass, and the run's failure was "no test failed".
3. A new hang still took two `--until first-failure` calls: a test with no
   history was slow only after 60 s, and a quiet run stalled after 30 s, the
   length of the first wait.

## Decision

1. **A quarantined failure passes the run only if every error-looking line
   of the output is accounted for** (replacing 0028 §1's crash-line check).
   A line is accounted for when it names a quarantined failing test as a
   whole name (not inside a longer one), lies in that test's own section of
   the output (pytest's `____ test ____` block, libtest's `---- test stdout
   ----`, Jest's `● test`, Go's `=== RUN`, a `go test -json` event of that
   test) or its own output or message, or is the runner's summary of failing
   tests: counts of failures with no errors, bare `FAIL`, a Go package's
   result line, nextest's and cargo's wrap-up, Maven's wrap-up of a failed
   test step, and make's line for a target the suite's command names.
   Error-looking lines now include type checkers, linters, coverage gates,
   `make: ***`, interrupted and internal-error sessions, unhandled errors and
   test timeouts. More error lines than prova keeps (2000) is itself a
   reason to refuse. A run killed by a signal (exit above 128) is never set
   aside. A refused quarantine says which condition did not hold.
2. **Repeats are not retries.** Iterations of a test within one run of its
   package (`go test -count=N`) are repeats: a failure in any of them fails
   the test. A rerun after the package's result (gotestsum
   `--rerun-fails`, a retry by the runner) is a retry, decided by its last
   attempt and flagged flaky (0024).
3. **A hang is named in the first wait.** A suite with no declared size, and
   a small one, stalls after 20 s of no activity; a test with no history in
   a suite with no declared size is slow after 20 s. Both fall inside the
   30 s `--until first-failure` wait. A slow budget at the 1 s minimum says
   so instead of "3x its usual time".
4. **Tests run through a container client.** When the command's client was
   started with `--name` or `--cidfile`, cancelling the run (or killing it at
   `kill_after`) kills that container too. Otherwise the summary warns that
   the container may still be running. Sizes say the tests were out of sight.

## Consequences

- A quarantine now refuses more runs; each refusal names the line or the
  condition, and false refusals are on the safe side.
- A quarantined test whose failure prints something prova cannot attribute
  (for example a runner's summary in a format not listed) fails the run
  until that format is taught to prova.
- A medium-sized wait of over 20 s with no CPU, output or test event, in a
  suite whose size is not declared, is reported as a stall; declaring
  `size = "medium"` restores 60 s.
