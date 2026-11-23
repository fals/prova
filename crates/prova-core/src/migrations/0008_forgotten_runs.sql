-- Runs taken out of every test's history by hand (`prova forget`), with
-- the reason (round 11: a poisoned run made a real bug "flaky" for good).
CREATE TABLE forgotten (
    run_id     TEXT PRIMARY KEY REFERENCES runs (id),
    reason     TEXT NOT NULL,
    created_ms INTEGER NOT NULL
) STRICT;
