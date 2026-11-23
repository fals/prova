---
name: prova-test
description: Run and fix tests through prova with fast feedback and memory of past runs. Use whenever running, debugging or fixing tests in a project that has prova.toml or a .prova directory, instead of running test commands directly.
---

# Test loop with prova

Never run the raw test command. Never wrap tests in `timeout` or wait on a
long command. prova records every run and returns as soon as something
needs attention.

## Loop

1. One command per intent. Start the run and wait for what you need:

   ```
   prova run <suite> --until first-failure   # returns on the first failure, a slow test, a stall, or the end (30 s budget)
   prova run <suite> --until done            # the result (budget 9 min, under a tool call's usual limit)
   ```

   When the budget ends first, prova exits 5 and prints the one command to
   continue (`next: prova wait <run> --until done`): run exactly that, once
   per tool call. Never wrap prova in shell loops, pipes, `sleep` or output
   parsing: every intent has a command, and `--json` gives one object when
   you need fields. If your tool calls are shorter than 9 minutes, pass
   `--max-wait`. While it waits, prova prints each new failure and a line
   every 30 s on stderr, for the person watching; the result is on stdout.
   Use `--detach` only to do other work meanwhile; then
   `prova wait <run>` (or `--until done`).

2. Read the result. Each failure carries history:

   - `[first time seen: no history yet]`: prova has never seen this test
     before (a new project, or a new test). It cannot tell whether your
     change caused it: run the suite once before changing code to get a
     baseline.
   - `[new failure]`: the test passed before and fails now. Likely your
     change, unless the next hint says otherwise.
   - `[has not passed yet]`, `[failed Nx before]`:
     it was already failing before; not caused by your change alone.
   - `UNFINISHED`: the test was still running when the run ended (cancel,
     kill); it says nothing about the code. `ERROR`: the test errored
     rather than failing an assertion.
   - `FLAKY` or `likely FLAKY: suite passed on this exact code before`: the
     same code both passed and failed. Do not chase it as your bug;
     confirm with `prova history <test>` and report it.
   - `[run cancelled or aborted: no evidence about the code]`: the run did
     not finish (cancel, `--kill-after`, a dead supervisor); its failures
     are often the cancel itself and count nowhere.
   - `SLOW`: over the test's budget, 3x its usual time (or the suite's size
     default when it has no history yet). A stall ends any wait: cancel.
   - `last passed at tree:...`: the last code where it was green.
   - `note:` lines say what prova could not see (e.g. quiet output hides
     passing tests). Take them seriously: flaky detection depends on it.
   - A test's history is shared by every suite that runs it (`make`,
     `default`, an ad-hoc `prova run -- ...`). Cancelled or aborted runs never
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

   Exit codes: 0 passed, 1 test failure, 3 stall/slow/killed, 4 prova or
   infrastructure error (also: the test command could not start), 5 still
   running (call `prova wait <run> --after <seq>`).

3. React immediately: fix the first failure while the run continues, or
   `prova cancel <run>`. On `SLOW` or `STALLED`, decide: cancel, or keep
   waiting if expected.

4. Rerun only what failed: `prova run <suite> --select <test name>`. To
   check whether a test is flaky, run it several times in one command:
   `prova run <suite> --select <test> --repeat 5` prints each test's tally
   (`FLAKY ...: failed 2 of 5`).

5. When green, run the whole suite once more. Before calling a failure
   new or flaky, check `prova history <test>`.

## Useful

- `prova status` recent runs; `prova status <run>` one run.
- `prova output <run> --test <name>` prints a failing test's own output
  (stack trace, captured output); `prova output <run>` prints the run's kept
  excerpt (first lines, error blocks, last lines). While the run is still
  going, `prova output <run>` shows everything printed so far.
- `prova diff <run-a> <run-b>` newly failing, fixed, slower tests.
- `prova flaky` known flaky tests. `prova sizes` tests by observed duration.

## Writing tests

Write the smallest test that can fail for the right reason: one process,
no network, no disk, no sleep (small). Use medium (localhost, temp files,
subprocesses) or large only with a one-line reason. Never use sleep to
wait for something; wait on a condition with a bound.
