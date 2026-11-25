# 0028: A quarantine sets aside only what its tests explain; a hang shows on the first wait

Status: Accepted (2026-11-24). Modelled in `spec/run.qnt` (`withQuarantine`, invariant `quarantineHidesOnlyItsOwn`, test `onlyQuarantinedFailingIsNoPassTest`). Amends [0027](0027-quarantine-and-slow-tests.md) §4 and §5, and [0004](0004-liveness-and-fail-fast.md) (stall default). §1 superseded by [0034](0034-a-quarantine-never-turns-a-failed-run-green.md): a quarantine never turns a failed run green.

## Context

Usability round 14 (four fixture agents and about 60 adversarial probes on
v0.1.5):

1. One quarantined test's failure passed runs that also showed a build
   failure in another package, a data race, a killed test binary, or a
   framework summary counting more failures than the quarantined test had.
   0027 §4's guard looked only at per-test outcomes, and those failures had
   none.
2. A parent's quarantine (`TestQ`) set aside a subtest first seen after it
   (`TestQ/new`): a new regression hidden by an old quarantine. Quarantining
   a subtest did nothing, since Go fails its parent too, and the subtest was
   listed as `not run` though it ran.
3. A run of only the quarantined test (`--select FlakyClock`) that failed was
   `PASSED`, and became the next run's "last green".
4. The quarantined test's own panic line made its set-aside pass
   "unverified: an error outside any failing test".
5. Two agents expected a quarantine to be visible to their team; it lives in
   one machine's store (0027 §1), and nothing said so.
6. A hang was never caught by the first `--until first-failure` wait (30 s):
   a suite with no declared size stalled after 60 s, and nextest reports a
   test's start only with its result or its own 60 s warning.

## Decision

1. **A quarantined failure passes the run only if it explains everything
   that went wrong.** On top of 0027 §4, the run fails (a contradiction,
   0017) when the framework's own count of failures is above the number of
   quarantined failures, or when the output shows a crash line (0017's
   "error outside any failing test", now also `WARNING: DATA RACE`,
   `signal: killed`, `[build failed]`, `[setup failed]`) that no quarantined
   failing test accounts for: it names one of them, or it is part of one's
   own output or failure message. The same attribution keeps a quarantined
   test's own crash from making its set-aside pass unverified.
2. **Subtests.** A parent's quarantine covers a subtest only if the subtest
   was known before the quarantine was added. A parent that failed only
   through its quarantined subtests is set aside with them. A quarantined
   subtest is listed by its own result.
3. **Something else must be verified.** A quarantine turns a failed run into
   a pass only when at least one test that is not quarantined passed.
4. **It is said everywhere.** The header is `PASSED (quarantined failures
   set aside)`; the run list marks such a run `passed*` (and explains `*`
   and `?` below the list); `--repeat` names it per run and in the tally;
   a running summary lists quarantined results so far. A quarantined test
   with no result is `not reported`, not `not run`, when the run gave no
   per-test results or was cut short.
5. **Where it lives is said.** 0027 §1 stands (the store, never the
   project). `prova quarantine --help` and `add` say that the quarantine is
   kept in this machine's store and that CI and teammates do not see it.
   `add` refuses a bare name that is also known qualified (`TestX` and
   `pkg::TestX`) and names the qualified one.
6. **A hang shows on the first wait.** A suite with no declared size stalls
   after 30 s of no activity, as a small one does (the help already said
   30 s). Every summary of a running run gives each running test's time so
   far (`running now: TestHang (for 28.4s)`).

## Consequences

- A quarantine can no longer stand in for unrelated breakage; the price is
  that a quarantined test whose failure prints an unattributable crash line
  fails the run, which is the safe side.
- A team that wants a shared quarantine still has none; deciding to keep one
  in the repository would supersede 0027 §1.
- A quiet medium test that waits over 30 s with no CPU, output or test
  event in a suite with no declared size is reported as stalled; the run
  goes on, and declaring `size = "medium"` restores 60 s.
