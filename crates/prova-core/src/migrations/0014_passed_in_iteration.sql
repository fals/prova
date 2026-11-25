-- An execution that passed within a run whose recorded outcome is a failure
-- standing from an earlier iteration of it (go test -count=N, ADR 0029
-- section 2): history reads it as a pass in a failed run, flaky evidence in
-- either order.
ALTER TABLE attempts ADD COLUMN passed_in_iteration INTEGER NOT NULL DEFAULT 0;
