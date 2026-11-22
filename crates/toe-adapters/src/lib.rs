//! Framework adapters and output recognizers (ADR 0003, 0013).

pub mod adopt;
pub mod bench;
pub mod inject;
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
    inject: Vec<String>,
    /// Where injected reports are written (the run's private directory).
    inject_dir: Option<std::path::PathBuf>,
    /// Side streams the framework appends to, read live.
    streams: Vec<Stream>,
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
        if toe_core::store::private_dir(&dir).is_err() {
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
            report.artifacts.extend(injected.artifacts);
            report.level = report.level.max(injected.level);
            for st in &self.streams {
                if st.path.exists() {
                    report.artifacts.push(("stream".into(), st.path.clone()));
                }
            }
        }
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
        let file_only = finals.keys().filter(|k| !live_names.contains(*k)).count() as u64;
        let seen = self.live.seen.len() as u64 + self.live.repeats + file_only;
        if self.live.total() > seen {
            report.events.push(EventKind::RunNote {
                message: format!(
                    "the framework reported {} tests but toe saw {seen} individually (condensed or quiet output): \
                     passing tests are not recorded, so flaky detection and durations are incomplete. \
                     Run the framework with verbose output or a JUnit report (`toe adopt` shows how).",
                    self.live.total()
                ),
            });
        }
        // Live events still held by a recognizer (after the file dedupe,
        // which they must survive).
        report.events.splice(0..0, held);
        let live = self.live.level;
        report.level = match (report.level, live) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        report
    }
}
