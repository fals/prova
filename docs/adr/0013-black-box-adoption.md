# 0013: Black-box adoption and fidelity ladder

Status: Accepted (2026-11-22). Amends [0001](0001-architecture-overview.md), [0003](0003-event-stream-and-junit.md), [0004](0004-liveness-and-fail-fast.md), [0005](0005-test-sizes.md), [0012](0012-skills-packaging.md).

## Context

The usual adopter is an established codebase whose tests already run through `make test`, a shell script, or a CI job. The owner's requirement: prova is a black box. The user brings their own test suite; the harness makes it run through prova with no effort and no change to the project.

ADRs 0001 to 0012 assumed a `prova.toml` and a framework adapter up front. That is ceremony a brownfield project will not accept, and it blocks the first useful run on configuration work.

## Decision

### 1. Any command is a valid suite

```
prova run -- make test
prova run -- ./scripts/test.sh --ci
```

No `prova.toml`, no adapter, no change to the project. The command is stored as part of the run's inputs ([0002](0002-formal-model.md)); identical commands in the same workspace form one implicit suite, so history accumulates from the first run.

`prova.toml` becomes optional. It names suites, records how to select a single test, and pins fidelity choices. It is written by the adoption skill (section 5), not by hand.

### 2. Fidelity ladder

Each run climbs as high as it can and records the level it reached.

| Level | Source | Requires | Gives |
|---|---|---|---|
| 0 Opaque | exit code, output, process tree | nothing | detach and wait, run record, stall detection, process-group kill, leaked processes, run-level history, failure fingerprint from output tail |
| 1 Result files | JUnit XML or TRX files written during the run | nothing, when the suite already writes them | per-test outcomes, test identity, flake statistics, per-test history |
| 2 Recognizers | known framework lines in live output (Go, libtest, pytest, TAP, ...) | nothing | first-failure event mid-run, per-test stall |
| 3 Native stream | framework's structured output via adapter ([0003](0003-event-stream-and-junit.md)) | a flag or reporter enabled, written into `prova.toml` | exact events, retries, parameters |

Rules:

- Level 1: prova snapshots candidate paths before the run and collects result files created or modified during it. Paths are searched in the workspace only, with common defaults (`**/junit*.xml`, `**/TEST-*.xml`, `**/*.trx`, `target/nextest/**`) and overrides in `prova.toml`.
- Level 2 is best effort: human-readable output drifts between versions. When level 1 files exist for the same run, they are authoritative and disagreements are recorded as recognizer defects.
- Every result says its level, so the agent knows what it can trust ("level 0: only run outcome known").
- Higher levels never remove lower ones: raw output and exit code are always kept.

### 3. Liveness at level 0

Without test events, stall is defined on the process tree: no CPU time consumed, no output written, and no process created across the stall budget. At levels 2 and 3 the structured definition of [0004](0004-liveness-and-fail-fast.md) applies.

### 4. Sizes are observed first

A black box cannot declare test sizes. prova records observed behaviour per test where the level allows it (duration, child processes, and, on Linux where cheap, network and file activity) and proposes a size. Declared sizes ([0005](0005-test-sizes.md)) become an optional override in `prova.toml`. At level 0 only the run as a whole is classified.

### 5. Adoption is the harness's job, done once

The `prova-adopt` skill is the first skill built ([0012](0012-skills-packaging.md)). The agent:

1. runs `prova run -- <existing command>` and reads the level reached;
2. reads the Makefile or script to find the framework underneath;
3. writes `prova.toml` with the suite and, where possible, a selection template:

   ```toml
   [suite.default]
   run    = "make test"
   select = "make test TEST={filter}"
   ```

4. asks prova to verify it: `prova adopt --verify` runs one known test through `select` and checks that only that test ran;
5. proposes, never applies silently, changes that would raise the level (for example enabling a JUnit reporter), as a separate commit the user can refuse.

The user's existing script is never edited by prova itself.

### 6. Transparent instrumentation

- **Environment injection: opt-in** (decided by the owner, implemented in #33). A suite lists recipes in `prova.toml` (`inject = ["pytest-junit"]`), or a bare run passes `prova run --inject <recipe> -- <command>`. Nothing is injected otherwise. Rules:
  - only recipes verified against the real framework exist; unknown names are refused before the run starts;
  - a recipe appends to the variable's existing value, never replaces it;
  - injected variables are stored in the run's inputs and shown as a note in every summary, so the change is never invisible;
  - reports a recipe produces are written to the run's private directory (`.prova/runs/<id>/inject/`), not the workspace.

  Recipes: `pytest-junit` (`PYTEST_ADDOPTS=--junitxml=...`, per-test results with messages and durations behind `pytest -q`), `go-nocache` (`GOFLAGS=-count=1`, so Go does not replay cached passes).
- **PATH shims** that intercept `go`, `pytest`, `cargo` inside scripts: **not in v1.** They break scripts that parse tool output and make failures hard to attribute.

## Consequences

- First useful run in one command, on any project, any language.
- History starts before anyone writes configuration.
- Many projects stay at level 0 or 1 until adopted; prova must be honest about that in every result.
- Recognizers are a maintenance surface; they are scoped to a few widespread formats and checked against level 1 files.

## Alternatives

- Require `prova.toml` and an adapter: blocks adoption, rejected.
- Rewrite the project's test command into prova's own form: violates the black-box requirement.
- Default-on environment injection and PATH shims: more fidelity with zero config, at the cost of invisible behaviour changes.
