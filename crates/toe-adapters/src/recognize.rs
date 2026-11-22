//! Levels 2 and 3: per-test events from live output (ADR 0013).
//!
//! Each recognizer looks at one line at a time and is cheap. They run on
//! every run; lines that match no known format are ignored. Go's
//! `test2json` stream is the framework's own structured output (level 3);
//! the others parse human-readable output (level 2).

use std::collections::{BTreeMap, BTreeSet};

use toe_core::model::{EventKind, Level, Outcome, TestKey};

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
        if self.started.insert(d) {
            out.push(EventKind::TestStarted { test: key });
        }
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
        if self.seen.contains_key(&d) {
            // Already reported: a pytest summary line, or (libtest, level
            // Recognized with a binary known) the same binary run again.
            if self.libtest_binary.is_some() && key.file == self.libtest_binary {
                self.repeats += 1;
            }
            return;
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
        if (t.starts_with("Failed!") || t.starts_with("Passed!")) && t.contains("Total:") {
            if let Some(total) = t
                .split(',')
                .find_map(|p| p.trim().strip_prefix("Total:"))
                .and_then(|v| v.trim().parse::<u64>().ok())
            {
                self.reported_total += total;
            }
            return;
        }
        let body = match t.strip_prefix("test result: ") {
            Some(rest) => rest.split_once(". ").map(|(_, b)| b).unwrap_or(""),
            None if t.contains(" in ") && (t.contains(" passed") || t.contains(" failed")) => t,
            None => return,
        };
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
            }
        }
        self.reported_total += n;
    }

    pub fn line(&mut self, line: &str) -> Vec<EventKind> {
        self.totals(line);
        let mut out = Vec::new();
        if self.dotnet_message.is_some() {
            if self.dotnet_collect(line) {
                return out;
            }
        } else if self.vitest_pending.is_some() && !line.trim_start().starts_with("→ ") {
            self.flush_vitest(&mut out);
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
            self.go_json(line, &mut out);
        } else {
            let _ = self.libtest(line, &mut out)
                || self.go_verbose(line, &mut out)
                || self.pytest(line, &mut out)
                || self.dotnet(line, &mut out)
                || self.vitest(line, &mut out)
                || self.tap(line, &mut out);
        }
        out
    }

    // ------------------------------------------------------------- go json

    fn go_json(&mut self, line: &str, out: &mut Vec<EventKind>) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
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

    // ------------------------------------------------------------- go -v

    fn go_verbose(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        let t = line.trim_start();
        if let Some(name) = t.strip_prefix("=== RUN") {
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
        // "     Running tests/live.rs (target/debug/deps/live-abc)" or
        // "   Doc-tests toe_core": tests of different binaries may share names.
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("Running ") {
            self.libtest_binary = Some(rest.split(" (").next().unwrap_or(rest).trim().to_owned());
            return true;
        }
        if let Some(rest) = t.strip_prefix("Doc-tests ") {
            self.libtest_binary = Some(format!("doctests {}", rest.trim()));
            return true;
        }
        // `cargo test --quiet` prints only failures: "path::name --- FAILED"
        if let Some(name) = line.strip_suffix(" --- FAILED")
            && !name.is_empty()
            && !name.contains(char::is_whitespace)
        {
            let key = self.libtest_key(name);
            self.finished(key, Outcome::Failed, None, None, Level::Recognized, out);
            return true;
        }
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

    /// Events still held when the output ends.
    pub fn finish(&mut self) -> Vec<EventKind> {
        let mut out = Vec::new();
        self.flush_vitest(&mut out);
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
        assert_eq!(test.file.as_deref(), Some("tests/it.rs"));
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
            "Compiling toe v0.1.0\n{\"not\": \"go\"}\nok\nFAILED\nnote: run with RUST_BACKTRACE=1",
        );
        assert!(ev.is_empty(), "{ev:?}");
        assert_eq!(r.level, None);
    }
}
