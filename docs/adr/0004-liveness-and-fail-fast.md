# 0004: Liveness from events, not wall-clock waits

Status: Accepted (2026-11-22). Amended by [0019](0019-one-command-per-intent.md): waits for the result default to 9 min. Amended by [0013](0013-black-box-adoption.md): at level 0, stall is defined on process-tree activity.

## Context

Harnesses wrap test commands in long timeouts and learn about a failure or a hang only when the timeout or the command ends. The README requires fail-safe behaviour: see problems fast, abort, fix, reiterate.

Prior art ([01](../research/01-formats-liveness-sizes-flakiness.md)): nextest separates "slow" (a mark) from "terminate" (opt-in kill) and kills by process group; Bazel maps size to timeout; CI inactivity watchdogs fail falsely when output is buffered.

Time cannot be removed entirely: a hang is only observable as absence of progress over some duration. What can be removed is the single long deadline that the agent blocks on.

## Decision

1. **Runs are detached.** Starting a run returns a run id immediately. No caller blocks for the length of a run.
2. **Waiting is for events, not for time.** `wait` returns on the first of: first failure, stall, run end, or a short maximum wait (default 30 s) after which it returns a progress summary and the caller may wait again. Each return carries the last sequence number so nothing is missed or repeated.
3. **Slow is an event, not a kill.** A test exceeding its budget emits `test_slow`. Budget = historical p95 of that test times a factor when history exists, otherwise the size default ([0005](0005-test-sizes.md)).
4. **Stall is defined on structured events.** `run_stalled` fires when no test event arrives within the stall budget. Raw stdout does not count as progress for streaming adapters. For the JUnit fallback adapter only process liveness and output activity are available, and the stall event says so.
5. **Kills are explicit and bounded.** Either the caller cancels, or a hard ceiling per size is reached. Kill = whole process group: SIGTERM, short grace, SIGKILL (job objects on Windows). Outcome `errored` with reason.
6. **Leaked processes are a finding.** Children that outlive their test are recorded and reported.
7. **Fail-fast is the default for agent loops**: the native flag is passed through where the framework has one, and the first failure is surfaced regardless. Full-suite mode is opt-in.
8. **Exit codes are part of the contract**: distinct codes for all passed, test failures, stall or kill, and infrastructure error.

## Consequences

- An agent reacts within seconds of the first failure instead of after the suite ends.
- Budgets self-tune from history; first runs use conservative size defaults.
- Still time-based underneath, but as many small, informative checks, never one opaque deadline.

## Alternatives

- Global timeout per command (status quo): rejected, it is the problem statement.
- Stall from stdout inactivity only: false positives with buffering.
