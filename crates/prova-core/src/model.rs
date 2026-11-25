//! Contract types (ADR 0002, 0003, 0013). Everything stored or printed as
//! JSON is defined here; schemas under `schemas/` are generated from it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Version of the event contract. Bump on any breaking change.
pub const EVENT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Created,
    Running,
    Finished,
    Cancelled,
    Aborted,
}

impl Status {
    pub fn is_terminal(self) -> bool {
        matches!(self, Status::Finished | Status::Cancelled | Status::Aborted)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Created => "created",
            Status::Running => "running",
            Status::Finished => "finished",
            Status::Cancelled => "cancelled",
            Status::Aborted => "aborted",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "created" => Status::Created,
            "running" => Status::Running,
            "finished" => Status::Finished,
            "cancelled" => Status::Cancelled,
            "aborted" => Status::Aborted,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    Skipped,
    Errored,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Passed => "passed",
            Outcome::Failed => "failed",
            Outcome::Skipped => "skipped",
            Outcome::Errored => "errored",
        }
    }

    pub fn parse(s: &str) -> Option<Outcome> {
        Some(match s {
            "passed" => Outcome::Passed,
            "failed" => Outcome::Failed,
            "skipped" => Outcome::Skipped,
            "errored" => Outcome::Errored,
            _ => return None,
        })
    }

    pub fn is_failure(self) -> bool {
        matches!(self, Outcome::Failed | Outcome::Errored)
    }
}

/// Fidelity ladder (ADR 0013). Ordered: higher knows more.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Exit code, output and process tree only.
    Opaque,
    /// Result files (JUnit XML, TRX) written during the run.
    ResultFiles,
    /// Known framework lines recognised in live output.
    Recognized,
    /// The framework's own structured stream.
    Native,
}

impl Level {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Level {
        match v {
            0 => Level::Opaque,
            1 => Level::ResultFiles,
            2 => Level::Recognized,
            _ => Level::Native,
        }
    }
}

/// Identifies a test inside a suite, as reported by the framework.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub struct TestKey {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Enclosing suites or classes, outermost first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<String>,
    pub name: String,
}

/// The `file` of a test whose directory could not be told apart from
/// another's of the same id (ADR 0031 section 2).
pub const UNKNOWN_PLACE: &str = "(unknown directory)";

impl TestKey {
    pub fn new(path: Vec<String>, name: impl Into<String>) -> TestKey {
        TestKey {
            file: None,
            path,
            name: name.into(),
        }
    }

    /// Key that tells apart same-named tests in different files or test
    /// binaries. Use for deduplication; `display` is for people.
    pub fn unique(&self) -> String {
        format!(
            "{}\u{1f}{}",
            self.file.as_deref().unwrap_or(""),
            self.display()
        )
    }

    /// Where the test lives when its name alone does not say (Rust: the
    /// test binary). Go, pytest and Vitest tests carry their package or file
    /// in the name already.
    pub fn place(&self) -> Option<&str> {
        self.file.as_deref().filter(|f| {
            !f.is_empty() && !f.starts_with("(package pending)") && !f.starts_with(UNKNOWN_PLACE)
        })
    }

    /// The test's directory could not be told (pytest from an unknown
    /// directory): no quarantine sets it aside.
    pub fn place_unknown(&self) -> bool {
        self.file.as_deref() == Some(UNKNOWN_PLACE)
    }

    /// Human-readable full name, `path::name`.
    pub fn display(&self) -> String {
        let mut parts: Vec<&str> = self.path.iter().map(String::as_str).collect();
        parts.push(&self.name);
        parts.join("::")
    }
}

/// What a stall was detected on (ADR 0004, 0013).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StallBasis {
    /// No CPU time, output or new processes in the process tree.
    ProcessTree,
    /// No structured test event.
    TestEvents,
}

/// Origin of a slow-test budget.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BudgetFrom {
    /// The test runner's own warning (libtest's 60 s, nextest's SLOW).
    #[default]
    Runner,
    /// Three times the test's usual duration (p95 of its history).
    History,
    /// The suite's size default: the test has no history yet.
    Default,
}

impl BudgetFrom {
    pub fn describe(self) -> &'static str {
        match self {
            BudgetFrom::Runner => "the test runner's own warning",
            BudgetFrom::History => "3x its usual time",
            BudgetFrom::Default => "the suite's default; no duration history yet",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    RunStarted {
        pid: u32,
    },
    TestStarted {
        test: TestKey,
    },
    TestSlow {
        test: TestKey,
        elapsed_ms: u64,
        budget_ms: u64,
        /// Where the budget came from, so the report can say (round 7).
        #[serde(default)]
        budget_from: BudgetFrom,
    },
    TestFinished {
        test: TestKey,
        attempt: u32,
        outcome: Outcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        source: Level,
        /// A result nested in another test that reports its own (Go's
        /// `TestX/case`, pytest's subtests): counted apart from tests.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        subtest: bool,
        /// This execution passed, but the outcome recorded is the failure
        /// of an earlier iteration of the same run, which stands (`go test
        /// -count=N`: repeats are not retries, ADR 0029 section 2). History
        /// reads it as a pass within a failed run: flaky evidence.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        passed_in_iteration: bool,
    },
    /// A live result whose name was ambiguous (VSTest's console names
    /// NUnit and MSTest tests by bare method) replaced by the result file's
    /// qualified one: `by` counts and keeps the history, `test` does not.
    TestSuperseded {
        test: TestKey,
        by: TestKey,
    },
    RunStalled {
        idle_ms: u64,
        basis: StallBasis,
    },
    /// Something the agent should know about how much prova could see.
    RunNote {
        message: String,
    },
    /// What the framework's own summary counted (ADR 0018), shown next to
    /// the tests prova saw individually so the counts never contradict.
    RunTotals {
        tests: u64,
        failed: u64,
    },
    /// The run passed but verified little (ADR 0017): no test observed,
    /// every test skipped, tests missing since the last pass, or error
    /// output outside any test. A run with a warning is not `verified`.
    RunWarning {
        message: String,
    },
    /// What a test used, when prova could attribute it (ADR 0025): its own
    /// process and descendants, or the run's group while it was the only
    /// test running. The size follows from processes and network only.
    TestSize {
        test: TestKey,
        attempt: u32,
        size: crate::config::Size,
        evidence: String,
    },
    /// What the whole command used (ADR 0025): the upper bound for tests
    /// prova could not attribute.
    RunSize {
        size: crate::config::Size,
        evidence: String,
    },
    RunFinished {
        status: Status,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Process ids still alive in the run's process group after it ended.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        leaked: Vec<u32>,
    },
}

impl EventKind {
    pub fn name(&self) -> &'static str {
        match self {
            EventKind::RunStarted { .. } => "run_started",
            EventKind::TestStarted { .. } => "test_started",
            EventKind::TestSlow { .. } => "test_slow",
            EventKind::TestFinished { .. } => "test_finished",
            EventKind::RunStalled { .. } => "run_stalled",
            EventKind::RunNote { .. } => "run_note",
            EventKind::RunWarning { .. } => "run_warning",
            EventKind::RunTotals { .. } => "run_totals",
            EventKind::TestSuperseded { .. } => "test_superseded",
            EventKind::TestSize { .. } => "test_size",
            EventKind::RunSize { .. } => "run_size",
            EventKind::RunFinished { .. } => "run_finished",
        }
    }
}

/// One observation in a run's ordered event stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    pub v: u32,
    pub run: String,
    /// Monotonic per run, starting at 1.
    pub seq: u64,
    /// Unix time in milliseconds.
    pub ts_ms: u64,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_kinds() -> Vec<EventKind> {
        let t = TestKey::new(vec!["mod".into()], "case");
        vec![
            EventKind::RunStarted { pid: 7 },
            EventKind::TestStarted { test: t.clone() },
            EventKind::TestSlow {
                test: t.clone(),
                elapsed_ms: 10,
                budget_ms: 5,
                budget_from: BudgetFrom::History,
            },
            EventKind::TestFinished {
                test: t,
                attempt: 1,
                outcome: Outcome::Failed,
                duration_ms: Some(3),
                message: Some("boom".into()),
                source: Level::Recognized,
                subtest: false,
                passed_in_iteration: false,
            },
            EventKind::RunStalled {
                idle_ms: 60_000,
                basis: StallBasis::ProcessTree,
            },
            EventKind::RunFinished {
                status: Status::Finished,
                exit_code: Some(1),
                reason: None,
                leaked: vec![42],
            },
        ]
    }

    #[test]
    fn every_event_kind_round_trips_through_json() {
        for (i, kind) in all_kinds().into_iter().enumerate() {
            let e = Event {
                v: EVENT_VERSION,
                run: "r1".into(),
                seq: i as u64 + 1,
                ts_ms: 1,
                kind,
            };
            let json = serde_json::to_string(&e).expect("serialize");
            assert!(
                json.contains(&format!("\"kind\":\"{}\"", e.kind.name())),
                "{json}"
            );
            let back: Event = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, e);
        }
    }

    #[test]
    fn levels_are_ordered_by_fidelity() {
        assert!(Level::Opaque < Level::ResultFiles);
        assert!(Level::Recognized < Level::Native);
        for l in [
            Level::Opaque,
            Level::ResultFiles,
            Level::Recognized,
            Level::Native,
        ] {
            assert_eq!(Level::from_u8(l.as_u8()), l);
        }
    }

    #[test]
    fn status_and_outcome_strings_round_trip() {
        for s in [
            Status::Created,
            Status::Running,
            Status::Finished,
            Status::Cancelled,
            Status::Aborted,
        ] {
            assert_eq!(Status::parse(s.as_str()), Some(s));
        }
        for o in [
            Outcome::Passed,
            Outcome::Failed,
            Outcome::Skipped,
            Outcome::Errored,
        ] {
            assert_eq!(Outcome::parse(o.as_str()), Some(o));
        }
    }
}
