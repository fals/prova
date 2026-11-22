-- Runs and their event streams (ADR 0002, 0008).

CREATE TABLE runs (
    id               TEXT PRIMARY KEY,
    suite            TEXT NOT NULL,
    spec             TEXT NOT NULL,          -- JSON: command, budgets (immutable inputs)
    cwd              TEXT NOT NULL,
    code             TEXT NOT NULL,          -- code identity
    env              TEXT NOT NULL,          -- JSON: recorded environment
    machine          TEXT NOT NULL,          -- JSON: machine fingerprint
    status           TEXT NOT NULL
        CHECK (status IN ('created', 'running', 'finished', 'cancelled', 'aborted')),
    outcome          TEXT CHECK (outcome IN ('passed', 'failed', 'skipped', 'errored')),
    level            INTEGER NOT NULL DEFAULT 0 CHECK (level BETWEEN 0 AND 3),
    created_ms       INTEGER NOT NULL,
    started_ms       INTEGER,
    ended_ms         INTEGER,
    exit_code        INTEGER,
    reason           TEXT,
    supervisor_pid   INTEGER,
    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    fingerprint      TEXT,
    CHECK ((status IN ('finished', 'cancelled', 'aborted')) = (ended_ms IS NOT NULL))
) STRICT;

CREATE INDEX runs_by_suite ON runs (suite, created_ms);
CREATE INDEX runs_by_created ON runs (created_ms);

CREATE TABLE events (
    run_id  TEXT NOT NULL REFERENCES runs (id),
    seq     INTEGER NOT NULL CHECK (seq > 0),
    ts_ms   INTEGER NOT NULL,
    kind    TEXT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (run_id, seq)
) STRICT;

-- spec/run.qnt inputsFrozen
CREATE TRIGGER runs_inputs_immutable
BEFORE UPDATE OF suite, spec, cwd, code, env, machine, created_ms ON runs
BEGIN
    SELECT RAISE(ABORT, 'run inputs are immutable');
END;

-- terminal states are absorbing
CREATE TRIGGER runs_terminal_absorbing
BEFORE UPDATE OF status ON runs
WHEN OLD.status IN ('finished', 'cancelled', 'aborted') AND NEW.status <> OLD.status
BEGIN
    SELECT RAISE(ABORT, 'run already ended');
END;

-- spec/run.qnt nothingAfterEnd
CREATE TRIGGER events_only_while_open
BEFORE INSERT ON events
WHEN (SELECT status FROM runs WHERE id = NEW.run_id) NOT IN ('created', 'running')
BEGIN
    SELECT RAISE(ABORT, 'run already ended; late event refused');
END;

-- events are append-only
CREATE TRIGGER events_append_only_update
BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;
