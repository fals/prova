# 0008: Storage: SQLite single file

Status: Proposed. Location superseded by [0014](0014-central-store-and-server.md): one central store under `TOE_HOME`, one SQLite file per repository, optionally owned by `toe serve`.

## Context

Needs: append-heavy event log, relational entities (runs, tests, attempts, experiments, trials, samples), analytical queries over history, local single file, concurrent access from several `toe` processes (a run supervisor writing while a `wait` reads), works on a container volume. Comparison in [03](../research/03-formalization-experiments-storage.md).

## Decision

SQLite, one file at `.toe/toe.db`, via `rusqlite` with the bundled library.

- WAL mode, `synchronous=NORMAL`, `busy_timeout` 5 s, `foreign_keys=ON`.
- STRICT tables with CHECK constraints mirroring the invariants of [0002](0002-formal-model.md) and [0007](0007-experiments.md).
- Append-only `event` table: `(run_id, seq)` primary key, kind, timestamp, JSON payload.
- Writers use short `BEGIN IMMEDIATE` transactions; events are batched per flush.
- Readers waiting for new events poll cheaply (`PRAGMA data_version`), no extra IPC channel.
- Large payloads (raw output, JUnit files) go to content-addressed files under `.toe/artifacts/`, referenced by hash.
- Migrations with `rusqlite_migration` (`user_version`), embedded in the binary.
- On a filesystem where WAL is unsafe (network mounts), fall back to `journal_mode=DELETE` and warn.
- Percentiles and statistical tests are computed in Rust over query results.

Retention: `toe gc` prunes raw events and artifacts older than a configured age while keeping per-run aggregates.

## Consequences

- No server, one file, trivial backup and inspection (`sqlite3`).
- One writer at a time; fine at test-event rates with batching.
- Heavy analytics later: export to Parquet and query with DuckDB, without changing the primary store.

## Alternatives

- DuckDB as primary: one read-write process per file; multi-process writes need a server.
- Turso (Rust rewrite): production readiness unclear.
- Key-value stores (redb, fjall): no SQL for ad hoc analysis.
- Postgres: a daemon, against the local single-file goal.
- JSONL files only: simple, but history queries become custom code.
