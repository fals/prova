//! Levels 2 and 3: per-test events from live output (ADR 0013).
//!
//! Each recognizer looks at one line at a time and is cheap. They run on
//! every run; lines that match no known format are ignored. Structured
//! streams are level 3 (native): Go's `test2json`, libtest JSON (`cargo test
//! -- --format json`, nextest `--message-format libtest-json`, and prova's own
//! Vitest reporter, which writes the same shape), and pytest-reportlog. The
//! others parse human-readable output (level 2). The same key is built for a
//! test whichever format reports it, so a test seen in two streams counts once.

use std::collections::{BTreeMap, BTreeSet};

use prova_core::model::{EventKind, Level, Outcome, TestKey};

const MAX_MESSAGE_LINES: usize = 12;

#[derive(Default)]
pub struct Recognizers {
    /// Highest level seen so far.
    pub level: Option<Level>,
    /// Final outcome of every test reported so far:
    /// unique key -> (display name, outcome).
    pub seen: BTreeMap<String, (String, Outcome)>,
    /// Vitest: current test file header, and a failure held for its message.
    vitest_file: Option<String>,
    vitest_pending: Option<EventKind>,
    /// VSTest: collecting the "Error Message:" block of the held failure.
    dotnet_message: Option<Vec<String>>,
    /// libtest: the test binary currently running ("Running tests/x.rs (...)").
    libtest_binary: Option<String>,
    started: BTreeSet<String>,
    /// Captured output per running test (Go), used as failure message.
    go_output: BTreeMap<String, Vec<String>>,
    /// Go -v: test currently printing output.
    go_current: Option<String>,
    /// Tests the framework says it ran (from its own summary lines).
    pub reported_total: u64,
    /// Totals from structured suite events (libtest JSON). When present they
    /// replace the text totals, which a runner may echo from inside its
    /// captured output (nextest prints libtest's "test result:" lines).
    pub structured_total: Option<u64>,
    /// Failures the framework's own summaries report (ADR 0017).
    pub reported_failed: u64,
    /// structured_total's failures (libtest JSON suite events).
    structured_failed: Option<u64>,
    /// A framework summary line was seen (its counts are meaningful).
    pub summary_seen: bool,
    /// A line in which the framework says that no test ran.
    pub no_tests_line: Option<String>,
    /// A line where the build tool says the build failed or a test process
    /// crashed (Maven `BUILD FAILURE`, Surefire's crashed fork).
    pub build_failed_line: Option<String>,
    /// `go test` package result lines in text output ("ok  \tpkg", "FAIL\tpkg").
    pub go_package_lines: u64,
    /// `=== RUN` lines: `go test -v`, which shows passing tests too.
    pub go_run_lines: u64,
    /// A native stream reports pytest / Vitest (opt-in recipe): their
    /// human output is then ignored, so one source decides each test.
    pub pytest_stream: bool,
    pub vitest_stream: bool,
    /// nextest: a FAIL line held while its captured output follows.
    nextest_pending: Option<(TestKey, Option<u64>, Vec<String>)>,
    /// A passing test's captured output (`--success-output final`): did
    /// libtest start, and did it report a result?
    nextest_pass_output: Option<(String, bool, bool)>,
    /// Tests nextest passed whose harness never reported a result: the
    /// test process exited early with code 0 (round 9: `exit(0)`).
    pub harness_died: Vec<String>,
    /// libtest's `running N tests` with how many results followed: a
    /// binary that stops before its `test result:` summary died mid-run.
    libtest_running: Option<(u64, u64)>,
    /// Test binaries that stopped early: "after K of N tests".
    pub harness_stopped: Vec<String>,
    /// Text results held while an injected stream is expected.
    held_text_events: Vec<EventKind>,
    /// Parsing into `held_text_events`: a held result is a fallback, not a
    /// report, so it must not mark its test seen or started, or the
    /// stream's own result is dropped as a duplicate (round 12).
    holding: bool,
    /// The longest time one of the framework's own summaries says its tests
    /// took (libtest `finished in`, go `ok pkg 1.5s`, pytest `in 1.50s`;
    /// packages may run in parallel, so not a sum): more than the
    /// whole run means the output was replayed, not produced (round 11).
    pub reported_secs: f64,
    /// libtest JSON: tests started in the current suite (one test binary)
    /// and not finished yet.
    libtest_open: Vec<TestKey>,
    /// `ignored` events seen since the suite started: those are already
    /// accounted for in the suite's end count.
    libtest_ignored_seen: usize,
    /// Failure messages, kept as a test's output when nothing richer is
    /// known (lowest priority).
    pub failure_fallback: Vec<(TestKey, String)>,
    /// libtest: the failing test whose "---- name stdout ----" block is
    /// being collected, and its lines.
    libtest_block: Option<(TestKey, Vec<String>)>,
    /// Failing tests' full output from structured streams (ADR 0016).
    pub failure_output: Vec<(TestKey, String)>,
    /// libtest results for a test already seen in this run (the same test
    /// binary executed twice); they count toward `reported_total`.
    pub repeats: u64,
    cache_noted: bool,
}

fn lift(level: &mut Option<Level>, l: Level) {
    if level.is_none_or(|cur| l > cur) {
        *level = Some(l);
    }
}

fn message(lines: &[String]) -> Option<String> {
    let kept: Vec<&str> = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("=== ") && !l.starts_with("--- "))
        .collect();
    if kept.is_empty() {
        return None;
    }
    let tail = &kept[kept.len().saturating_sub(MAX_MESSAGE_LINES)..];
    Some(tail.join("\n"))
}

impl Recognizers {
    fn started(&mut self, key: TestKey, out: &mut Vec<EventKind>) {
        let d = key.unique();
        if self.holding {
            out.push(EventKind::TestStarted { test: key });
            return;
        }
        // A stream can report a start after another source reported the end
        // (pytest -v text before its reportlog line): never restart a test.
        if self.seen.contains_key(&d) {
            return;
        }
        if self.started.insert(d) {
            out.push(EventKind::TestStarted { test: key });
        }
    }

    /// A later attempt of a test that failed (a retry): its result is the
    /// test's new outcome (ADR 0024: the last attempt decides).
    fn retry(
        &mut self,
        key: TestKey,
        attempt: u32,
        event: &str,
        v: &serde_json::Value,
        out: &mut Vec<EventKind>,
    ) {
        let outcome = match event {
            "ok" => Outcome::Passed,
            "failed" => Outcome::Failed,
            _ => return, // "started": the retry's start is implied
        };
        let duration_ms = v["exec_time"].as_f64().map(|s| (s * 1000.0).round() as u64);
        let message = (outcome == Outcome::Failed)
            .then(|| v["stdout"].as_str().unwrap_or("").to_owned())
            .filter(|m| !m.is_empty());
        self.seen.insert(key.unique(), (key.display(), outcome));
        out.push(EventKind::TestFinished {
            test: key,
            attempt,
            outcome,
            duration_ms,
            message,
            source: Level::Native,
        });
    }

    fn finished(
        &mut self,
        key: TestKey,
        outcome: Outcome,
        duration_ms: Option<u64>,
        message: Option<String>,
        level: Level,
        out: &mut Vec<EventKind>,
    ) {
        let d = key.unique();
        if self.holding {
            out.push(EventKind::TestFinished {
                test: key,
                attempt: 1,
                outcome,
                duration_ms,
                message,
                source: level,
            });
            return;
        }
        if self.seen.contains_key(&d) {
            // Already reported: a pytest summary line, or (libtest, level
            // Recognized with a binary known) the same binary run again.
            if self.libtest_binary.is_some() && key.file == self.libtest_binary {
                self.repeats += 1;
            }
            return;
        }
        if outcome.is_failure()
            && let Some(m) = &message
        {
            self.failure_fallback.push((key.clone(), m.clone()));
        }
        self.seen.insert(d, (key.display(), outcome));
        lift(&mut self.level, level);
        out.push(EventKind::TestFinished {
            test: key,
            attempt: 1,
            outcome,
            duration_ms,
            message,
            source: level,
        });
    }

    /// Framework summary lines: "test result: ok. 4 passed; 1 failed; 0 ignored"
    /// (libtest) or "1 failed, 4 passed in 1.52s" (pytest).
    fn totals(&mut self, line: &str) {
        let t = line.trim().trim_matches('=').trim();
        self.no_tests(t);
        self.build_failed(t);
        if (t.starts_with("Failed!") || t.starts_with("Passed!")) && t.contains("Total:") {
            self.summary_seen = true;
            let field = |name: &str| {
                t.split(',')
                    .find_map(|p| {
                        p.trim()
                            .trim_start_matches("Failed!")
                            .trim_start_matches("Passed!")
                            .trim()
                            .trim_start_matches('-')
                            .trim()
                            .strip_prefix(name)
                    })
                    .and_then(|v| v.trim().parse::<u64>().ok())
            };
            if let Some(total) = field("Total:") {
                self.reported_total += total;
            }
            self.reported_failed += field("Failed:").unwrap_or(0);
            return;
        }
        // Jest "Tests:       1 failed, 4 passed, 5 total"; Vitest
        // "Tests  1 failed | 4 passed (5)".
        if let Some(rest) = t
            .strip_prefix("Tests:")
            .or_else(|| t.strip_prefix("Tests "))
        {
            let mut seen_any = false;
            for part in rest.split([',', '|']) {
                let mut w = part.split_whitespace();
                if let (Some(num), Some(word)) = (w.next(), w.next())
                    && let Ok(v) = num.parse::<u64>()
                {
                    seen_any = true;
                    if word == "failed" {
                        self.reported_failed += v;
                    }
                    if word == "total" {
                        self.reported_total += v;
                    }
                }
            }
            // Vitest puts the total in parentheses at the end.
            if let Some(total) = rest
                .rsplit_once('(')
                .and_then(|(_, n)| n.trim_end_matches(')').trim().parse::<u64>().ok())
            {
                self.reported_total += total;
            }
            self.summary_seen |= seen_any;
            return;
        }
        let body = match t.strip_prefix("test result: ") {
            Some(rest) => rest.split_once(". ").map(|(_, b)| b).unwrap_or(""),
            None if t.contains(" in ")
                && [
                    " passed",
                    " failed",
                    " error",
                    " skipped",
                    " deselected",
                    "no tests ran",
                ]
                .iter()
                .any(|w| t.contains(w)) =>
            {
                t
            }
            None => return,
        };
        self.summary_seen = true;
        let mut n = 0;
        for part in body.split([';', ',']) {
            let mut w = part.split_whitespace();
            if let (Some(num), Some(word)) = (w.next(), w.next())
                && let Ok(v) = num.parse::<u64>()
                && [
                    "passed", "failed", "ignored", "skipped", "error", "errors", "xfailed",
                    "xpassed",
                ]
                .contains(&word)
            {
                n += v;
                if ["failed", "error", "errors"].contains(&word) {
                    self.reported_failed += v;
                }
            }
        }
        self.reported_total += n;
    }

    /// Framework messages that say no test ran at all.
    fn build_failed(&mut self, t: &str) {
        const PHRASES: &[&str] = &[
            "BUILD FAILURE",                                            // maven
            "Crashed tests:",                                           // surefire
            "The forked VM terminated without properly saying goodbye", // surefire
            "There was an error in the forked process",                 // surefire
            "Build failures were ignored",                              // maven -fn
        ];
        if self.build_failed_line.is_none() && PHRASES.iter().any(|p| t.contains(p)) {
            self.build_failed_line = Some(t.chars().take(200).collect());
        }
    }

    fn no_tests(&mut self, t: &str) {
        const PHRASES: &[&str] = &[
            "no tests ran",                              // pytest
            "/ 0 selected",                              // pytest, everything deselected
            "collected 0 items",                         // pytest
            "[no test files]",                           // go
            "No tests found",                            // jest
            " 0 tests run:",                             // nextest, selection matched nothing
            "No test files found",                       // vitest
            "No tests to run",                           // maven surefire
            "No test is available in",                   // vstest
            "No test matches the given testcase filter", // vstest
        ];
        if self.no_tests_line.is_none() && PHRASES.iter().any(|p| t.contains(p)) {
            self.no_tests_line = Some(t.chars().take(200).collect());
        }
    }

    /// Failures the framework reported (structured streams win over text).
    pub fn failed_reported(&self) -> u64 {
        self.structured_failed.unwrap_or(self.reported_failed)
    }

    pub fn line(&mut self, line: &str) -> Vec<EventKind> {
        let mut out = Vec::new();
        // nextest: output captured under a FAIL line belongs to that test
        // (its echoed libtest summary must not count as the run's totals).
        // JSON events (nextest's own, at the end of each binary) are never
        // part of a test's captured output.
        {
            let t = line.trim();
            let secs = |w: &str| {
                w.trim_end_matches(['s', ',', '='])
                    .trim()
                    .parse::<f64>()
                    .ok()
            };
            if t.starts_with("test result:")
                && let Some(i) = t.find("finished in ")
            {
                self.reported_secs = self.reported_secs.max(secs(&t[i + 12..]).unwrap_or(0.0));
            } else if let Some(rest) = line.strip_prefix("ok  \t")
                && let Some(last) = rest.rsplit('\t').next()
            {
                self.reported_secs = self.reported_secs.max(secs(last).unwrap_or(0.0));
            } else if t.starts_with('=')
                && t.ends_with('=')
                && let Some(i) = t.rfind(" in ")
            {
                self.reported_secs = self.reported_secs.max(
                    t[i + 4..]
                        .split_whitespace()
                        .next()
                        .and_then(secs)
                        .unwrap_or(0.0),
                );
            }
        }
        // Plain libtest: "running N tests", N results, "test result:". A
        // binary that stops in between died (round 11: `exit(0)` in a test
        // under `--test-threads=1` gave a verified pass). nextest's own
        // per-test output is handled apart.
        if self.nextest_pass_output.is_none() && self.nextest_pending.is_none() {
            let t = line.trim();
            if let Some(n) = t
                .strip_prefix("running ")
                .and_then(|r| r.split_whitespace().next())
                .and_then(|n| n.parse::<u64>().ok())
                .filter(|_| t.ends_with(" tests") || t.ends_with(" test"))
            {
                self.close_libtest_running();
                if n > 0 {
                    self.libtest_running = Some((n, 0));
                }
            } else if t.starts_with("test result:") {
                self.libtest_running = None;
            } else if t.starts_with("test ")
                && (t.ends_with(" ok") || t.contains(" ... FAILED") || t.contains(" ... ignored"))
                && let Some((_, seen)) = self.libtest_running.as_mut()
            {
                *seen += 1;
            }
        }
        // A passing test's own output: libtest says "running 1 test" first
        // and "test result:" last; an `exit(0)` inside the test skips it.
        if let Some((_, running, result)) = self.nextest_pass_output.as_mut()
            && nextest_status(line).is_none()
            && !line.starts_with('{')
        {
            let t = line.trim();
            if t.starts_with("running 1 test") {
                *running = true;
            } else if t.starts_with("test result:") {
                *result = true;
            } else if t.starts_with("Summary [") {
                self.close_pass_output();
            }
        }
        if self.nextest_pending.is_some() && !line.starts_with('{') {
            let t = line.trim_start();
            // cargo's own lines end the block: `cargo test --doc` after
            // nextest printed its doctests into the held failure (round 11:
            // a failing doctest was lost when a unit test failed too).
            let cargo_line = [
                "Doc-tests ",
                "Running ",
                "Compiling ",
                "Finished ",
                "error: test failed",
            ]
            .iter()
            .any(|p| t.starts_with(p));
            if nextest_status(line).is_none()
                && !t.starts_with("────")
                && !t.starts_with("Summary [")
                && !cargo_line
            {
                if let Some((_, _, lines)) = self.nextest_pending.as_mut() {
                    lines.push(line.to_owned());
                }
                return out;
            }
            self.flush_nextest(&mut out);
        }
        if self.nextest(line, &mut out) {
            return out;
        }
        self.totals(line);
        if self.dotnet_message.is_some() {
            if self.dotnet_collect(line) {
                return out;
            }
        } else if self.vitest_pending.is_some() && !line.trim_start().starts_with("→ ") {
            if self.vitest_stream {
                let mut held = std::mem::take(&mut self.held_text_events);
                self.flush_vitest(&mut held);
                self.held_text_events = held;
            } else {
                self.flush_vitest(&mut out);
            }
        }
        if line.starts_with("ok  \t") || line.starts_with("FAIL\t") {
            self.go_package_lines += 1;
        }
        // Go replays cached results without running tests: say so once.
        if !self.cache_noted
            && line.contains("(cached)")
            && (line.starts_with("ok") || line.contains("\"Output\":\"ok"))
        {
            self.cache_noted = true;
            out.push(EventKind::RunNote {
                message: "go replayed cached test results: these tests did not run again; add -count=1 to the suite command".into(),
            });
        }
        if line.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if v.get("Action").is_some() {
                    self.go_json(&v, &mut out);
                } else if v.get("$report_type").is_some() {
                    self.reportlog(&v, &mut out);
                } else if v.get("type").is_some() && v.get("event").is_some() {
                    self.libtest_json(&v, &mut out);
                }
            }
        } else {
            // With a stream injected, its events win; the framework's text
            // is held, and used only if the stream never arrives (round 11:
            // a suite whose command did not load the reporter ran blind).
            let streamed = self.level >= Some(Level::Native);
            let mut held = std::mem::take(&mut self.held_text_events);
            let _ = self.libtest(line, &mut out)
                || self.go_verbose(line, &mut out)
                || (self.pytest_stream && !streamed && self.hold(|r| r.pytest(line, &mut held)))
                || (!self.pytest_stream && self.pytest(line, &mut out))
                || self.dotnet(line, &mut out)
                || (self.vitest_stream && !streamed && self.hold(|r| r.vitest(line, &mut held)))
                || (!self.vitest_stream && self.vitest(line, &mut out))
                || self.tap(line, &mut out);
            self.held_text_events = held;
        }
        out
    }

    fn hold(&mut self, f: impl FnOnce(&mut Self) -> bool) -> bool {
        self.holding = true;
        let r = f(self);
        self.holding = false;
        r
    }

    // ------------------------------------------------------------- go json

    fn go_json(&mut self, v: &serde_json::Value, out: &mut Vec<EventKind>) {
        let (Some(action), Some(pkg)) = (v["Action"].as_str(), v["Package"].as_str()) else {
            return;
        };
        let test = v["Test"].as_str();
        let key = |name: &str| TestKey::new(vec![pkg.to_owned()], name);
        let elapsed = v["Elapsed"].as_f64().map(|s| (s * 1000.0).round() as u64);
        lift(&mut self.level, Level::Native);
        match (action, test) {
            ("run", Some(t)) => self.started(key(t), out),
            ("output", Some(t)) => {
                let o = v["Output"].as_str().unwrap_or("").trim_end().to_owned();
                self.go_output
                    .entry(format!("{pkg}\0{t}"))
                    .or_default()
                    .push(o);
            }
            ("output", None) => {
                let o = v["Output"].as_str().unwrap_or("").trim_end().to_owned();
                self.go_output
                    .entry(format!("{pkg}\0"))
                    .or_default()
                    .push(o);
            }
            (a @ ("pass" | "fail" | "skip"), Some(t)) => {
                let outcome = match a {
                    "pass" => Outcome::Passed,
                    "fail" => Outcome::Failed,
                    _ => Outcome::Skipped,
                };
                let lines = self
                    .go_output
                    .remove(&format!("{pkg}\0{t}"))
                    .unwrap_or_default();
                let msg = if outcome == Outcome::Failed {
                    self.failure_output.push((key(t), lines.join("\n")));
                    message(&lines)
                } else {
                    None
                };
                self.finished(key(t), outcome, elapsed, msg, Level::Native, out);
            }
            ("fail", None) => {
                // Package failed without any failing test: build error or crash.
                let prefix = format!("{pkg}\0");
                let any_failed = self
                    .seen
                    .values()
                    .any(|(d, o)| d.starts_with(&format!("{pkg}::")) && o.is_failure());
                let lines = self.go_output.remove(&prefix).unwrap_or_default();
                if !any_failed {
                    self.finished(
                        key("(package)"),
                        Outcome::Errored,
                        elapsed,
                        message(&lines),
                        Level::Native,
                        out,
                    );
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------- libtest json

    /// libtest's JSON events (`{"type": "test", "event": "ok", "name": ..}`).
    /// Names: `tests::adds` (cargo; the binary comes from cargo's "Running"
    /// line), `binary-id$tests::adds` (nextest), or a `file` field plus a
    /// `describe > test` name (prova's Vitest reporter).
    fn libtest_json(&mut self, v: &serde_json::Value, out: &mut Vec<EventKind>) {
        let (Some(kind), Some(event)) = (v["type"].as_str(), v["event"].as_str()) else {
            return;
        };
        if kind == "suite" {
            if event == "started" {
                self.libtest_open.clear();
                self.libtest_ignored_seen = 0;
            } else {
                // nextest reports ignored tests as started, then only counts
                // them in the suite's end event: those still open are them.
                // nextest runs tests in parallel: a test still running when
                // another batch ends is open too (round 9: a 1.5 s test that
                // passed was recorded as skipped). Only ignored tests not yet
                // reported by their own event are the open ones.
                let ignored = (v["ignored"].as_u64().unwrap_or(0) as usize)
                    .saturating_sub(std::mem::take(&mut self.libtest_ignored_seen));
                if ignored > 0
                    && !self.libtest_open.is_empty()
                    && self.libtest_open.len() <= ignored
                {
                    for key in std::mem::take(&mut self.libtest_open) {
                        self.finished(key, Outcome::Skipped, None, None, Level::Native, out);
                    }
                }
                lift(&mut self.level, Level::Native);
                let n = ["passed", "failed", "ignored", "measured"]
                    .iter()
                    .filter_map(|k| v[*k].as_u64())
                    .sum::<u64>();
                *self.structured_total.get_or_insert(0) += n;
                *self.structured_failed.get_or_insert(0) += v["failed"].as_u64().unwrap_or(0);
                self.summary_seen = true;
            }
            return;
        }
        let Some(name) = v["name"].as_str().filter(|_| kind == "test") else {
            return;
        };
        // nextest names a retry `test#2`: the same test, another attempt
        // (round 11: retries counted as a second test that failed).
        let (name, attempt) = match name.rsplit_once('#') {
            Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
                (base, n.parse::<u32>().unwrap_or(1))
            }
            _ => (name, 1),
        };
        let key = if let Some(file) = v["file"].as_str() {
            TestKey {
                file: None,
                path: vec![file.to_owned()],
                name: name.to_owned(),
            }
        } else if let Some((binary, test)) = name.split_once('$') {
            let (path, leaf) = split_rust_path(test);
            TestKey {
                file: Some(nextest_binary(binary)),
                path,
                name: leaf,
            }
        } else {
            self.libtest_key(name)
        };
        lift(&mut self.level, Level::Native);
        if attempt > 1 {
            return self.retry(key, attempt, event, v, out);
        }
        if event == "started" {
            self.libtest_open.push(key.clone());
            return self.started(key, out);
        }
        self.libtest_open.retain(|k| k != &key);
        if event == "ignored" {
            self.libtest_ignored_seen += 1;
        }
        let outcome = match event {
            "ok" => Outcome::Passed,
            "failed" => Outcome::Failed,
            "ignored" => Outcome::Skipped,
            _ => return, // "timeout" is a warning; the test is still running
        };
        let duration = v["exec_time"].as_f64().map(|s| (s * 1000.0).round() as u64);
        let msg = if outcome == Outcome::Failed {
            let text = v["message"].as_str().or(v["stdout"].as_str()).unwrap_or("");
            let full = match v["stack"].as_str() {
                Some(stack) => format!("{text}\n{stack}"),
                None => text.to_owned(),
            };
            if !full.trim().is_empty() {
                self.failure_output
                    .push((key.clone(), full.trim().to_owned()));
            }
            message(&text.lines().map(str::to_owned).collect::<Vec<_>>())
        } else {
            None
        };
        self.finished(key, outcome, duration, msg, Level::Native, out);
    }

    // ------------------------------------------------------------- pytest-reportlog

    /// pytest-reportlog: one `TestReport` per phase (setup, call,
    /// teardown). Setup starts the test (or ends it when it fails or skips),
    /// call ends it.
    fn reportlog(&mut self, v: &serde_json::Value, out: &mut Vec<EventKind>) {
        if v["$report_type"] != "TestReport" {
            return;
        }
        let (Some(id), Some(when), Some(result)) = (
            v["nodeid"].as_str(),
            v["when"].as_str(),
            v["outcome"].as_str(),
        ) else {
            return;
        };
        lift(&mut self.level, Level::Native);
        let key = pytest_key(id);
        let outcome = match (when, result) {
            ("setup", "passed") => return self.started(key, out),
            ("setup", "failed") => Outcome::Errored,
            ("call", "passed") => Outcome::Passed,
            ("call", "failed") => Outcome::Failed,
            (_, "skipped") => Outcome::Skipped,
            ("teardown", "failed") => Outcome::Errored,
            _ => return,
        };
        let longrepr = &v["longrepr"];
        if outcome.is_failure() {
            let text = reportlog_text(longrepr);
            if !text.is_empty() {
                self.failure_output.push((key.clone(), text));
            }
        }
        let msg = if outcome.is_failure() {
            longrepr["reprcrash"]["message"]
                .as_str()
                .or(longrepr.as_str())
                .map(|m| m.trim().to_owned())
        } else {
            None
        };
        let duration = v["duration"].as_f64().map(|s| (s * 1000.0).round() as u64);
        self.finished(key, outcome, duration, msg, Level::Native, out);
    }

    // ------------------------------------------------------------- go -v

    fn go_verbose(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        let t = line.trim_start();
        if let Some(name) = t.strip_prefix("=== RUN") {
            self.go_run_lines += 1;
            let name = name.trim().to_owned();
            self.go_current = Some(name.clone());
            self.started(TestKey::new(Vec::new(), name), out);
            return true;
        }
        for (prefix, outcome) in [
            ("--- PASS: ", Outcome::Passed),
            ("--- FAIL: ", Outcome::Failed),
            ("--- SKIP: ", Outcome::Skipped),
        ] {
            if let Some(rest) = t.strip_prefix(prefix) {
                let (name, dur) = match rest.rsplit_once(" (") {
                    Some((n, d)) => (
                        n.trim(),
                        d.trim_end_matches(')')
                            .trim_end_matches('s')
                            .parse::<f64>()
                            .ok(),
                    ),
                    None => (rest.trim(), None),
                };
                let lines = self
                    .go_output
                    .remove(&format!("v\0{name}"))
                    .unwrap_or_default();
                let msg = if outcome == Outcome::Failed {
                    message(&lines)
                } else {
                    None
                };
                self.finished(
                    TestKey::new(Vec::new(), name),
                    outcome,
                    dur.map(|s| (s * 1000.0).round() as u64),
                    msg,
                    Level::Recognized,
                    out,
                );
                return true;
            }
        }
        // Indented output belongs to the test that is running.
        if line.starts_with("    ")
            && let Some(cur) = &self.go_current
        {
            self.go_output
                .entry(format!("v\0{cur}"))
                .or_default()
                .push(line.to_owned());
        }
        false
    }

    // ------------------------------------------------------------- libtest

    fn libtest_key(&self, name: &str) -> TestKey {
        let (path, leaf) = split_rust_path(name);
        TestKey {
            file: self.libtest_binary.clone(),
            path,
            name: leaf,
        }
    }

    fn libtest(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        // A failing test's captured output: "---- path::name stdout ----"
        // up to the next block or the "failures:" list.
        if let Some(name) = line.strip_prefix("---- ").and_then(|r| {
            r.strip_suffix(" stdout ----")
                .or_else(|| r.strip_suffix(" stderr ----"))
        }) {
            self.close_libtest_block();
            self.libtest_block = Some((self.libtest_key(name.trim()), Vec::new()));
            return true;
        }
        if self.libtest_block.is_some() {
            let t = line.trim();
            if t == "failures:" || t.starts_with("test result:") {
                self.close_libtest_block();
            } else if let Some((_, lines)) = self.libtest_block.as_mut() {
                lines.push(line.to_owned());
                return true;
            }
        }
        // "     Running tests/live.rs (target/debug/deps/live-abc)" or
        // "   Doc-tests prova_core": tests of different binaries may share names.
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("Running ") {
            self.libtest_binary = Some(cargo_binary(rest));
            return true;
        }
        if let Some(rest) = t.strip_prefix("Doc-tests ") {
            self.libtest_binary = Some(format!("doctests {}", rest.trim()));
            return true;
        }
        // `cargo test --quiet` prints only failures: "path::name --- FAILED"
        // (a doctest's name has spaces: "src/lib.rs - div (line 7)", round 9)
        if let Some(name) = line.strip_suffix(" --- FAILED")
            && !name.is_empty()
            && (!name.contains(char::is_whitespace) || is_doctest_name(name))
        {
            let key = self.libtest_key(name);
            self.finished(key, Outcome::Failed, None, None, Level::Recognized, out);
            return true;
        }
        // `cargo test --quiet` prints progress dots with no newline, so a
        // later "test ..." message can follow them on the same line.
        let line = match line.find("test ") {
            Some(i) if i > 0 && line[..i].chars().all(|c| matches!(c, '.' | 'F' | 'i')) => {
                &line[i..]
            }
            _ => line,
        };
        let Some(rest) = line.strip_prefix("test ") else {
            return false;
        };
        if let Some((name, status)) = rest.rsplit_once(" ... ") {
            let outcome = match status.trim() {
                "ok" => Outcome::Passed,
                "FAILED" => Outcome::Failed,
                s if s.starts_with("ignored") => Outcome::Skipped,
                _ => return false,
            };
            let key = self.libtest_key(name.trim());
            self.finished(key, outcome, None, None, Level::Recognized, out);
            return true;
        }
        if let Some(name) = rest.strip_suffix(" has been running for over 60 seconds") {
            out.push(EventKind::TestSlow {
                test: self.libtest_key(name.trim()),
                elapsed_ms: 60_000,
                budget_ms: 60_000,
                budget_from: prova_core::model::BudgetFrom::Runner,
            });
            return true;
        }
        false
    }

    // ------------------------------------------------------------- pytest

    fn pytest(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        // Verbose: "tests/test_x.py::test_a PASSED  [ 50%]"
        // Summary: "FAILED tests/test_x.py::test_b - AssertionError: boom"
        for (word, outcome) in [("FAILED", Outcome::Failed), ("ERROR", Outcome::Errored)] {
            if let Some(rest) = line.strip_prefix(word).and_then(|r| r.strip_prefix(' '))
                && rest.contains("::")
            {
                let (id, msg) = match rest.split_once(" - ") {
                    Some((i, m)) => (i.trim(), Some(m.trim().to_owned())),
                    None => (rest.trim(), None),
                };
                self.finished(pytest_key(id), outcome, None, msg, Level::Recognized, out);
                return true;
            }
        }
        let Some((id, rest)) = line.split_once(' ') else {
            return false;
        };
        if !id.contains("::") || !id.contains(".py") {
            return false;
        }
        let outcome = match rest.split_whitespace().next() {
            Some("PASSED") | Some("XFAIL") => Outcome::Passed,
            Some("FAILED") | Some("XPASS") => Outcome::Failed,
            Some("SKIPPED") => Outcome::Skipped,
            Some("ERROR") => Outcome::Errored,
            _ => return false,
        };
        // Failures are recorded now to keep first-failure fast; the message
        // only appears later in the summary section.
        self.finished(pytest_key(id), outcome, None, None, Level::Recognized, out);
        true
    }

    // ------------------------------------------------------------- vitest

    /// Verbose reporter: " ✓ src/a.test.ts > group > name 2ms", "×" failed,
    /// "↓" skipped.
    /// Vitest reporters. Verbose: "✓ src/a.test.ts > group > name 2ms".
    /// Default: a file header "❯ src/a.test.ts (5 tests | 1 failed)" then
    /// "✓ group > name 1ms", "× group > name", "→ message". Keys match
    /// Vitest's JUnit report: path [file], name "group > name".
    fn vitest(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        let t = line.trim_start();
        // The message of the previous failure, if this line carries it.
        if let Some(msg) = t.strip_prefix("→ ")
            && let Some(EventKind::TestFinished { message, .. }) = self.vitest_pending.as_mut()
        {
            if message.is_none() {
                *message = Some(msg.trim().to_owned());
            }
            return true;
        }
        self.flush_vitest(out);
        let (outcome, rest) = if let Some(r) = t.strip_prefix("✓ ") {
            (Outcome::Passed, r)
        } else if let Some(r) = t.strip_prefix("× ").or_else(|| t.strip_prefix("✗ ")) {
            (Outcome::Failed, r)
        } else if let Some(r) = t.strip_prefix("↓ ") {
            (Outcome::Skipped, r)
        } else if let Some(r) = t.strip_prefix("❯ ") {
            (Outcome::Passed, r) // only a file header, handled below
        } else {
            return false;
        };
        let is_file = |s: &str| {
            [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"]
                .iter()
                .any(|e| s.ends_with(e))
        };
        // File header: "src/a.test.ts (5 tests | 1 failed) 12ms"
        if let Some((f, _)) = rest.split_once(" (")
            && is_file(f.trim())
            && !rest.contains(" > ")
        {
            self.vitest_file = Some(f.trim().to_owned());
            return true;
        }
        if t.starts_with("❯ ") {
            return false;
        }
        let mut parts: Vec<String> = rest.split(" > ").map(|p| p.trim().to_owned()).collect();
        let file = if parts.len() > 1 && is_file(&parts[0]) {
            Some(parts.remove(0))
        } else if parts.len() > 1 || self.vitest_file.is_some() {
            self.vitest_file.clone()
        } else {
            return false;
        };
        let mut duration = None;
        if let Some(last) = parts.last_mut()
            && let Some((name, d)) = last.rsplit_once(' ')
            && let Some(ms) = d.strip_suffix("ms").and_then(|v| v.parse::<f64>().ok())
        {
            duration = Some(ms.round() as u64);
            *last = name.trim_end().to_owned();
        }
        let key = TestKey {
            file: None,
            path: file.into_iter().collect(),
            name: parts.join(" > "),
        };
        let mut held = Vec::new();
        self.finished(key, outcome, duration, None, Level::Recognized, &mut held);
        // Hold failures one line: Vitest prints the message right after.
        match held.pop() {
            Some(e) if outcome == Outcome::Failed => self.vitest_pending = Some(e),
            Some(e) => out.push(e),
            None => {}
        }
        true
    }

    fn flush_vitest(&mut self, out: &mut Vec<EventKind>) {
        if let Some(e) = self.vitest_pending.take() {
            out.push(e);
        }
    }

    /// VSTest console ("dotnet test"): "  Failed Ns.Class.Test [3 ms]",
    /// "  Passed ..." (normal verbosity), then an "Error Message:" block.
    fn dotnet(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        let t = line.trim_start();
        let (outcome, rest) = if let Some(r) = t.strip_prefix("Failed ") {
            (Outcome::Failed, r)
        } else if let Some(r) = t.strip_prefix("Passed ") {
            (Outcome::Passed, r)
        } else if let Some(r) = t.strip_prefix("Skipped ") {
            (Outcome::Skipped, r)
        } else {
            return false;
        };
        let Some((name, dur)) = rest
            .trim_end()
            .strip_suffix(']')
            .and_then(|r| r.rsplit_once(" ["))
        else {
            return false;
        };
        if name.contains(char::is_whitespace) || name.is_empty() {
            return false;
        }
        let ms = dur
            .strip_suffix(" ms")
            .and_then(|v| v.trim().parse::<f64>().ok())
            .or_else(|| {
                dur.strip_suffix(" s")
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .map(|s| s * 1000.0)
            })
            .map(|v| v.round() as u64);
        self.flush_vitest(out);
        let mut held = Vec::new();
        self.finished(
            TestKey::new(Vec::new(), name),
            outcome,
            ms,
            None,
            Level::Recognized,
            &mut held,
        );
        match held.pop() {
            // Hold the failure until its error message block has been read.
            Some(e) if outcome == Outcome::Failed => {
                self.vitest_pending = Some(e);
                self.dotnet_message = Some(Vec::new());
            }
            Some(e) => out.push(e),
            None => {}
        }
        true
    }

    /// Returns true while the line belongs to the held failure's message.
    fn dotnet_collect(&mut self, line: &str) -> bool {
        let t = line.trim();
        let Some(lines) = self.dotnet_message.as_mut() else {
            return false;
        };
        if t == "Error Message:" {
            return true;
        }
        if t.is_empty()
            || t == "Stack Trace:"
            || t.starts_with("Failed ")
            || t.starts_with("Passed ")
            || t.starts_with("Failed!")
        {
            let msg = lines.join("\n");
            self.dotnet_message = None;
            if let Some(EventKind::TestFinished { message, .. }) = self.vitest_pending.as_mut()
                && !msg.is_empty()
            {
                *message = Some(msg);
            }
            // "Stack Trace:" ends the message; other lines are processed normally.
            return t == "Stack Trace:";
        }
        if lines.len() < 12 {
            lines.push(t.to_owned());
        }
        true
    }

    /// Tests the framework says it ran.
    pub fn total(&self) -> u64 {
        self.structured_total.unwrap_or(self.reported_total)
    }

    // ------------------------------------------------------------- nextest

    /// nextest's human status lines, printed live (ADR 0018):
    /// `PASS [   0.004s] (1/5) calc tests::adds`, `SLOW [> 60.000s] ...`.
    fn close_libtest_running(&mut self) {
        if let Some((n, seen)) = self.libtest_running.take()
            && seen < n
        {
            self.harness_stopped.push(format!(
                "a test binary stopped after {seen} of {n} tests, before its summary"
            ));
        }
    }

    fn close_pass_output(&mut self) {
        if let Some((name, running, result)) = self.nextest_pass_output.take()
            && running
            && !result
        {
            self.harness_died.push(name);
        }
    }

    fn nextest(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        let Some((status, dur, binary, test)) = nextest_status(line) else {
            return false;
        };
        self.close_pass_output();
        let (path, name) = split_rust_path(test);
        let key = TestKey {
            file: Some(nextest_binary(binary)),
            path,
            name,
        };
        let ms = dur
            .trim_start_matches('>')
            .trim()
            .trim_end_matches('s')
            .parse::<f64>()
            .ok()
            .map(|s| (s * 1000.0).round() as u64);
        match status {
            "SLOW" => {
                self.started(key.clone(), out);
                if let Some(ms) = ms {
                    out.push(EventKind::TestSlow {
                        test: key,
                        elapsed_ms: ms,
                        budget_ms: ms,
                        budget_from: prova_core::model::BudgetFrom::Runner,
                    });
                }
            }
            "PASS" | "LEAK" => {
                self.nextest_pass_output = Some((key.display(), false, false));
                self.finished(key, Outcome::Passed, ms, None, Level::Recognized, out)
            }
            "SKIP" => self.finished(key, Outcome::Skipped, None, None, Level::Recognized, out),
            _ => {
                // FAIL, TIMEOUT, SIGSEGV, ABORT, LEAK-FAIL: hold for the output.
                self.flush_nextest(out);
                self.nextest_pending = Some((key, ms, Vec::new()));
            }
        }
        true
    }

    fn flush_nextest(&mut self, out: &mut Vec<EventKind>) {
        let Some((key, ms, lines)) = self.nextest_pending.take() else {
            return;
        };
        // Keep what the test printed, not nextest's frame or libtest's echo.
        let body: Vec<String> = lines
            .iter()
            .map(|l| l.trim())
            .filter(|l| {
                !l.is_empty()
                    && !l.ends_with("───")
                    && !l.starts_with("running ")
                    && !l.starts_with("test ")
                    && !l.starts_with("failures:")
                    && !l.starts_with("note: run with `RUST_BACKTRACE")
            })
            .take_while(|l| !l.starts_with("error: test run failed"))
            .map(str::to_owned)
            .collect();
        let body: Vec<String> = body
            .into_iter()
            .filter(|l| !l.contains("::") || l.contains("panicked"))
            .collect();
        if !body.is_empty() {
            self.failure_output.push((key.clone(), body.join("\n")));
        }
        let msg = message(&body);
        self.finished(key, Outcome::Failed, ms, msg, Level::Recognized, out);
    }

    fn close_libtest_block(&mut self) {
        if let Some((key, lines)) = self.libtest_block.take() {
            let text = lines.join("\n").trim().to_owned();
            if !text.is_empty() {
                self.failure_output.push((key, text));
            }
        }
    }

    /// Events still held when the output ends.
    pub fn finish(&mut self) -> Vec<EventKind> {
        self.close_pass_output();
        self.close_libtest_running();
        self.close_libtest_block();
        let mut out = Vec::new();
        // The injected stream never came: the text results are all there is.
        if self.level < Some(Level::Native) {
            out.append(&mut self.held_text_events);
        }
        self.flush_nextest(&mut out);
        if self.vitest_stream {
            let mut held = Vec::new();
            self.flush_vitest(&mut held);
            if self.level < Some(Level::Native) {
                out.extend(held);
            }
        } else {
            self.flush_vitest(&mut out);
        }
        out
    }

    // ------------------------------------------------------------- TAP

    fn tap(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        let (ok, rest) = if let Some(r) = line.strip_prefix("not ok ") {
            (false, r)
        } else if let Some(r) = line.strip_prefix("ok ") {
            (true, r)
        } else {
            return false;
        };
        // TAP producers number their tests ("ok 3 - name"); requiring the
        // number keeps Go's "ok  \tpkg\t0.1s" package lines out.
        if !rest.starts_with(|c: char| c.is_ascii_digit()) {
            return false;
        }
        let rest = rest
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start();
        let rest = rest.strip_prefix("- ").unwrap_or(rest);
        let (desc, directive) = match rest.split_once(" # ") {
            Some((d, dir)) => (d.trim(), Some(dir.trim().to_ascii_uppercase())),
            None => (rest.trim(), None),
        };
        if desc.is_empty() {
            return false;
        }
        let outcome = match (&directive, ok) {
            (Some(d), _) if d.starts_with("SKIP") => Outcome::Skipped,
            (Some(d), _) if d.starts_with("TODO") => Outcome::Skipped,
            (_, true) => Outcome::Passed,
            (_, false) => Outcome::Failed,
        };
        self.finished(
            TestKey::new(Vec::new(), desc),
            outcome,
            None,
            None,
            Level::Recognized,
            out,
        );
        true
    }
}

/// pytest's failure report as pytest prints it: traceback lines, then the
/// crash message.
fn reportlog_text(longrepr: &serde_json::Value) -> String {
    if let Some(s) = longrepr.as_str() {
        return s.trim().to_owned();
    }
    let mut lines: Vec<String> = Vec::new();
    for entry in longrepr["reprtraceback"]["reprentries"]
        .as_array()
        .into_iter()
        .flatten()
    {
        for l in entry["data"]["lines"].as_array().into_iter().flatten() {
            lines.push(l.as_str().unwrap_or("").to_owned());
        }
        if let Some(loc) = entry["data"]["reprfileloc"].as_object() {
            lines.push(format!(
                "{}:{}: {}",
                loc.get("path").and_then(|v| v.as_str()).unwrap_or(""),
                loc.get("lineno").and_then(|v| v.as_u64()).unwrap_or(0),
                loc.get("message").and_then(|v| v.as_str()).unwrap_or("")
            ));
        }
    }
    for section in longrepr["sections"].as_array().into_iter().flatten() {
        if let (Some(title), Some(body)) = (section[0].as_str(), section[1].as_str()) {
            lines.push(format!("--- {title} ---"));
            lines.push(body.trim_end().to_owned());
        }
    }
    lines.join("\n").trim().to_owned()
}

/// A nextest status line: (status, duration text, binary id, test name).
fn nextest_status(line: &str) -> Option<(&str, &str, &str, &str)> {
    const STATUSES: &[&str] = &[
        "PASS",
        "FAIL",
        "SKIP",
        "SLOW",
        "TIMEOUT",
        "SIGSEGV",
        "SIGABRT",
        "ABORT",
        "LEAK",
        "LEAK-FAIL",
    ];
    let t = line.trim_start();
    let t = match t.strip_prefix("TRY ") {
        // Retries: only the final verdict counts ("TRY 2 PASS").
        Some(r) => r
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start(),
        None => t,
    };
    let (status, rest) = t.split_once(' ')?;
    if !STATUSES.contains(&status) {
        return None;
    }
    let rest = rest.trim_start().strip_prefix('[')?;
    let (dur, rest) = rest.split_once(']')?;
    let mut words = rest.split_whitespace();
    let mut binary = words.next()?;
    if binary.starts_with('(') {
        // "(1/5)" progress counter, possibly with a space inside.
        while !binary.ends_with(')') {
            binary = words.next()?;
        }
        binary = words.next()?;
    }
    let test = words.next()?;
    if words.next().is_some() || test.contains('/') && !test.contains("::") {
        return None;
    }
    Some((status, dur.trim(), binary, test))
}

/// Rust test binaries are named by target (ADR 0018): nextest's binary id
/// keeps its last segment (`calc::calc` and `calc` give `calc`,
/// `prova-cli::live` gives `live`, `crate::bin/tool` gives `tool`).
fn nextest_binary(id: &str) -> String {
    let last = id.rsplit("::").next().unwrap_or(id);
    last.trim_start_matches("bin/").replace('-', "_")
}

/// cargo's "Running unittests src/lib.rs (target/debug/deps/calc-1a2b)":
/// the deps file stem without its hash (`calc`), as nextest names it.
fn cargo_binary(rest: &str) -> String {
    if let Some((_, path)) = rest.rsplit_once(" (")
        && let Some(file) = path.trim_end_matches(')').rsplit(['/', '\\']).next()
    {
        let stem = file.trim_end_matches(".exe");
        if let Some((name, hash)) = stem.rsplit_once('-')
            && !hash.is_empty()
            && hash.chars().all(|c| c.is_ascii_hexdigit())
        {
            return name.replace('-', "_");
        }
        return stem.replace('-', "_");
    }
    rest.split(" (").next().unwrap_or(rest).trim().to_owned()
}

fn split_rust_path(name: &str) -> (Vec<String>, String) {
    match name.rsplit_once("::") {
        Some((p, leaf)) => (vec![p.to_owned()], leaf.to_owned()),
        None => (Vec::new(), name.to_owned()),
    }
}

/// "tests/test_x.py::TestCls::test_a[p]" -> path ["tests.test_x.TestCls"],
/// name "test_a[p]": the same key pytest's JUnit report produces
/// (classname, name), so live and file results are one test.
fn pytest_key(id: &str) -> TestKey {
    let mut parts: Vec<&str> = id.split("::").collect();
    let name = parts.pop().unwrap_or(id).to_owned();
    let mut class: Vec<String> = Vec::new();
    if let Some(file) = parts.first() {
        class.push(file.trim_end_matches(".py").replace('/', "."));
    }
    class.extend(parts.iter().skip(1).map(|p| p.to_string()));
    let path = if class.is_empty() {
        Vec::new()
    } else {
        vec![class.join(".")]
    };
    TestKey {
        file: None,
        path,
        name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(lines: &str) -> (Recognizers, Vec<EventKind>) {
        let mut r = Recognizers::default();
        let mut all = Vec::new();
        for l in lines.lines() {
            all.extend(r.line(l));
        }
        (r, all)
    }

    fn finished(events: &[EventKind]) -> Vec<(String, Outcome)> {
        events
            .iter()
            .filter_map(|e| match e {
                EventKind::TestFinished { test, outcome, .. } => Some((test.display(), *outcome)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn go_json_stream_is_native() {
        let input = std::fs::read_to_string(format!(
            "{}/fixtures/go-json.txt",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let (r, ev) = feed(&input);
        assert_eq!(r.level, Some(Level::Native));
        assert_eq!(
            finished(&ev),
            vec![
                ("example.com/gofix::TestPass".into(), Outcome::Passed),
                ("example.com/gofix::TestFail".into(), Outcome::Failed),
                ("example.com/gofix::TestSub/a".into(), Outcome::Passed),
                ("example.com/gofix::TestSub/b".into(), Outcome::Skipped),
                ("example.com/gofix::TestSub".into(), Outcome::Passed),
            ]
        );
        let msg = ev.iter().find_map(|e| match e {
            EventKind::TestFinished {
                outcome: Outcome::Failed,
                message,
                ..
            } => message.clone(),
            _ => None,
        });
        assert_eq!(msg.as_deref(), Some("x_test.go:6: want 2 got 3"));
        assert!(matches!(ev[0], EventKind::TestStarted { .. }));
    }

    #[test]
    fn go_build_failure_is_an_errored_package() {
        let (_, ev) = feed(
            r#"{"Action":"output","Package":"p","Output":"x.go:3: undefined: y\n"}
{"Action":"fail","Package":"p","Elapsed":0.1}"#,
        );
        assert_eq!(
            finished(&ev),
            vec![("p::(package)".into(), Outcome::Errored)]
        );
    }

    #[test]
    fn go_verbose_text() {
        let (r, ev) = feed(
            "=== RUN   TestFail\n    x_test.go:6: want 2 got 3\n--- FAIL: TestFail (0.25s)\n=== RUN   TestSub\n=== RUN   TestSub/a\n    --- PASS: TestSub/a (0.00s)\n--- PASS: TestSub (0.00s)\nFAIL",
        );
        assert_eq!(r.level, Some(Level::Recognized));
        let f = finished(&ev);
        assert_eq!(f[0], ("TestFail".into(), Outcome::Failed));
        assert_eq!(f[1], ("TestSub/a".into(), Outcome::Passed));
        let EventKind::TestFinished {
            message,
            duration_ms,
            ..
        } = &ev[1]
        else {
            panic!()
        };
        assert_eq!(message.as_deref(), Some("x_test.go:6: want 2 got 3"));
        assert_eq!(*duration_ms, Some(250));
    }

    #[test]
    fn libtest_human_output() {
        let (_, ev) = feed(
            "running 3 tests\nengine::quiet --- FAILED\ntest store::tests::opens ... ok\ntest engine::races ... FAILED\ntest slow ... ignored, needs db\ntest engine::hang has been running for over 60 seconds\ntest result: FAILED. 1 passed; 1 failed",
        );
        assert_eq!(
            finished(&ev),
            vec![
                ("engine::quiet".into(), Outcome::Failed),
                ("store::tests::opens".into(), Outcome::Passed),
                ("engine::races".into(), Outcome::Failed),
                ("slow".into(), Outcome::Skipped),
            ]
        );
        assert!(matches!(ev.last(), Some(EventKind::TestSlow { .. })));
    }

    #[test]
    fn pytest_verbose_and_summary() {
        let (_, ev) = feed(
            "tests/test_x.py::test_a PASSED                 [ 33%]\ntests/test_x.py::TestK::test_b[1-2] FAILED      [ 66%]\nFAILED tests/test_x.py::test_c - AssertionError: assert 1 == 2\nFAILED tests/test_x.py::TestK::test_b[1-2] - ValueError",
        );
        let f = finished(&ev);
        assert_eq!(
            f.len(),
            3,
            "summary line for test_b is not a second result: {f:?}"
        );
        let EventKind::TestFinished { test, message, .. } = &ev[2] else {
            panic!()
        };
        assert_eq!(
            test.path,
            vec!["tests.test_x".to_string()],
            "same as pytest JUnit classname"
        );
        assert_eq!(message.as_deref(), Some("AssertionError: assert 1 == 2"));
        let EventKind::TestFinished { test, .. } = &ev[1] else {
            panic!()
        };
        assert_eq!(test.path, vec!["tests.test_x.TestK".to_string()]);
    }

    fn stream_fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/fixtures/stream/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn libtest_json_from_cargo_is_native_with_durations_and_messages() {
        let (r, events) = feed(&stream_fixture("libtest-json.txt"));
        assert_eq!(r.level, Some(Level::Native));
        let f = finished(&events);
        assert_eq!(f.len(), 5, "{f:?}");
        assert!(f.contains(&("tests::divides".into(), Outcome::Failed)));
        let slow = events.iter().find_map(|e| match e {
            EventKind::TestFinished {
                test,
                duration_ms,
                message,
                ..
            } if test.name == "slow_integration" => {
                assert_eq!(
                    test.file.as_deref(),
                    Some("calc"),
                    "binary from cargo's Running line"
                );
                assert!(message.is_none());
                *duration_ms
            }
            _ => None,
        });
        assert_eq!(slow, Some(1500));
        let msg = events.iter().find_map(|e| match e {
            EventKind::TestFinished {
                message: Some(m), ..
            } => Some(m.clone()),
            _ => None,
        });
        assert!(msg.unwrap().contains("left: 16"));
        assert_eq!(r.total(), 5, "suite totals count");
        let started = events
            .iter()
            .filter(|e| matches!(e, EventKind::TestStarted { .. }))
            .count();
        assert_eq!(started, 5);
    }

    #[test]
    fn libtest_json_names_from_nextest_and_the_vitest_reporter() {
        let (_, events) = feed(concat!(
            r#"{"type":"test","event":"started","name":"calc$tests::adds"}"#,
            "\n",
            r#"{"type":"test","event":"failed","name":"calc$tests::adds","exec_time":0.002,"stdout":"boom"}"#,
            "\n",
            r#"{"type":"test","event":"ok","name":"calc > divides","file":"src/calc.test.ts","exec_time":0.001}"#,
            "\n",
        ));
        let keys: Vec<(Option<String>, String)> = events
            .iter()
            .filter_map(|e| match e {
                EventKind::TestFinished { test, .. } => Some((test.file.clone(), test.display())),
                _ => None,
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                (Some("calc".into()), "tests::adds".into()),
                (None, "src/calc.test.ts::calc > divides".into()),
            ]
        );
    }

    #[test]
    fn nextest_libtest_json_keys_by_binary() {
        let (r, events) = feed(&stream_fixture("nextest-libtest-json.txt"));
        assert_eq!(r.level, Some(Level::Native));
        let f = finished(&events);
        assert!(f.len() == 5, "{f:?}");
        assert!(
            f.contains(&("tests::divides".into(), Outcome::Failed)),
            "{f:?}"
        );
        let file = events.iter().find_map(|e| match e {
            EventKind::TestFinished { test, .. } => test.file.clone(),
            _ => None,
        });
        assert_eq!(
            file.as_deref(),
            Some("calc"),
            "same name as cargo gives the binary"
        );
        assert_eq!(r.total(), 5, "echoed text totals do not add up");
    }

    /// Regression (usability round 3): `prova output --test` found nothing for
    /// failures seen only in libtest's text output.
    #[test]
    fn libtest_text_failures_keep_their_stdout_block() {
        let (mut r, _) = feed(concat!(
            "     Running unittests src/lib.rs (target/debug/deps/calc-1)\n",
            "test tests::divides ... FAILED\n",
            "\nfailures:\n\n",
            "---- tests::divides stdout ----\n",
            "thread 'tests::divides' panicked at src/lib.rs:23:9:\n",
            "  left: 16\n right: 4\n",
            "\nfailures:\n    tests::divides\n",
            "test result: FAILED. 4 passed; 1 failed\n",
        ));
        r.finish();
        let (key, text) = r.failure_output.last().unwrap();
        assert_eq!(
            (key.name.as_str(), key.file.as_deref()),
            ("divides", Some("calc"))
        );
        assert!(
            text.starts_with("thread 'tests::divides' panicked") && text.ends_with("right: 4"),
            "{text}"
        );
    }

    #[test]
    fn framework_failures_and_no_test_messages_are_read() {
        let (r, _) = feed("test a ... ok\ntest result: FAILED. 1 passed; 3 failed; 0 ignored");
        assert_eq!(
            (r.failed_reported(), r.total(), r.summary_seen),
            (3, 4, true)
        );
        let (r, _) = feed("=== 1 failed, 4 passed, 2 errors in 0.5s ===");
        assert_eq!(r.failed_reported(), 3);
        let (r, _) = feed("Tests:       1 failed, 4 passed, 5 total");
        assert_eq!((r.failed_reported(), r.total()), (1, 5));
        let (r, _) = feed("      Tests  1 failed | 4 passed (5)");
        assert_eq!((r.failed_reported(), r.total()), (1, 5));
        let (r, _) = feed("Failed!  - Failed:     2, Passed:     3, Skipped:     0, Total:     5");
        assert_eq!((r.failed_reported(), r.total()), (2, 5));
        for line in [
            "no tests ran in 0.01s",
            "collected 4 items / 4 deselected / 0 selected",
            "?   \texample.com/x\t[no test files]",
            "No tests found, exiting with code 0",
            "No test files found, exiting with code 1",
            "[INFO] No tests to run.",
            "No test is available in /x.dll. Make sure that test discoverer & executors are registered",
        ] {
            let (r, _) = feed(line);
            assert!(r.no_tests_line.is_some(), "{line}");
        }
        let (r, _) = feed(
            "running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out",
        );
        assert_eq!((r.total(), r.summary_seen), (0, true));
    }

    /// ADR 0018: nextest's human lines are read live; the JSON that arrives
    /// when the binary ends confirms the same tests without counting twice.
    #[test]
    fn nextest_human_lines_are_live_and_match_the_json() {
        let mut r = Recognizers::default();
        let ev = r.line("        PASS [   0.004s] (1/5) calc tests::adds");
        assert!(
            matches!(
                &ev[..],
                [EventKind::TestFinished {
                    duration_ms: Some(4),
                    ..
                }]
            ),
            "{ev:?}"
        );
        assert!(
            r.line("        FAIL [   0.004s] (4/5) calc tests::divides")
                .is_empty(),
            "held for output"
        );
        r.line("  output ───");
        r.line("    running 1 test");
        r.line("    thread 'tests::divides' (1) panicked at src/lib.rs:23:9:");
        r.line("      left: 16");
        r.line("    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.00s");
        let ev = r.line("        PASS [   1.504s] (5/5) calc tests::slow_integration");
        let msg = ev.iter().find_map(|e| match e {
            EventKind::TestFinished {
                test,
                message,
                outcome: Outcome::Failed,
                ..
            } if test.name == "divides" => message.clone(),
            _ => None,
        });
        assert!(msg.unwrap().contains("left: 16"));
        assert_eq!(r.total(), 0, "echoed libtest totals are not the run's");
        // A JSON line inside a held FAIL block is an event, not output.
        r.line("        FAIL [   0.002s] (5/6) calc tests::later");
        r.line("    thread 'tests::later' panicked at src/lib.rs:9:9:");
        let ev = r.line(r#"{"type":"test","event":"ok","name":"calc$tests::from_json"}"#);
        assert!(
            ev.iter().any(
                |e| matches!(e, EventKind::TestFinished { test, .. } if test.name == "from_json")
            ),
            "the JSON line was read as an event: {ev:?}"
        );
        r.line("        PASS [   0.001s] (6/6) calc tests::last");
        let later = r
            .failure_output
            .iter()
            .find(|(k, _)| k.name == "later")
            .unwrap();
        assert!(!later.1.contains("from_json"), "{}", later.1);
        // The JSON at the end of the binary: same keys, no new results.
        let ev = r.line(r#"{"type":"test","event":"failed","name":"calc::calc$tests::divides","exec_time":0.004,"stdout":"x"}"#);
        assert!(
            !ev.iter()
                .any(|e| matches!(e, EventKind::TestFinished { .. }))
        );
        assert_eq!(r.level, Some(Level::Native));
        let ev = r.line("        SLOW [> 60.000s] calc tests::hangs_when_asked");
        assert!(
            ev.iter().any(|e| matches!(
                e,
                EventKind::TestSlow {
                    elapsed_ms: 60000,
                    ..
                }
            )),
            "{ev:?}"
        );
        assert!(
            nextest_status("     Summary [   1.504s] 5 tests run: 4 passed, 1 failed").is_none()
        );
        assert_eq!(
            cargo_binary("tests/live.rs (target/debug/deps/live-0123abcd)"),
            "live"
        );
        assert_eq!(nextest_binary("prova-cli::live"), "live");
        assert_eq!(nextest_binary("prova-cli::bin/prova"), "prova");
    }

    /// Round 5: in quiet mode libtest's 60 s warning followed the progress
    /// dots on one line and was missed, so a hang was never named.
    #[test]
    fn quiet_progress_dots_do_not_hide_the_slow_warning() {
        let (_, ev) = feed("....test tests::hangs_when_asked has been running for over 60 seconds");
        assert!(
            matches!(
                &ev[..],
                [EventKind::TestSlow {
                    elapsed_ms: 60000,
                    ..
                }]
            ),
            "{ev:?}"
        );
    }

    /// Round 5: pytest -v printed "PASSED" before the reportlog stream's
    /// setup line was read; the late start restarted a finished test, which
    /// then went "slow".
    #[test]
    fn a_late_start_never_restarts_a_finished_test() {
        let mut r = Recognizers::default();
        r.line("tests/test_calc.py::test_add PASSED");
        let ev = r.line(r#"{"nodeid": "tests/test_calc.py::test_add", "when": "setup", "outcome": "passed", "duration": 0.0, "$report_type": "TestReport"}"#);
        assert!(ev.is_empty(), "{ev:?}");
        let mut streamed = Recognizers {
            pytest_stream: true,
            ..Recognizers::default()
        };
        assert!(
            streamed
                .line("tests/test_calc.py::test_add PASSED")
                .is_empty(),
            "the stream decides"
        );
    }

    /// Round 12 (blocker): with an injected stream, pytest -v and Vitest
    /// print a test's result before the stream file is read. The held text
    /// result marked the test as seen, so the stream's result was dropped as
    /// a duplicate and the held one discarded: fast tests, failures among
    /// them, vanished from the run and from history.
    #[test]
    fn a_held_text_result_never_hides_the_streams_result() {
        let mut r = Recognizers {
            pytest_stream: true,
            ..Recognizers::default()
        };
        assert!(r.line("tests/test_calc.py::test_div FAILED").is_empty());
        let ev = r.line(r#"{"nodeid": "tests/test_calc.py::test_div", "when": "call", "outcome": "failed", "duration": 0.0, "longrepr": {"reprcrash": {"message": "assert 16 == 4"}}, "$report_type": "TestReport"}"#);
        assert_eq!(
            finished(&ev),
            vec![("tests.test_calc::test_div".into(), Outcome::Failed)],
            "{ev:?}"
        );
        let mut v = Recognizers {
            vitest_stream: true,
            ..Recognizers::default()
        };
        v.line(" ❯ src/calc.test.ts (5 tests | 1 failed) 6ms");
        v.line("   × calc > divides 4ms");
        let ev = v.line(r#"{"type":"test","name":"calc > divides","file":"src/calc.test.ts","event":"failed","exec_time":0.004,"message":"expected 16 to be 4"}"#);
        let f = finished(&ev);
        assert!(
            f.len() == 1 && f[0].0.contains("divides") && f[0].1 == Outcome::Failed,
            "{ev:?}"
        );
    }

    #[test]
    fn nextest_ignored_tests_are_skipped_at_suite_end() {
        let (_, events) = feed(concat!(
            r#"{"type":"suite","event":"started","test_count":2}"#,
            "\n",
            r#"{"type":"test","event":"started","name":"cli::large$go"}"#,
            "\n",
            r#"{"type":"test","event":"started","name":"cli::large$rust"}"#,
            "\n",
            r#"{"type":"suite","event":"ok","passed":0,"failed":0,"ignored":2,"measured":0,"filtered_out":0}"#,
            "\n",
        ));
        let f = finished(&events);
        assert_eq!(
            f,
            vec![
                ("go".into(), Outcome::Skipped),
                ("rust".into(), Outcome::Skipped)
            ]
        );
    }

    #[test]
    fn pytest_reportlog_is_native_and_matches_verbose_keys() {
        let (r, events) = feed(&stream_fixture("pytest-reportlog.jsonl"));
        assert_eq!(r.level, Some(Level::Native));
        let f = finished(&events);
        assert_eq!(f.len(), 5, "{f:?}");
        assert!(
            f.contains(&("tests.test_calc::test_div".into(), Outcome::Failed)),
            "{f:?}"
        );
        let msg = events.iter().find_map(|e| match e {
            EventKind::TestFinished {
                message: Some(m), ..
            } => Some(m.clone()),
            _ => None,
        });
        assert!(msg.unwrap().starts_with("assert 16 == 4"));
        let (key, text) = &r.failure_output[0];
        assert_eq!(key.name, "test_div");
        assert!(
            text.contains("def test_div():") && text.contains("AssertionError"),
            "{text}"
        );
        // The same test from pytest -v output is not counted twice.
        let (_, again) = {
            let mut r = r;
            let e = r.line("tests/test_calc.py::test_div FAILED");
            (r.seen.len(), e)
        };
        assert!(again.is_empty());
    }

    #[test]
    fn tap() {
        let (_, ev) = feed(
            "TAP version 14\n1..4\nok 1 - adds\nnot ok 2 - divides\nok 3 - later # SKIP no db\nok 4\n",
        );
        assert_eq!(
            finished(&ev),
            vec![
                ("adds".into(), Outcome::Passed),
                ("divides".into(), Outcome::Failed),
                ("later".into(), Outcome::Skipped),
            ]
        );
    }

    #[test]
    fn vitest_verbose() {
        let (_, ev) = feed(
            " ✓ src/sum.test.ts > sum > adds 1ms\n × src/sum.test.ts > sum > subtracts 3ms\n ↓ src/sum.test.ts > later\n ✓ src/sum.test.ts (3 tests) 5ms",
        );
        assert_eq!(
            finished(&ev),
            vec![
                ("src/sum.test.ts::sum > adds".into(), Outcome::Passed),
                ("src/sum.test.ts::sum > subtracts".into(), Outcome::Failed),
                ("src/sum.test.ts::later".into(), Outcome::Skipped),
            ]
        );
    }

    #[test]
    fn dotnet_vstest_console() {
        let mut r = Recognizers::default();
        let mut ev = Vec::new();
        let text = "[xUnit.net 00:00:01.58]     Calc.CalcTests.Divides [FAIL]\n  Failed Calc.CalcTests.Divides [3 ms]\n  Error Message:\n   Assert.Equal() Failure: Values differ\nExpected: 4\nActual:   16\n  Stack Trace:\n     at Calc.CalcTests.Divides() in /w/CalcTests.cs:line 13\n  Passed Calc.CalcTests.Adds [1 ms]\nFailed!  - Failed:     1, Passed:     4, Skipped:     0, Total:     5, Duration: 1 s - Calc.Tests.dll (net9.0)";
        for l in text.lines() {
            ev.extend(r.line(l));
        }
        ev.extend(r.finish());
        assert_eq!(
            finished(&ev),
            vec![
                ("Calc.CalcTests.Divides".into(), Outcome::Failed),
                ("Calc.CalcTests.Adds".into(), Outcome::Passed),
            ]
        );
        let EventKind::TestFinished {
            message,
            duration_ms,
            ..
        } = &ev[0]
        else {
            panic!()
        };
        assert_eq!(
            message.as_deref(),
            Some("Assert.Equal() Failure: Values differ\nExpected: 4\nActual:   16")
        );
        assert_eq!(*duration_ms, Some(3));
        assert_eq!(r.reported_total, 5);
    }

    #[test]
    fn vitest_default_reporter_with_messages() {
        let mut r = Recognizers::default();
        let mut ev = Vec::new();
        for l in " ❯ src/calc.test.ts (3 tests | 1 failed) 1507ms\n   ✓ calc > adds 1ms\n   × calc > divides 3ms\n     → expected 16 to be 4\n   ✓ calc > slow  1501ms".lines() {
            ev.extend(r.line(l));
        }
        ev.extend(r.finish());
        assert_eq!(
            finished(&ev),
            vec![
                ("src/calc.test.ts::calc > adds".into(), Outcome::Passed),
                ("src/calc.test.ts::calc > divides".into(), Outcome::Failed),
                ("src/calc.test.ts::calc > slow".into(), Outcome::Passed),
            ]
        );
        let EventKind::TestFinished { message, .. } = &ev[1] else {
            panic!()
        };
        assert_eq!(message.as_deref(), Some("expected 16 to be 4"));
    }

    #[test]
    fn same_names_in_different_test_binaries_stay_apart() {
        let (r, ev) = feed(
            "     Running unittests src/lib.rs (target/debug/deps/a-1)\ntest tests::x ... ok\n     Running tests/it.rs (target/debug/deps/it-2)\ntest tests::x ... FAILED",
        );
        assert_eq!(finished(&ev).len(), 2);
        assert_eq!(r.seen.len(), 2);
        let EventKind::TestFinished { test, .. } = &ev[1] else {
            panic!()
        };
        assert_eq!(
            test.file.as_deref(),
            Some("it"),
            "named by target, as nextest names it"
        );
    }

    #[test]
    fn go_test_cache_is_called_out() {
        let (_, ev) = feed("ok  \texample.com/calc\t(cached)\nok  \texample.com/other\t(cached)");
        assert_eq!(ev.len(), 1);
        assert!(matches!(&ev[0], EventKind::RunNote { message } if message.contains("-count=1")));
    }

    #[test]
    fn framework_totals_are_counted() {
        let (r, _) = feed(
            "test result: FAILED. 4 passed; 1 failed; 2 ignored; 0 measured; 0 filtered out\ntest result: ok. 3 passed; 0 failed; 0 ignored\n",
        );
        assert_eq!(r.reported_total, 10);
        let (r, _) = feed("========== 1 failed, 4 passed in 1.52s ==========");
        assert_eq!(r.reported_total, 5);
    }

    #[test]
    fn unrelated_output_is_ignored() {
        let (_, ev) = feed("ok  \texample.com/calc\t1.502s\nFAIL\texample.com/calc\t1.5s");
        assert!(ev.is_empty(), "go package lines are not tests: {ev:?}");
        let (r, ev) = feed(
            "Compiling prova v0.1.0\n{\"not\": \"go\"}\nok\nFAILED\nnote: run with RUST_BACKTRACE=1",
        );
        assert!(ev.is_empty(), "{ev:?}");
        assert_eq!(r.level, None);
    }
}

/// A doctest's name as libtest prints it: `src/lib.rs - div (line 7)`.
fn is_doctest_name(name: &str) -> bool {
    name.contains(" - ") && name.trim_end().ends_with(')') && name.contains("(line ")
}

/// Tests that passed in a finished command's output (an experiment oracle):
/// so prova can tell an oracle that checked from one that exited 0 without
/// running its tests (round 7: a candidate's own `TestMain` skipped them).
pub fn passed_tests(output: &str) -> Vec<String> {
    let mut r = Recognizers::default();
    let mut events = Vec::new();
    for line in output.lines() {
        events.extend(r.line(line));
    }
    events.extend(r.finish());
    let mut passed: Vec<String> = events
        .into_iter()
        .filter_map(|e| match e {
            EventKind::TestFinished {
                test,
                outcome: Outcome::Passed,
                ..
            } => Some(test.display()),
            _ => None,
        })
        .collect();
    passed.sort();
    passed.dedup();
    passed
}

#[cfg(test)]
mod passed_tests_tests {
    #[test]
    fn reads_passes_of_a_finished_output() {
        let out = "=== RUN   TestConformance\n--- PASS: TestConformance (0.01s)\nPASS\nok  \texample.com/calc\t0.02s\n";
        assert_eq!(super::passed_tests(out), vec!["TestConformance".to_owned()]);
        assert!(super::passed_tests("ok  \texample.com/calc\t0.02s\n").is_empty());
    }

    /// Round 9: nextest runs tests in parallel; a batch's end event counted
    /// an ignored test that had its own `ignored` event, and the test still
    /// running (which then passed) was recorded as skipped.
    #[test]
    fn a_running_test_is_not_taken_for_an_ignored_one() {
        use super::Recognizers;
        use prova_core::model::{EventKind, Outcome};
        let mut r = Recognizers::default();
        let mut ev = Vec::new();
        for l in [
            r#"{"type":"test","event":"started","name":"calc::calc$tests::slow"}"#,
            r#"{"type":"test","event":"ignored","name":"calc::calc$tests::divides"}"#,
            r#"{"type":"suite","event":"failed","passed":2,"failed":1,"ignored":1,"measured":0,"filtered_out":0}"#,
            r#"{"type":"test","event":"ok","name":"calc::calc$tests::slow","exec_time":1.5}"#,
        ] {
            ev.extend(r.line(l));
        }
        let slow: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                EventKind::TestFinished { test, outcome, .. } if test.name == "slow" => {
                    Some(*outcome)
                }
                _ => None,
            })
            .collect();
        assert_eq!(slow, vec![Outcome::Passed], "{ev:?}");
    }

    /// Round 9: a failing doctest in quiet mode was not named.
    #[test]
    fn a_quiet_doctest_failure_is_named() {
        use super::Recognizers;
        use prova_core::model::{EventKind, Outcome};
        let mut r = Recognizers::default();
        let mut ev = Vec::new();
        for l in ["   Doc-tests calc", "src/lib.rs - div (line 7) --- FAILED"] {
            ev.extend(r.line(l));
        }
        assert!(
            ev.iter().any(|e| matches!(e, EventKind::TestFinished { test, outcome: Outcome::Failed, .. } if test.name.contains("div (line 7)"))),
            "{ev:?}"
        );
    }

    /// Round 9: a test calling `exit(0)` passed under nextest; with its
    /// captured output shown, libtest never reports its result.
    #[test]
    fn a_passing_test_whose_harness_never_reported_is_caught() {
        use super::Recognizers;
        let mut r = Recognizers::default();
        for l in [
            "        PASS [   0.003s] (1/2) calc tests::adds",
            "  output ───",
            "    running 1 test",
            "    test tests::adds ... ok",
            "    test result: ok. 1 passed; 0 failed; 0 ignored",
            "        PASS [   0.004s] (2/2) calc tests::divides",
            "  output ───",
            "    running 1 test",
            "     Summary [   0.004s] 2 tests run: 2 passed",
        ] {
            r.line(l);
        }
        r.finish();
        assert_eq!(r.harness_died, vec!["tests::divides".to_owned()]);
    }

    /// Round 11: nextest names a retry `test#2`; it is the same test's
    /// second attempt, not another test.
    #[test]
    fn a_nextest_retry_is_another_attempt_of_the_same_test() {
        use super::Recognizers;
        use prova_core::model::{EventKind, Outcome};
        let mut r = Recognizers::default();
        let mut ev = Vec::new();
        for l in [
            r#"{"type":"test","event":"started","name":"calc::calc$tests::flaky"}"#,
            r#"{"type":"test","event":"failed","name":"calc::calc$tests::flaky","exec_time":0.001,"stdout":"boom"}"#,
            r#"{"type":"test","event":"started","name":"calc::calc$tests::flaky#2"}"#,
            r#"{"type":"test","event":"ok","name":"calc::calc$tests::flaky#2","exec_time":0.001}"#,
        ] {
            ev.extend(r.line(l));
        }
        let finished: Vec<(String, u32, Outcome)> = ev
            .iter()
            .filter_map(|e| match e {
                EventKind::TestFinished {
                    test,
                    attempt,
                    outcome,
                    ..
                } => Some((test.name.clone(), *attempt, *outcome)),
                _ => None,
            })
            .collect();
        assert_eq!(
            finished,
            vec![
                ("flaky".to_owned(), 1, Outcome::Failed),
                ("flaky".to_owned(), 2, Outcome::Passed)
            ]
        );
    }

    /// Round 11: a failing doctest after a nextest failure was taken as that
    /// failure's captured output and lost.
    #[test]
    fn a_doctest_after_a_nextest_failure_is_its_own_result() {
        use super::Recognizers;
        use prova_core::model::{EventKind, Outcome};
        let mut r = Recognizers::default();
        let mut ev = Vec::new();
        for l in [
            "        FAIL [   0.003s] (1/1) calc tests::adds",
            "  stdout ───",
            "    thread panicked",
            "   Doc-tests calc",
            "running 1 test",
            "test src/lib.rs - add (line 3) ... FAILED",
        ] {
            ev.extend(r.line(l));
        }
        ev.extend(r.finish());
        assert!(
            ev.iter().any(|e| matches!(e, EventKind::TestFinished { test, outcome: Outcome::Failed, .. } if test.name.contains("add (line 3)"))),
            "{ev:?}"
        );
    }
}
