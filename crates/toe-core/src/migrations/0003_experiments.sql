-- Experiments (ADR 0007): frozen protocol, variants, comparisons, trials,
-- samples, verdicts and the improvement-loop decision log.

CREATE TABLE experiments (
    name          TEXT PRIMARY KEY,
    hypothesis    TEXT NOT NULL,
    protocol      TEXT NOT NULL,              -- JSON
    protocol_hash TEXT NOT NULL,
    frozen        INTEGER NOT NULL DEFAULT 0 CHECK (frozen IN (0, 1)),
    cwd           TEXT NOT NULL,              -- relative to the root
    best          TEXT NOT NULL DEFAULT 'baseline',
    created_ms    INTEGER NOT NULL
) STRICT;

-- spec/experiment.qnt protocolFrozen
CREATE TRIGGER experiments_protocol_frozen
BEFORE UPDATE OF protocol, protocol_hash, hypothesis ON experiments
WHEN OLD.frozen = 1
BEGIN
    SELECT RAISE(ABORT, 'protocol frozen at first trial');
END;

CREATE TRIGGER experiments_stay_frozen
BEFORE UPDATE OF frozen ON experiments
WHEN OLD.frozen = 1 AND NEW.frozen = 0
BEGIN
    SELECT RAISE(ABORT, 'protocol frozen at first trial');
END;

CREATE TABLE variants (
    experiment TEXT NOT NULL REFERENCES experiments (name),
    name       TEXT NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('ref', 'workdir')),
    commit_sha TEXT,                          -- for ref variants
    code       TEXT NOT NULL,                 -- code identity when created
    params     TEXT NOT NULL,                 -- JSON object
    created_ms INTEGER NOT NULL,
    PRIMARY KEY (experiment, name),
    CHECK ((kind = 'ref') = (commit_sha IS NOT NULL))
) STRICT;

CREATE TABLE comparisons (
    id               TEXT PRIMARY KEY,
    experiment       TEXT NOT NULL REFERENCES experiments (name),
    baseline         TEXT NOT NULL,
    candidate        TEXT NOT NULL,
    status           TEXT NOT NULL CHECK (status IN ('running', 'decided', 'failed', 'cancelled')),
    verdict          TEXT CHECK (verdict IN ('accept', 'reject', 'inconclusive', 'invalid')),
    result           TEXT,                    -- JSON verdict details
    worker_pid       INTEGER,
    cancel_requested INTEGER NOT NULL DEFAULT 0,
    created_ms       INTEGER NOT NULL,
    ended_ms         INTEGER,
    FOREIGN KEY (experiment, baseline) REFERENCES variants (experiment, name),
    FOREIGN KEY (experiment, candidate) REFERENCES variants (experiment, name),
    CHECK ((status = 'decided') = (verdict IS NOT NULL))
) STRICT;

CREATE TRIGGER comparisons_end_absorbing
BEFORE UPDATE OF status ON comparisons
WHEN OLD.status <> 'running' AND NEW.status <> OLD.status
BEGIN
    SELECT RAISE(ABORT, 'comparison already ended');
END;

CREATE TABLE trials (
    comparison  TEXT NOT NULL REFERENCES comparisons (id),
    seq         INTEGER NOT NULL CHECK (seq > 0),
    arm         TEXT NOT NULL CHECK (arm IN ('baseline', 'candidate')),
    warmup      INTEGER NOT NULL CHECK (warmup IN (0, 1)),
    state       TEXT NOT NULL CHECK (state IN ('complete', 'failed')),
    exit_code   INTEGER,
    fingerprint TEXT NOT NULL,
    started_ms  INTEGER NOT NULL,
    ended_ms    INTEGER NOT NULL,
    PRIMARY KEY (comparison, seq)
) STRICT;

-- completed trials are immutable
CREATE TRIGGER trials_immutable
BEFORE UPDATE ON trials
BEGIN
    SELECT RAISE(ABORT, 'trials are immutable');
END;

CREATE TABLE samples (
    comparison TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    metric     TEXT NOT NULL,
    value      REAL NOT NULL,
    unit       TEXT,
    PRIMARY KEY (comparison, seq, metric),
    FOREIGN KEY (comparison, seq) REFERENCES trials (comparison, seq)
) STRICT;

CREATE TABLE decisions (
    experiment TEXT NOT NULL REFERENCES experiments (name),
    comparison TEXT NOT NULL REFERENCES comparisons (id),
    candidate  TEXT NOT NULL,
    decision   TEXT NOT NULL CHECK (decision IN ('keep', 'discard', 'crash')),
    reason     TEXT NOT NULL,
    created_ms INTEGER NOT NULL,
    PRIMARY KEY (comparison)
) STRICT;
