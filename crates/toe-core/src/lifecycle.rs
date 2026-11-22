//! Run lifecycle state machine. Mirrors `spec/run.qnt` action by action;
//! `tests/spec_replay.rs` checks the two agree on generated traces.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Outcome, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleError {
    #[error("run is {0:?}; this action needs another state")]
    WrongState(Status),
    #[error("run already ended; late event dropped")]
    RunTerminal,
    #[error("test already started or finished")]
    DuplicateTest,
    #[error("test was not started")]
    NotStarted,
    #[error("end status must be terminal")]
    NotTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lifecycle {
    status: Status,
    open: BTreeSet<String>,
    outcomes: BTreeMap<String, Outcome>,
    dropped: u64,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Lifecycle::new()
    }
}

impl Lifecycle {
    pub fn new() -> Lifecycle {
        Lifecycle {
            status: Status::Created,
            open: BTreeSet::new(),
            outcomes: BTreeMap::new(),
            dropped: 0,
        }
    }

    /// Rebuild from a stored status, for processes that only need to guard
    /// the next transition (open tests are not needed for that).
    pub fn at(status: Status) -> Lifecycle {
        Lifecycle {
            status,
            ..Lifecycle::new()
        }
    }

    pub fn status(&self) -> Status {
        self.status
    }
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
    pub fn open(&self) -> &BTreeSet<String> {
        &self.open
    }
    pub fn outcomes(&self) -> &BTreeMap<String, Outcome> {
        &self.outcomes
    }

    pub fn start(&mut self) -> Result<(), LifecycleError> {
        if self.status != Status::Created {
            return Err(LifecycleError::WrongState(self.status));
        }
        self.status = Status::Running;
        Ok(())
    }

    fn reject_late(&mut self) -> Result<(), LifecycleError> {
        if self.status.is_terminal() {
            self.dropped += 1;
            return Err(LifecycleError::RunTerminal);
        }
        if self.status != Status::Running {
            return Err(LifecycleError::WrongState(self.status));
        }
        Ok(())
    }

    pub fn test_started(&mut self, test: &str) -> Result<(), LifecycleError> {
        self.reject_late()?;
        if self.open.contains(test) || self.outcomes.contains_key(test) {
            return Err(LifecycleError::DuplicateTest);
        }
        self.open.insert(test.to_owned());
        Ok(())
    }

    /// Records an outcome. Frameworks that report only results (no start
    /// event) are accepted: the start is implied.
    pub fn test_finished(&mut self, test: &str, outcome: Outcome) -> Result<(), LifecycleError> {
        self.reject_late()?;
        if !self.open.remove(test) && self.outcomes.contains_key(test) {
            return Err(LifecycleError::DuplicateTest);
        }
        self.outcomes.insert(test.to_owned(), outcome);
        Ok(())
    }

    /// Ends the run; open tests become errored.
    pub fn end(&mut self, status: Status) -> Result<Vec<String>, LifecycleError> {
        if !status.is_terminal() {
            return Err(LifecycleError::NotTerminal);
        }
        match self.status {
            Status::Running => {}
            Status::Created if status == Status::Cancelled => {}
            s if s.is_terminal() => return Err(LifecycleError::RunTerminal),
            s => return Err(LifecycleError::WrongState(s)),
        }
        let closed: Vec<String> = std::mem::take(&mut self.open).into_iter().collect();
        for t in &closed {
            self.outcomes.insert(t.clone(), Outcome::Errored);
        }
        self.status = status;
        Ok(closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_result_after_cancel_is_rejected() {
        let mut l = Lifecycle::new();
        l.start().unwrap();
        l.test_started("a").unwrap();
        assert_eq!(l.end(Status::Cancelled).unwrap(), vec!["a".to_string()]);
        assert_eq!(
            l.test_finished("a", Outcome::Passed),
            Err(LifecycleError::RunTerminal)
        );
        assert_eq!(l.status(), Status::Cancelled);
        assert_eq!(l.outcomes().get("a"), Some(&Outcome::Errored));
        assert_eq!(l.dropped(), 1);
    }

    #[test]
    fn terminal_states_are_absorbing() {
        let mut l = Lifecycle::new();
        l.start().unwrap();
        l.end(Status::Finished).unwrap();
        assert_eq!(l.end(Status::Aborted), Err(LifecycleError::RunTerminal));
        assert_eq!(l.start(), Err(LifecycleError::WrongState(Status::Finished)));
        assert_eq!(l.status(), Status::Finished);
    }

    #[test]
    fn result_only_frameworks_imply_start() {
        let mut l = Lifecycle::new();
        l.start().unwrap();
        l.test_finished("x", Outcome::Passed).unwrap();
        assert_eq!(
            l.test_finished("x", Outcome::Failed),
            Err(LifecycleError::DuplicateTest)
        );
    }

    #[test]
    fn cancel_before_start_is_allowed() {
        let mut l = Lifecycle::new();
        l.end(Status::Cancelled).unwrap();
        assert_eq!(l.status(), Status::Cancelled);
        let mut l = Lifecycle::new();
        assert_eq!(
            l.end(Status::Finished),
            Err(LifecycleError::WrongState(Status::Created))
        );
    }
}
