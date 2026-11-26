# 0006: Test identity, history, flake statistics

Status: Accepted (2026-11-22). Amended by [0018](0018-honest-history-and-live-nextest.md): one history per test across suites; only finished runs count. Amended by [0038](0038-same-content-same-code-and-stricter-outcomes.md).

## Context

Agents iterate without memory: what was run, what failed before, whether a failure is new or a known flake. History needs a stable key per test and cheap, trustworthy statistics ([01](../research/01-formats-liveness-sizes-flakiness.md)).

## Decision

**Identity.** `test_id = hash(suite, framework, file, suite path, name, normalised parameters)`. Environment is not part of identity; it is a variant dimension on the run. Renames produce a new identity (no rename tracking in v1).

**Code identity.** Git tree hash, plus a hash of the uncommitted diff when the tree is dirty. Two runs are "same code" only when both match. Outside git, a content hash of the tracked files declared in `prova.toml`.

**Run record.** Command, arguments, selection, selected environment variables, toolchain versions, machine fingerprint, code identity, start and end, per-attempt outcomes and durations.

**Failure fingerprint.** Hash of the failure message with volatile parts normalised (numbers, addresses, paths, timestamps) plus the top stack frames. Groups repeats of one failure across runs and tests.

**Statistics, computed from the store:**

- flaky by retry: failed then passed within one run;
- flaky by flip: different outcomes on the same code identity;
- transition count over the last N runs;
- failure rate over a window;
- duration p50 and p95, and regression against the previous window;
- first-seen and last-seen per failure fingerprint.

**Agent-facing answers.** On a failure, prova states: new or seen before (fingerprint), flaky or consistent on this code, and which code identity last passed. `prova history <test>` and `prova diff <run> <run>` expose the rest.

## Consequences

- "Did my change break this, or was it already broken or flaky?" becomes a lookup.
- Fingerprint normalisation will need tuning per language.
- A probabilistic flakiness score (Meta-style) can be added later on the same data.

## Alternatives

- Key by name only: collides across files and parameters.
- Include environment in identity: fragments history; variants cover it.
