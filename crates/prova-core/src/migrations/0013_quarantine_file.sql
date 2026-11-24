-- The file (test binary, module) of a quarantined test, when its name is
-- shared by tests in several (round 16: `tests::flaky` of crate calc set
-- aside crate geo's `tests::flaky` too). NULL: the name alone.
ALTER TABLE quarantine ADD COLUMN file TEXT;
