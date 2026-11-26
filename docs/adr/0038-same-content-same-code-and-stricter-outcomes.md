# 0038: Same content is the same code; stricter outcomes; misplaced flags are usage errors

Status: Accepted (2026-11-26). Amends [0017](0017-no-false-passes.md) (what a pass verifies), [0006](0006-identity-history-flakiness.md) (the code version) and [0004](0004-liveness-and-fail-fast.md) (which runs exit 2).

## Context

Release-candidate acceptance of 0.2.0 and 0.3.0 left three behaviours the
owner chose to change:

- An uncommitted tree whose content equals a commit was another code
  version (`tree:X+dirty:Y` against `tree:Z`): a test that failed before a
  commit and after it on the same content was not "the same code", so a
  flaky test started as "new failure" and flaky detection waited.
- A command that exited 0 while a process it started still ran passed on
  the results seen so far; a cancelled run kept no output of the test that
  had failed before the cancel.
- `prova run -- make test --until done` (a prova option after `--`) exited
  4, infrastructure, though it is the caller's usage error.

## Decision

1. **The code version is the content.** A working tree's identity is the
   git tree id its tracked and untracked (not ignored) content would have,
   computed without writing objects; cache directories stay excluded. A
   clean checkout keeps `tree:<HEAD's tree>`; an edited one whose content
   equals a commit gets that commit's id. Content no commit has gets
   `tree:<id>` of that content (no `+dirty` suffix); repositories whose
   object format prova cannot hash keep the previous form.
2. **A pass while the command's output is still open is unverified.** When
   the command exits 0 while a process it started still holds the run's
   output (it is killed, as before), the pass is `PASSED (unverified)`:
   results that process would have printed were never seen. A leftover
   process that let go of the output (a build server, a daemon) is reported
   and killed, and the pass stands.
3. **A cancelled run keeps what failed.** A test that failed before the
   cancel keeps the output it printed, as in a run that ended. Runners that
   print failure details only at the end (pytest's `-rfE` summary, libtest's
   failures block) have none to keep before a cancel.
4. **No test ran: unchanged, and said.** A run fails when it is known that
   no test ran (0017); when prova could not see tests (opaque output, `go
   test` without `-v`) the pass stays unverified, because failing every
   command whose output prova cannot read would break `prova run -- <any
   command>` (0013).
5. **A prova option after `--` is a usage error** (exit 2), like any other
   misuse of the command line.

## Consequences

- Histories recorded under `tree:X+dirty:Y` keep that id; new runs of the
  same content get the content's tree id, so they start their own history.
- Scripts that checked exit 4 for a misplaced prova option see 2.
