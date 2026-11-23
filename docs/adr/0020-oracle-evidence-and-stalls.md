# 0020: The oracle shows its work; a stall ends every wait

Status: Proposed (2026-11-23). Amends [0015](0015-spec-first-experiments.md) (what makes an oracle hold), [0019](0019-one-command-per-intent.md) (which waits a stall ends) and [0018](0018-honest-history-and-live-nextest.md) (tests killed by a cancel).

## Context

A seventh round of cold agents on the fixtures found three ways prova still
misled:

1. **An oracle passed without checking.** In an experiment the candidate
   added a test file whose `TestMain` exited 0 unless benchmarks were
   requested. The oracle `make conformance` exited 0, prova printed
   `specification: holds on both arms`, and accepted an implementation that
   returns `CountPrimes(2) = 1`. Only the registered spec files are frozen;
   the oracle was judged by its exit code alone, and its log was discarded
   because it passed.
2. **A plain run waited forever on a hang.** `prova run -- make test`
   printed `stalled` and kept waiting; 0019 left attached runs and
   `--until done` to wait out a stall, contrary to 0004's "no long waits".
3. **A killed test looked like a failure.** nextest reports the test it was
   running when prova cancels the run as failed, so a cancelled hang read
   `FAIL ... has been running for over 60 seconds`.

## Decision

1. **The candidate's oracle must pass what the baseline's oracle passed.**
   prova reads the oracle's output with its recognizers. Every test that
   passed in the baseline's oracle run must pass in the candidate's; a
   missing one is a violation ("its checks were skipped"), rejected
   unmeasured like any other. When the oracle shows no individual test, the
   verdict says it holds **by exit code only** and asks for a verbose oracle;
   with tests, it says how many passed on each arm.
2. **The oracle's log is always kept** (as an excerpt, 0016), so a "holds"
   can be audited, not only a violation.
3. **A stall ends every wait**: attached runs, `--until done`, `wait`. The
   run keeps going detached and the summary says to cancel it. While a
   cancel is under way, a stall no longer ends the wait for its end.
4. **A test that fails while the run is being cancelled or killed is
   unfinished** (errored, "did not finish"), not failed: the cancel caused
   it. Such runs already count nowhere as evidence (0018).

## Consequences

- An oracle that prints nothing still works, with a weaker verdict that
  says so; the prova-spec skill asks for verbose oracles.
- A candidate can still defeat an oracle that prints nothing; freezing the
  oracle's own files (`--spec`) and a verbose oracle close that.
- A long attached run with a quiet phase longer than `--stall-after`
  returns early; continue with the printed `prova wait`.
