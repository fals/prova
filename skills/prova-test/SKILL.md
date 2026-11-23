---
name: prova-test
description: Runs and fixes tests through prova with fast feedback and memory of past runs. Use whenever running, debugging or fixing tests in a project that has prova.toml or a prova section in AGENTS.md, instead of running test commands directly.
---

# Test loop with prova

Never run the raw test command. Never wrap tests in `timeout` or wait on a
long command. prova records every run and returns as soon as something
needs attention.

## Loop

1. One command per intent. Start the run and wait for what you need:

   ```sh
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

2. Read the result. Each failure carries a label; these change what you do
   next:

   - `[new failure]`: the test passed before and fails now. Likely your
     change.
   - `[first time seen: no history yet]`: prova cannot tell whether your
     change caused it. Run the suite once before changing code to get a
     baseline.
   - `[has not passed yet]`, `[failed Nx before]`: already failing before
     your change.
   - `FLAKY` or `likely FLAKY`: the same code both passed and failed. Do not
     chase it as your bug; confirm with `prova history <test>` and report it.
   - `SLOW`, or a stall: cancel, or keep waiting if it is expected.
   - `UNFINISHED`, `[run cancelled or aborted: ...]`: the run did not
     finish; no evidence about the code.
   - `PASSED (unverified: ...)` with `warning:` lines: the command succeeded
     but proved little (no test observed, tests skipped, stale results). Do
     not report "tests pass"; read the warnings and make the suite run its
     tests.
   - `note:` lines say what prova could not see (e.g. quiet output hides
     passing tests). Take them seriously: flaky detection depends on it.

   Every other label and warning is explained in
   [references/results.md](references/results.md).

   Exit codes: 0 passed, 1 test failure, 2 usage error, 3 stall/slow/killed,
   4 prova or infrastructure error (also: the test command could not start),
   5 still running (run the printed `next:` command).

3. React immediately: fix the first failure while the run continues, or
   `prova cancel <run>`. On `SLOW` or `STALLED`, decide: cancel, or keep
   waiting if expected.

4. Rerun only what failed: `prova run <suite> --select <test name>`. To
   check whether a test is flaky, run it several times in one command:
   `prova run <suite> --select <test> --repeat 5` prints each test's tally
   (`FLAKY ...: failed 2 of 5`). `--repeat` runs each run to its end within
   one budget (9 min, or `--max-wait`); `--until done` may be added, other
   `--until` values may not.

5. When green, run the whole suite once more. Before calling a failure
   new or flaky, check `prova history <test>`.

## Useful

- `prova status` recent runs; `prova status <run>` one run.
- `prova output <run> --test <name>` prints a failing test's own output
  (stack trace, captured output); `prova output <run>` prints the run's kept
  excerpt (first lines, error blocks, last lines). While the run is still
  going, `prova output <run>` shows everything printed so far.
- `prova diff <run-a> <run-b>` newly failing, fixed, slower tests; a known
  flaky test that changed is listed apart (`changed, but known flaky`), never
  as fixed.
- `prova flaky` known flaky tests. `prova sizes` how many tests of each size.

## Writing tests

Write the smallest test that can fail for the right reason. A test's size
is what it is allowed to use, never how long it takes (Software Engineering
at Google, ch. 11; ADR 0025):

| Size | Processes | Network | Also |
|---|---|---|---|
| small | one (the test's own) | none | no disk, no sleep, no blocking calls |
| medium | several, one machine | `localhost` only | a local database, temp files |
| large | any | other hosts | full-system, end-to-end |

- A fast test that starts a database or a server is medium, not small.
  A slow pure computation is small.
- Size is not scope. "Unit", "integration" and "end-to-end" say how much
  code a test covers; size says what it runs on. A unit test that talks to
  a local database is medium; an integration test of two in-process
  components is small.
- Use medium or large only with a one-line reason the test needs it.
- Never use sleep to wait for something; wait on a condition with a
  bound.
