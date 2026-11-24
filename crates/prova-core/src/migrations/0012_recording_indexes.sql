-- Recording a result looks up the test's history (ADR 0004 slow budget). Two
-- lookups scanned: the "unverified run" check read every event of every
-- candidate run (cost grew with history, issue #91), and tests were matched
-- by name without an index.
CREATE INDEX events_warnings ON events (run_id) WHERE kind = 'run_warning';
CREATE INDEX tests_by_name ON tests (name);
-- observed_size read every event of every run that ran the test.
CREATE INDEX events_run_sizes ON events (run_id) WHERE kind = 'run_size';
