# Architecture Decision Records

Format: one file per decision, `NNNN-title.md`, sections Status, Context, Decision, Consequences, Alternatives.
Status values: Proposed, Accepted, Superseded by NNNN. An accepted ADR is never edited in substance; it is superseded.

Records 0001 to 0027 are **Accepted**, each on the date in its row. Later amendments are noted in each record's status line. Evidence lives in [`../research/`](../research/).

| # | Decision | Status | Deviates from README? |
|---|---|---|---|
| [0001](0001-architecture-overview.md) | Architecture: one Rust binary, skills as the face | Accepted 2026-11-22 | no |
| [0002](0002-formal-model.md) | Formal model of test, run, attempt, event | Accepted 2026-11-22 | no |
| [0003](0003-event-stream-and-junit.md) | prova event stream as core contract, JUnit XML as ingest and export | Accepted 2026-11-22 | refines "JUnit output" |
| [0004](0004-liveness-and-fail-fast.md) | Liveness from events, not wall-clock waits | Accepted 2026-11-22 | no |
| [0005](0005-test-sizes.md) | Small, medium, large test sizes | Accepted 2026-11-22 | no |
| [0006](0006-identity-history-flakiness.md) | Test identity, history, flake statistics | Accepted 2026-11-22 | no |
| [0007](0007-experiments.md) | Experiments: pre-registered protocol, trials, verdict | Accepted 2026-11-22 | no |
| [0008](0008-storage-sqlite.md) | Storage: SQLite single file | Accepted 2026-11-22 | no |
| [0009](0009-conduit-cli-plus-mcp.md) | Conduit: CLI core plus thin MCP facade | Accepted 2026-11-22 | **yes**: MCP is not primary |
| [0010](0010-deployment.md) | Deployment: static binary, container image as packaging | Accepted 2026-11-22 | **yes**: not container-only |
| [0011](0011-formalization-quint-not-lean.md) | Formalization: Quint spec, Lean deferred | Accepted 2026-11-22 | **yes**: Lean not used in v1 |
| [0012](0012-skills-packaging.md) | Skills: Agent Skills standard, workflow plus language skills | Accepted 2026-11-22 | no |
| [0013](0013-black-box-adoption.md) | Black-box adoption and fidelity ladder; amends 0001, 0003, 0004, 0005, 0012 | Accepted 2026-11-22 | refines "bring your own suite" |
| [0014](0014-central-store-and-server.md) | Central store partitioned by repository; local everywhere, server deferred; supersedes the location in 0008 | Accepted 2026-11-22 | **yes**: not per-workspace |
| [0015](0015-spec-first-experiments.md) | Spec-first experiments: the specification is the oracle; any objective; amends 0007, 0011 | Accepted 2026-11-22 | **yes**: Lean usable by projects |
| [0016](0016-distilled-output-no-log-files.md) | Distilled output: everything in SQLite, no log files after a run; supersedes raw-output parts of 0001, 0008, 0013 | Accepted 2026-11-22 | no |
| [0017](0017-no-false-passes.md) | No false passes: a pass needs evidence; amends 0004, 0013 | Accepted 2026-11-22 | no |
| [0018](0018-honest-history-and-live-nextest.md) | Honest history labels (one history per test, finished runs only) and live nextest; amends 0006, 0017 | Accepted 2026-11-23 | no |
| [0019](0019-one-command-per-intent.md) | One command per intent: `run --until`, harness-sized waits, no shell loops around prova; amends 0004, 0012 | Accepted 2026-11-23 | no |
| [0020](0020-oracle-evidence-and-stalls.md) | The oracle shows its work (baseline's oracle tests must pass on the candidate); a stall ends every wait; amends 0015, 0018, 0019 | Accepted 2026-11-23 | no |
| [0021](0021-verdicts-under-adversarial-candidates.md) | Verdicts under careless or adversarial candidates (scope, metric line counts, leaked processes); idle runtimes stall; amends 0004, 0007, 0015 | Accepted 2026-11-23 | no |
| [0022](0022-checkouts-load-and-impossible-results.md) | Read-only checked checkouts, hidden store, load checks, trusted references; impossible or skipped results are unverified; failing now beats flaky; amends 0006, 0017, 0021 | Accepted 2026-11-23 | no |
| [0023](0023-evidence-hygiene-and-experiment-isolation.md) | Unverified runs are no evidence; more unverified causes; build failure lines; failing-now scaled to the flake rate; `--change` adds; more arm isolation, drift check; amends 0017, 0021, 0022 | Accepted 2026-11-23 | no |
| [0024](0024-sandbox-retries-and-abba.md) | Sandboxed experiment arms (bubblewrap); retries decided by the last attempt; nextest early-exit detection; ABBA trials; amends 0002, 0007, 0017, 0023 | Accepted 2026-11-23 | no |
| [0025](0025-sizes-by-resources.md) | Test sizes come from the resources a test used (processes, network), never from its duration; amends 0005, 0013 | Accepted 2026-11-23 | no |
| [0026](0026-sizes-from-process-start-times-and-the-run.md) | Sizes from process start times and from the whole run; small says it is the absence of evidence; amends 0025 | Accepted 2026-11-24 | no |
| [0027](0027-quarantine-and-slow-tests.md) | Quarantine for known-flaky tests (store, reason, expiry, always listed); a left-out flaky test is warned about; `prova slowest`; amends 0017, 0018 | Accepted 2026-11-24 | no |
| [0028](0028-quarantine-sets-aside-only-what-it-explains.md) | A quarantine sets aside only what its tests explain (counts, crash lines, subtests, something else verified); marked everywhere; stall after 30 s with no size, running tests' time so far; amends 0004, 0027 | Accepted 2026-11-24 | no |

The deviations from the README were accepted with the records.
