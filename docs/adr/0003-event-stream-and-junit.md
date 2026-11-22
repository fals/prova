# 0003: toe event stream as core contract, JUnit XML as ingest and export

Status: Proposed

## Context

The README proposes JUnit output as the known format. Research ([01](../research/01-formats-liveness-sizes-flakiness.md)): JUnit XML is the only format nearly every framework emits, but it is written at end of run, has no official schema, and no standard for retries or flaky results. No cross-language streaming format exists; each ecosystem has its own (Go `-json`, pytest-reportlog, Vitest reporter API, Open Test Reporting, nextest).

Fast feedback needs per-test events during the run. JUnit XML alone cannot give that.

## Decision

1. **Core contract is the toe event stream**: JSON Lines, one event per line, kinds from [0002](0002-formal-model.md). Versioned (`"v": 1`). Schema published as JSON Schema generated from the Rust types.
2. **Adapters** convert native framework output into toe events. Two tiers:
   - **Streaming adapter**: reads the framework's live output. Gives first-failure and stall detection.
   - **JUnit fallback adapter**: runs any command, then ingests the JUnit XML file(s) it wrote. Lenient parser accepting the superset of known flavors. Gives history and statistics, but only run-level liveness.
3. **Every finished run can be exported as JUnit XML** (`toe report --junit`), using the Surefire-style `flakyFailure`/`rerunFailure` elements for retries, so existing CI tooling keeps working.
4. Field names borrow from CTRF where a concept matches. CTRF and OpenTelemetry are possible export targets later, not part of v1.
5. Raw framework output is kept as an artifact, so an adapter bug never loses information.

Initial streaming adapters, in order: Go (`go test -json`), pytest (reportlog), Vitest (reporter), cargo-nextest (stable JUnit first; experimental JSON behind a version gate), JUnit Platform (Open Test Reporting events). .NET and GoogleTest start on the JUnit/TRX fallback.

## Consequences

- JUnit XML stays the lowest common denominator, as the README intended, without capping feedback speed at end of run.
- Each streaming adapter is maintenance; the fallback keeps any framework usable from day one.
- Cross-check possible: streaming result versus the JUnit file from the same run.

## Alternatives

- JUnit XML only: no first-failure event. Rejected.
- Adopt Open Test Reporting as the core: well designed but XML and JVM-only in practice.
- Adopt CTRF as the core: report object, not an event stream.
