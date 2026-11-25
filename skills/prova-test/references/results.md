# Reading a prova result: every label

Contents: [Failure history](#failure-history) ·
[Flaky tests](#flaky-tests) · [Run state](#run-state) ·
[Unverified passes](#unverified-passes) · [Failed runs](#failed-runs) ·
[Notes and counts](#notes-and-counts)

## Failure history

- `[first time seen: no history yet]`: prova has never seen this test
  before (a new project, or a new test). It cannot tell whether your change
  caused it: run the suite once before changing code to get a baseline.
- `[new failure]`: the test passed before and fails now. Likely your
  change, unless the next hint says otherwise.
- `[has not passed yet]`, `[failed Nx before]`: it was already failing
  before; not caused by your change alone.
- `ERROR`: the test errored rather than failing an assertion.
- `last passed at tree:...`: the last code where it was green.
- A test's history is shared by every suite that runs it (`make`,
  `default`, an ad-hoc `prova run -- ...`).

## Flaky tests

- `FLAKY` or `likely FLAKY: suite passed on this exact code before`: the
  same code both passed and failed. Do not chase it as your bug; confirm
  with `prova history <test>` and report it.
- `failed, then passed on a retry`: the run passes (a test's last attempt
  decides) but unverified: the test is flaky.
- `failing now: more failures in a row than its flakiness explains`: a
  test once flaky that has failed too many times in a row for its usual
  failure rate (3 for a rare flake, about 7 for a coin flip). Treat it as a
  real failure.
- `note: N known flaky test(s) passed this time`: the run is green, but
  those passes prove little. Say so when you report the suite green.
- `environment-dependent, not flaky`: the test passed on this code only
  with different environment variables (named); find which one matters.
- `warning: N known flaky test(s) did not run: ...`: the command left a
  known flaky test out (an exclude filter, a narrower suite). That is not a
  pass of it, and the run is unverified. To set a flaky test aside in the
  open instead, `prova quarantine add <test> --reason "..."` (expires in 14
  days, `--days N` up to 90): it still runs and keeps its history; if it is
  the only failure the run passes, verified for the rest, with the header
  `PASSED (quarantined failures set aside)`. Every summary lists it as
  `quarantined: <test> <result> (<reason>; expires in Nd)` (`not reported`
  when the run gave no per-test results or was cut short, `not seen (quiet
  output)` when quiet output hides passing tests);
  `prova quarantine list` and `remove` manage it. **A quarantine sets aside
  the failures of one test-runner call, nothing more.** The suite's command
  must be the runner alone (`go test ./...`, `cargo nextest run`, `pytest`,
  `vitest run`, `mvn test`, `dotnet test`, ...), optionally behind `sh -c`,
  `docker run`/`exec`, `npx`, `uv run`, `npm test` (read through
  package.json) or a script given the command as one quoted argument. A
  `make` target, a script, or a chain (`&&`, `;`, `|`) fails the run
  while the quarantined test fails, and `detail:` says what made it so:
  another step could have failed or been skipped, and prova cannot see it.
  The fix is a second suite whose command is the runner itself, next to the
  full one. Vitest with coverage thresholds and a Go package with
  `TestMain` are refused too (the runner skips a check once a test fails).
  Within one runner call, every error line of the output must be its
  own (in its section, naming it, or the runner's summary of failures), so
  another failure, an error, a framework count above its failures, or a run
  killed by a signal fails the run, and `detail:` says which. Nothing may
  follow the tests' summary but the runner's own wrap-up. When tests of several binaries share a name,
  `prova quarantine add <test> --in <binary>` names which; a parent's quarantine does not cover a
  subtest first seen after it; a run where only quarantined tests ran and
  failed fails. A quarantine lives in this machine's prova store, not in
  the repository: CI and teammates do not see it. Do not quarantine a test
  that fails every time now; that is a real failure (`--repeat` says "broken,
  not flaky" when a quarantined test failed in every run).
- `slow tests: a 1.5s, b 1.1s (prova slowest)`: the slowest tests of this run
  (1 s or more). `prova slowest [--suite S] [-n N]` lists p50 and p95 from
  history, worst first.

## Run state

- `UNFINISHED`: the test was still running when the run ended (cancel,
  kill); it says nothing about the code.
- `[run cancelled or aborted: no evidence about the code]`: the run did
  not finish (cancel, `--kill-after`, a dead supervisor). Tests it cut off
  are often failed by the cancel itself and count nowhere: cancelling a
  hung test does not make it flaky. Tests that finished before the cancel
  are real results and stay in history; a cut-short run says nothing about
  sizes.
- `SLOW`: over the test's budget; the line says where it came from (3x its
  usual time, the suite's default, or the test runner's own warning).
- A stall ends any wait, including a plain `prova run -- ...`: cancel it.
- `running now:` names the tests still running, with how long each has
  run (`TestHang (for 28.4s)`): one running far longer than it usually
  takes is the likely hang, before any budget says so. A suite with no
  declared size stalls after 20 s of no activity, and a test with no history
  is slow after 20 s: a hang is named within the first 30 s wait.

## Unverified passes

`PASSED (unverified: ...)` with `warning:` lines: the command succeeded but
proved little. The warnings name the reason:

- no test was observed, or every test was skipped;
- tests from the last passing run did not run or were skipped (`#[ignore]`,
  `@Disabled`, `skip`; also when they passed in another suite or command);
- a test reports (or always took) longer than the whole run;
- the results equal an earlier run's to the millisecond (stale files);
- the command used no CPU at all (results printed, not run);
- another run in the same directory may have written the result files;
- the output shows a crash outside any test;
- a test passed only on a retry.

A warning that ends with "no evidence about the tests" (impossible or
identical durations, no CPU, shared result files, replayed output) keeps
the run out of every test's history.

`prova status` lists an unverified run as `passed?`, and a run that passed
with quarantined failures set aside as `passed*`. In `--json`,
`verified` is `true` only for a clean pass. Do not report "tests pass" on
an unverified run; read the warnings and make the suite run its tests.

`prova forget <run> --reason "..."` takes a run whose results were not real
out of every test's history (and the flaky labels it caused). A run that
failed before any test ran (a missing tool or SDK) says so and suggests it;
history lists such runs as `failed (no test seen)`.

## Failed runs

A run fails, even with exit 0, when:

- the framework or build tool says it failed (`BUILD FAILURE`, a crashed
  test JVM) or ran nothing (`detail:` names the reason);
- a test never finished;
- the suite's tests vanished since its last pass.

## Notes and counts

- `note:` lines say what prova could not see (e.g. quiet output hides
  passing tests). Take them seriously: flaky detection depends on it.
- `framework counted N` gives the framework's own count when its output
  hid individual tests.
- `(and N subtests, M failed)`: results nested under a test that reports
  its own (Go's `TestX/case` under `TestX`). The counts before it are
  tests; each subtest still has its own history and failure line.
- The run's output excerpt, when shown, comes first; prova's verdict
  (counts, failures, history, `next:`) always comes last.
