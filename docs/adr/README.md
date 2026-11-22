# Architecture Decision Records

Format: one file per decision, `NNNN-title.md`, sections Status, Context, Decision, Consequences, Alternatives.
Status values: Proposed, Accepted, Superseded by NNNN. An accepted ADR is never edited in substance; it is superseded.

All records below are **Proposed** and wait for review. Evidence lives in [`../research/`](../research/).

| # | Decision | Deviates from README? |
|---|---|---|
| [0001](0001-architecture-overview.md) | Architecture: one Rust binary, skills as the face | no |
| [0002](0002-formal-model.md) | Formal model of test, run, attempt, event | no |
| [0003](0003-event-stream-and-junit.md) | toe event stream as core contract, JUnit XML as ingest and export | refines "JUnit output" |
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
| [0014](0014-central-store-and-server.md) | Central store partitioned by repository; optional `toe serve` for clusters; supersedes the location in 0008 | **yes**: adds an optional service (0001) |

The three deviations are the points most worth challenging in review.
