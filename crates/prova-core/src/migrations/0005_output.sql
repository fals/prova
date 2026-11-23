-- Distilled output (ADR 0016): what is kept of a run's output once it ends.
-- Blobs are deflate-compressed; NULL once dropped to stay within budget.

CREATE TABLE run_output (
    run_id     TEXT PRIMARY KEY REFERENCES runs (id),
    excerpt    BLOB,                          -- JSON Excerpt
    full       BLOB,                          -- whole output, only with --keep-output
    lines      INTEGER NOT NULL,
    bytes      INTEGER NOT NULL,
    pruned_ms  INTEGER,
    created_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE test_output (
    run_id     TEXT NOT NULL REFERENCES runs (id),
    test_id    TEXT NOT NULL REFERENCES tests (id),
    output     BLOB,                          -- a failing test's own output
    created_ms INTEGER NOT NULL,
    PRIMARY KEY (run_id, test_id)
) STRICT;

CREATE TABLE comparison_output (
    comparison TEXT NOT NULL REFERENCES comparisons (id),
    name       TEXT NOT NULL,                 -- "3" (trial), "setup-baseline", "oracle-candidate"
    excerpt    BLOB,
    created_ms INTEGER NOT NULL,
    PRIMARY KEY (comparison, name)
) STRICT;

CREATE INDEX run_output_by_age ON run_output (created_ms);
