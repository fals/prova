-- Test identity and per-test attempts (ADR 0006); result-file artifacts.

CREATE TABLE tests (
    id            TEXT PRIMARY KEY,           -- hash of suite, file, path, name
    suite         TEXT NOT NULL,
    file          TEXT,
    path          TEXT NOT NULL,              -- JSON array
    name          TEXT NOT NULL,
    display       TEXT NOT NULL,
    first_seen_ms INTEGER NOT NULL
) STRICT;

CREATE INDEX tests_by_display ON tests (display);
CREATE INDEX tests_by_suite ON tests (suite);

CREATE TABLE attempts (
    run_id      TEXT NOT NULL REFERENCES runs (id),
    test_id     TEXT NOT NULL REFERENCES tests (id),
    attempt     INTEGER NOT NULL CHECK (attempt > 0),
    outcome     TEXT NOT NULL CHECK (outcome IN ('passed', 'failed', 'skipped', 'errored')),
    duration_ms INTEGER,
    message     TEXT,
    fingerprint TEXT,
    source      INTEGER NOT NULL CHECK (source BETWEEN 0 AND 3),
    ts_ms       INTEGER NOT NULL,
    PRIMARY KEY (run_id, test_id, attempt)
) STRICT;

CREATE INDEX attempts_by_test ON attempts (test_id, ts_ms);

CREATE TRIGGER attempts_immutable
BEFORE UPDATE ON attempts
BEGIN
    SELECT RAISE(ABORT, 'attempts are immutable');
END;

CREATE TABLE artifacts (
    run_id TEXT NOT NULL REFERENCES runs (id),
    kind   TEXT NOT NULL,
    source TEXT NOT NULL,                     -- original path, relative to root
    sha256 TEXT NOT NULL,                     -- stored at .prova/artifacts/<sha256>
    PRIMARY KEY (run_id, source)
) STRICT;
