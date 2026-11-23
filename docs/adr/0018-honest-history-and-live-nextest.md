# 0018: Honest history labels, and nextest results while they happen

Status: Accepted (2026-11-23). Amends [0006](0006-identity-history-flakiness.md) (what one test's history is; which runs count) and [0017](0017-no-false-passes.md) (what a thin pass says).

## Context

The fourth cold-agent check (three agents on the fixtures, after ADR 0017)
found no false pass, but several labels that point the wrong way, and one
path that stopped being live:

- **"New failure" without history.** On a project's first toe run, a failure
  that existed before the agent touched anything reads `[new failure]`, which
  the skill defines as "likely your change".
- **History split by suite.** A test run through `make test`, through the
  `default` suite and through an ad-hoc `toe run -- ...` has three histories,
  because `test_id` hashes the suite (0006). The same flaky test was `[new
  failure]` in one suite while `toe flaky` listed it as flaky; `toe history`
  silently picked one suite.
- **Cancelled runs make tests flaky.** A test that hung (`HANG=1`) and was
  cancelled was closed as errored; that errored attempt then counted as a flip
  on the same code, and the test became `FLAKY`. The same cancelled run was
  `CANCELLED` in status, `failed` in history and `errored` in the next run's
  history line.
- **Rust binaries have three names.** cargo's "Running" line names the source
  (`unittests src/lib.rs`), nextest's human output `calc`, its JSON
  `calc::calc`. The same test got different identities by runner.
- **nextest is not live.** nextest writes its libtest JSON only when a test
  binary finishes, so a one-binary project showed nothing until the end, and
  a hung test was never reported: nextest's own `SLOW [> 60s]` lines were
  ignored, and as output they kept resetting toe's stall timer. toe's own
  suites run this way.
- **Contradicting counts.** A quiet run printed `0 passed` and `warning: no
  test was observed` next to `note: the framework reported 5 tests`.
- **An accepted experiment did not say its oracle ran.**

## Decision

1. **One history per test in a repository.** A test's history, flakiness,
   "seen before" and "last passed" are computed over every suite that ran it.
   Two results are the same test when their file (normalised, below) and full
   name are equal. `test_id` stays per suite (selection and suite statistics
   still use it); the shared key is `(file, display)`. `toe history <name>`
   shows every suite where the name ran, or asks to be more specific.
2. **Rust test binaries are named by target.** cargo's deps file stem
   (`calc` from `target/debug/deps/calc-1a2b`), nextest's binary id (last
   segment: `calc::calc` and `calc` give `calc`, `toe-cli::live` gives
   `live`). Doc-tests stay `doctests <crate>`. Histories recorded under the
   old names stay, under those names.
3. **Labels say what toe knows:**
   - no earlier result of this test anywhere: `first time seen`, never `new
     failure`; with earlier results that all passed: `new failure`;
   - `FLAKY` and `new failure` are never shown together.
4. **Only runs that finished count as evidence about a test.** Cancelled and
   aborted runs, and tests closed as unfinished when a run ended, do not count
   toward flakiness, failure rate, "seen before", or "the suite passed on this
   code". Everywhere a run is named, a cancelled or aborted run is shown as
   cancelled or aborted, not as an outcome.
5. **nextest is read live.** Its human status lines (`PASS`, `FAIL`, `SKIP`,
   `SLOW`, `TIMEOUT`, retries) are recognized as they print, with the
   duration they carry and the failure output nextest prints under a `FAIL`;
   its `SLOW` line becomes toe's slow-test event, so `toe wait` returns on a
   hung test. The JSON that follows at the end of each binary confirms the
   same tests (same identity) at native level.
6. **Counts never contradict each other.** When the framework reports tests
   toe could not see individually, the summary says `framework counted N (M
   failed)`, and the thin-pass warning says the tests were not visible, not
   that none ran.
7. **Experiments say their oracle ran.** A comparison under a specification
   records `specification: holds on both arms` (with the oracle command) in
   its result, shown with the verdict and in `--json`.
8. **Smaller clarity fixes from the same check:** ratios read `31.6x better`
   / `62.9x worse`; outliers name their arm; a revalidation's decision says
   so; flips are reported per code version; a stall or slow report names the
   tests still running.

## Consequences

- "Is this mine?" no longer depends on which suite ran the test, nor on a
  cancelled run.
- On a project's very first run, nothing is called new; an agent that needs
  to know whether a failure predates its change runs the suite once before
  changing code, as the test skill says.
- Identity collisions: two Rust test binaries with the same target name in
  one workspace, or same-named tests with no file attribute in different
  places, share a history. Rare; the price of one history per test.
- The environment is still not compared: a failure caused by a variable toe
  does not record can still look flaky if the run finished. Recorded
  variables and the cancel rule cover the cases seen so far.

## Alternatives

- `test_id` without the suite: one identity, but selection and suite
  statistics need the per-suite id; a shared key is simpler than a rewrite.
- Requiring nextest's JSON only: it is not live; human lines are.
