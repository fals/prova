//! Framework adapters and output recognizers (ADR 0003, 0013).

pub mod adopt;
pub mod bench;
pub mod inject;
pub mod junit;
pub mod recognize;
pub mod results;

use std::path::Path;

use prova_core::config::RunSpec;
use prova_core::engine::{ExitReport, Observer};
use prova_core::model::{EventKind, Level};

/// Every run gets the whole ladder: recognizers on live output, then
/// result files after exit. Nothing to configure.
pub struct Ladder {
    live: recognize::Recognizers,
    files: results::ResultFiles,
    inject: Vec<String>,
    /// Where injected reports are written (the run's private directory).
    inject_dir: Option<std::path::PathBuf>,
    /// Side streams the framework appends to, read live.
    streams: Vec<Stream>,
    /// "No test ran" is suspicious (ADR 0017), unless the suite says so.
    expect_tests: bool,
}

/// A file read like output: from where the last read stopped, whole lines.
struct Stream {
    path: std::path::PathBuf,
    offset: u64,
    partial: String,
}

impl Stream {
    fn read_lines(&mut self) -> Vec<String> {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(&self.path) else {
            return Vec::new();
        };
        let mut buf = Vec::new();
        if f.seek(SeekFrom::Start(self.offset)).is_err() || f.read_to_end(&mut buf).is_err() {
            return Vec::new();
        }
        self.offset += buf.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&buf));
        let mut lines: Vec<String> = self.partial.split('\n').map(str::to_owned).collect();
        self.partial = lines.pop().unwrap_or_default();
        lines
    }
}

pub fn observer_for(spec: &RunSpec, _root: &Path) -> Box<dyn Observer> {
    Box::new(Ladder {
        live: recognize::Recognizers::default(),
        files: results::ResultFiles::new(&spec.results),
        inject: spec.inject.clone(),
        inject_dir: None,
        streams: Vec::new(),
        expect_tests: spec.expects_tests(),
    })
}

impl Observer for Ladder {
    fn live_level(&self) -> Level {
        self.live.level.unwrap_or(Level::Opaque)
    }

    fn env(&mut self, run_dir: &Path) -> Vec<(String, String)> {
        if self.inject.is_empty() {
            return Vec::new();
        }
        let dir = run_dir.join("inject");
        if prova_core::store::private_dir(&dir).is_err() {
            return Vec::new();
        }
        for (name, content) in inject::files(&self.inject) {
            if std::fs::write(dir.join(name), content).is_err() {
                return Vec::new();
            }
        }
        self.streams = inject::streams(&self.inject)
            .into_iter()
            .map(|name| Stream {
                path: dir.join(name),
                offset: 0,
                partial: String::new(),
            })
            .collect();
        self.live.pytest_stream = self.inject.iter().any(|r| r == "pytest-reportlog");
        self.live.vitest_stream = self.inject.iter().any(|r| r == "vitest-stream");
        self.inject_dir = Some(dir.clone());
        inject::env(&self.inject, &dir, |k| std::env::var(k).ok())
    }

    fn on_line(&mut self, line: &str, _now_ms: u64) -> Vec<EventKind> {
        self.live.line(line)
    }

    fn poll(&mut self, _now_ms: u64) -> Vec<EventKind> {
        let mut out = Vec::new();
        for i in 0..self.streams.len() {
            for line in self.streams[i].read_lines() {
                out.extend(self.live.line(line.trim_end()));
            }
        }
        out
    }

    /// Result files are authoritative for tests the live output missed;
    /// tests already reported live are not reported twice. Disagreements
    /// are logged as recognizer defects (supervisor.log).
    fn on_exit(&mut self, root: &Path, started_ms: u64) -> ExitReport {
        // Whatever the streams got after the last tick, including a final
        // line without a newline.
        let mut held = self.poll(0);
        for i in 0..self.streams.len() {
            let rest = std::mem::take(&mut self.streams[i].partial);
            if !rest.trim().is_empty() {
                held.extend(self.live.line(rest.trim_end()));
            }
        }
        held.extend(self.live.finish());
        let mut report = self.files.on_exit(root, started_ms);
        if let Some(dir) = &self.inject_dir {
            let injected = self.files.ingest(results::files_in(dir));
            report.events.extend(injected.events);
            report.test_outputs.extend(injected.test_outputs);
            report.level = report.level.max(injected.level);
        }
        // Live and file results are matched by display name (best effort:
        // result files rarely carry the same file attribute).
        let live_names: std::collections::BTreeSet<String> =
            self.live.seen.values().map(|(d, _)| d.clone()).collect();
        let mut finals: std::collections::BTreeMap<String, prova_core::model::Outcome> =
            Default::default();
        for e in &report.events {
            if let EventKind::TestFinished { test, outcome, .. } = e {
                finals.insert(test.display(), *outcome);
            }
        }
        for (name, live) in self.live.seen.values() {
            if let Some(file) = finals.get(name).or_else(|| {
                finals
                    .iter()
                    .find(|(d, _)| d.ends_with(&format!("::{name}")))
                    .map(|(_, o)| o)
            }) && file.is_failure() != live.is_failure()
            {
                eprintln!(
                    "prova: recognizer disagreement for {name}: live {live:?}, result file {file:?}"
                );
            }
        }
        report.events.retain(|e| match e {
            // nextest's JUnit names `calc::tests::adds` what its stream
            // calls `tests::adds` (round 9: every test counted twice).
            EventKind::TestFinished { test, .. } => {
                let d = test.display();
                !live_names
                    .iter()
                    .any(|l| *l == d || d.ends_with(&format!("::{l}")))
            }
            _ => true,
        });
        // `go test` without -v or -json prints failures only (round 5).
        // (Not with -v: its `=== RUN` lines show it; round 9 noted it anyway.)
        if self.live.go_package_lines > 0
            && self.live.go_run_lines == 0
            && !self
                .live
                .seen
                .values()
                .any(|(_, o)| *o == prova_core::model::Outcome::Passed)
            && finals.is_empty()
        {
            report.events.push(EventKind::RunNote {
                message: "`go test` without -v or -json prints only failures: passing tests are not recorded, so flaky detection is blind; run `go test -json` (prova adopt proposes it; if prova.toml has such a suite, run that suite)".into(),
            });
        }
        // Say plainly when the output hid tests (e.g. `cargo test --quiet`).
        let file_only = finals.keys().filter(|k| !live_names.contains(*k)).count() as u64;
        let seen = self.live.seen.len() as u64 + self.live.repeats + file_only;
        if self.live.summary_seen {
            report.events.push(EventKind::RunTotals {
                tests: self.live.total(),
                failed: self.live.failed_reported(),
            });
        }
        // ADR 0017: evidence against a pass, and passes that verified little.
        let mut outcomes: std::collections::BTreeMap<String, prova_core::model::Outcome> = self
            .live
            .seen
            .values()
            .map(|(d, o)| (d.clone(), *o))
            .collect();
        outcomes.extend(finals.iter().map(|(k, o)| (k.clone(), *o)));
        let failed_seen = outcomes.values().filter(|o| o.is_failure()).count() as u64;
        let failed_reported = self.live.failed_reported();
        if failed_reported > failed_seen {
            report.contradictions.push(format!(
                "the framework reported {failed_reported} failed test(s) but prova recorded {failed_seen}: its summary is believed over its exit code"
            ));
        }
        // The build itself says it failed or crashed, whatever the exit code
        // (round 10: `mvn -fn`, a forked JVM that died, a crashed class).
        if let Some(line) = &self.live.build_failed_line {
            report
                .contradictions
                .push(format!("the build says it failed: {line:?}"));
        }
        if seen == 0 && self.expect_tests {
            match &self.live.no_tests_line {
                // One package without test files is normal when others
                // printed `ok` (round 10: a `cmd/` dir failed a green run).
                Some(line) if line.contains("[no test files]") && self.live.go_package_lines > 0 => {
                    report.warnings.push(format!(
                        "go reported {} package(s) as ok but showed no test (no -v or -json): only the exit code backs this pass",
                        self.live.go_package_lines
                    ))
                }
                Some(line) => report
                    .contradictions
                    .push(format!("the framework says no test ran: {line:?}")),
                None if self.live.summary_seen && self.live.total() == 0 => report
                    .contradictions
                    .push("the framework's summary counts 0 tests: nothing ran".into()),
                None if self.live.total() > 0 => report.warnings.push(format!(
                    "the framework counted {} test(s), but none was visible individually (quiet output): only its counts and the exit code back this pass",
                    self.live.total()
                )),
                None => report
                    .warnings
                    .push("no test was observed: this verdict rests on the exit code only".into()),
            }
        } else if !outcomes.is_empty()
            && outcomes
                .values()
                .all(|o| *o == prova_core::model::Outcome::Skipped)
        {
            report.warnings.push(format!(
                "all {} observed tests were skipped: nothing was verified",
                outcomes.len()
            ));
        }
        if self.live.total() > seen {
            report.events.push(EventKind::RunNote {
                message: format!(
                    "the framework reported {} tests but prova saw {seen} individually (condensed or quiet output): \
                     passing tests are not recorded, so flaky detection and durations are incomplete. \
                     Run the framework with verbose output or a JUnit report (`prova adopt --write` sets it up).",
                    self.live.total()
                ),
            });
        }
        // Live events still held by a recognizer (after the file dedupe,
        // which they must survive).
        report.events.splice(0..0, held);
        // Failing tests' output, lowest priority first (later entries win):
        // the failure message, a result file's body, then live output.
        let mut outputs = std::mem::take(&mut self.live.failure_fallback);
        outputs.append(&mut report.test_outputs);
        outputs.append(&mut self.live.failure_output);
        report.test_outputs = outputs;
        let live = self.live.level;
        report.level = match (report.level, live) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        report
    }
}
