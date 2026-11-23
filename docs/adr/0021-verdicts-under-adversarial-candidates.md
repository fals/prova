# 0021: Verdicts under careless or adversarial candidates; idle runtimes stall

Status: Accepted (2026-11-23). Amends [0007](0007-experiments.md) and [0015](0015-spec-first-experiments.md) (what a candidate may change, what voids a comparison) and [0004](0004-liveness-and-fail-fast.md) (what counts as activity).

## Context

In usability round 8 an agent red-teamed experiments on the Go fixture and
got five false ACCEPTs:

1. a new test file whose `TestMain` printed the oracle's `--- PASS` lines
   and exited (accepted, "the same 3 tests passed on each");
2. code that detects the oracle and is only correct there;
3. memoization across benchmark iterations (76169x);
4. a new file printing fake `BenchmarkCountPrimes ... ns/op` lines: the
   median of real and fake lines was the fake value;
5. a smaller benchmark input in the benchmark file;
6. a `setsid` process, outside the trial's group, that burned CPU during the
   other arm's trials (2.40x, p = 0.026, identical code).

In the same round, a hung JUnit test and a hung xUnit test were never
reported: an idle JVM or .NET runtime still uses a little CPU (GC, timers),
and any CPU tick counted as activity.

## Decision

1. **A candidate may change only its scope.** The protocol has a scope
   (`--change <glob>`, repeatable). Without one, a candidate may change
   anything except the files that decide the verdict: tests, benchmarks,
   build and runner files (a fixed list of conventions per ecosystem:
   `*_test.go`, `test_*.py`, `conftest.py`, `*.test.*`, `*.bench.*`,
   `src/test/**`, `*Tests.cs`, `Makefile`, `*.sh`, `package.json`,
   `Cargo.toml`, `pom.xml`, ...). The candidate's diff against the baseline
   is checked before the oracle; out of scope is rejected unmeasured.
2. **Each arm reports the metric equally often.** The number of output lines
   that report the primary metric in a trial must be the same on both arms;
   otherwise the comparison is void (forged lines, or a changed workload).
3. **A trial leaves nothing running.** The experiment worker is a child
   subreaper; any process a trial leaves behind, even outside its group, is
   killed and voids the comparison.
4. **Specification files must be committed**: `exp new` refuses a `--spec`
   file that differs from the baseline ref.
5. **Implausible wins are flagged.** An ACCEPT of 1000x or more carries a
   warning to check the workload and caching.
6. **The best can be reset by hand** (`prova exp best <exp> <variant>
   --reason`), recorded next to the decision log (migration 0006).
7. **Activity needs real CPU.** A process tree is active when it used at
   least 10 clock ticks (100 ms) in the last 5 s, i.e. 2% of one core, or
   started a process, or printed output. A stalled run with no test seen
   running names the suspects: tests of the last finished run that have not
   reported.

## Consequences

- prova defends against changed harness files, skipped or forged oracle
  runs from added files, forged metric lines, and leaked processes. It does
  not defend against code under test that detects the oracle or benchmark
  and behaves differently there (attack 2), or caches across iterations
  (attack 3) beyond the plausibility warning: reading the diff remains the
  reviewer's job, and the skills say so.
- An honest change that needs a dependency or build change must say so with
  `--change`.
- A test that legitimately waits with almost no CPU and no output for
  longer than `--stall-after` is reported as a stall, which is a signal to
  decide on, not a kill.
