//! Framework adapters and output recognizers (ADR 0003, 0013).

pub mod adopt;
pub mod junit;
pub mod recognize;
pub mod results;

use std::path::Path;

use toe_core::config::RunSpec;
use toe_core::engine::{ExitReport, Observer};
use toe_core::model::{EventKind, Level};

/// Every run gets the whole ladder: recognizers on live output, then
/// result files after exit. Nothing to configure.
pub struct Ladder {
    live: recognize::Recognizers,
    files: results::ResultFiles,
}

pub fn observer_for(spec: &RunSpec, _root: &Path) -> Box<dyn Observer> {
    Box::new(Ladder {
        live: recognize::Recognizers::default(),
        files: results::ResultFiles::new(&spec.results),
    })
}

impl Observer for Ladder {
    fn live_level(&self) -> Level {
        self.live.level.unwrap_or(Level::Opaque)
    }

    fn on_line(&mut self, line: &str, _now_ms: u64) -> Vec<EventKind> {
        self.live.line(line)
    }

    /// Result files are authoritative for tests the live output missed;
    /// tests already reported live are not reported twice. Disagreements
    /// are logged as recognizer defects (supervisor.log).
    fn on_exit(&mut self, root: &Path, started_ms: u64) -> ExitReport {
        let mut report = self.files.on_exit(root, started_ms);
        // Live and file results are matched by display name (best effort:
        // result files rarely carry the same file attribute).
        let live_names: std::collections::BTreeSet<String> =
            self.live.seen.values().map(|(d, _)| d.clone()).collect();
        let mut finals: std::collections::BTreeMap<String, toe_core::model::Outcome> =
            Default::default();
        for e in &report.events {
            if let EventKind::TestFinished { test, outcome, .. } = e {
                finals.insert(test.display(), *outcome);
            }
        }
        for (name, live) in self.live.seen.values() {
            if let Some(file) = finals.get(name)
                && file.is_failure() != live.is_failure()
            {
                eprintln!(
                    "toe: recognizer disagreement for {name}: live {live:?}, result file {file:?}"
                );
            }
        }
        report.events.retain(|e| match e {
            EventKind::TestFinished { test, .. } => !live_names.contains(&test.display()),
            _ => true,
        });
        // Say plainly when the output hid tests (e.g. `cargo test --quiet`).
        let file_only = finals
            .keys()
            .filter(|k| !live_names.contains(*k))
            .count() as u64;
        let seen = self.live.seen.len() as u64 + file_only;
        if self.live.reported_total > seen {
            report.events.push(EventKind::RunNote {
                message: format!(
                    "the framework reported {} tests but toe saw {seen} individually (condensed or quiet output): \
                     passing tests are not recorded, so flaky detection and durations are incomplete. \
                     Run the framework with verbose output or a JUnit report (`toe adopt` shows how).",
                    self.live.reported_total
                ),
            });
        }
        let live = self.live.level;
        report.level = match (report.level, live) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        report
    }
}
