-- Where a run happened (ADR 0014): host, pod, node, actor, as JSON.

ALTER TABLE runs ADD COLUMN origin TEXT NOT NULL DEFAULT '{}';

DROP TRIGGER runs_inputs_immutable;
CREATE TRIGGER runs_inputs_immutable
BEFORE UPDATE OF suite, spec, cwd, code, env, machine, created_ms, origin ON runs
BEGIN
    SELECT RAISE(ABORT, 'run inputs are immutable');
END;
