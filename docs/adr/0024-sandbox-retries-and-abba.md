# 0024: Sandboxed arms, retries decided by their last attempt, ABBA trials

Status: Accepted (2026-11-23). Amends [0002](0002-formal-model.md) and [0017](0017-no-false-passes.md) (retries), [0007](0007-experiments.md) (trial order) and [0023](0023-evidence-hygiene-and-experiment-isolation.md) (isolation, evidence). Superseded in part by [0037](0037-prova-keeps-what-it-can-know.md).

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
   filesystem read-only (shared toolchain caches included), writable only
   the arm's own checkout, a private per-arm state directory and a private
   `/tmp`; its own PID namespace, killed with the trial; no network for
   trials and oracle (loopback only) unless the protocol says
   `--allow-network` (setup keeps it, to install dependencies). Each arm
   gets its own build caches (`GOCACHE`, `XDG_CACHE_HOME`, npm, Python
   bytecode) whether sandboxed or not, and secret-named variables are
   removed from its environment. Every verdict says whether it was
   sandboxed; without bubblewrap (most containers) it says the arms were
   not isolated.
2. **What runs is the commit**: prova's git ignores replace objects, and
   each snapshot checkout is verified file by file against the commit's own
   blobs (no smudge filter or replaced object can change it).
3. **Disturbed arms void the comparison**: besides load (0021), an arm whose
   CPU-bound trials ran under 60% of their time while the other's ran 90%
   or more (stopped from outside, starved) voids it.
4. **The record is checked against the samples**: `exp status` re-derives
   every ACCEPT from the immutable samples, with the Holm family as it was
   when decided, and checks the best against the record; a mismatch is
   shown as a warning (the same user can edit the database file).
5. **Weak protocols are flagged** in every verdict (alpha above 0.1, a
   minimum effect under 1%, other experiments on the same baseline and
   metric), and a foreground `exp run` waits at most 9 minutes.
6. **A retry restarts a failed test, and its last attempt decides** (spec
   `run.qnt`: a test starts again only after a failed attempt). A pass on
   retry passes the run, unverified: "failed, then passed on a retry:
   flaky, not green".
7. **A test process that ends early fails the run**: nextest runs with
   `--success-output final`, and a passing test whose captured output starts
   libtest but never reports a result exited early; a plain libtest binary
   that stops after K of N tests, before its summary, did too. Such a run is
   also no evidence about any test. A framework summary longer than the
   whole run means replayed output: unverified, no evidence.
8. **Trials run in ABBA order** (spec `experiment.qnt`): when the arms are
   even, pairs alternate which arm goes first, so a drift within a pair
   favours neither; the arms never differ by more than one trial.
9. **Flakiness needs the same code and the same environment**: each run
   records a fingerprint of its environment (variable names with a short
   hash of each value; values are never stored, secret-like names are left
   out). A flip across environments is labelled environment-dependent, not
   flaky. "Failing now" applies only to tests history calls flaky. `prova
   forget <run> --reason` takes a run out of every test's history.
10. History answers by exact name first. Only warnings that discredit the
   results themselves (impossible or identical results, no CPU, concurrent
   result files) remove a run from test history; a duration is impossible
   only against the same code; identical-results detection needs 100 ms of
   real time.

## Consequences

- What remains open to a determined same-user adversary: code under test
  that detects the oracle or benchmark and behaves differently there, and
  output rewritten inside its own process. Reading the candidate's diff
  stays part of reviewing a win.
- Per-arm build caches start cold: the first trial of each arm builds from
  scratch (warmup trials absorb it).
- Where bubblewrap is missing, the defenses of 0021 to 0023 still apply and
  the verdict says the arms were not isolated.
- nextest output is larger (every passing test's output); prova distils it.
