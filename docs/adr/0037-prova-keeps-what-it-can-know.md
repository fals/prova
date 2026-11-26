# 0037: prova keeps what it can know

Status: Accepted (2026-11-26). Supersedes [0018](0018-honest-history-and-live-nextest.md) §1 and [0036](0036-a-flaky-flip-happens-within-one-suite.md) (history), [0031](0031-quarantine-needs-one-runner-call-and-tests-keep-their-place.md) §4, [0035](0035-places-come-from-the-repository-and-containers-are-claimed-once.md) §3 (containers), the place inference of [0033](0033-places-are-checked-on-disk-and-quarantines-keep-them.md) §1 and 0035 §1, and the adversarial defences of [0021](0021-verdicts-under-adversarial-candidates.md), [0022](0022-checkouts-load-and-impossible-results.md), [0023](0023-evidence-hygiene-and-experiment-isolation.md), [0024](0024-sandbox-retries-and-abba.md), [0032](0032-revalidation-replicates-and-the-oracle-leaves-nothing-behind.md) and 0035 §4 listed below. Restores [0029](0029-quarantine-accounts-for-every-error-and-hangs-in-the-first-wait.md) §4.

## Context

Eighteen usability rounds found blockers, and most fixes taught prova to
guess in one more corner: which container belongs to a run, where a test
lives when its runner does not say, which runs of other suites are the same
test, and how a candidate written to fool an experiment could cheat. Each
guess had holes the next round found; the source doubled in three days and
releases kept shipping regressions (the v0.2.0 candidate found a false
`FLAKY` and a disk filled by experiment scratch space, both from these
layers). Meanwhile the one place agents had to leave prova was ordinary:
reading an experiment's evidence (#120). The owner asked for a stable,
lean version: prova promises what it can know and says when it cannot
([scope](../scope.md)).

## Decision

1. **Containers prova was told about.** A cancel or `kill_after` stops a
   container only when the client named it (`--name`, `--cidfile`), as 0029
   §4. Otherwise nothing is killed and the summary says the container may
   still run and how to stop it. Matching containers by image, command,
   mounts, creation time or rival runs (0031 §4, 0035 §3) is removed. The
   longer first wait through a container client stays.
2. **A test's place is what its runner prints**, relative to the
   repository root: Vitest's root, pytest's rootdir, cargo's test binary and
   crate, nextest's binary id, Go's package. Inferring it from a panic path,
   from container mounts, from a `cd` in the command or from a search of the
   disk is removed. An unknown place stays unknown and never shares a
   history, a flaky label or a quarantine.
3. **History is per suite.** A test's history, flakiness, "first time seen"
   and "last passed" come from runs of the same suite; an ad hoc command is
   its own suite. `prova history <name>` lists one entry per suite.
4. **Experiments defend against honest mistakes, not adversaries.** Kept:
   the frozen protocol, interleaved (ABBA) trials, statistics and Holm,
   verdicts, the oracle and specification check, revalidation (a
   replication that does not accept withdraws the accept, and so does a
   void one: a leaked process, a rewritten tracked file or miscounted metric
   lines, in either arm; revalidate again), the harness
   scope (tests, benchmarks, build files, expected data), per-arm caches and
   scratch space reclaimed when a comparison ends, leaked processes, tracked
   files a trial rewrote, metric line counts, the base 1000x warning,
   `PROVA_HOME` and Python's user site removed from trials. Removed: the
   bubblewrap sandbox and the oracle's throwaway overlay, private `/tmp` and
   its report, read-only checkouts and the git filter and replace checks,
   secret stripping, load, stopped-arm and drift voids and the void
   attribution that sorted them, the record re-derivation in `status`, the
   1000x check against the original baseline, and the runner-config,
   compiled-module and standard-library-shadowing harness rules.
5. **No code in the user's test processes.** The Vitest worker setup file
   and the reporter's worker hooks are removed; the reporter stays.
6. **An experiment's evidence is readable through prova.** `prova exp export
   <name> [--comparison <id>]` prints the protocol, comparisons, trials and
   samples as `prova.exp-export.v1` JSON (schema in `schemas/`), read-only,
   marked partial while a comparison runs; `exp log` and `exp status` take
   `--comparison <id>`.

## Consequences

- An unnamed container behind a wrapper keeps running after a cancel; the
  summary says so. Agents give containers `--name`.
- Same-named tests whose runner prints no place (quiet `cargo test`, pytest
  in a container) are shown with an unknown place.
- Switching between `make test` and a direct suite starts a new history.
- Experiment verdicts carry no isolation guarantee against a hostile
  candidate; review its diff. Workloads see the session's environment and
  the network; `exp new --allow-network` is gone (there is no sandbox for it
  to open). Protocols stored with `allow_network` still load unchanged.
