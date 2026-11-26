# What prova promises

prova is one binary that runs a project's existing test command for a
coding agent, tells it what happened as soon as it can, and remembers it. It
promises what it can know from the command, its output, its result files and
its process tree. Where it cannot know, it says so and does not guess.

## Promised

**The test loop**, for any command (`prova run -- <cmd>`) or a suite in
`prova.toml`:

- the run's outcome and exit code (0 passed, 1 test failure, 2 usage,
  3 stall/slow/killed, 4 infrastructure, 5 still running), never green when
  the command or a test failed;
- the first failure while the suite still runs, slow and stalled tests, all
  within a bounded wait that ends with the one command to run next;
- rerunning one test (`--select`), repeating a run (`--repeat`), cancelling
  it (the whole process group);
- per-test history **within a suite** (an ad hoc command is its own suite):
  first time seen, new failure, failed before, last passed, flaky (passed and
  failed on the same code in the same suite), failing now;
- quarantine that names known flaky tests in the open and never makes a run
  pass;
- test sizes from what tests used, slowest tests, diffs between runs;
- `prova adopt` proposing a `prova.toml` and verifying single-test selection.

**Fidelity** rises with what the tools print: opaque process, result files
(JUnit), recognised output, native streams (nextest, `go test -json`, the
Vitest reporter, pytest-reportlog).

**Experiments** (`prova exp`): a frozen protocol, interleaved trials,
Mann-Whitney with an effect interval and Holm correction, verdicts, an
oracle or specification check that rejects a candidate that breaks the
behaviour, revalidation of the best, and the raw evidence behind every
verdict (`prova exp export`).

## Not promised

- **Containers prova was not told about.** A cancel stops a container only
  when the command names it (`--name`, `--cidfile`). Otherwise prova says
  the container may still run and how to stop it.
- **Test places prova cannot read.** A test's place is what its runner
  prints (Vitest root, pytest rootdir, cargo and nextest binaries, Go
  package), relative to the repository. prova does not infer it from panic
  paths, container mounts or a search of the disk; an unknown place is shown
  as unknown and never shares the history of a test whose place is known.
- **History across suites.** Each suite and each ad hoc command keeps its
  own history; the same test run another way starts its own.
- **Deliberately adversarial experiment candidates.** prova keeps a
  candidate from changing the harness (tests, benchmarks, build files,
  expected data), checks the oracle, reports leaked processes and tracked
  files a trial rewrote, and flags implausible wins. It does not sandbox
  arms, hide secrets from them, or detect candidates written to fool the
  measurement; review the candidate's diff.
- **Code inside the user's tests.** prova never injects code into test
  processes. Opt-in recipes (`--inject`) only set the runner's own
  options and reporters. So a stream is as prompt as its runner: Vitest's
  reporter hears of a start or an early failure at Vitest's own reporting
  cadence, which can be a few seconds late.

Decisions behind this page: [ADR 0037](adr/0037-prova-keeps-what-it-can-know.md).
