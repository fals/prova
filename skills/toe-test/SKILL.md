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
   toe run <suite> --detach          # prints the run id
   toe wait <run> --max-wait 30s     # returns on first failure, slow test, stall, or end
   ```

   `toe run <suite>` without `--detach` does both and blocks until the end;
   use it only for short suites.

2. Read the result. Each failure carries history:

   - `[new failure]`: toe has not seen this failure before. Likely your
     change, unless the next hint says otherwise.
   - `FLAKY` or `likely FLAKY: suite passed on this exact code before`: the
     same code both passed and failed. Do not chase it as your bug;
     confirm with `toe history <test>` and report it.
   - `[seen 3x before]`: pre-existing failure.
   - `last passed at tree:...`: the last code where it was green.
   - `note:` lines say what toe could not see (e.g. quiet output hides
     passing tests). Take them seriously: flaky detection depends on it.

   Exit codes: 0 passed, 1 test failure, 3 stall/slow/killed, 4 toe or
   infrastructure error, 5 still running (call `toe wait <run> --after <seq>`).

3. React immediately: fix the first failure while the run continues, or
   `toe cancel <run>`. On `SLOW` or `STALLED`, decide: cancel, or keep
   waiting if expected.

4. Rerun only what failed: `toe run <suite> --select <test name>`.

5. When green, run the whole suite once more. Before calling a failure
   new or flaky, check `toe history <test>`.

## Useful

- `toe status` recent runs; `toe status <run>` one run.
- `toe output <run> --tail 200` raw output when messages are not enough.
- `toe diff <run-a> <run-b>` newly failing, fixed, slower tests.
- `toe flaky` known flaky tests. `toe sizes` tests by observed duration.

## Writing tests

Write the smallest test that can fail for the right reason: one process,
no network, no disk, no sleep (small). Use medium (localhost, temp files,
subprocesses) or large only with a one-line reason. Never use sleep to
wait for something; wait on a condition with a bound.
