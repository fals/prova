# 0035: Places come from the repository; history compares like with like; containers are claimed once; experiments close runner configs

Status: Accepted (2026-11-25). Amends [0031](0031-quarantine-needs-one-runner-call-and-tests-keep-their-place.md) §2 and §4, [0033](0033-places-are-checked-on-disk-and-quarantines-keep-them.md), and [0032](0032-revalidation-replicates-and-the-oracle-leaves-nothing-behind.md) §1.

## Context

Usability round 18 (five fixture agents, two through Docker wrappers, and
about 60 adversarial probes on v0.1.10), apart from the quarantine findings
that [0034](0034-a-quarantine-never-turns-a-failed-run-green.md) settled:

1. Test names depended on where prova was started: `cd packages/b && prova
   run` named `src/calc.test.ts` like the root's, merging their histories;
   quiet `cargo test` names (`tests::flaky_clock`, no crate) took another
   crate's history; pytest run in another directory of a container shared a
   name with the root's; `vitest --root` counted one failure twice;
   Surefire's summary lines made phantom tests. A failing doctest passed
   because `adopt` dropped the doctest step for a workspace.
2. History labels compared unlike runs: a run of another suite that never
   ran the test, one earlier pass called "environment-dependent".
3. A cancel missed the run's own container behind a `sh -c "$*"` wrapper,
   and with two runs in one directory killed the other run's container;
   the first wait through a container client overran the 30 s it announced.
4. A revalidation whose effect had vanished by more than half was void, not
   failed, so the accept stood; runner configuration files (`.npmrc`,
   `.cargo/config.toml`) and compiled modules (`*.pyc`) forged accepts.

## Decision

1. **Places come from the repository.** Test names are read relative to the
   repository root (the git work tree, else the directory holding
   `prova.toml`), whatever directory prova was started in, and the code tree
   id is the repository's. A path printed inside a container is mapped
   through the client's mounts. A name whose place is unknown (quiet `cargo
   test`, pytest from a directory prova cannot tell) is never matched to a
   known place's history, flaky label or quarantine when the name is shared;
   a panic or traceback path that names the place gives it.
2. **History compares like with like.** A run of another suite that did not
   run the test, and a selection run, are not "passed on this exact code"
   for a failing test; one earlier pass is too little history to call a
   failure flaky or environment-dependent. A duration of 0 ms is a duration
   (under 1 ms), not a missing one.
3. **Containers are claimed once.** A container matches a run when its
   command and mounts match the client's own (a wrapper's `sh -c "$*"`
   joined as the container holds it). It is a cancel candidate only if no
   other live run of the same directory could own it (another run started
   before it was created and has not ended); otherwise prova names the
   candidates and kills none. A run through a container client announces
   its longer first wait (65 s) and never waits past what it announced.
4. **Experiments.** A revalidation of the current best that is void because
   the workload's output drifted withdraws the accept as a failed one does.
   Runner configuration (`.npmrc`, `.yarnrc*`, `.cargo/config*`,
   `rust-toolchain*`, `go.env`, `pip.conf`, `sitecustomize.py`,
   `usercustomize.py`) and compiled modules (`*.pyc`, `__pycache__/`) are
   harness files. The over-1000x warning compares with the original
   baseline, not only the current best. Files a trial leaves in its private
   `/tmp` are reported.

## Consequences

- Histories recorded under names relative to a subdirectory start anew under
  their repository-relative names.
- With two runs live in one directory, a cancel through an unnamed container
  client may name candidates instead of killing one.
