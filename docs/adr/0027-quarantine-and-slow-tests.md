# 0027: Quarantine for known-flaky tests, and slow tests made visible

Status: Accepted (2026-11-24). Modelled in `spec/run.qnt` (`withQuarantine`, invariant `quarantineHidesOnlyItsOwn`). Amends [0017](0017-no-false-passes.md) (what a pass may exclude) and [0018](0018-honest-history-and-live-nextest.md) (what the flaky label does to a verdict). §4 superseded by [0034](0034-a-quarantine-never-turns-a-failed-run-green.md): a quarantine never turns a failed run green.

## Context

Usability round 13 (four fixture agents):

1. A suite with a known-flaky test never reaches a verified green. Every pass
   is `PASSED (unverified)`, "known flaky test passed, proves little", and
   prova offers no way to say "I know, set it aside". The agent's only move
   was to exclude the test in the suite command (`-E 'not test(flaky_clock)'`).
   That run was a clean, verified `PASSED` with no warning: the same test
   simply vanished. Excluding it silently is a false impression; prova's own
   check (the "did not run" warning of 0017 §3) did not fire because it
   compares a suite with its **own** last passing run, and the new suite had
   none.
2. Slow tests are invisible. A 1.5 s test is never labelled and nothing lists
   slow tests; only `prova history <test>` shows its p50.

## Decision

### A. Quarantine

1. **Quarantine lives in prova's store**, per repository (0014), never in the
   user's project (0013). A row names a test by its full display name (the
   name 0018 already uses to share history across suites), a mandatory
   reason, when it was added, and when it expires. Removal is soft: the row
   stays with `removed_ms`, so an old run can always be explained.
2. **Commands** (one per intent, 0019):
   `prova quarantine add <test> --reason <why> [--days N]`,
   `prova quarantine list [--all] [--json]`,
   `prova quarantine remove <test>`.
   `add` needs a test prova has seen (exact name, or a substring matching one
   name); an unknown name is a usage error, not a silent typo.
3. **Expiry forces review.** Default 14 days, at most 90. After that the
   quarantine no longer applies: the test counts again (its failure fails the
   run, its pass is "known flaky, proves little"), and the run says
   `quarantine of X expired on <date>`. `add` again renews it, with a reason.
4. **What a quarantine changes.**
   - A quarantined test's **pass** does not make the run unverified: it is
     excluded from the flaky warning and from the "failed then passed on a
     retry" warning.
   - A quarantined test's **failure does not fail the run** when it is the
     only thing wrong: every failing test of the run is quarantined and
     `failed` (not `errored` or unfinished), the run ended on its own, no
     contradiction was found, and the level is per-test (recognized output
     or better). Otherwise the run fails as before. The command's raw exit
     code is still shown. This overlay sits on top of the lifecycle verdict
     (`spec/run.qnt` is unchanged: the lifecycle's verdict stays `failed`;
     the engine reports `passed` only under the guard above). **Owner:
     decide whether the spec should model it before this is Accepted.**
   - Everything else is unchanged: counts keep counting the test, and
     `prova history` keeps its attempts (a quarantine hides nothing from
     history, it only sets the test out of the verdict).
5. **It cannot hide.** Every summary (text and JSON) lists the quarantined
   tests with their result in that run (`passed`, `failed`, `skipped`, `not
   run`), the reason and the expiry, whether the run passed or failed, in
   `prova status`, `run`, `wait` alike. The quarantine in force is the one
   that was in force when the run was created, so removing it later does not
   rewrite old runs. A failing quarantined test does not end a
   `--until first-failure` wait.
6. **Leaving a known-flaky test out is said.** At the end of a full run (no
   selection), every test with flaky evidence (0018) that has no result in
   this run, and that ran in this suite or in another suite that shares a test
   with this run, produces a warning (the run is unverified):
   `N known flaky test(s) did not run: ...; excluding a flaky test in the
   command hides it, quarantine it with prova quarantine add`. A quarantined
   test that did not run is listed as `not run`, with no warning: setting it
   aside was done in the open. The existing "did not run since the last
   passing run" warning likewise ignores quarantined tests.

### B. Slow tests

7. `prova slowest [--suite S] [-n N] [--json]` lists tests worst first by
   median duration (p50, then p95) over their last 20 finished, non-forgotten
   results, merged across suites like history. Default `-n 10`.
8. A finished run's summary names, on one line, the up to three slowest tests
   of **that run** that took 1 s or more (`slow tests: a 1.5s, b 1.1s; prova
   slowest`). Nothing is printed when no test took 1 s. A threshold, not a
   label on every test, keeps the output compact (0019); the full list is one
   command away. No new ADR was needed for B on its own; it is recorded here
   so the two changes share one review.

## Consequences

- A suite with a flaky test can be green and verified: the owner (or an agent
  with a reason) quarantines it for at most 90 days at a time, and every
  result names it.
- The risk is a quarantine hiding a real regression in that test. It is
  bounded by expiry, by the visible list, by the guard in §4, and by history.
- Excluding a flaky test in the command is no longer silent, for any suite.

## Alternatives

- Quarantine in `prova.toml`: edits the user's project, and survives forever
  unreviewed (0013 and ground rule 3).
- A quarantined failure still fails the run: safe, but the suite then never
  reaches green, which is the problem reported.
- Hide quarantined tests from counts and history: makes them invisible.
