-- Known-flaky tests set aside from the verdict, by hand, with a reason and
-- an expiry (ADR 0027). Rows are never deleted: a removal sets removed_ms,
-- so a run made while a quarantine was in force can always be explained.
CREATE TABLE quarantine (
    id         INTEGER PRIMARY KEY,
    test       TEXT    NOT NULL,              -- full display name (shared across suites)
    reason     TEXT    NOT NULL,
    created_ms INTEGER NOT NULL,
    expires_ms INTEGER NOT NULL,
    removed_ms INTEGER
) STRICT;
CREATE INDEX quarantine_by_test ON quarantine (test);
