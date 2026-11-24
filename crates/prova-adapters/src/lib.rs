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

/// Built before the process starts: the result files already on disk are
/// remembered, so only the ones this run writes are read at the end.
pub fn observer_for(spec: &RunSpec, root: &Path) -> Box<dyn Observer> {
    let mut files = results::ResultFiles::new(&spec.results);
    files.snapshot(root);
    Box::new(Ladder {
        live: recognize::Recognizers::default(),
        files,
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
        // Each live result covers one file result, of the same name or one
        // that ends in it: nextest's JUnit says `calc::tests::adds` for its
        // stream's `tests::adds` (round 9: every test counted twice), TRX
        // `Nu.CalcTests.Same` for VSTest's console `Same`. One each: three
        // classes' `Same` are three tests, and a live `Same` covers one of
        // them (round 12 audit: 39 .NET tests counted 47, or merged into
        // fewer). Same pass/fail first; otherwise the live result stands.
        let mut live: Vec<(String, prova_core::model::Outcome, bool)> = self
            .live
            .seen
            .values()
            .map(|(d, o)| {
                let base = d.strip_suffix(" (second result)").unwrap_or(d);
                (base.to_owned(), *o, false)
            })
            .collect();
        // The live key each `live` entry came from (same order).
        let live_keys: Vec<prova_core::model::TestKey> = self
            .live
            .seen
            .iter()
            .map(|(unique, (d, _))| {
                let file = unique
                    .split_once('\u{1f}')
                    .map(|(f, _)| f)
                    .filter(|f| !f.is_empty());
                prova_core::model::TestKey {
                    file: file.map(str::to_owned),
                    path: Vec::new(),
                    name: d.clone(),
                }
            })
            .collect();
        // A bare live name the file qualifies (VSTest's console `Divide`,
        // the TRX's `Ns.CalcTests.Divide`): the file's name is the test's
        // identity. Keeping the bare one made two classes' `Divide` swap
        // histories with the console's order, and a deterministic failure
        // read as flaky (round 13).
        let qualifies =
            |l: &str, d: &str| d != l && !l.contains("::") && d.ends_with(&format!(".{l}"));
        let mut superseded: Vec<(usize, usize)> = Vec::new();
        let covers = |l: &str, d: &str| {
            l == d || d.ends_with(&format!("::{l}")) || d.ends_with(&format!(".{l}"))
        };
        // Matched in passes over all file results (exact outcome, then
        // pass/fail, then any), not greedily: a passing `B.Same` must not
        // take the live failing `Same` that `C.Same` is.
        let files: Vec<(usize, String, prova_core::model::Outcome)> = report
            .events
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e {
                EventKind::TestFinished { test, outcome, .. } => {
                    Some((i, test.display(), *outcome))
                }
                _ => None,
            })
            .collect();
        let mut covered = vec![false; report.events.len()];
        // A failure on either side wins (round 13: live pass, file fail
        // recorded as passed while the framework said 1 failed).
        let mut disagreed: Vec<(usize, String)> = Vec::new();
        type Same = fn(prova_core::model::Outcome, prova_core::model::Outcome) -> bool;
        let passes: [Same; 3] = [
            |a, b| a == b,
            |a, b| a.is_failure() == b.is_failure(),
            |_, _| true,
        ];
        for same in passes {
            for (i, d, o) in &files {
                if covered[*i] {
                    continue;
                }
                if let Some(j) = live
                    .iter()
                    .position(|(l, lo, used)| !*used && same(*lo, *o) && covers(l, d))
                {
                    let l = &mut live[j];
                    l.2 = true;
                    if qualifies(&l.0, d) {
                        // The file's result stays; the live one is replaced.
                        superseded.push((j, *i));
                    } else if o.is_failure() && !l.1.is_failure() {
                        // The file's failure stays as an event.
                        disagreed.push((*i, d.clone()));
                    } else {
                        covered[*i] = true;
                    }
                }
            }
        }
        // The engine drops a second result for a test already finished as
        // passed, so the file's failure is recorded under its own name.
        for (i, _) in &disagreed {
            if let EventKind::TestFinished { test, .. } = &mut report.events[*i] {
                test.name = format!("{} (result file)", test.name);
            }
        }
        let supersede: Vec<EventKind> = superseded
            .iter()
            .filter_map(|(j, i)| match &report.events[*i] {
                EventKind::TestFinished { test, .. } => Some(EventKind::TestSuperseded {
                    test: live_keys[*j].clone(),
                    by: test.clone(),
                }),
                _ => None,
            })
            .collect();
        let mut i = 0;
        report.events.retain(|_| {
            i += 1;
            !covered[i - 1]
        });
        // After the file results they name (the store links the two).
        report.events.extend(supersede);
        for (_, d) in disagreed {
            report.contradictions.push(format!(
                "{d}: the live output said it passed but the result file says it failed (the failure is recorded)"
            ));
        }
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
        let file_only = report
            .events
            .iter()
            .filter(|e| matches!(e, EventKind::TestFinished { .. }))
            .count() as u64;
        // A superseded live result and the file's one are one test.
        let replaced = report
            .events
            .iter()
            .filter(|e| matches!(e, EventKind::TestSuperseded { .. }))
            .count() as u64;
        let seen =
            (self.live.seen.len() as u64 + self.live.repeats + file_only).saturating_sub(replaced);
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
        // nextest counts a test that exited 0 early as passed; its captured
        // output shows libtest never reported it (round 9).
        report.not_passed = self.live.harness_died.clone();
        let wall_ms = prova_core::ids::now_ms().saturating_sub(started_ms) as f64;
        if self.live.reported_secs * 1000.0 > wall_ms + 300.0 {
            report.warnings.push(format!(
                "the framework's own summaries say the tests took {:.1}s, but the whole run took {:.1}s: this output was replayed, not produced now{}",
                self.live.reported_secs,
                wall_ms / 1000.0,
                prova_core::engine::NO_EVIDENCE
            ));
        }
        if let Some(what) = self.live.harness_stopped.first() {
            report.contradictions.push(format!(
                "{what}: the test process ended (a test that exits it early?) and the tests after it never ran"
            ));
        }
        if let Some(t) = self.live.harness_died.first() {
            report.contradictions.push(format!(
                "{} test(s) passed by exit code but their test harness never reported a result (a test that exits the process early?), e.g. {t}",
                self.live.harness_died.len()
            ));
        }
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
        // Passing tests seen one by one: the output was not quiet, so verbose
        // output is not the advice (round 12 audit: a collection error or a
        // subtest the framework counts on its own line).
        let passes_seen = outcomes
            .values()
            .any(|o| *o == prova_core::model::Outcome::Passed);
        if self.live.total() > seen && passes_seen {
            report.events.push(EventKind::RunNote {
                message: format!(
                    "the framework counted {} tests but prova saw {seen} individually: the rest left no \
                     per-test result prova reads (a collection or setup error?); `prova output` shows the run",
                    self.live.total()
                ),
            });
        } else if self.live.total() > seen {
            report.events.push(EventKind::RunNote {
                message: format!(
                    "the framework reported {} tests but prova saw {seen} individually (condensed or quiet output): \
                     passing tests are not recorded, so flaky detection and durations are incomplete. \
                     Run the framework with verbose output or a JUnit report (`prova adopt` proposes a suite that does).",
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

#[cfg(test)]
mod tests {
    use super::*;
    use prova_core::model::Outcome;

    /// Live pass, file failure: the failure is recorded (round 13: pytest
    /// said 1 failed, prova recorded a pass). Temp dir: the file is the point.
    #[test]
    fn a_failure_in_the_result_file_beats_a_live_pass() {
        let root = std::env::temp_dir().join(format!("prova-ladder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("junit.xml"),
            r#"<testsuite><testcase classname="tests.t" name="test_a"><failure message="boom"/></testcase></testsuite>"#,
        )
        .unwrap();
        let mut ladder = Ladder {
            live: recognize::Recognizers::default(),
            files: results::ResultFiles::new(&[]),
            inject: Vec::new(),
            inject_dir: None,
            streams: Vec::new(),
            expect_tests: true,
        };
        let live = ladder.on_line("tests/t.py::test_a PASSED   [100%]", 0);
        assert!(matches!(
            live.last(),
            Some(EventKind::TestFinished {
                outcome: Outcome::Passed,
                ..
            })
        ));
        let report = ladder.on_exit(&root, 0);
        let _ = std::fs::remove_dir_all(&root);
        let failed: Vec<_> = report
            .events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    EventKind::TestFinished {
                        outcome: Outcome::Failed,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(failed.len(), 1, "{:?}", report.events);
        assert!(
            report.contradictions.iter().any(|c| c.contains("test_a")),
            "{:?}",
            report.contradictions
        );
    }
}
