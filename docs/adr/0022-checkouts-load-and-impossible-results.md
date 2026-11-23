# 0022: Read-only checkouts, load checks and impossible results

Status: Accepted (2026-11-23). Amends [0021](0021-verdicts-under-adversarial-candidates.md) (more defenses, and their limits), [0017](0017-no-false-passes.md) (what makes a pass unverified) and [0006](0006-identity-history-flakiness.md) (when a flaky test stops being dismissed).

## Context

Round 9 of the usability check red-teamed both promises again.

Experiments (seven false ACCEPTs, with the defenses of 0021 in place):
the candidate's own code, inside its allowed scope, forged the oracle's
output from `init()`; rewrote its frozen reference table at run time; edited
the baseline's checkout through the sibling worktree; rewrote the
baseline's samples in `prova.db` through `PROVA_HOME`; rewrote its own
benchmark output by re-executing itself; and loaded the machine through
`systemd-run`, outside its process tree. Two protocol loopholes: a no-op
compared `--against` a discarded slower variant, and a slowed-down commit
given as `--baseline`.

Test runs (clean PASSED on tests that did not pass): `#[ignore]` on a test
that passed before; a Makefile that echoes `test x ... ok`; a stale JUnit
file refreshed with `touch`; nextest skipping doctests. And a deterministic
but environment-dependent test, flaky once, dismissed as flaky forever.

## Decision

Experiments:

1. **Checkouts are read-only and checked.** The tracked files of both arms'
   snapshot checkouts are made read-only before the oracle; after every
   oracle run and every trial, `git status` of both checkouts must be what
   it was. A change voids the comparison (a violation when it happens under
   the oracle).
2. **Trials cannot reach prova's data.** `PROVA_HOME` is removed from the
   environment of trials, setup and oracle; samples and trials are immutable
   in the store (triggers, migration 0007).
3. **Load aimed at one arm voids the comparison.** Each trial records the
   CPU other processes used meanwhile; when the two arms' rates (cores busy
   with other work) differ by more than half a core and half the larger
   rate, for trials of a second or more, the comparison is void.
4. **References are trusted.** `--against` and `exp best` take only the
   baseline or a variant that was kept and not reset away from. An ACCEPT
   against an original baseline that is not an ancestor of the candidate
   says so; `exp new` prints the baseline commit.
5. **Zero medians are implausible**, like 1000x: flagged.

Test runs:

6. **A test skipped now that passed in the last verified pass did not run**:
   the pass is unverified. Baselines are verified passes only.
7. **Results that cannot come from this run make the pass unverified**: a
   passed test reporting a duration longer than the run, or whose fastest
   earlier pass (two or more) is longer than the run.
8. **Failing now beats flaky.** A test that failed its last three runs in a
   row is "failing in each of its last 3 runs", not flaky.
9. Rust adoption runs `cargo test --doc` after nextest for a library; a
   dead supervisor's command is killed when the run is reaped or cancelled.

## Consequences

- Still out of reach without a sandbox: code under test that detects the
  oracle or the benchmark, or rewrites its own output in process. A
  namespace or container sandbox per trial (read-only bind mounts, no view
  of other checkouts or of the store) would close the same-user class
  entirely; it is deferred as a larger change. The skills say to read the
  candidate's diff before reporting any win.
- A cached `go test` run replays old durations and reads as unverified when
  they exceed the run: correct, since nothing ran.
- nextest still reports a test that calls `exit(0)` as passed; the Rust
  skill says to keep a `cargo test` suite for that.
