# 0023: Evidence hygiene for test history; more isolation between arms

Status: Accepted (2026-11-23). Amends [0022](0022-checkouts-load-and-impossible-results.md) (failing now, scope, isolation), [0021](0021-verdicts-under-adversarial-candidates.md) (`--change`) and [0017](0017-no-false-passes.md) (more causes of an unverified pass). Superseded in part by [0037](0037-prova-keeps-what-it-can-know.md).

## Context

Round 10 of the usability check: a Java false-pass hunt, a Python red team
on experiments, an honest Go optimization loop, a hurried C# first-timer.

- A pass prova had flagged as unverified (stale reports) still counted in
  each test's history, and made a real, deterministic bug "FLAKY".
- Clean passes on runs that did not pass: a crash in a later test class
  (`BUILD FAILURE`, a dead fork) after earlier classes passed; stale reports
  refreshed with `touch` in a run longer than any test; a run that read
  another concurrent run's report files; `--- PASS` lines echoed by a
  command that did no work; `@Disabled` on a test that passed in another
  command.
- "Failing now" (0022) fired on a coin-flip test after 3 failures in a row,
  which happens 1 run in 8.
- `go test` with a `cmd/` package printed `[no test files]`, and a green run
  failed.
- Experiments: Python's user site-packages and a `.pyc` planted in the other
  arm's ignored `__pycache__` crossed arms; `.pytest.ini` and a top-level
  `random.py` changed the oracle; a candidate accepted `--against baseline`
  replaced a faster best; `--change go.mod` narrowed the scope to go.mod; a
  comparison that failed at its oracle froze the protocol.

## Decision

Test runs:

1. **An unverified run is no evidence about any test**: its attempts are
   left out of every test's history, and "last green" names verified passes
   only.
2. **More causes of an unverified pass**: every duration equal to an earlier
   run's to the millisecond; a command that used no CPU time at all; result
   files that another run in the same directory, at the same time, may have
   written; a test skipped now that last passed in any suite or command.
3. **The build's own failure lines fail the run** (`BUILD FAILURE`, a
   crashed or terminated test fork, `Build failures were ignored`).
4. **"Failing now" needs a streak the test's own failure rate makes
   unlikely** (under 1% by chance, at least 3): about 7 for a coin flip.
5. A Go package without test files next to packages that passed is not "no
   test ran"; a run failed only by rerun passes says so.

Experiments:

6. **`--change` adds to the default scope**; `--change-only` makes it the
   whole scope. Python configuration and startup files (`.pytest.ini`,
   `sitecustomize.py`, `*.pth`) and top-level modules that shadow the
   standard library are files that decide the verdict.
7. **More isolation between arms**: trials, setup and oracle run without
   Python's user site-packages and with a private bytecode cache per
   checkout; a process the oracle leaves behind is a violation.
8. **A variant that drifts from its own history voids the comparison**: its
   median moved 2x or more from its earlier comparisons in the experiment.
9. **Only a win over the current best changes the best.**
10. The protocol freezes at the first trial, not at a comparison that
    failed before measuring.

## Consequences

- Echo-based test fixtures must do a little real work; prova's own tests do.
- The same-user limit stands: a candidate can still reach the other arm
  through paths outside both checkouts that its own code can find (the
  shared temporary directory, the user's home). A per-trial sandbox (mount
  and user namespaces, read-only bind mounts, private home and tmp) is the
  complete answer and the next step; the skills say to read the candidate's
  diff, and to keep the candidate's code out of the oracle's process.
