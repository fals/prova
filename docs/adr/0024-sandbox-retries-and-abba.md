# 0024: Sandboxed arms, retries decided by their last attempt, ABBA trials

Status: Proposed (2026-11-23). Amends [0002](0002-formal-model.md) and [0017](0017-no-false-passes.md) (retries), [0007](0007-experiments.md) (trial order) and [0023](0023-evidence-hygiene-and-experiment-isolation.md) (isolation, evidence).

## Context

The problems left open after ADR 0023:

1. A candidate could still reach the other arm through paths outside both
   checkouts (the user's home, the shared temporary directory) because both
   ran as the same user with the same view of the filesystem.
2. A test that failed and passed on a retry (Surefire's `flakyFailure`,
   nextest retries) made the run FAILED with exit 0 and nothing failing.
3. nextest counts a test that calls `exit(0)` as passed.
4. An unchanged TypeScript candidate was accepted once under parallel load;
   trials ran baseline-first in every pair.
5. `prova history <name>` listed matches instead of answering when an old,
   renamed test shared part of the name.

## Decision

1. **Experiment arms run in a sandbox when bubblewrap works**: the whole
   filesystem read-only, writable only the arm's own checkout, a private
   per-arm state directory (bytecode, caches), a private `/tmp`, and the
   toolchain caches builds must write (`~/.cache`, `~/.cargo`, `~/.m2`,
   `~/.nuget`, `~/.npm`, `~/go`, `~/.gradle`, `~/.dotnet`); its own PID
   namespace, killed with the trial. Trials, setup and oracle all run so.
   Every verdict says whether it was sandboxed; without bubblewrap (as in
   most containers) it says the arms were not isolated.
2. **A retry restarts a failed test, and its last attempt decides** (spec
   `run.qnt`: a test starts again only after a failed attempt). A pass on
   retry passes the run, unverified: "failed, then passed on a retry:
   flaky, not green".
3. **nextest runs with `--success-output final`**: a passing test whose
   captured output starts libtest (`running 1 test`) but never reports
   (`test result:`) exited early; the run fails and the test is recorded as
   errored.
4. **Trials run in ABBA order** (spec `experiment.qnt`): when the arms are
   even, pairs alternate which arm goes first, so a drift within a pair
   favours neither; the arms never differ by more than one trial.
5. History answers by exact name first. Only warnings that discredit the
   results themselves (impossible or identical results, no CPU, concurrent
   result files) remove a run from test history; a duration is impossible
   only against the same code; identical-results detection needs 100 ms of
   real time.

## Consequences

- The shared toolchain caches remain a channel between arms; reading the
  candidate's diff stays part of reviewing a win.
- Where bubblewrap is missing, the defenses of 0021 to 0023 still apply and
  the verdict says the arms were not isolated.
- nextest output is larger (every passing test's output); prova distils it.
