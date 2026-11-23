-- A best variant set by hand (`prova exp best`), e.g. after an ACCEPT that
-- turned out wrong: recorded, so the decision log stays complete (round 8).
CREATE TABLE best_resets (
    experiment TEXT NOT NULL REFERENCES experiments (name),
    from_best  TEXT NOT NULL,
    to_best    TEXT NOT NULL,
    reason     TEXT NOT NULL,
    created_ms INTEGER NOT NULL
) STRICT;
