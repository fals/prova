-- Live results replaced by a result file's qualified name (round 13: VSTest's
-- console names NUnit and MSTest tests by bare method, so two classes' tests
-- swapped histories and a deterministic failure was called flaky). Attempts
-- stay immutable; history leaves superseded ones out.
CREATE TABLE superseded (
    run_id  TEXT NOT NULL REFERENCES runs (id),
    test_id TEXT NOT NULL REFERENCES tests (id),
    by_id   TEXT NOT NULL REFERENCES tests (id),
    PRIMARY KEY (run_id, test_id)
) STRICT;
