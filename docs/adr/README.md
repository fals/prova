# Architecture Decision Records

Format: one file per decision, `NNNN-title.md`, sections Status, Context, Decision, Consequences, Alternatives.
Status values: Proposed, Accepted, Superseded by NNNN. An accepted ADR is never edited in substance; it is superseded.

All records below are **Accepted** (2026-11-22, or the date in the row). Later amendments are noted in each record's status line. Evidence lives in [`../research/`](../research/).

| # | Decision | Deviates from README? |
|---|---|---|
| [0001](0001-architecture-overview.md) | Architecture: one Rust binary, skills as the face | no |
| [0002](0002-formal-model.md) | Formal model of test, run, attempt, event | no |
| [0003](0003-event-stream-and-junit.md) | prova event stream as core contract, JUnit XML as ingest and export | refines "JUnit output" |
| [0004](0004-liveness-and-fail-fast.md) | Liveness from events, not wall-clock waits | no |
| [0005](0005-test-sizes.md) | Small, medium, large test sizes | no |
| [0006](0006-identity-history-flakiness.md) | Test identity, history, flake statistics | no |
| [0007](0007-experiments.md) | Experiments: pre-registered protocol, trials, verdict | no |
| [0008](0008-storage-sqlite.md) | Storage: SQLite single file | no |
| [0009](0009-conduit-cli-plus-mcp.md) | Conduit: CLI core plus thin MCP facade | **yes**: MCP is not primary |
| [0010](0010-deployment.md) | Deployment: static binary, container image as packaging | **yes**: not container-only |
| [0011](0011-formalization-quint-not-lean.md) | Formalization: Quint spec, Lean deferred | **yes**: Lean not used in v1 |
| [0012](0012-skills-packaging.md) | Skills: Agent Skills standard, workflow plus language skills | no |
| [0013](0013-black-box-adoption.md) | Black-box adoption and fidelity ladder; amends 0001, 0003, 0004, 0005, 0012 | refines "bring your own suite" |
| [0014](0014-central-store-and-server.md) | Central store partitioned by repository; local everywhere, server deferred; supersedes the location in 0008 | **yes**: not per-workspace |
| [0015](0015-spec-first-experiments.md) | Spec-first experiments: the specification is the oracle; any objective; amends 0007, 0011 | **yes**: Lean usable by projects |
| [0016](0016-distilled-output-no-log-files.md) | Distilled output: everything in SQLite, no log files after a run; supersedes raw-output parts of 0001, 0008, 0013 | no |
| [0017](0017-no-false-passes.md) | No false passes: a pass needs evidence; amends 0004, 0013 | no |
| [0018](0018-honest-history-and-live-nextest.md) | Honest history labels (one history per test, finished runs only) and live nextest; amends 0006, 0017 | no |
| [0019](0019-one-command-per-intent.md) | One command per intent: `run --until`, harness-sized waits, no shell loops around prova; amends 0004, 0012 | no |
| [0020](0020-oracle-evidence-and-stalls.md) | The oracle shows its work (baseline's oracle tests must pass on the candidate); a stall ends every wait; amends 0015, 0018, 0019 | no |
| [0021](0021-verdicts-under-adversarial-candidates.md) | Verdicts under careless or adversarial candidates (scope, metric line counts, leaked processes); idle runtimes stall; amends 0004, 0007, 0015 | no |
| [0022](0022-checkouts-load-and-impossible-results.md) | Read-only checked checkouts, hidden store, load checks, trusted references; impossible or skipped results are unverified; failing now beats flaky; amends 0006, 0017, 0021 | no |
| [0023](0023-evidence-hygiene-and-experiment-isolation.md) | Unverified runs are no evidence; more unverified causes; build failure lines; failing-now scaled to the flake rate; `--change` adds; more arm isolation, drift check; amends 0017, 0021, 0022 | no |

The deviations from the README were accepted with the records.
