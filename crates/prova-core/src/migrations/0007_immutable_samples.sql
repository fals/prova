-- Measurements are written once (round 9: a candidate's code rewrote the
-- baseline's samples in prova.db during its own trial). Defense in depth:
-- trials no longer see PROVA_HOME either.
CREATE TRIGGER samples_immutable_update BEFORE UPDATE ON samples
BEGIN SELECT RAISE(ABORT, 'samples are immutable'); END;
CREATE TRIGGER samples_immutable_delete BEFORE DELETE ON samples
BEGIN SELECT RAISE(ABORT, 'samples are immutable'); END;
CREATE TRIGGER trials_immutable_update BEFORE UPDATE ON trials
BEGIN SELECT RAISE(ABORT, 'trials are immutable'); END;
