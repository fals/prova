# 0017: No false passes: a pass needs evidence

Status: Accepted (2026-11-22). Amends [0004](0004-liveness-and-fail-fast.md) (how the run outcome is decided; exit codes keep their meaning) and [0013](0013-black-box-adoption.md) (what each fidelity level may conclude).

## Context

An agent trusts `PASSED`. If toe says it when nothing was verified, the agent
ships a bug with toe's endorsement. Today the run outcome is the exit code,
lowered to failed when a recognized test failed. Probing toe with commands
that print what real frameworks print (2026-11-22, toe at f1d7a87) shows the
ways a run can look green while hiding a problem:

| # | Command prints / does | Exit | toe says | Problem |
|---|---|---|---|---|
| 1 | nothing | 0 | PASSED, 0 tests | nothing verified, no warning |
| 2 | libtest `running 0 tests` (all filtered out) | 0 | PASSED | nothing ran |
| 3 | Go `[no test files]` | 0 | PASSED | nothing ran |
| 4 | Jest `No tests found, exiting with code 0` | 0 | PASSED | nothing ran |
| 5 | Maven `No tests to run.` / `BUILD SUCCESS` | 0 | PASSED | nothing ran |
| 6 | VSTest `No test is available` | 0 | PASSED | nothing ran |
| 7 | every test skipped | 0 | PASSED | nothing verified |
| 8 | a test started and never finished | 0 | PASSED, 1 errored | **an errored test, still passed** |
| 9 | summary `1 passed; 3 failed`, one `ok` line seen | 0 | PASSED, soft note | **the framework says it failed** |
| 10 | traceback / `ModuleNotFoundError`, no tests | 0 | PASSED | the suite did not even load |
| 11 | panic after the summary | 0 | PASSED | crash after reporting |
| 12 | a suite that ran 3 tests now runs 1 | 0 | PASSED | 2 tests silently disappeared |
| 13 | a suite that ran tests now runs none | 0 | PASSED, opaque | discovery broke |
| 14 | pytest `no tests ran` / all deselected | 5 | FAILED, 0 tests | right outcome, no explanation |
| 15 | `command not found` | 127 | FAILED | not a test failure: the suite could not start |
| 16 | compile error, no test ran | 101 | FAILED, 0 tests | right outcome, says nothing about why |

Already right: a recognized failure with exit 0 (`| tee`, `make -i`) fails
the run; stale result files from before the run are ignored; a hang is a
stall; a selection matching nothing gets a note; Go's test cache gets a note.

## Decision

The run outcome is decided from all the evidence, and a pass must be earned.

1. **Contradictions fail the run** (outcome `failed`, exit 1), whatever the
   exit code:
   - a test errored or never finished (case 8);
   - the framework's own summary reports more failures than toe recorded
     (case 9): the framework is believed over its exit code;
   - the framework says no test ran (cases 2 to 6, 14), or a selection
     matched no test;
   - the suite ran tests in its last passing run and this run observed none
     (case 13).
2. **Not a test failure, errored** (outcome `errored`, exit 4): the command
   could not start (exit 126, 127: not executable, not found) (case 15).
3. **Passes that verified little say so.** `PASSED` stays, with a qualifier on
   the summary line and in JSON (`verified: false`, a `warnings` list):
   - no test was observed at all (exit code only, cases 1, 10 at level 0);
   - every observed test was skipped (case 7);
   - tests that ran in the suite's last passing run did not run now, with the
     count and the first names (case 12);
   - the output of a passing run contains error blocks (a traceback, a panic)
     outside any test (cases 10, 11): their first lines are shown.
4. **Failures without tests explain themselves**: when a run fails and no test
   failed, the summary says that no test failed, gives the framework's own
   reason when recognized (`no tests ran`, exit 5, `No test files found`) and
   shows the first error block of the excerpt (cases 14, 16).
5. **Specified first.** `spec/run.qnt` gains the run verdict: `passed`
   implies no failed or errored test and no contradiction; the rules above
   are checked by the model checker and replayed against the engine.
6. Exit codes keep their meaning (0 passed, 1 test failure, 4 infrastructure);
   what changes is which runs earn 0. The qualifier is never silent: an
   agent reading only the exit code still gets no false 0 for cases 2 to 9
   and 13.

## Consequences

- A suite whose discovery breaks, whose framework crashes after reporting, or
  whose summary disagrees with its exit code no longer passes.
- A command that legitimately runs no tests (a lint step) still passes, marked
  `verified: false`; a suite meant to run no tests can say so with
  `expect_tests = false` in `toe.toml`.
- Outcomes depend on the suite's history (cases 12, 13), so the same command
  can pass in a fresh repository and fail after its tests disappear. The
  summary always names the earlier run it compared with.

## Alternatives

- Warnings only, never a different outcome: agents read the outcome first;
  a green outcome with a footnote is the false impression this ADR removes.
- Fail every run with zero observed tests: breaks opaque commands with no
  test output, which ADR 0013 promises to support.
