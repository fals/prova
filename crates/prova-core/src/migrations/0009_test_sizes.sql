-- What each test attempt was seen to use (ADR 0025): its size from
-- processes and network, with the evidence. Only attempts prova could
-- attribute have a row.
CREATE TABLE test_sizes (
    run_id   TEXT    NOT NULL REFERENCES runs (id),
    test_id  TEXT    NOT NULL,
    attempt  INTEGER NOT NULL,
    size     TEXT    NOT NULL CHECK (size IN ('small', 'medium', 'large')),
    evidence TEXT    NOT NULL,
    PRIMARY KEY (run_id, test_id, attempt)
) STRICT;
CREATE INDEX test_sizes_test ON test_sizes (test_id);
