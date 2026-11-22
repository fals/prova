# 0002: Formal model of test, run, attempt, event

Status: Accepted (2026-11-22)

## Context

The README asks for a formal definition: a test is a function executed in the system under test by the harness, with parameters, producing an observable output, and never fully dependent on bounded time. Every later ADR needs shared vocabulary.

## Decision

**Test.** A test is a function

```
t : (Code, Params, Env) -> Trace
```

- `Code`: the tree hash of the workspace (plus a dirty-diff hash when uncommitted changes exist).
- `Params`: declared inputs: selection filter, test parameters, seed.
- `Env`: recorded environment: toolchain versions, selected environment variables, machine fingerprint.
- `Trace`: an ordered, finite or still-growing sequence of events ending, if it ends, in exactly one outcome.

**Outcome.** `passed | failed | skipped | errored`. `errored` means the test could not be evaluated (crash, build failure, killed). An unfinished trace has no outcome; "timed out" is not an outcome but a reason attached to `errored` after a kill.

**Attempt.** One execution of one test. Retries create more attempts of the same test in the same run.

**Run.** One invocation of a suite: a set of attempts sharing `(Code, Params, Env)` and a command. Run states:

```
created -> running -> finished
                   -> cancelled
                   -> aborted      (infrastructure failure)
```

Terminal states are absorbing. A run's inputs are immutable once it leaves `created`.

**Event.** The unit of observation. Minimum kinds: `run_started`, `test_started`, `test_output`, `test_slow`, `test_finished`, `run_stalled`, `run_finished`. Each carries run id, monotonic sequence number, timestamp, and test identity where applicable.

**Determinism claim.** For fixed `(Code, Params, Env)` a test should yield the same outcome. Two attempts with equal inputs and different outcomes define **flaky** ([0006](0006-identity-history-flakiness.md)).

**Observation without bounded time.** A consumer never needs to wait for the end of a run to learn something. It asks for "the next significant event after sequence n". Liveness is defined on the event stream ([0004](0004-liveness-and-fail-fast.md)), so no decision depends on a single long deadline.

## Consequences

- Everything stored is either an input tuple, an event, or derived from events.
- Comparability across iterations is a query: same test identity, different `Code`.
- Adapters have one job: produce this event sequence.

## Alternatives

- Model only final results (JUnit-shaped): cannot express first-failure or stall. Rejected.
