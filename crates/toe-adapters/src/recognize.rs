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
    /// Final outcome of every test reported so far, by display name.
    pub seen: BTreeMap<String, Outcome>,
    started: BTreeSet<String>,
    /// Captured output per running test (Go), used as failure message.
    go_output: BTreeMap<String, Vec<String>>,
    /// Go -v: test currently printing output.
    go_current: Option<String>,
    /// Tests the framework says it ran (from its own summary lines).
    pub reported_total: u64,
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
        let d = key.display();
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
        let d = key.display();
        if self.seen.contains_key(&d) {
            return; // already reported by an earlier line (e.g. pytest summary)
        }
        self.seen.insert(d, outcome);
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
        if line.starts_with('{') {
            self.go_json(line, &mut out);
        } else {
            let _ = self.libtest(line, &mut out)
                || self.go_verbose(line, &mut out)
                || self.pytest(line, &mut out)
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
                    .iter()
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

    fn libtest(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        // `cargo test --quiet` prints only failures: "path::name --- FAILED"
        if let Some(name) = line.strip_suffix(" --- FAILED")
            && !name.is_empty()
            && !name.contains(char::is_whitespace)
        {
            let (path, leaf) = split_rust_path(name);
            self.finished(
                TestKey::new(path, leaf),
                Outcome::Failed,
                None,
                None,
                Level::Recognized,
                out,
            );
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
            let (path, leaf) = split_rust_path(name.trim());
            self.finished(
                TestKey::new(path, leaf),
                outcome,
                None,
                None,
                Level::Recognized,
                out,
            );
            return true;
        }
        if let Some(name) = rest.strip_suffix(" has been running for over 60 seconds") {
            let (path, leaf) = split_rust_path(name.trim());
            out.push(EventKind::TestSlow {
                test: TestKey::new(path, leaf),
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
    fn vitest(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        let t = line.trim_start();
        let (outcome, rest) = if let Some(r) = t.strip_prefix("✓ ") {
            (Outcome::Passed, r)
        } else if let Some(r) = t.strip_prefix("× ").or_else(|| t.strip_prefix("✗ ")) {
            (Outcome::Failed, r)
        } else if let Some(r) = t.strip_prefix("↓ ") {
            (Outcome::Skipped, r)
        } else {
            return false;
        };
        let parts: Vec<&str> = rest.split(" > ").collect();
        if parts.len() < 2 {
            return false; // file-level summary line, not a test
        }
        let mut last = parts[parts.len() - 1].trim().to_owned();
        let mut duration = None;
        if let Some((name, d)) = last.rsplit_once(' ')
            && let Some(ms) = d.strip_suffix("ms").and_then(|v| v.parse::<f64>().ok())
        {
            duration = Some(ms.round() as u64);
            last = name.to_owned();
        }
        let file = Some(parts[0].trim().to_owned());
        let path = parts[1..parts.len() - 1]
            .iter()
            .map(|p| p.trim().to_owned())
            .collect();
        self.finished(
            TestKey {
                file,
                path,
                name: last,
            },
            outcome,
            duration,
            None,
            Level::Recognized,
            out,
        );
        true
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

fn pytest_key(id: &str) -> TestKey {
    // "tests/test_x.py::TestCls::test_a[param]"
    let mut parts: Vec<&str> = id.split("::").collect();
    let name = parts.pop().unwrap_or(id).to_owned();
    let file = parts.first().map(|f| f.to_string());
    let path = parts.iter().skip(1).map(|p| p.to_string()).collect();
    TestKey { file, path, name }
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
        assert_eq!(test.file.as_deref(), Some("tests/test_x.py"));
        assert_eq!(message.as_deref(), Some("AssertionError: assert 1 == 2"));
        let EventKind::TestFinished { test, .. } = &ev[1] else {
            panic!()
        };
        assert_eq!(test.path, vec!["TestK".to_string()]);
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
                ("sum::adds".into(), Outcome::Passed),
                ("sum::subtracts".into(), Outcome::Failed),
                ("later".into(), Outcome::Skipped),
            ]
        );
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
        let (r, ev) = feed(
            "Compiling toe v0.1.0\n{\"not\": \"go\"}\nok\nFAILED\nnote: run with RUST_BACKTRACE=1",
        );
        assert!(ev.is_empty(), "{ev:?}");
        assert_eq!(r.level, None);
    }
}
