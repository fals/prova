---
name: toe-test
description: Run and fix tests through toe with fast feedback and memory of past runs. Use whenever running, debugging or fixing tests in a project that has toe.toml or a .toe directory, instead of running test commands directly.
---

# Test loop with toe

Never run the raw test command. Never wrap tests in `timeout` or wait on a
long command. toe records every run and returns as soon as something
needs attention.

## Loop

1. Start the run detached and wait for the first significant event:

   ```
   toe run <suite> --detach          # the run id on stdout (hints on stderr): r=$(toe run <suite> --detach)
   toe wait <run> --max-wait 30s     # returns on first failure, slow test, stall, or end
   ```

   `toe run <suite>` without `--detach` does both and blocks until the end;
   use it only for short suites.

2. Read the result. Each failure carries history:

   - `[first time seen: no history yet]`: toe has never seen this test
     before (a new project, or a new test). It cannot tell whether your
     change caused it: run the suite once before changing code to get a
     baseline.
   - `[new failure]`: the test passed before and fails now. Likely your
     change, unless the next hint says otherwise.
   - `[has not passed yet]`, `[failed Nx before, with another message]`:
     it was already failing before; not caused by your change alone.
   - `UNFINISHED`: the test was still running when the run ended (cancel,
     kill); it says nothing about the code. `ERROR`: the test errored
     rather than failing an assertion.
   - `FLAKY` or `likely FLAKY: suite passed on this exact code before`: the
     same code both passed and failed. Do not chase it as your bug;
     confirm with `toe history <test>` and report it.
   - `[seen 3x before]`: pre-existing failure.
   - `last passed at tree:...`: the last code where it was green.
   - `note:` lines say what toe could not see (e.g. quiet output hides
     passing tests). Take them seriously: flaky detection depends on it.
   - A test's history is shared by every suite that runs it (`make`,
     `default`, an ad-hoc `toe run -- ...`). Cancelled or aborted runs never
     count as evidence: cancelling a hung test does not make it flaky.
   - `running now:` names the tests still running when a run stalls or a
     test is slow; `framework counted N` gives the framework's own count
     when its output hid individual tests.

   - `PASSED (unverified: ...)` with `warning:` lines: the command
     succeeded but proved little: no test was observed, every test was
     skipped, tests from the last passing run did not run, or the output
     shows a crash outside any test. Do not report "tests pass" on an
     unverified run; read the warnings and make the suite run its tests.
     In `--json`, `verified` is `true` only for a clean pass.
   - A run fails, even with exit 0, when the framework itself says it
     failed or ran nothing (`detail:` names the reason), when a test never
     finished, or when the suite's tests vanished since its last pass.

   Exit codes: 0 passed, 1 test failure, 3 stall/slow/killed, 4 toe or
   infrastructure error (also: the test command could not start), 5 still
   running (call `toe wait <run> --after <seq>`).

3. React immediately: fix the first failure while the run continues, or
   `toe cancel <run>`. On `SLOW` or `STALLED`, decide: cancel, or keep
   waiting if expected.

4. Rerun only what failed: `toe run <suite> --select <test name>`.

5. When green, run the whole suite once more. Before calling a failure
   new or flaky, check `toe history <test>`.

## Useful

- `toe status` recent runs; `toe status <run>` one run.
- `toe output <run> --test <name>` prints a failing test's own output
  (stack trace, captured output); `toe output <run>` prints the run's kept
  excerpt (first lines, error blocks, last lines). While the run is still
  going, `toe output <run>` shows everything printed so far.
- `toe diff <run-a> <run-b>` newly failing, fixed, slower tests.
- `toe flaky` known flaky tests. `toe sizes` tests by observed duration.

## Writing tests

Write the smallest test that can fail for the right reason: one process,
no network, no disk, no sleep (small). Use medium (localhost, temp files,
subprocesses) or large only with a one-line reason. Never use sleep to
wait for something; wait on a condition with a bound.
