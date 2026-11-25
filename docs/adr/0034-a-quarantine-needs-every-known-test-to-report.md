# 0034: A quarantine needs every known test to report; places come from the repository; containers are claimed once

Status: Accepted (2026-11-25). Amends [0031](0031-quarantine-needs-one-runner-call-and-tests-keep-their-place.md) §1, §2 and §4, [0033](0033-places-are-checked-on-disk-and-quarantines-keep-them.md), and [0032](0032-revalidation-replicates-and-the-oracle-leaves-nothing-behind.md) §1.

## Context

Usability round 18 (five fixture agents, two through Docker wrappers, and
about 60 adversarial probes on v0.1.10):

1. A quarantined failure still passed runs that hid another failure: a
   runner told to stop at its first failure (`go test -failfast`, `pytest
   -x`/`--maxfail`, plain `cargo test` stopping after a failed test binary)
   never ran the broken tests after the quarantined one. The `TestMain`
   refusal never fired, Vitest coverage set in an imported module or a
   `--config` file was not seen, and a wrapper script given the runner as
   its argument (`./ci.sh "vitest run"`) ran a lint step after it.
2. Test names depended on where prova was started: `cd packages/b && prova
   run` named `src/calc.test.ts` like the root's, merging their histories;
   quiet `cargo test` names (`tests::flaky_clock`, no crate) took another
   crate's history and quarantine; a failing doctest passed because
   `adopt` dropped the doctest step for a workspace.
3. A cancel missed the run's own container (its creation time misread) and,
   with two runs in one directory, killed the other run's container; the
   first wait through a container client overran the 30 s it announced.
4. A revalidation whose effect had vanished by more than half was void, not
   failed, so the accept stood; runner configuration files (`.npmrc`,
   `.cargo/config.toml`) and compiled modules (`*.pyc`) forged accepts.

## Decision

1. **Every known test must report.** A quarantined failure is set aside
   only if every test that the suite's last full passing run reported has a
   result in this run (passed, failed, skipped), and no test of unknown
   place failed. A test that did not report is named, and the run fails as
   if nothing were quarantined. With no earlier full pass, the framework's
   own count must equal the tests prova saw. This holds whatever stopped
   the runner (a fail-fast flag, a crash, a skipped binary), so options are
   not enumerated. 0031 §1 stays: the command must be one runner call; a
   wrapper script qualifies only when its body is one `exec` of a known
   runner or container client with the script's arguments. Vitest's own
   `Coverage enabled` line refuses as the flag does, and a Go package with
   `TestMain` refuses whatever file defines it.
2. **Places come from the repository.** Test names are read relative to the
   repository root (the git work tree, else the directory holding
   `prova.toml`), whatever directory prova was started in, and the code tree
   id is the repository's. A name whose place is unknown (quiet `cargo
   test`, pytest from a directory prova cannot tell) is never matched to a
   known place's history, flaky label or quarantine when the name is shared;
   a panic or traceback path that names the place gives it.
3. **History compares like with like.** A pass with quarantined failures
   set aside, a run of another suite that did not run the test, and a
   selection run are not "passed on this exact code" for a failing test;
   one earlier pass is too little history to call a failure flaky or
   environment-dependent.
4. **Containers are claimed once.** Creation times are read with their time
   zone. A container is a cancel candidate only if no other live run of the
   same directory could own it (another run started before it was created
   and has not ended); otherwise prova names the candidates and kills none.
   A run through a container client announces the longer first wait (65 s)
   when it starts waiting.
5. **Experiments.** A revalidation of the current best that is void because
   the workload's output drifted withdraws the accept as a failed one does.
   Runner configuration (`.npmrc`, `.yarnrc*`, `.cargo/config*`,
   `rust-toolchain*`, `go.env`, `pip.conf`, `sitecustomize.py`,
   `usercustomize.py`) and compiled modules (`*.pyc`, `__pycache__/`) are
   harness files. The over-1000x warning compares with the original
   baseline, not only the current best. A trial that writes files outside
   the checkout and reads them back is reported.

## Consequences

- The first run after tests are added cannot set a quarantine aside until a
  full run passes with them.
- A wrapper script that does anything besides `exec` its runner refuses a
  quarantine; a second suite that runs the runner directly is the fix.
- Histories recorded under names relative to a subdirectory start anew under
  their repository-relative names.
