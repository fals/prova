# 0034: A quarantine never turns a failed run green

Status: Accepted (2026-11-25). Supersedes [0027](0027-quarantine-and-slow-tests.md) §4, [0028](0028-quarantine-sets-aside-only-what-it-explains.md) §1, [0029](0029-quarantine-accounts-for-every-error-and-hangs-in-the-first-wait.md) §1, [0030](0030-nothing-after-the-summary-and-named-selections.md) §1 and [0031](0031-quarantine-needs-one-runner-call-and-tests-keep-their-place.md) §1.

## Context

ADR 0027 let a run whose only failures were quarantined tests pass. Each
usability round since (14 to 18) found another way that pass hid a real
failure, and each fix added a rule: crash lines (0028), every error line
(0029), nothing after the tests' summary (0030), one test-runner call
(0031). Round 18 still found five false passes: runners told to stop at
the first failure (`go test -failfast`, `pytest -x`, `cargo test` stopping
after a failed test binary) never ran the tests after the quarantined one;
a `TestMain` refusal that never fired; Vitest coverage set in another
file; a wrapper script running a lint step. A run's output cannot prove
that nothing but the quarantined test went wrong: what did not run, or ran
out of sight, prints nothing.

## Decision

1. **A quarantine never makes a run look better.** It changes neither the
   verdict, nor the exit code, nor whether a pass counts as verified: a run
   with a failing quarantined test fails (exit 1), whatever else it shows,
   and the warnings a pass gets (a flaky test that passed, a known test
   that did not run or was skipped, a pass on a retry) apply to quarantined
   tests as to any other. The only thing it changes is that a first-failure
   wait does not wake for a failure that is already known (0027 §5).
2. **It still says what it knows.** The summary lists each quarantined test
   with its result and reason, and when every failing test of the run is
   quarantined it says so (`every failing test is quarantined: ...`),
   without claiming nothing else went wrong. The test still runs and keeps
   its history; `--repeat` tallies it; quarantines stay local, named and
   expiring (0027 §1 to §3, §5 and §6 stand).
3. The rules that existed only to decide whether a quarantined failure
   could pass a run (0028 §1, 0029 §1, 0030 §1, 0031 §1) are removed with
   their code. The spec drops `withQuarantine`.

## Consequences

- A suite with a known flaky test stays red until the test is fixed or
  left out of the command: the honest state. A quarantine tells an agent
  which failure is already known, not that the run is fine.
- Passes recorded before this decision with quarantined failures set aside
  stay in history as they were.
