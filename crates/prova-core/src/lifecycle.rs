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
    /// Exit code of the process; `None` until it exits, or when a signal
    /// ended it.
    exit_code: Option<i32>,
    /// The framework's own report contradicts a pass (ADR 0017).
    contradiction: bool,
    verdict: Option<Outcome>,
}

/// The run's verdict (ADR 0017, `decide` in `spec/run.qnt`): a pass must be
/// earned. `Passed`, `Failed` (a test failed, or the evidence contradicts a
/// pass, or a non-zero exit) or `Errored` (the run did not finish normally,
/// the command could not start, or a signal ended it).
pub fn decide(
    status: Status,
    exit_code: Option<i32>,
    outcomes: &BTreeMap<String, Outcome>,
    contradiction: bool,
) -> Outcome {
    if status != Status::Finished {
        return Outcome::Errored;
    }
    match exit_code {
        Some(126 | 127) | None => return Outcome::Errored,
        _ => {}
    }
    if outcomes
        .values()
        .any(|o| matches!(o, Outcome::Failed | Outcome::Errored))
        || contradiction
    {
        return Outcome::Failed;
    }
    if exit_code != Some(0) {
        return Outcome::Failed;
    }
    Outcome::Passed
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
            exit_code: None,
            contradiction: false,
            verdict: None,
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
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }
    pub fn contradicted(&self) -> bool {
        self.contradiction
    }
    /// The verdict, once the run ended.
    pub fn verdict(&self) -> Option<Outcome> {
        self.verdict
    }

    /// The process exited (`None`: a signal ended it).
    pub fn exited(&mut self, code: Option<i32>) -> Result<(), LifecycleError> {
        if self.status != Status::Running {
            return Err(LifecycleError::WrongState(self.status));
        }
        self.exit_code = code;
        Ok(())
    }

    /// The framework's own report contradicts a pass.
    pub fn contradict(&mut self) -> Result<(), LifecycleError> {
        if self.status != Status::Running {
            return Err(LifecycleError::WrongState(self.status));
        }
        self.contradiction = true;
        Ok(())
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

    /// Starts a test once, or again after a failed attempt: a retry
    /// (spec/run.qnt, ADR 0024).
    pub fn test_started(&mut self, test: &str) -> Result<(), LifecycleError> {
        self.reject_late()?;
        if self.open.contains(test) || self.outcomes.get(test).is_some_and(|o| !o.is_failure()) {
            return Err(LifecycleError::DuplicateTest);
        }
        self.open.insert(test.to_owned());
        Ok(())
    }

    /// Records an outcome. Frameworks that report only results (no start
    /// event) are accepted: the start is implied.
    /// A retry's start may be implied too: after a failed attempt.
    pub fn test_finished(&mut self, test: &str, outcome: Outcome) -> Result<(), LifecycleError> {
        self.reject_late()?;
        if !self.open.remove(test) && self.outcomes.get(test).is_some_and(|o| !o.is_failure()) {
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
        self.verdict = Some(decide(
            status,
            self.exit_code,
            &self.outcomes,
            self.contradiction,
        ));
        Ok(closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_must_be_earned() {
        let run = |exit: Option<i32>, tests: &[(&str, Outcome)], unfinished: bool, contra: bool| {
            let mut l = Lifecycle::new();
            l.start().unwrap();
            for (t, o) in tests {
                l.test_finished(t, *o).unwrap();
            }
            if unfinished {
                l.test_started("hung").unwrap();
            }
            if contra {
                l.contradict().unwrap();
            }
            l.exited(exit).unwrap();
            l.end(Status::Finished).unwrap();
            l.verdict().unwrap()
        };
        let ok = [("a", Outcome::Passed)];
        assert_eq!(run(Some(0), &ok, false, false), Outcome::Passed);
        assert_eq!(
            run(Some(0), &ok, true, false),
            Outcome::Failed,
            "unfinished test"
        );
        assert_eq!(
            run(Some(0), &ok, false, true),
            Outcome::Failed,
            "framework says otherwise"
        );
        assert_eq!(
            run(Some(0), &[("a", Outcome::Errored)], false, false),
            Outcome::Failed
        );
        assert_eq!(
            run(Some(127), &[], false, false),
            Outcome::Errored,
            "could not start"
        );
        assert_eq!(run(None, &ok, false, false), Outcome::Errored, "signal");
        assert_eq!(run(Some(1), &ok, false, false), Outcome::Failed);
    }

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

    /// ADR 0024: a failed test may run again; its last attempt decides.
    #[test]
    fn a_retry_after_a_failure_replaces_it() {
        let mut l = Lifecycle::new();
        l.start().unwrap();
        l.test_finished("x", Outcome::Failed).unwrap();
        l.test_finished("x", Outcome::Passed).unwrap();
        l.exited(Some(0)).unwrap();
        l.end(Status::Finished).unwrap();
        assert_eq!(l.verdict(), Some(Outcome::Passed));
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
