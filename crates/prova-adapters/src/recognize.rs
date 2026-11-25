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
    /// The Go module of the run's directory (go.mod), for packages printed
    /// as `.`.
    pub go_module: Option<String>,
    /// gotestsum `pkgname`: each package's last status line (`✓`/`✖`), the
    /// failures it listed, and how many runs `--rerun-fails` made. A
    /// package green on its last run after failures means its failed tests
    /// passed on a rerun (round 16: reported as failures, exit 0).
    gts_pkg_last: BTreeMap<String, bool>,
    gts_failed: Vec<TestKey>,
    gts_runs: u64,
    /// The key each `seen` entry was reported under (unique key -> key):
    /// a result superseded later names the test it was stored as.
    pub seen_keys: BTreeMap<String, TestKey>,
    /// Vitest: current test file header, and a failure held for its message.
    vitest_file: Option<String>,
    vitest_pending: Option<EventKind>,
    /// VSTest: collecting the "Error Message:" block of the held failure.
    dotnet_message: Option<Vec<String>>,
    /// VSTest `Total tests:` block seen: its `Failed:` line follows.
    vstest_totals: bool,
    /// nextest `SKIP` lines named individually.
    nextest_skip_lines: u64,
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
    /// gotestsum's own stream is injected (`gotestsum-json`): its text is
    /// not read, or every test would count twice.
    pub gotestsum_stream: bool,
    /// gotestsum's text was recognized (its `DONE` line or result lines).
    pub gotestsum_seen: bool,
    /// A gotestsum `=== FAIL:` block being read: its lines are the message.
    gts_pending: Option<(TestKey, Outcome, Option<u64>, Vec<String>)>,
    /// `FAIL <pkg>` lines (`testname` format), kept until the end: only a
    /// confirmed gotestsum run makes them package failures.
    gts_pkg_failed: Vec<String>,
    /// Packages with a failing test (`testname` format).
    gts_pkg_with_failure: std::collections::BTreeSet<String>,
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
    /// pytest subtests reported without a label, per parent test.
    subtest_counts: BTreeMap<String, u32>,
    /// Quint: the module whose tests are being listed, and a failure whose
    /// detail block is being read.
    quint_module: Option<String>,
    quint_detail: Option<String>,
    /// Tests that failed an attempt and run again (pytest-rerunfailures):
    /// unique key -> the failed attempt number. The next result is the
    /// following attempt.
    reruns: BTreeMap<String, u32>,
    /// pytest reportlog: a passing call held until its teardown report
    /// (nodeid, key, duration): a teardown error makes the test errored.
    pytest_call_pass: Option<(String, TestKey, Option<u64>)>,
    /// pytest verbose text: a passing line held one line, for the same
    /// reason (pytest prints the teardown error as a second line).
    pytest_text_pass: Option<(String, TestKey)>,
    /// pytest's FAILURES / ERRORS section is being read: its blocks
    /// (header of `___ test_q ___`, lines), claimed by the short summary's
    /// `FAILED id - message` lines that follow.
    pytest_in_failures: bool,
    pytest_blocks: Vec<(String, Vec<String>)>,
    /// Test files (Jest `Test Suites:`, Vitest `Test Files`) the framework's
    /// own summary says failed. Such a file can fail without any test of it
    /// failing (an import error), so the count is its own evidence.
    pub suites_failed: u64,
    /// Vitest's `Errors  N error(s)` line: errors no test owns.
    pub unhandled_errors: u64,
    /// The `Unhandled Errors` block was seen (and recorded as an entry).
    pub unhandled_block: bool,
    /// Lines of a Vitest unhandled-errors block being read: its error and
    /// where it originated name the entry (round 16: one fixed name for
    /// every unhandled error gave a new one an unrelated history).
    unhandled_lines: Option<Vec<String>>,
    /// Jest files read: its totals are believed against what was recognized.
    pub jest_files: u64,
    jest: JestState,
    /// Maven's summary section being read (`Failures:`, `Errors:`,
    /// `Flakes:`), and the rerun test whose `Run N:` lines are being read.
    surefire_section: Option<SurefireSection>,
    surefire_rerun: Option<SurefireRerun>,
    /// Go text: results since the last package line. `go test -v` names the
    /// package only after its tests, so they are reported under a provisional
    /// name at once and re-keyed when the package is known (round 14).
    go_pending: Vec<GoRun>,
    /// Packages seen so far in this run, as the full import path.
    go_segment: u32,
    go_full_pkgs: Vec<String>,
    /// Executions of each test so far (`-count=N`, reruns): its attempts.
    go_attempts: BTreeMap<String, u32>,
    /// Tests that failed and passed on a rerun: failures a summary counted.
    go_recovered: u64,
    /// Packages in which a test failed.
    go_failed_pkgs: BTreeSet<String>,
    /// `go test -json` packages whose own result event was read: a test run
    /// again after it is a retry, not an iteration of `-count`.
    go_pkg_done: BTreeSet<String>,
    /// Go text: tests started and not finished in the current package. A
    /// package result line printed while one runs is held: a test can print
    /// anything, `ok  \tfake\t0.1s` included.
    go_open: BTreeSet<String>,
    go_tentative: Option<(String, BTreeSet<String>)>,
    /// The test whose `--- FAIL` line was the last thing printed: a panic
    /// right after it is that test's.
    go_after_fail: Option<String>,
    go_panic_block: bool,
    /// `go test -json`: per test, how many subtests were skipped and how
    /// many were not.
    go_subs: BTreeMap<String, (u32, u32)>,
    /// Passing tests whose every subtest was skipped: nothing ran in them.
    go_all_skipped: Vec<String>,
    /// gotestsum printed a `DONE` line already.
    gts_done_seen: bool,
    /// Output since the last package line, for a package failure's message.
    go_segment_text: Vec<String>,
    /// `running tests:` of a Go test timeout is being read; the tests named.
    go_reading_hung: bool,
    go_hung: Vec<String>,
    /// `ok` packages that ran tests, `ok` packages that ran none, `FAIL`.
    go_ok_pkgs: u64,
    go_no_tests_pkgs: u64,
    go_fail_pkgs: u64,
    /// gotestsum `testname` result lines (`PASS pkg.Test`): it prints every test.
    gts_testname_lines: u64,
    /// The held gotestsum result is a `testname` line followed by its output.
    gts_inline: bool,
}

/// A rerun test: its key, its section, and its attempts so far.
type SurefireRerun = (TestKey, SurefireSection, Vec<(Outcome, Option<String>)>);

#[derive(Clone, Copy, PartialEq, Eq)]
enum SurefireSection {
    Failures,
    Errors,
    Flakes,
}

/// Jest's console reporter: the test file whose results are being read.
#[derive(Default)]
struct JestState {
    /// Verbose failures held until their `●` block gives the message.
    held: Vec<(TestKey, Option<u64>)>,
    /// A `Test suite failed to run` block was read in this file.
    suite_failed_block: bool,
    file: Option<String>,
    /// The file's header said FAIL, and a failing test of it was named.
    failed: bool,
    named: bool,
    /// Describe blocks open (indent, name).
    stack: Vec<(usize, String)>,
    /// `console.log` output of a test, until its stack line and a blank.
    console: Option<bool>,
    /// A failure block being read: the test's name ("a > b"; None for
    /// "Test suite failed to run") and its lines.
    block: Option<(Option<String>, Vec<String>)>,
    /// What "Test suite failed to run" said.
    suite_error: Vec<String>,
}

/// One execution of a Go test.
struct GoExec {
    outcome: Outcome,
    duration_ms: Option<u64>,
    message: Option<String>,
    subtest: bool,
}

/// A Go test reported before its package is known, with its executions.
struct GoRun {
    key: TestKey,
    execs: Vec<GoExec>,
}

fn lift(level: &mut Option<Level>, l: Level) {
    if level.is_none_or(|cur| l > cur) {
        *level = Some(l);
    }
}

/// "0.00s" or "0.00" seconds as ms.
fn secs_ms(s: &str) -> Option<u64> {
    s.trim()
        .trim_end_matches('s')
        .parse::<f64>()
        .ok()
        .map(|v| (v * 1000.0).round() as u64)
}

/// A line of gotestsum's text output (held back when its stream is read).
/// A line of `go test -v` text: a test's start, pause, result, or a
/// package's result.
fn is_go_text(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("=== RUN")
        || t.starts_with("=== PAUSE")
        || t.starts_with("=== CONT")
        || t.starts_with("=== NAME")
        || t.starts_with("--- PASS")
        || t.starts_with("--- FAIL")
        || t.starts_with("--- SKIP")
        || matches!(line.trim_end(), "PASS" | "FAIL")
        || line.starts_with("ok  \t")
        || line.starts_with("FAIL\t")
}

fn is_gotestsum_text(line: &str) -> bool {
    let t = line.trim_end();
    t.starts_with("=== FAIL: ")
        || t.starts_with("=== SKIP: ")
        || t == "=== Failed"
        || t == "=== Errors"
        || t == "=== Skipped"
}

/// A Jest failure block's message: its first assertion lines, up to the code
/// frame or the stack.
fn jest_message(body: &[String]) -> Option<String> {
    let frame = |l: &str| {
        l.starts_with('>')
            || l.split_once('|').is_some_and(|(n, _)| {
                !n.trim().is_empty() && n.trim().bytes().all(|b| b.is_ascii_digit())
            })
    };
    let lines: Vec<&str> = body
        .iter()
        .map(String::as_str)
        .filter(|l| !l.is_empty())
        .take_while(|l| !frame(l) && !l.starts_with("at "))
        .take(3)
        .collect();
    if lines.is_empty() {
        body.iter().find(|l| !l.is_empty()).cloned()
    } else {
        Some(lines.join("\n"))
    }
}

/// A ruler line (nextest's `------------`): never a message.
fn is_separator(l: &str) -> bool {
    let l = l.trim();
    l.chars().count() >= 3 && l.chars().all(|c| matches!(c, '-' | '\u{2500}' | '='))
}

fn message(lines: &[String]) -> Option<String> {
    let kept: Vec<&str> = lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| {
            !l.is_empty() && !l.starts_with("=== ") && !l.starts_with("--- ") && !is_separator(l)
        })
        .collect();
    if kept.is_empty() {
        return None;
    }
    // A Go panic: the panic line and the test's own frame, not the last
    // goroutine frames (round 13, gotestsum: "testing.tRunner.func1.2").
    if let Some(p) = kept.iter().position(|l| l.starts_with("panic: ")) {
        let mut m = vec![kept[p]];
        if let Some(frame) = kept[p..].iter().find(|l| l.contains("_test.go:")) {
            m.push(frame);
        }
        return Some(m.join("\n"));
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
        if self.seen.contains_key(&d)
            || self
                .pytest_text_pass
                .as_ref()
                .is_some_and(|(_, k)| k.unique() == d)
        {
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
        self.seen_keys.insert(key.unique(), key.clone());
        self.seen.insert(key.unique(), (key.display(), outcome));
        out.push(EventKind::TestFinished {
            test: key,
            attempt,
            outcome,
            duration_ms,
            message,
            source: Level::Native,
            subtest: false,
        });
    }

    /// An explicit later (or earlier) attempt of a test: the retry's result
    /// replaces the earlier one (ADR 0024: the last attempt decides), and a
    /// pass after a failure is flaky.
    #[allow(clippy::too_many_arguments)]
    fn attempt_result(
        &mut self,
        key: TestKey,
        attempt: u32,
        outcome: Outcome,
        duration_ms: Option<u64>,
        message: Option<String>,
        level: Level,
        out: &mut Vec<EventKind>,
    ) {
        self.seen_keys.insert(key.unique(), key.clone());
        self.seen.insert(key.unique(), (key.display(), outcome));
        lift(&mut self.level, level);
        out.push(EventKind::TestFinished {
            test: key,
            attempt,
            outcome,
            duration_ms,
            message,
            source: level,
            subtest: false,
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
        self.result(key, outcome, duration_ms, message, level, false, out);
    }

    /// A test's or a subtest's result. Only the recognizer knows what is a
    /// subtest (Go: a `/` in the name; pytest: a subtest report): a name
    /// rule applied to every framework took JUnit and xUnit display names
    /// for subtests (round 12 audit).
    #[allow(clippy::too_many_arguments)]
    fn result(
        &mut self,
        key: TestKey,
        outcome: Outcome,
        duration_ms: Option<u64>,
        message: Option<String>,
        level: Level,
        subtest: bool,
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
                subtest,
            });
            return;
        }
        // The text said it passed and held that one line: report it first.
        if self
            .pytest_text_pass
            .as_ref()
            .is_some_and(|(_, k)| k.unique() == d)
            && let Some((_, held)) = self.pytest_text_pass.take()
        {
            self.result(
                held,
                Outcome::Passed,
                None,
                None,
                Level::Recognized,
                false,
                out,
            );
        }
        // The result of a rerun's next attempt (pytest-rerunfailures).
        if let Some(n) = self.reruns.remove(&d) {
            return self.attempt_result(key, n + 1, outcome, duration_ms, message, level, out);
        }
        if let Some((_, before)) = self.seen.get(&d) {
            // A different result under the same name is another test (two
            // test binaries both with `tests::it_works`, round 12): keep it,
            // or a failure hides behind a pass.
            if before.is_failure() != outcome.is_failure() {
                let mut other = key.clone();
                other.name = format!("{} (second result)", key.name);
                return self.result(other, outcome, duration_ms, message, level, subtest, out);
            }
            // Already reported: a pytest summary line, or (libtest, level
            // Recognized with a binary known) the same binary run again.
            if self.libtest_binary.is_some() && key.file == self.libtest_binary {
                self.repeats += 1;
            }
            // The pytest summary line carries the message the verbose line
            // did not: keep it as the test's output.
            if before.is_failure()
                && outcome.is_failure()
                && let Some(m) = message
            {
                self.failure_fallback.push((key, m));
            }
            return;
        }
        if outcome.is_failure()
            && let Some(m) = &message
        {
            self.failure_fallback.push((key.clone(), m.clone()));
        }
        self.seen_keys.insert(d.clone(), key.clone());
        self.seen.insert(d, (key.display(), outcome));
        lift(&mut self.level, level);
        out.push(EventKind::TestFinished {
            test: key,
            attempt: 1,
            outcome,
            duration_ms,
            message,
            source: level,
            subtest,
        });
    }

    /// Framework summary lines: "test result: ok. 4 passed; 1 failed; 0 ignored"
    /// (libtest) or "1 failed, 4 passed in 1.52s" (pytest).
    fn totals(&mut self, line: &str) {
        // gotestsum: "DONE 3 tests, 1 skipped, 3 failures, 1 error in 0.004s".
        if let Some(rest) = line.trim().strip_prefix("DONE ")
            && let Some((counts, _)) = rest.rsplit_once(" in ")
            && counts.contains(" test")
        {
            let mut total = None;
            let mut failed = 0;
            let mut errors = 0;
            for part in counts.split(", ") {
                let mut w = part.split_whitespace();
                if let (Some(n), Some(word)) =
                    (w.next().and_then(|n| n.parse::<u64>().ok()), w.next())
                {
                    match word {
                        "test" | "tests" => total = Some(n),
                        "failure" | "failures" => failed += n,
                        "error" | "errors" => errors += n,
                        _ => {}
                    }
                }
            }
            if let Some(n) = total {
                self.summary_seen = true;
                self.gotestsum_seen = true;
                // `--rerun-fails` ends with `DONE 2 runs, 4 tests, ...`: the
                // tests of every run, reruns again. The first DONE is the
                // suite's count.
                if counts.contains(" runs")
                    && let Some(n) = counts
                        .split(", ")
                        .find_map(|p| p.strip_suffix(" runs")?.trim().parse::<u64>().ok())
                {
                    self.gts_runs = self.gts_runs.max(n);
                }
                if self.gts_done_seen && counts.contains(" runs") {
                    return;
                }
                self.gts_done_seen = true;
                self.reported_total += n;
                // A build error is listed among the failures too: the larger
                // of the two, so errors alone count and nothing counts twice.
                self.reported_failed += failed.max(errors);
                return;
            }
        }
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
        // VSTest console at normal verbosity: "Total tests: 14", then
        // "     Failed: 4" (Passed and Skipped lines add nothing).
        if let Some(n) = t.strip_prefix("Total tests:") {
            if let Ok(v) = n.trim().parse::<u64>() {
                self.summary_seen = true;
                self.reported_total += v;
                self.vstest_totals = true;
            }
            return;
        }
        if self.vstest_totals {
            if let Some(n) = t.strip_prefix("Failed:") {
                if let Ok(v) = n.trim().parse::<u64>() {
                    self.reported_failed += v;
                }
                self.vstest_totals = false;
            } else if t.starts_with("Total time:") {
                self.vstest_totals = false;
            }
            return;
        }
        // nextest "Summary [ 0.2s] 11 tests run: 8 passed, 3 failed, 2 skipped".
        // Its skipped count also covers tests a filter left out, which no
        // framework counts as tests: only those it named with SKIP lines
        // are added.
        if t.starts_with("Summary [")
            && let Some((_, rest)) = t.split_once("] ")
            && let Some((head, counts)) = rest.split_once(" run: ")
            && let Some(n) = head
                .split_whitespace()
                .next()
                .map(|w| w.rsplit('/').next().unwrap_or(w))
                .and_then(|w| w.parse::<u64>().ok())
            && (head.ends_with(" tests") || head.ends_with(" test"))
        {
            let mut skipped = 0;
            let mut failed = 0;
            for part in counts.split(',') {
                let mut w = part.split_whitespace();
                if let (Some(num), Some(word)) = (w.next(), w.next())
                    && let Ok(v) = num.parse::<u64>()
                {
                    match word {
                        "failed" | "timed" => failed += v,
                        "skipped" => skipped += v,
                        _ => {}
                    }
                }
            }
            self.summary_seen = true;
            self.reported_total += n + skipped.min(self.nextest_skip_lines);
            self.reported_failed += failed;
            return;
        }
        // Maven Surefire's aggregate "Tests run: 22, Failures: 4, Errors: 1,
        // Skipped: 2": the per-class lines also say "Time elapsed".
        if let Some(rest) = t
            .strip_prefix("[INFO] ")
            .or_else(|| t.strip_prefix("[ERROR] "))
            .or_else(|| t.strip_prefix("[WARNING] "))
            .unwrap_or(t)
            .strip_prefix("Tests run:")
            && !rest.contains("Time elapsed")
        {
            let field = |name: &str| {
                rest.split(',').find_map(|p| {
                    p.trim()
                        .strip_prefix(name)
                        .and_then(|v| v.trim().parse::<u64>().ok())
                })
            };
            if let Some(n) = field("") {
                self.summary_seen = true;
                self.reported_total += n;
                self.reported_failed +=
                    field("Failures:").unwrap_or(0) + field("Errors:").unwrap_or(0);
            }
            return;
        }
        // Vitest "Errors  1 error": unhandled errors no test owns.
        if let Some(rest) = t.strip_prefix("Errors ") {
            let mut w = rest.split_whitespace();
            if let (Some(n), Some("error" | "errors"), None) = (
                w.next().and_then(|n| n.parse::<u64>().ok()),
                w.next(),
                w.next(),
            ) {
                self.unhandled_errors += n;
                return;
            }
        }
        // Jest "Test Suites: 1 failed, 1 passed, 2 total"; Vitest "Test Files
        // 1 failed | 1 passed (2)": a file that fails to load fails no test.
        if let Some(rest) = t
            .strip_prefix("Test Suites:")
            .or_else(|| t.strip_prefix("Test Files "))
        {
            for part in rest.split([',', '|']) {
                let mut w = part.split_whitespace();
                if let (Some(num), Some("failed")) = (w.next(), w.next())
                    && let Ok(v) = num.parse::<u64>()
                {
                    self.suites_failed += v;
                }
            }
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
            // pytest's summary ends in its duration (`in 0.12s`): tsc's
            // `Found 1 error in src/calc.ts:4` is no test summary (round 16).
            None if t
                .trim_matches('=')
                .trim()
                .rsplit_once(" in ")
                .and_then(|(_, d)| d.split_whitespace().next())
                .is_some_and(|d| {
                    d.ends_with('s') && d.starts_with(|c: char| c.is_ascii_digit())
                })
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
            // pytest 9: "7 subtests passed" (failed subtests are in "failed").
            if let (Some(num), Some("subtests"), Some("passed" | "skipped")) = (
                part.split_whitespace().next(),
                part.split_whitespace().nth(1),
                part.split_whitespace().nth(2),
            ) && let Ok(v) = num.parse::<u64>()
            {
                n += v;
                continue;
            }
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
        self.structured_failed
            .unwrap_or(self.reported_failed.saturating_sub(self.go_recovered))
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
                && nextest_flaky(line).is_none()
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
        if let Some(lines) = self.unhandled_lines.as_mut() {
            let t = line.trim();
            let ends = (t.contains('\u{23af}') && !t.contains("Unhandled"))
                || t.starts_with("Test Files")
                || t.starts_with("Tests ")
                || lines.len() >= 60;
            if ends {
                self.flush_unhandled(&mut out);
            } else {
                lines.push(t.to_owned());
                return out;
            }
        }
        if !self.unhandled_block && line.contains("Unhandled Errors") && line.contains('\u{23af}') {
            self.unhandled_block = true;
            self.unhandled_lines = Some(Vec::new());
            return out;
        }
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
        self.go_settle_tentative(line, &mut out);
        if line.starts_with("ok  \t") || line.starts_with("FAIL\t") {
            if self.go_open.is_empty() {
                self.go_package_lines += 1;
                self.go_package(line, &mut out);
            } else {
                // A test is running: this may be its own output.
                self.go_tentative = Some((line.to_owned(), self.go_open.clone()));
            }
        } else if !line.starts_with('{') && !line.starts_with("?   \t") {
            self.go_segment_line(line);
        }
        // Go replays cached results without running tests: say so once.
        if !self.cache_noted
            && line.contains("(cached)")
            && (line.starts_with("ok") || line.contains("\"Output\":\"ok"))
        {
            self.cache_noted = true;
            out.push(EventKind::RunNote {
                message: "go replayed cached test results: these tests did not run again; add -count=1 to the go test command, or, when it is not yours to edit (make test), `inject = [\"go-nocache\"]` to the suite in prova.toml".into(),
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
                || (!self.gotestsum_stream && self.gotestsum(line, &mut out))
                || (self.gotestsum_stream && is_gotestsum_text(line))
                // gotestsum echoes `go test -v` lines for failing tests:
                // with its stream read, they are held like any streamed
                // framework's text (round 15: an unqualified test that
                // never finished).
                || (self.gotestsum_stream && streamed && is_go_text(line))
                || (self.gotestsum_stream
                    && !streamed
                    && self.hold(|r| r.go_verbose(line, &mut held)))
                || (!self.gotestsum_stream && self.go_verbose(line, &mut out))
                || (self.pytest_stream && !streamed && self.hold(|r| r.pytest(line, &mut held)))
                || (!self.pytest_stream && self.pytest(line, &mut out))
                || self.dotnet(line, &mut out)
                || self.surefire(line, &mut out)
                || self.jest(line, &mut out)
                || (self.vitest_stream && !streamed && self.hold(|r| r.vitest(line, &mut held)))
                || (!self.vitest_stream && self.vitest(line, &mut out))
                || self.tap(line, &mut out)
                || self.quint(line, &mut out);
            self.held_text_events = held;
        }
        out
    }

    /// The start of an unfinished output line. A pytest `-v` line shows the
    /// test's id, then its outcome once it ends: an id followed by a space
    /// and nothing else is a test running now (round 15: a stall named the
    /// test that had just passed). The previous test's held pass is
    /// reported first, as the next line would have.
    pub fn partial(&mut self, text: &str) -> Vec<EventKind> {
        let mut out = Vec::new();
        if self.pytest_stream
            || !text.ends_with(' ')
            || self.holding
            || !text.contains(".py::")
            || text.starts_with(' ')
        {
            return out;
        }
        let id = text.trim_end();
        // Whole parameters only: a read boundary can cut the id short, and
        // spaces are part of an id only inside its brackets.
        let outside = {
            let mut depth = 0i32;
            let mut s = String::new();
            for c in id.chars() {
                match c {
                    '[' => depth += 1,
                    ']' => depth -= 1,
                    _ if depth == 0 => s.push(c),
                    _ => {}
                }
            }
            (depth == 0).then_some(s)
        };
        if outside.is_none_or(|o| o.contains(char::is_whitespace)) {
            return out;
        }
        let key = pytest_key(id);
        if self
            .pytest_text_pass
            .as_ref()
            .is_some_and(|(held, _)| held != id)
        {
            out.extend(self.release_held_pass());
        }
        self.started(key, &mut out);
        out
    }

    /// A pytest pass held for a teardown error, when nothing followed it.
    pub fn release_held_pass(&mut self) -> Vec<EventKind> {
        let mut out = Vec::new();
        if let Some((_, key)) = self.pytest_text_pass.take() {
            self.finished(
                key,
                Outcome::Passed,
                None,
                None,
                Level::Recognized,
                &mut out,
            );
        }
        out
    }

    /// The entry for a Vitest unhandled-errors block: named by the file it
    /// originated in (`src/x.test.ts::(unhandled error)`), else by Vitest's
    /// own name; its message is the error itself.
    fn flush_unhandled(&mut self, out: &mut Vec<EventKind>) {
        let Some(lines) = self.unhandled_lines.take() else {
            return;
        };
        let origin = lines.iter().find_map(|l| {
            let rest = l.split_once("originated in \"")?.1;
            rest.split_once('"').map(|(f, _)| f.to_owned())
        });
        let error = lines
            .iter()
            .map(|l| l.trim())
            .find(|l| {
                !l.is_empty()
                    && !l.contains('\u{23af}')
                    && !l.starts_with("Vitest caught")
                    && !l.starts_with("This might cause")
            })
            .map(str::to_owned);
        let key = match origin {
            Some(f) => TestKey::new(vec![f], "(unhandled error)"),
            None => TestKey::new(vec!["vitest".to_owned()], "(unhandled errors)"),
        };
        if !lines.is_empty() {
            self.failure_output.push((key.clone(), lines.join("\n")));
        }
        self.finished(
            key,
            Outcome::Errored,
            None,
            Some(error.unwrap_or_else(|| {
                "Vitest caught unhandled errors during the run (see its output)".to_owned()
            })),
            Level::Recognized,
            out,
        );
    }

    fn hold(&mut self, f: impl FnOnce(&mut Self) -> bool) -> bool {
        self.holding = true;
        let r = f(self);
        self.holding = false;
        r
    }

    // ------------------------------------------------------------- gotestsum

    /// gotestsum's text formats. `pkgname` (its default) prints only the
    /// failures, at the end: `=== FAIL: p1 TestBad (0.00s)` then the
    /// test's output, `=== FAIL: p2  (0.00s)` for a package that failed
    /// without a test (a build error). `testname`: `PASS p1.TestOk
    /// (0.00s)`, `FAIL p1.TestBad (0.00s)`, `FAIL p2` (round 13: prova saw
    /// none of it). Names match `go test -v`'s (the package is relative).
    fn gotestsum(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        let t = line.trim_end();
        // `pkgname`'s package status lines: `✓  pkg (2ms)`, `✖  pkg (1.5s)`.
        for (mark, passed) in [('\u{2713}', true), ('\u{2716}', false)] {
            if let Some(rest) = t.strip_prefix(mark)
                && let Some(pkg) = rest.split_whitespace().next()
                && rest.starts_with("  ")
            {
                let pkg = self.go_pkg(pkg);
                self.gts_pkg_last.insert(pkg, passed);
                self.gotestsum_seen = true;
                return true;
            }
        }
        // A failing `testname` line is followed by what the test printed.
        if self.gts_inline && self.gts_pending.is_some() {
            if line.starts_with([' ', '\t']) {
                if let Some((_, _, _, lines)) = self.gts_pending.as_mut() {
                    lines.push(t.to_owned());
                }
                return true;
            }
            self.flush_gotestsum(out);
        }
        if t.starts_with("=== ") || t.starts_with("DONE ") {
            self.flush_gotestsum(out);
        }
        for (prefix, outcome) in [
            ("=== FAIL: ", Outcome::Failed),
            ("=== SKIP: ", Outcome::Skipped),
        ] {
            if let Some(rest) = t.strip_prefix(prefix) {
                let (rest, _) = strip_rerun(rest);
                let rest = rest.as_str();
                let (head, dur) = match rest.rsplit_once(" (") {
                    Some((h, d)) => (h, secs_ms(d.trim_end_matches(')'))),
                    None => (rest, None),
                };
                let (pkg, test) = head.split_once(' ').unwrap_or((head, ""));
                let pkg = self.go_pkg(pkg);
                let (key, outcome) = if test.trim().is_empty() {
                    (TestKey::new(vec![pkg], "(package)"), Outcome::Errored)
                } else {
                    (TestKey::new(vec![pkg], test.trim()), outcome)
                };
                self.gotestsum_seen = true;
                self.gts_inline = false;
                self.gts_pending = Some((key, outcome, dur, Vec::new()));
                return true;
            }
        }
        if t == "=== Failed" || t == "=== Errors" || t == "=== Skipped" {
            self.gotestsum_seen = true;
            return true;
        }
        if let Some((_, _, _, lines)) = self.gts_pending.as_mut() {
            lines.push(t.to_owned());
            return true;
        }
        // testname: "PASS p1.TestOk (0.00s)", "FAIL p2", a rerun: "PASS
        // p1.TestOk (re-run 1)", a package that did not build: "FAIL p2
        // [build failed]".
        if let Some(pkg) = t.strip_prefix("FAIL ").and_then(|r| {
            r.strip_suffix(" [build failed]")
                .or(r.strip_suffix(" [setup failed]"))
        }) && !pkg.contains(' ')
        {
            self.gts_pkg_failed.push(pkg.to_owned());
            return false;
        }
        let (stripped, rerun) = strip_rerun(t);
        let mut w = stripped.split_whitespace();
        let (Some(word), Some(name)) = (w.next(), w.next()) else {
            return false;
        };
        let dur = w
            .next()
            .map(|d| d.trim_matches(['(', ')']))
            .and_then(secs_ms);
        if w.next().is_some() {
            return false;
        }
        let outcome = match word {
            "PASS" => Outcome::Passed,
            "FAIL" => Outcome::Failed,
            "SKIP" => Outcome::Skipped,
            "EMPTY" => return false,
            _ => return false,
        };
        // The module's root package is printed with no package at all:
        // `PASS TestAdd (0.00s)` is a test of `.` (round 15: its passes were
        // dropped and `FAIL TestDiv` read as a package).
        let root_test = dur.is_some()
            && ["Test", "Example", "Fuzz", "Benchmark"]
                .iter()
                .any(|k| name.starts_with(k));
        let rooted;
        let name = if root_test {
            rooted = format!("..{name}");
            rooted.as_str()
        } else {
            name
        };
        let split = ["Test", "Example", "Fuzz", "Benchmark"]
            .iter()
            .filter_map(|k| name.find(&format!(".{k}")))
            .min();
        match split {
            Some(i) => {
                let test = &name[i + 1..];
                self.gotestsum_seen = true;
                self.gts_testname_lines += 1;
                if outcome.is_failure() {
                    self.gts_pkg_with_failure.insert(name[..i].to_owned());
                }
                let pkg = self.go_pkg(&name[..i]);
                // The `--- PASS: TestOk` line before it is this result.
                if self.go_qualify_one(test, &pkg, out) {
                    return true;
                }
                let key = TestKey::new(vec![pkg], test);
                if outcome.is_failure() && !self.seen.contains_key(&key.unique()) {
                    // What the test printed follows: hold it for that.
                    self.gts_inline = true;
                    self.gts_pending = Some((key, outcome, dur, Vec::new()));
                } else {
                    let sub = test.contains('/');
                    self.go_result(key, outcome, dur, None, Level::Recognized, sub, rerun, out);
                }
                true
            }
            None if outcome == Outcome::Failed && !name.contains("::") => {
                // A package line: decided at the end.
                self.gts_pkg_failed.push(name.to_owned());
                false
            }
            None => false,
        }
    }

    fn flush_gotestsum(&mut self, out: &mut Vec<EventKind>) {
        if let Some((key, outcome, dur, lines)) = self.gts_pending.take() {
            let body: Vec<String> = lines
                .iter()
                .rev()
                .skip_while(|l| l.trim().is_empty())
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .cloned()
                .collect();
            let msg = if outcome.is_failure() {
                if !body.is_empty() {
                    self.failure_output.push((key.clone(), body.join("\n")));
                }
                message(&body)
            } else {
                None
            };
            // The package may have been printed in full since the header.
            let key = match key.path.as_slice() {
                [pkg] => TestKey::new(vec![self.go_pkg(pkg)], key.name.clone()),
                _ => key,
            };
            let sub = key.name.contains('/');
            if self.gts_inline {
                // A `testname` result line: an execution of the test.
                self.gts_inline = false;
                self.go_result(key, outcome, dur, msg, Level::Recognized, sub, false, out);
            } else if self.seen.contains_key(&key.unique()) {
                // The summary block repeats a result already reported: it
                // carries the output, not another execution.
                if let Some(m) = msg {
                    self.failure_fallback.push((key, m));
                }
            } else {
                if outcome.is_failure() {
                    self.gts_failed.push(key.clone());
                }
                self.result(key, outcome, dur, msg, Level::Recognized, sub, out);
            }
        }
    }

    /// gotestsum reran its failures and the last run of their package was
    /// green: each passed on a rerun (it exits non-zero if any still fails).
    fn gotestsum_reruns_passed(&mut self, out: &mut Vec<EventKind>) {
        if self.gts_runs < 2 {
            return;
        }
        for key in std::mem::take(&mut self.gts_failed) {
            let green = key
                .path
                .first()
                .and_then(|p| self.gts_pkg_last.get(p))
                .copied()
                .unwrap_or(false);
            if green {
                let sub = key.name.contains('/');
                self.go_result(
                    key,
                    Outcome::Passed,
                    None,
                    None,
                    Level::Recognized,
                    sub,
                    true,
                    out,
                );
            }
        }
    }

    // ------------------------------------------------------------- go json

    fn go_json(&mut self, v: &serde_json::Value, out: &mut Vec<EventKind>) {
        // Go 1.24+: the compiler's errors come as `build-output` events
        // keyed by ImportPath ("pkg [pkg.test]"); they explain the
        // package's `FailedBuild` failure.
        if v["Action"] == "build-output"
            && let (Some(path), Some(o)) = (v["ImportPath"].as_str(), v["Output"].as_str())
        {
            let pkg = path.split(' ').next().unwrap_or(path);
            if !o.starts_with("# ") {
                self.go_output
                    .entry(format!("{pkg}\0"))
                    .or_default()
                    .push(o.trim_end().to_owned());
            }
            return;
        }
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
                // A test that passed with every subtest skipped ran nothing.
                if let Some(&(skipped, other)) = self.go_subs.get(&format!("{pkg}\0{t}"))
                    && outcome == Outcome::Passed
                    && skipped > 0
                    && other == 0
                {
                    let name = format!("{pkg}::{t}");
                    if !self.go_all_skipped.contains(&name) {
                        self.go_all_skipped.push(name);
                    }
                }
                if let Some((parent, _)) = t.rsplit_once('/') {
                    let e = self.go_subs.entry(format!("{pkg}\0{parent}")).or_default();
                    if outcome == Outcome::Skipped {
                        e.0 += 1;
                    } else {
                        e.1 += 1;
                    }
                }
                // Go test names cannot contain '/': one that does is a
                // subtest (`t.Run`).
                // A repeated execution (`-count=N`, gotestsum's reruns) is
                // another attempt of the test.
                self.go_result(
                    key(t),
                    outcome,
                    elapsed,
                    msg,
                    Level::Native,
                    t.contains('/'),
                    self.go_pkg_done.contains(pkg),
                    out,
                );
            }
            ("pass" | "skip", None) => {
                self.go_pkg_done.insert(pkg.to_owned());
            }
            ("fail", None) => {
                self.go_pkg_done.insert(pkg.to_owned());
                // Package failed without any failing test: build error or crash.
                let prefix = format!("{pkg}\0");
                let any_failed = self.go_failed_pkgs.contains(pkg)
                    || self
                        .seen
                        .values()
                        .any(|(d, o)| d.starts_with(&format!("{pkg}::")) && o.is_failure());
                let lines = self.go_output.remove(&prefix).unwrap_or_default();
                let crashed = lines.iter().any(|l| go_crash_line(l));
                if !any_failed || crashed {
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
            // The Vitest reporter names a test file that failed to load
            // `(file)`: an error of the file, like Go's `(package)`.
            "failed" if key.name == "(file)" => Outcome::Errored,
            "failed" => Outcome::Failed,
            "ignored" => Outcome::Skipped,
            _ => return, // "timeout" is a warning; the test is still running
        };
        let duration = v["exec_time"].as_f64().map(|s| (s * 1000.0).round() as u64);
        let msg = if outcome.is_failure() {
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
        // pytest 9 names the class `SubtestReport`; reportlog plugins and
        // older previews wrote `SubTestReport`.
        if v["$report_type"] == "SubTestReport" || v["$report_type"] == "SubtestReport" {
            return self.reportlog_subtest(v, out);
        }
        // A held passing call is decided by its own teardown report; any
        // other report ends the wait.
        let teardown_of_held = v["$report_type"] == "TestReport"
            && v["when"] == "teardown"
            && self
                .pytest_call_pass
                .as_ref()
                .is_some_and(|(held, _, _)| v["nodeid"].as_str() == Some(held));
        if !teardown_of_held {
            self.flush_call_pass(out);
        }
        if v["$report_type"] == "CollectReport" {
            return self.reportlog_collect(v, out);
        }
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
            ("call", "passed") => {
                let duration = v["duration"].as_f64().map(|s| (s * 1000.0).round() as u64);
                self.pytest_call_pass = Some((id.to_owned(), key, duration));
                return;
            }
            // pytest-rerunfailures: this attempt failed and will run again.
            (_, "rerun") => {
                let n = self.reruns.get(&key.unique()).copied().unwrap_or(0) + 1;
                self.reruns.insert(key.unique(), n);
                let msg = reportlog_text(&v["longrepr"]);
                self.attempt_result(
                    key,
                    n,
                    Outcome::Failed,
                    None,
                    Some(msg).filter(|m| !m.is_empty()),
                    Level::Native,
                    out,
                );
                return;
            }
            ("call", "failed") => Outcome::Failed,
            (_, "skipped") => Outcome::Skipped,
            ("teardown", "failed") => Outcome::Errored,
            ("teardown", _) => return self.flush_call_pass(out),
            _ => return,
        };
        // A teardown error on a passed call: the one test errored.
        if when == "teardown" {
            self.pytest_call_pass = None;
        }
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

    /// The passing call held for its teardown report is a pass.
    fn flush_call_pass(&mut self, out: &mut Vec<EventKind>) {
        if let Some((_, key, duration)) = self.pytest_call_pass.take() {
            self.finished(key, Outcome::Passed, duration, None, Level::Native, out);
        }
    }

    /// A pytest collection error (an import error, a syntax error): the
    /// file is the test that errored, named like a Go package.
    fn reportlog_collect(&mut self, v: &serde_json::Value, out: &mut Vec<EventKind>) {
        let Some(id) = v["nodeid"].as_str().filter(|i| !i.is_empty()) else {
            return;
        };
        if v["outcome"] != "failed" {
            return;
        }
        lift(&mut self.level, Level::Native);
        let key = TestKey::new(vec![id.to_owned()], "(file)");
        let text = reportlog_text(&v["longrepr"]);
        if !text.is_empty() {
            self.failure_output.push((key.clone(), text.clone()));
        }
        let msg = v["longrepr"]["reprcrash"]["message"]
            .as_str()
            .map(|m| m.trim().to_owned())
            .or_else(|| text.lines().last().map(str::to_owned));
        self.finished(key, Outcome::Errored, None, msg, Level::Native, out);
    }

    /// pytest 9 subtests (`subtests.test(...)`, unittest's `subTest`): one
    /// report per subtest, labelled like pytest's own output.
    fn reportlog_subtest(&mut self, v: &serde_json::Value, out: &mut Vec<EventKind>) {
        let (Some(id), Some(result)) = (v["nodeid"].as_str(), v["outcome"].as_str()) else {
            return;
        };
        lift(&mut self.level, Level::Native);
        let ctx = &v["_subtest.context"];
        let mut label = String::new();
        if let Some(m) = ctx["msg"].as_str() {
            label.push_str(&format!("[{m}] "));
        }
        if let Some(kw) = ctx["kwargs"].as_object()
            && !kw.is_empty()
        {
            let args: Vec<String> = kw
                .iter()
                .map(|(k, x)| format!("{k}={}", x.as_str().map_or(x.to_string(), str::to_owned)))
                .collect();
            label.push_str(&format!("({})", args.join(", ")));
        }
        let outcome = match result {
            "passed" => Outcome::Passed,
            "failed" => Outcome::Failed,
            "skipped" => Outcome::Skipped,
            _ => return,
        };
        let parent = pytest_key(id);
        let label = if label.trim().is_empty() {
            // No context to tell subtests apart: by order within the
            // parent, so none is merged into another or into the parent.
            let n = self.subtest_counts.entry(parent.unique()).or_insert(0);
            *n += 1;
            format!("(subtest {n})")
        } else {
            label
        };
        let key = pytest_subtest(parent, label.trim());
        let longrepr = &v["longrepr"];
        let msg = if outcome.is_failure() {
            let text = reportlog_text(longrepr);
            if !text.is_empty() {
                self.failure_output.push((key.clone(), text));
            }
            longrepr["reprcrash"]["message"]
                .as_str()
                .or(longrepr.as_str())
                .map(|m| m.trim().to_owned())
        } else {
            None
        };
        self.result(key, outcome, None, msg, Level::Native, true, out);
    }

    // ------------------------------------------------------------- go -v

    fn go_verbose(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        let t = line.trim_start();
        // Parallel tests interleave: `=== NAME` / `=== CONT` say whose
        // output follows.
        for word in ["=== NAME", "=== CONT", "=== PAUSE"] {
            if let Some(name) = t.strip_prefix(word)
                && name.starts_with(' ')
            {
                if word != "=== PAUSE" && !name.trim().is_empty() {
                    self.go_current = Some(name.trim().to_owned());
                }
                return true;
            }
        }
        if let Some(name) = t.strip_prefix("=== RUN") {
            self.go_run_lines += 1;
            let name = name.trim().to_owned();
            self.go_current = Some(name.clone());
            self.go_open.insert(name.clone());
            let key = self.go_key(&name);
            self.started(key, out);
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
                self.go_open.remove(name);
                // Without `-v` a failure's message follows its line; with
                // it, only a panic does.
                if outcome == Outcome::Failed && !line.starts_with(char::is_whitespace) {
                    self.go_current = Some(name.to_owned());
                    self.go_after_fail = Some(name.to_owned());
                }
                let lines = self
                    .go_output
                    .remove(&format!("v\0{name}"))
                    .unwrap_or_default();
                let msg = if outcome == Outcome::Failed {
                    message(&lines)
                } else {
                    None
                };
                // With gotestsum's own stream the same tests come qualified.
                if !self.gotestsum_stream {
                    let ms = dur.map(|s| (s * 1000.0).round() as u64);
                    self.go_bare(name, outcome, ms, msg, out);
                }
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
            // Text says `name - should panic`, JSON and nextest say `name`:
            // one test, one history.
            let name = name.trim();
            let name = name.strip_suffix(" - should panic").unwrap_or(name);
            let key = self.libtest_key(name);
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
        // pytest-xdist: "[gw0] [ 33%] FAILED tests/test_a.py::test_q" is
        // the verbose line with the outcome first. Read it as pytest's own.
        if let Some(rest) = line.strip_prefix("[gw")
            && let Some((_, rest)) = rest.split_once("] ")
        {
            let rest = rest.trim_start();
            let rest = match rest.strip_prefix('[') {
                Some(r) => r.split_once("] ").map_or(rest, |(_, r)| r),
                None => rest,
            };
            if let Some((word, id)) = rest.trim().split_once(' ')
                && matches!(
                    word,
                    "PASSED" | "FAILED" | "SKIPPED" | "ERROR" | "XFAIL" | "XPASS"
                )
                && id.contains(".py::")
            {
                return self.pytest(&format!("{} {word}", id.trim()), out);
            }
            return false;
        }
        self.pytest_section(line);
        // A pass held one line: pytest prints a teardown error as a second
        // line for the same test, and that is the test's result.
        if let Some((id, hkey)) = self.pytest_text_pass.take() {
            let errored = line
                .trim_start()
                .strip_prefix(id.as_str())
                .is_some_and(|r| r.trim_start().starts_with("ERROR"));
            if errored {
                self.finished(hkey, Outcome::Errored, None, None, Level::Recognized, out);
                return true;
            }
            self.finished(hkey, Outcome::Passed, None, None, Level::Recognized, out);
        }
        // A collection error: "_____ ERROR collecting tests/test_e.py _____".
        if let Some(f) = line
            .trim()
            .trim_matches('_')
            .trim()
            .strip_prefix("ERROR collecting ")
            && is_pytest_file(f)
        {
            self.result(
                TestKey::new(vec![f.trim().to_owned()], "(file)"),
                Outcome::Errored,
                None,
                None,
                Level::Recognized,
                false,
                out,
            );
            return true;
        }
        // Summary lines (`-rA`, and failures always):
        //   "FAILED tests/test_x.py::test_b - AssertionError: boom"
        //   "SUBFAILED[case] (i=2) tests/test_x.py::test_b - assert 2 != 2"
        for (word, outcome) in [
            ("FAILED", Outcome::Failed),
            ("ERROR", Outcome::Errored),
            ("PASSED", Outcome::Passed),
            ("XPASS", Outcome::Passed),
            ("XFAIL", Outcome::Skipped),
            ("SUBFAILED", Outcome::Failed),
            ("SUBPASSED", Outcome::Passed),
        ] {
            let Some(rest) = line.strip_prefix(word) else {
                continue;
            };
            let sub = word.starts_with("SUB");
            if !sub && !rest.starts_with(' ') {
                continue;
            }
            let (left, msg) = match split_summary(rest) {
                Some((i, m)) => (i, Some(m.trim().to_owned())),
                None => (rest, None),
            };
            // "ERROR tests/test_e.py - ImportError": a collection error.
            if word == "ERROR" && !left.contains(".py::") {
                let f = left.trim();
                let f = f.strip_prefix("collecting ").unwrap_or(f).trim();
                if is_pytest_file(f) {
                    let msg = msg.filter(|m| !m.is_empty());
                    self.result(
                        TestKey::new(vec![f.to_owned()], "(file)"),
                        Outcome::Errored,
                        None,
                        msg,
                        Level::Recognized,
                        false,
                        out,
                    );
                    return true;
                }
            }
            // The id is the word with ".py::" (a subtest's label comes first).
            let Some(at) = left.find(".py::") else {
                continue;
            };
            let start = left[..at].rfind(' ').map_or(0, |i| i + 1);
            let id = left[start..].trim();
            let label = left[..start].trim();
            let key = pytest_key(id);
            let (key, sub) = if sub {
                (pytest_subtest(key, label), true)
            } else {
                (key, false)
            };
            let msg = msg.filter(|_| outcome.is_failure());
            if outcome.is_failure() && !sub {
                self.claim_pytest_block(&key);
            }
            // A teardown error after a pass the output already reported:
            // the test is errored, once (round 15: it was another test).
            if outcome == Outcome::Errored
                && matches!(self.seen.get(&key.unique()), Some((_, Outcome::Passed)))
            {
                self.attempt_result(key, 2, outcome, None, msg, Level::Recognized, out);
                return true;
            }
            self.result(key, outcome, None, msg, Level::Recognized, sub, out);
            return true;
        }
        // Verbose: "tests/test_x.py::test_a[g h] PASSED  [ 50%]",
        // "tests/test_x.py::test_c SUBPASSED[case] (i=0)  [100%]". An id may
        // contain spaces: the status word decides where it ends.
        if !line.contains(".py::") {
            return false;
        }
        const STATUS: [(&str, Outcome); 13] = [
            // pytest-rerunfailures: a failed attempt that runs again.
            ("RERUN", Outcome::Failed),
            ("SUBPASSED", Outcome::Passed),
            ("SUBFAILED", Outcome::Failed),
            ("SUBSKIPPED", Outcome::Skipped),
            // Older pytest-subtests: `test_x [case 2] SUBFAIL` (round 16:
            // the failure was attributed to no test).
            ("SUBPASS", Outcome::Passed),
            ("SUBFAIL", Outcome::Failed),
            ("SUBSKIP", Outcome::Skipped),
            ("PASSED", Outcome::Passed),
            ("FAILED", Outcome::Failed),
            ("SKIPPED", Outcome::Skipped),
            ("ERROR", Outcome::Errored),
            // As pytest's own reports: an expected failure is skipped, an
            // unexpected (non-strict) pass is a pass. A strict XPASS prints
            // FAILED.
            ("XFAIL", Outcome::Skipped),
            ("XPASS", Outcome::Passed),
        ];
        let mut found: Option<(usize, &str, Outcome)> = None;
        for (word, outcome) in STATUS {
            for (i, _) in line.match_indices(&format!(" {word}")) {
                let after = &line[i + 1 + word.len()..];
                let bounded = after.is_empty()
                    || after.starts_with([' ', '[', '('])
                    || word.starts_with("SUB");
                // Never inside the id's own parameters.
                if bounded && line[..i].contains(".py::") && found.is_none_or(|f| i > f.0) {
                    found = Some((i, word, outcome));
                }
            }
        }
        let Some((i, word, outcome)) = found else {
            return false;
        };
        let id = line[..i].trim();
        if id.contains(' ') && !id.contains('[') {
            return false; // not an id: prose that mentions a test file
        }
        // Older pytest-subtests prints the subtest's label before the word.
        if matches!(word, "SUBPASS" | "SUBFAIL" | "SUBSKIP")
            && let Some((parent, label)) = id.rsplit_once(" [")
        {
            let key = pytest_subtest(pytest_key(parent.trim()), &format!("[{label}"));
            self.result(key, outcome, None, None, Level::Recognized, true, out);
            return true;
        }
        let key = pytest_key(id);
        if word == "RERUN" {
            let n = self.reruns.get(&key.unique()).copied().unwrap_or(0) + 1;
            self.reruns.insert(key.unique(), n);
            self.attempt_result(key, n, Outcome::Failed, None, None, Level::Recognized, out);
            return true;
        }
        if word.starts_with("SUB") {
            let rest = &line[i + 1 + word.len()..];
            // Drop the progress column: "[100%]".
            let label = match rest.trim_end().rsplit_once(" [") {
                Some((l, p)) if p.ends_with("%]") => l,
                _ => rest,
            };
            let key = pytest_subtest(key, label.trim());
            self.result(key, outcome, None, None, Level::Recognized, true, out);
            return true;
        }
        // Failures are recorded now to keep first-failure fast; the message
        // only appears later in the summary section. A pass waits one line:
        // a teardown error follows it as a second line for the same test.
        if outcome == Outcome::Passed && !self.holding && !self.seen.contains_key(&key.unique()) {
            self.pytest_text_pass = Some((id.to_owned(), key));
            return true;
        }
        self.finished(key, outcome, None, None, Level::Recognized, out);
        true
    }

    /// Reads pytest's FAILURES / ERRORS section into blocks, one per
    /// `____ test_q ____` header, to be named by the summary that follows.
    fn pytest_section(&mut self, line: &str) {
        let t = line.trim_end();
        let bar = t.trim_matches('=').trim();
        if t.starts_with("==") && matches!(bar, "FAILURES" | "ERRORS") {
            self.pytest_in_failures = true;
            self.pytest_blocks.clear();
            return;
        }
        if !self.pytest_in_failures {
            return;
        }
        if t.starts_with('=') {
            self.pytest_in_failures = false;
            return;
        }
        let head = t.trim_matches('_').trim();
        if t.starts_with("___") && t.ends_with("___") && !head.is_empty() {
            let head = head.strip_prefix("ERROR at teardown of ").unwrap_or(head);
            let head = head.strip_prefix("ERROR at setup of ").unwrap_or(head);
            self.pytest_blocks.push((head.to_owned(), Vec::new()));
        } else if let Some((_, lines)) = self.pytest_blocks.last_mut()
            && lines.len() < 80
        {
            lines.push(t.to_owned());
        }
    }

    /// The failure block pytest printed for this test, kept as its output.
    fn claim_pytest_block(&mut self, key: &TestKey) {
        let Some(at) = self
            .pytest_blocks
            .iter()
            .position(|(h, _)| h == &key.name || h.ends_with(&format!(".{}", key.name)))
        else {
            return;
        };
        let (_, lines) = self.pytest_blocks.remove(at);
        let text = lines.join("\n").trim().to_owned();
        if !text.is_empty() {
            self.failure_output.push((key.clone(), text));
        }
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
        // "FAIL  src/b.test.ts [ src/b.test.ts ]": the file failed to load
        // (an import error); no test of it ran.
        if let Some(r) = t.strip_prefix("FAIL ")
            && let Some((f, bracket)) = r.trim().split_once(" [ ")
            && bracket.trim_end().trim_end_matches(']').trim() == f.trim()
            && is_js_file(f.trim())
        {
            let key = TestKey::new(vec![f.trim().to_owned()], "(file)");
            self.finished(
                key,
                Outcome::Errored,
                None,
                Some("the test file failed to load or run".to_owned()),
                Level::Recognized,
                out,
            );
            return true;
        }
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

    // ------------------------------------------------------------- surefire

    /// Maven's summary of failures, printed after the run:
    ///   `[ERROR] Failures: ` / `[ERROR] Errors: `, then
    ///   `[ERROR]   CalcTest.divides:15 expected:<2> but was:<3>` and
    ///   `[ERROR]   CalcTest.io \u{bb} IO disk full`.
    /// The result files may name only some of the classes (a fork that
    /// died, a partial report): what the console names counts too.
    fn surefire(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        let line = line.trim_end();
        let Some(body) = line
            .strip_prefix("[ERROR] ")
            .or_else(|| line.strip_prefix("[WARNING] "))
            .or_else(|| line.strip_prefix("[INFO] "))
        else {
            self.surefire_flush(out);
            self.surefire_section = None;
            return false;
        };
        let head = body.trim();
        let header = match head {
            "Failures:" => Some(SurefireSection::Failures),
            "Errors:" => Some(SurefireSection::Errors),
            "Flakes:" => Some(SurefireSection::Flakes),
            _ => None,
        };
        if header.is_some() {
            self.surefire_flush(out);
            self.surefire_section = header;
            return true;
        }
        let Some(section) = self.surefire_section else {
            return false;
        };
        let class_ok = |c: &str| {
            !c.is_empty()
                && c.chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '$'))
        };
        // `Run 1: Class.method:12 expected...` / `Run 2: PASS`: a rerun's
        // attempts, under the test's own (unindented) name.
        if let Some(run) = head.strip_prefix("Run ")
            && let Some((n, rest)) = run.split_once(':')
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
            && let Some((_, _, runs)) = self.surefire_rerun.as_mut()
        {
            let rest = rest.trim();
            let (outcome, msg) = if rest == "PASS" {
                (Outcome::Passed, None)
            } else {
                let outcome = if section == SurefireSection::Errors {
                    Outcome::Errored
                } else {
                    Outcome::Failed
                };
                (outcome, Some(rest.to_owned()).filter(|m| !m.is_empty()))
            };
            runs.push((outcome, msg));
            return true;
        }
        self.surefire_flush(out);
        if !body.starts_with("  ") {
            // A rerun's test: its full name alone on the line.
            if let Some((class, method)) = head.rsplit_once('.')
                && class_ok(class)
                && class_ok(method)
            {
                self.surefire_rerun = Some((
                    TestKey::new(vec![class.to_owned()], method),
                    section,
                    Vec::new(),
                ));
                return true;
            }
            self.surefire_section = None;
            return false;
        }
        if section == SurefireSection::Flakes {
            return false;
        }
        let (token, rest) = head.split_once(' ').unwrap_or((head, ""));
        let (id, has_line) = match token.rsplit_once(':') {
            Some((id, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => (id, true),
            _ => (token, false),
        };
        if !has_line && !rest.starts_with('\u{bb}') {
            return false;
        }
        let Some((class, method)) = id.rsplit_once('.') else {
            return false;
        };
        if !class_ok(class) || method.is_empty() {
            return false;
        }
        let msg = rest.trim().trim_start_matches('\u{bb}').trim();
        // The section says what kind of entry it is: `Errors:` lists
        // errors whether or not the line has a `\u{bb}` (round 15).
        let outcome = if rest.starts_with('\u{bb}') || section == SurefireSection::Errors {
            Outcome::Errored
        } else {
            Outcome::Failed
        };
        self.result(
            TestKey::new(vec![class.to_owned()], method),
            outcome,
            None,
            Some(msg.to_owned()).filter(|m| !m.is_empty()),
            Level::Recognized,
            false,
            out,
        );
        true
    }

    /// The rerun test being read ended: its attempts, in order. A test that
    /// failed and then passed is flaky; one that failed every time failed.
    fn surefire_flush(&mut self, out: &mut Vec<EventKind>) {
        let Some((key, _, runs)) = self.surefire_rerun.take() else {
            return;
        };
        match runs.as_slice() {
            [] => {}
            [(outcome, msg)] => self.result(
                key,
                *outcome,
                None,
                msg.clone(),
                Level::Recognized,
                false,
                out,
            ),
            _ => {
                for (i, (outcome, msg)) in runs.into_iter().enumerate() {
                    self.attempt_result(
                        key.clone(),
                        i as u32 + 1,
                        outcome,
                        None,
                        msg,
                        Level::Recognized,
                        out,
                    );
                }
            }
        }
    }

    // ------------------------------------------------------------- jest

    /// Jest's console reporter: ` PASS  src/a.test.js` / ` FAIL ...` headers,
    /// then with `--verbose` the describe blocks and `✓ name (2 ms)` /
    /// `✕ name` lines, and `● describe › name` failure blocks (also without
    /// `--verbose`, where they are the only per-test lines). A FAIL file in
    /// which no test failed ("● Test suite failed to run") is its own errored
    /// entry, `file::(file)`. Keys match Vitest's: path [file], name
    /// "describe > name".
    fn jest(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        let t = line.trim();
        // " PASS  src/a.test.js (5.2 s)": one path, no brackets or arrows.
        let header = t
            .strip_prefix("PASS ")
            .map(|r| (false, r))
            .or_else(|| t.strip_prefix("FAIL ").map(|r| (true, r)));
        if let Some((failed, rest)) = header {
            let rest = rest.trim();
            let rest = match rest.rsplit_once(" (") {
                Some((f, d)) if d.ends_with(" s)") || d.ends_with(" ms)") => f.trim_end(),
                _ => rest,
            };
            if !rest.contains(' ') && is_js_file(rest) {
                self.jest_close_file(out);
                self.jest_files += 1;
                self.jest.file = Some(rest.to_owned());
                self.jest.failed = failed;
                return true;
            }
        }
        let Some(file) = self.jest.file.clone() else {
            return false;
        };
        // The summary ends the file section.
        if [
            "Test Suites:",
            "Tests:",
            "Snapshots:",
            "Time:",
            "Ran all test",
        ]
        .iter()
        .any(|p| t.starts_with(p))
            || t.starts_with("Summary of all failing tests")
        {
            self.jest_close_file(out);
            return false;
        }
        let indent = line.len() - line.trim_start().len();
        if let Some(name) = t.strip_prefix("● ") {
            self.jest_close_block(&file, out);
            self.jest.console = None;
            let name = name.trim();
            self.jest.block = if name == "Test suite failed to run" {
                self.jest.suite_failed_block = true;
                Some((None, Vec::new()))
            } else if name.starts_with("Console") || name.starts_with("Obsolete") {
                None
            } else {
                Some((Some(name.replace(" › ", " > ")), Vec::new()))
            };
            return true;
        }
        if let Some((_, lines)) = self.jest.block.as_mut() {
            lines.push(t.to_owned());
            return true;
        }
        // console output looks like the tree (`✕ x`, `✓ y`, headings): it
        // runs from its `console.log` header to the stack line that ends it
        // (round 15). Nothing in it is a test.
        if matches!(
            t,
            "console.log"
                | "console.warn"
                | "console.error"
                | "console.info"
                | "console.debug"
                | "console.trace"
                | "console.table"
                | "console.dir"
        ) {
            self.jest.console = Some(false);
            return true;
        }
        match self.jest.console {
            Some(false) => {
                if t.starts_with("at ") {
                    self.jest.console = Some(true);
                }
                return true;
            }
            Some(true) if t.starts_with("at ") => return true,
            Some(true) => self.jest.console = None,
            None => {}
        }
        if t.is_empty() {
            return true;
        }
        let marker = [
            ('✓', Outcome::Passed),
            ('√', Outcome::Passed),
            ('✕', Outcome::Failed),
            ('×', Outcome::Failed),
            ('○', Outcome::Skipped),
            ('✎', Outcome::Skipped),
        ]
        .into_iter()
        .find(|(m, _)| t.starts_with(*m));
        while self.jest.stack.last().is_some_and(|(i, _)| *i >= indent) {
            self.jest.stack.pop();
        }
        let Some((m, outcome)) = marker else {
            // A describe heading (verbose output).
            if indent >= 2 {
                self.jest.stack.push((indent, t.to_owned()));
            }
            return true;
        };
        let mut name = t[m.len_utf8()..].trim().to_owned();
        let mut duration = None;
        if let Some((n, d)) = name.rsplit_once(" (")
            && let Some(ms) = d.strip_suffix(" ms)").and_then(|v| v.parse::<f64>().ok())
        {
            duration = Some(ms.round() as u64);
            name = n.trim_end().to_owned();
        }
        // " todo" tests print as "✎ todo name".
        let mut parts: Vec<String> = self.jest.stack.iter().map(|(_, n)| n.clone()).collect();
        parts.push(name);
        let key = TestKey::new(vec![file], parts.join(" > "));
        if outcome.is_failure() {
            // Held for its `●` block, which has the message.
            self.jest.named = true;
            self.jest.held.push((key, duration));
            return true;
        }
        self.finished(key, outcome, duration, None, Level::Recognized, out);
        true
    }

    /// A failure block ended: it is the output of a test (named now when the
    /// output was not verbose) or what a file said when it failed to load.
    fn jest_close_block(&mut self, file: &str, out: &mut Vec<EventKind>) {
        let Some((name, lines)) = self.jest.block.take() else {
            return;
        };
        let body: Vec<String> = lines.iter().skip_while(|l| l.is_empty()).cloned().collect();
        let Some(name) = name else {
            self.jest.suite_error = body;
            return;
        };
        self.jest.named = true;
        let key = TestKey::new(vec![file.to_owned()], name);
        let text = body.join("\n").trim().to_owned();
        if !text.is_empty() {
            self.failure_output.push((key.clone(), text));
        }
        let msg = jest_message(&body);
        let duration = self
            .jest
            .held
            .iter()
            .position(|(k, _)| *k == key)
            .and_then(|i| self.jest.held.remove(i).1);
        self.finished(key, Outcome::Failed, duration, msg, Level::Recognized, out);
    }

    /// The file's section ended. A FAIL file without a failing test failed
    /// to run: one errored entry for the file.
    fn jest_close_file(&mut self, out: &mut Vec<EventKind>) {
        let Some(file) = self.jest.file.clone() else {
            return;
        };
        self.jest_close_block(&file, out);
        for (key, duration) in std::mem::take(&mut self.jest.held) {
            self.finished(key, Outcome::Failed, duration, None, Level::Recognized, out);
        }
        // A suite that failed to run is its own entry, with or without a
        // failing test beside it (an `afterAll` that threw).
        if self.jest.suite_failed_block || (self.jest.failed && !self.jest.named) {
            let body = std::mem::take(&mut self.jest.suite_error);
            let key = TestKey::new(vec![file.clone()], "(file)");
            let text = body.join("\n").trim().to_owned();
            if !text.is_empty() {
                self.failure_output.push((key.clone(), text));
            }
            let msg = body
                .iter()
                .find(|l| !l.is_empty())
                .cloned()
                .or_else(|| Some("the test file failed to run".to_owned()));
            self.finished(key, Outcome::Errored, None, msg, Level::Recognized, out);
        }
        self.jest = JestState::default();
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
        // A name with spaces (a theory's `Slash(s: "x/y")`, MSTest's
        // `Slash ("x/y")`) only counts on VSTest's own two-space indent:
        // prose that starts with "Failed " is not a result.
        if name.is_empty()
            || (name.contains(char::is_whitespace)
                && (!line.starts_with("  ") || line.starts_with("   ")))
        {
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
            || t.starts_with("Skipped ")
            || t.starts_with("Total tests:")
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
        // "FLAKY 2/3 [ 0.003s] calc tests::x": the test passed on its 2nd of
        // 3 allowed attempts. The TRY lines may have shown it already.
        if let Some((n, dur, binary, test)) = nextest_flaky(line) {
            self.close_pass_output();
            self.flush_nextest(out);
            let (path, name) = split_rust_path(test);
            let key = TestKey {
                file: Some(nextest_binary(binary)),
                path,
                name,
            };
            let ms = nextest_ms(dur);
            if self.seen.get(&key.unique()).map(|(_, o)| *o) != Some(Outcome::Passed) {
                if !self.seen.contains_key(&key.unique()) {
                    self.attempt_result(
                        key.clone(),
                        1,
                        Outcome::Failed,
                        None,
                        None,
                        Level::Recognized,
                        out,
                    );
                }
                self.attempt_result(
                    key,
                    n.max(2),
                    Outcome::Passed,
                    ms,
                    None,
                    Level::Recognized,
                    out,
                );
            }
            return true;
        }
        let Some((status, dur, binary, test)) = nextest_status(line) else {
            return false;
        };
        self.close_pass_output();
        let attempt = nextest_try(line);
        let (path, name) = split_rust_path(test);
        let key = TestKey {
            file: Some(nextest_binary(binary)),
            path,
            name,
        };
        let ms = nextest_ms(dur);
        match status {
            // A retry that passed: another attempt of a test that failed.
            "PASS" | "LEAK"
                if attempt > 1
                    && self
                        .seen
                        .get(&key.unique())
                        .is_some_and(|(_, o)| o.is_failure()) =>
            {
                self.nextest_pass_output = Some((key.display(), false, false));
                self.attempt_result(
                    key,
                    attempt,
                    Outcome::Passed,
                    ms,
                    None,
                    Level::Recognized,
                    out,
                );
            }
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
            "SKIP" => {
                self.nextest_skip_lines += 1;
                self.finished(key, Outcome::Skipped, None, None, Level::Recognized, out)
            }
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
                    && !is_separator(l)
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
        let mut gts = Vec::new();
        self.flush_unhandled(&mut gts);
        self.go_confirm_tentative(&mut gts);
        self.go_flush_tails();
        self.go_note_all_skipped(&mut gts);
        self.flush_gotestsum(&mut gts);
        self.gotestsum_reruns_passed(&mut gts);
        if self.gotestsum_seen {
            for pkg in std::mem::take(&mut self.gts_pkg_failed) {
                if !self.gts_pkg_with_failure.contains(&pkg)
                    && !self.go_failed_pkgs.contains(&self.go_pkg(&pkg))
                {
                    let pkg = self.go_pkg(&pkg);
                    self.finished(
                        TestKey::new(vec![pkg], "(package)"),
                        Outcome::Errored,
                        None,
                        Some("the package failed without a failing test (a build error?)".into()),
                        Level::Recognized,
                        &mut gts,
                    );
                }
            }
        }
        self.close_pass_output();
        self.close_libtest_running();
        self.close_libtest_block();
        let mut held_pass = Vec::new();
        self.surefire_flush(&mut held_pass);
        self.jest_close_file(&mut held_pass);
        self.flush_call_pass(&mut held_pass);
        if let Some((_, key)) = self.pytest_text_pass.take() {
            self.finished(
                key,
                Outcome::Passed,
                None,
                None,
                Level::Recognized,
                &mut held_pass,
            );
        }
        let mut out = held_pass;
        // The injected stream never came: the text results are all there is.
        if self.level < Some(Level::Native) {
            out.append(&mut self.held_text_events);
        }
        out.append(&mut gts);
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

    // ------------------------------------------------------------- Quint

    /// `quint test`: a module header, then `    ok NAME passed N test(s)` and
    /// `    1) NAME failed after N test(s)`; the failures' details follow in
    /// `  1) NAME:` blocks, and the totals as `  N passing` / `  N failed`.
    fn quint(&mut self, line: &str, out: &mut Vec<EventKind>) -> bool {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if indent == 2 && !t.contains(' ') && !t.ends_with(':') && !t.ends_with(')') {
            // Candidate module header; only used if quint lines follow.
            self.quint_module = Some(t.to_owned());
            return false;
        }
        let key = |this: &Self, name: &str| {
            TestKey::new(this.quint_module.iter().cloned().collect(), name)
        };
        if indent >= 2
            && let Some(rest) = t.strip_prefix("ok ")
            && let Some((name, tail)) = rest.split_once(" passed ")
            && tail.ends_with(" test(s)")
            && !name.contains(' ')
        {
            let key = key(self, name);
            self.finished(key, Outcome::Passed, None, None, Level::Recognized, out);
            return true;
        }
        // "1) badTest failed after 1 test(s)" and its detail block "1) badTest:".
        if let Some((n, rest)) = t.split_once(") ")
            && n.chars().all(|c| c.is_ascii_digit())
            && !n.is_empty()
        {
            if let Some((name, tail)) = rest.split_once(" failed after ")
                && tail.ends_with(" test(s)")
                && !name.contains(' ')
            {
                let key = key(self, name);
                self.finished(key, Outcome::Failed, None, None, Level::Recognized, out);
                return true;
            }
            if let Some(name) = rest.strip_suffix(':')
                && !name.contains(' ')
                && self.seen.contains_key(&key(self, name).unique())
            {
                self.quint_detail = Some(name.to_owned());
                return true;
            }
        }
        if let Some(name) = self.quint_detail.clone()
            && !t.is_empty()
        {
            self.quint_detail = None;
            let key = key(self, &name);
            self.failure_fallback.push((key, t.to_owned()));
            return true;
        }
        false
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

/// The attempt a nextest line is about: "TRY 2 PASS ..." is the 2nd.
fn nextest_try(line: &str) -> u32 {
    line.trim_start()
        .strip_prefix("TRY ")
        .and_then(|r| {
            let n: String = r.chars().take_while(char::is_ascii_digit).collect();
            n.parse().ok()
        })
        .unwrap_or(1)
}

fn nextest_ms(dur: &str) -> Option<u64> {
    dur.trim_start_matches('>')
        .trim()
        .trim_end_matches('s')
        .parse::<f64>()
        .ok()
        .map(|s| (s * 1000.0).round() as u64)
}

/// "FLAKY 2/3 [ 0.003s] binary test": (attempt that passed, duration,
/// binary id, test name).
fn nextest_flaky(line: &str) -> Option<(u32, &str, &str, &str)> {
    let rest = line.trim_start().strip_prefix("FLAKY ")?;
    let (frac, rest) = rest.split_once(' ')?;
    let n = frac.split_once('/')?.0.parse().ok()?;
    let rest = rest.trim_start().strip_prefix('[')?;
    let (dur, rest) = rest.split_once(']')?;
    let mut w = rest.split_whitespace();
    let (binary, test) = (w.next()?, w.next()?);
    w.next().is_none().then_some((n, dur.trim(), binary, test))
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

/// Rust test binaries are named by crate and target (ADR 0018, 0031): the
/// crate's own binary is `calc` (`calc::calc` too), another target of it
/// `calc::live`, a bin `calc::tool`. The crate stays in the name: two
/// crates' `it` integration tests are two binaries, whatever order they ran
/// in (round 17).
fn nextest_binary(id: &str) -> String {
    let id = id.replace('-', "_");
    match id.rsplit_once("::") {
        Some((krate, target)) => {
            let target = target.trim_start_matches("bin/");
            if krate == target {
                krate.to_owned()
            } else {
                format!("{krate}::{target}")
            }
        }
        None => id,
    }
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

/// A pytest subtest: the parent's key, named as pytest prints it
/// ("test_b [case] (i=2)", "test_b (i=1)").
/// Splits "id - message" at the first ` - ` outside brackets: a parameter
/// id may contain it (`test_p[a - b] - AssertionError`).
fn split_summary(rest: &str) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    for (i, c) in rest.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth = (depth - 1).max(0),
            ' ' if depth == 0 && rest[i..].starts_with(" - ") => {
                return Some((&rest[..i], &rest[i + 3..]));
            }
            _ => {}
        }
    }
    // Unbalanced brackets: fall back to the first separator.
    rest.split_once(" - ")
}

fn is_js_file(s: &str) -> bool {
    [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"]
        .iter()
        .any(|e| s.ends_with(e))
}

/// A path that names a Python test file ("tests/test_e.py").
fn is_pytest_file(f: &str) -> bool {
    let f = f.trim();
    f.ends_with(".py") && !f.contains(' ') && !f.contains("::")
}

fn pytest_subtest(mut parent: TestKey, label: &str) -> TestKey {
    if !label.is_empty() {
        parent.name = format!("{} {label}", parent.name);
    }
    parent
}

/// "tests/test_x.py::TestCls::test_a[p]" -> path ["tests.test_x.TestCls"],
/// name "test_a[p]": the same key pytest's JUnit report produces
/// (classname, name), so live and file results are one test.
fn pytest_key(id: &str) -> TestKey {
    // "::" inside a parameter id ("test[c::d]") is not a separator.
    let mut parts: Vec<&str> = Vec::new();
    let (mut depth, mut from) = (0i32, 0);
    let b = id.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'[' => depth += 1,
            b']' => depth -= 1,
            b':' if depth <= 0 && b.get(i + 1) == Some(&b':') => {
                parts.push(&id[from..i]);
                from = i + 2;
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&id[from..]);
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

    /// Results that stand: a result re-keyed when its package became known
    /// (Go text) is the re-keyed one only.
    fn finished(events: &[EventKind]) -> Vec<(String, Outcome)> {
        let superseded: BTreeSet<String> = events
            .iter()
            .filter_map(|e| match e {
                EventKind::TestSuperseded { test, .. } => Some(test.unique()),
                _ => None,
            })
            .collect();
        events
            .iter()
            .filter_map(|e| match e {
                EventKind::TestFinished { test, outcome, .. }
                    if !superseded.contains(&test.unique()) =>
                {
                    Some((test.display(), *outcome))
                }
                _ => None,
            })
            .collect()
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    fn feed_all(input: &str) -> (Recognizers, Vec<EventKind>) {
        let (mut r, mut ev) = feed(input);
        ev.extend(r.finish());
        (r, ev)
    }

    fn messages(events: &[EventKind]) -> Vec<(String, String)> {
        events
            .iter()
            .filter_map(|e| match e {
                EventKind::TestFinished {
                    test,
                    message: Some(m),
                    ..
                } => Some((test.display(), m.clone())),
                _ => None,
            })
            .collect()
    }

    /// Owner's report (round 13): gotestsum's default output reported a
    /// failure and an error, and prova saw none of it.
    #[test]
    fn gotestsum_default_output_names_failures_and_build_errors() {
        let (r, ev) = feed_all(&fixture("gotestsum-pkgname.txt"));
        let mut got = finished(&ev);
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            got,
            vec![
                ("example.com/gts/p2::(package)".into(), Outcome::Errored),
                ("p1::TestBad".into(), Outcome::Failed),
                ("p3::TestPanics".into(), Outcome::Failed),
            ]
        );
        assert_eq!((r.total(), r.failed_reported()), (3, 3));
        let m = messages(&ev);
        assert!(
            m.iter().any(|(t, m)| t == "p3::TestPanics"
                && m.starts_with("panic: assignment to entry in nil map")),
            "{m:?}"
        );
        assert!(
            m.iter()
                .any(|(t, m)| t == "p1::TestBad" && m.contains("expected 4, got 16")),
            "{m:?}"
        );
    }

    #[test]
    fn gotestsum_testname_output_counts_passes_and_packages() {
        let (_, ev) = feed_all(&fixture("gotestsum-testname.txt"));
        let mut got = finished(&ev);
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            got,
            vec![
                ("example.com/gts/p2::(package)".into(), Outcome::Errored),
                ("p1::TestBad".into(), Outcome::Failed),
                ("p1::TestOk".into(), Outcome::Passed),
                ("p3::TestPanics".into(), Outcome::Failed),
            ]
        );
    }

    /// `FAIL src/a.test.js` (Jest) is not a Go package failure: only a
    /// confirmed gotestsum run turns `FAIL <pkg>` into one. It is a Jest
    /// file that failed without a failing test: the file is the entry.
    #[test]
    fn a_bare_fail_line_is_not_a_package_without_gotestsum() {
        let (_, ev) = feed_all("FAIL src/a.test.js\nPASS src/b.test.js");
        assert_eq!(
            finished(&ev),
            vec![("src/a.test.js::(file)".into(), Outcome::Errored)]
        );
    }

    #[test]
    fn gotestsum_json_stream_carries_build_errors() {
        let (_, ev) = feed_all(&fixture("gotestsum-json.jsonl"));
        let m = messages(&ev);
        assert!(
            m.iter().any(|(t, m)| t == "example.com/gts/p2::(package)"
                && m.contains("undefined: undefinedThing")),
            "{m:?}"
        );
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

    fn subtests(events: &[EventKind]) -> Vec<(String, Outcome)> {
        events
            .iter()
            .filter_map(|e| match e {
                EventKind::TestFinished {
                    test,
                    outcome,
                    subtest: true,
                    ..
                } => Some((test.display(), *outcome)),
                _ => None,
            })
            .collect()
    }

    /// Round 12 count audit: pytest's own counts, in text.
    #[test]
    fn pytest_text_counts_as_pytest_does() {
        let (r, ev) = feed(
            "tests/t.py::test_xfail XFAIL (not yet)                     [ 20%]\n\
             tests/t.py::test_xpass XPASS (fixed)                       [ 40%]\n\
             tests/t.py::test_p[g h] PASSED                             [ 60%]\n\
             tests/t.py::test_p[c::d] FAILED                            [ 80%]\n\
             tests/t.py::test_p[x PASSED y] PASSED                      [100%]\n\
             FAILED tests/t.py::test_p[c::d] - assert 1 == 2\n\
             ===== 1 failed, 2 passed, 1 xfailed, 1 xpassed in 0.01s =====",
        );
        assert_eq!(
            finished(&ev),
            vec![
                ("tests.t::test_xfail".into(), Outcome::Skipped),
                ("tests.t::test_xpass".into(), Outcome::Passed),
                ("tests.t::test_p[g h]".into(), Outcome::Passed),
                ("tests.t::test_p[c::d]".into(), Outcome::Failed),
                ("tests.t::test_p[x PASSED y]".into(), Outcome::Passed),
            ]
        );
        assert_eq!((r.total(), r.failed_reported()), (5, 1));
    }

    /// `-rA`: the short summary names passing tests too.
    #[test]
    fn pytest_short_summary_names_passes() {
        let (_, ev) = feed("PASSED tests/t.py::test_a\nXFAIL tests/t.py::test_b - reason");
        assert_eq!(
            finished(&ev),
            vec![
                ("tests.t::test_a".into(), Outcome::Passed),
                ("tests.t::test_b".into(), Outcome::Skipped),
            ]
        );
    }

    /// pytest 9.2's subtests, as it prints them (round 12 count audit: a
    /// failing unittest subTest was recorded as a pass).
    #[test]
    fn pytest_subtests_in_text_are_subtests() {
        let (r, ev) = feed(
            "tests/test_s.py::T::test_subs_fail PASSED                                [ 33%]\n\
             tests/test_s.py::T::test_subs_pass PASSED                                [ 66%]\n\
             tests/test_s.py::test_native_subtests SUBPASSED[case] (i=0)              [100%]\n\
             tests/test_s.py::test_native_subtests SUBPASSED[case] (i=1)              [100%]\n\
             tests/test_s.py::test_native_subtests SUBFAILED[case] (i=2)              [100%]\n\
             tests/test_s.py::test_native_subtests FAILED                             [100%]\n\
             SUBFAILED(i=1) tests/test_s.py::T::test_subs_fail - AssertionError: 1 == 1\n\
             SUBFAILED[case] (i=2) tests/test_s.py::test_native_subtests - assert 2 != 2\n\
             FAILED tests/test_s.py::test_native_subtests - contains 1 failed subtest\n\
             ================ 3 failed, 2 passed, 7 subtests passed in 0.02s ================",
        );
        assert_eq!(
            subtests(&ev),
            vec![
                (
                    "tests.test_s::test_native_subtests [case] (i=0)".into(),
                    Outcome::Passed
                ),
                (
                    "tests.test_s::test_native_subtests [case] (i=1)".into(),
                    Outcome::Passed
                ),
                (
                    "tests.test_s::test_native_subtests [case] (i=2)".into(),
                    Outcome::Failed
                ),
                (
                    "tests.test_s.T::test_subs_fail (i=1)".into(),
                    Outcome::Failed
                ),
            ]
        );
        assert_eq!(r.total(), 12);
    }

    #[test]
    fn pytest_reportlog_subtests_are_subtests() {
        let (_, ev) = feed(concat!(
            r#"{"nodeid": "tests/test_s.py::T::test_subs_fail", "when": "call", "outcome": "failed", "_subtest.context": {"msg": null, "kwargs": {"i": "1"}}, "longrepr": {"reprcrash": {"message": "AssertionError: 1 == 1"}}, "$report_type": "SubTestReport"}"#,
            "\n",
            r#"{"nodeid": "tests/test_s.py::test_native_subtests", "when": "call", "outcome": "passed", "_subtest.context": {"msg": "case", "kwargs": {"i": "0"}}, "$report_type": "SubTestReport"}"#,
        ));
        assert_eq!(
            subtests(&ev),
            vec![
                (
                    "tests.test_s.T::test_subs_fail (i=1)".into(),
                    Outcome::Failed
                ),
                (
                    "tests.test_s::test_native_subtests [case] (i=0)".into(),
                    Outcome::Passed
                ),
            ]
        );
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
        assert_eq!(nextest_binary("prova-cli::live"), "prova_cli::live");
        assert_eq!(nextest_binary("prova-cli::bin/prova"), "prova_cli::prova");
        assert_eq!(nextest_binary("calc::calc"), "calc");
        assert_eq!(nextest_binary("calc"), "calc");
        // Two crates' integration tests of one name are two binaries.
        assert_ne!(nextest_binary("alpha::it"), nextest_binary("beta::it"));
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

    /// Round 12 (false pass): two test binaries with the same test name
    /// (`tests::it_works` in lib.rs and main.rs); the second, failing,
    /// result was dropped as a duplicate of the first, passing one.
    #[test]
    fn a_second_result_under_the_same_name_is_kept() {
        let (_, events) = feed(concat!(
            "test tests::it_works ... ok\n",
            "test tests::it_works ... FAILED\n",
        ));
        let f = finished(&events);
        assert!(
            f.iter().any(|(_, o)| *o == Outcome::Failed),
            "the failure must not be dropped: {f:?}"
        );
        let (_, events) = feed(concat!(
            r#"{"type":"test","event":"started","name":"m::a"}"#,
            "\n",
            r#"{"type":"test","name":"m::a","event":"ok"}"#,
            "\n",
            r#"{"type":"test","event":"started","name":"m::a"}"#,
            "\n",
            r#"{"type":"test","name":"m::a","event":"failed"}"#,
            "\n",
        ));
        assert!(finished(&events).iter().any(|(_, o)| *o == Outcome::Failed));
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
    fn vstest_names_with_spaces_and_console_totals() {
        let (r, ev) = feed(
            "  Passed Xu.CalcTests.Adds [1 ms]\n  Failed Xu.CalcTests.Slash(s: \"x/y\") [1 ms]\n  Error Message:\n   boom\n  Stack Trace:\n  Failed Slash (\"x/y\") [< 1 ms]\n  Skipped Unsure [< 1 ms]\nTotal tests: 14\n     Passed: 9\n     Failed: 4\n    Skipped: 1\n Total time: 0.3 Seconds\n",
        );
        assert_eq!(
            finished(&ev),
            vec![
                ("Xu.CalcTests.Adds".into(), Outcome::Passed),
                ("Xu.CalcTests.Slash(s: \"x/y\")".into(), Outcome::Failed),
                ("Slash (\"x/y\")".into(), Outcome::Failed),
                ("Unsure".into(), Outcome::Skipped),
            ]
        );
        assert_eq!(r.total(), 14);
        assert_eq!(r.failed_reported(), 4);
        let (_, ev) = feed("Failed all the tests we could [1 ms]\n");
        assert!(finished(&ev).is_empty(), "prose is not a result");
    }

    #[test]
    fn nextest_summary_is_the_total_and_named_skips_count() {
        let (r, _) = feed(
            "        PASS [   0.005s] ( 1/3) rc tests::a\n        FAIL [   0.005s] ( 2/3) rc tests::b\n     Summary [   0.206s] 3 tests run: 2 passed, 1 failed, 2 skipped\n",
        );
        assert_eq!((r.total(), r.failed_reported()), (3, 1));
        let (r, ev) = feed(
            "        SKIP [         ] rc tests::ig\n        PASS [   0.005s] ( 1/1) rc tests::a\n     Summary [   0.006s] 1 test run: 1 passed, 1 skipped\n",
        );
        assert_eq!(finished(&ev).len(), 2);
        assert_eq!(r.total(), 2, "the skipped test seen by name counts");
    }

    #[test]
    fn surefire_aggregate_line_is_the_total() {
        let (r, _) = feed(
            "[ERROR] Tests run: 19, Failures: 4, Errors: 0, Skipped: 2, Time elapsed: 0.001 s <<< FAILURE! -- in ex.CalcTest\n[INFO] Tests run: 2, Failures: 0, Errors: 0, Skipped: 0, Time elapsed: 0.019 s -- in ex.OtherTest\n[ERROR] Tests run: 22, Failures: 4, Errors: 1, Skipped: 2\n",
        );
        assert_eq!((r.total(), r.failed_reported()), (22, 5));
        let (r, _) = feed("Tests run: 3, Failures: 0, Errors: 0, Skipped: 1\n");
        assert_eq!(r.total(), 3);
    }

    #[test]
    fn libtest_text_should_panic_has_the_json_name() {
        let (_, ev) = feed(
            "test tests::panics - should panic ... ok\ntest tests::bad - should panic ... FAILED\n",
        );
        assert_eq!(
            finished(&ev),
            vec![
                ("tests::panics".into(), Outcome::Passed),
                ("tests::bad".into(), Outcome::Failed),
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
        let (_, ev) = feed("ok  \texample.com/calc\t1.502s");
        assert!(ev.is_empty(), "go package lines are not tests: {ev:?}");
        // A failed package without a failing test is an errored package.
        let (_, ev) = feed("FAIL\texample.com/calc\t1.5s");
        assert_eq!(
            finished(&ev),
            vec![("example.com/calc::(package)".into(), Outcome::Errored)]
        );
        let (r, ev) = feed(
            "Compiling prova v0.1.0\n{\"not\": \"go\"}\nok\nFAILED\nnote: run with RUST_BACKTRACE=1",
        );
        assert!(ev.is_empty(), "{ev:?}");
        assert_eq!(r.level, None);
    }

    fn msg_of<'a>(ev: &'a [EventKind], name: &str) -> Option<&'a str> {
        ev.iter().find_map(|e| match e {
            EventKind::TestFinished { test, message, .. } if test.display() == name => {
                message.as_deref()
            }
            _ => None,
        })
    }

    #[test]
    fn quint_test_results_and_failure_message() {
        let (_, ev) = feed(
            "  t\n    ok okTest passed 1 test(s)\n    1) badTest failed after 1 test(s)\n\n  1 passing (13ms)\n  1 failed\n\n  1) badTest:\n       Error [QNT508]: Expect condition does not hold true\n        at t.qnt:6:17\n",
        );
        assert_eq!(
            finished(&ev),
            vec![
                ("t::okTest".into(), Outcome::Passed),
                ("t::badTest".into(), Outcome::Failed)
            ]
        );
    }

    #[test]
    fn quint_failure_detail_becomes_the_tests_output() {
        let (r, _) = feed(
            "  t\n    1) badTest failed after 1 test(s)\n\n  1) badTest:\n       Error [QNT508]: Expect condition does not hold true\n",
        );
        assert_eq!(r.failure_fallback.len(), 1);
        assert_eq!(
            r.failure_fallback[0].1,
            "Error [QNT508]: Expect condition does not hold true"
        );
    }

    #[test]
    fn pytest9_subtest_report_class_name_is_accepted() {
        let (_, ev) = feed(
            r#"{"nodeid": "tests/t.py::test_a", "when": "call", "outcome": "failed", "_subtest.context": {"msg": "m", "kwargs": {}}, "$report_type": "SubtestReport"}"#,
        );
        assert_eq!(
            subtests(&ev),
            vec![("tests.t::test_a [m]".into(), Outcome::Failed)]
        );
    }

    #[test]
    fn pytest_subtests_without_context_stay_distinct() {
        let (_, ev) = feed_all(concat!(
            r#"{"nodeid": "tests/t.py::test_a", "when": "call", "outcome": "passed", "$report_type": "SubtestReport"}"#,
            "\n",
            r#"{"nodeid": "tests/t.py::test_a", "when": "call", "outcome": "failed", "$report_type": "SubtestReport"}"#,
            "\n",
            r#"{"nodeid": "tests/t.py::test_a", "when": "call", "outcome": "passed", "$report_type": "TestReport"}"#,
        ));
        let f = finished(&ev);
        assert_eq!(f.len(), 3, "{f:?}");
        assert_eq!(
            f[0],
            ("tests.t::test_a (subtest 1)".into(), Outcome::Passed)
        );
        assert_eq!(
            f[1],
            ("tests.t::test_a (subtest 2)".into(), Outcome::Failed)
        );
        assert_eq!(f[2], ("tests.t::test_a".into(), Outcome::Passed));
    }

    #[test]
    fn pytest_summary_splits_id_at_separator_outside_brackets() {
        let (_, ev) =
            feed("FAILED tests/t.py::test_p[a - b] - AssertionError: assert 'a - b' == 'c'");
        assert_eq!(
            finished(&ev),
            vec![("tests.t::test_p[a - b]".into(), Outcome::Failed)]
        );
        assert_eq!(
            msg_of(&ev, "tests.t::test_p[a - b]"),
            Some("AssertionError: assert 'a - b' == 'c'")
        );
    }

    /// Round 15: a stall in `pytest -v` named the test that had just
    /// passed, not the one hanging (its line had no outcome yet).
    #[test]
    fn pytest_unfinished_line_starts_that_test_and_releases_the_held_pass() {
        let mut r = Recognizers::default();
        let mut ev = r.line("tests/t.py::test_slow PASSED   [ 50%]");
        // The pass waits one line for a teardown error.
        assert!(finished(&ev).is_empty());
        ev.extend(r.partial("tests/t.py::test_"));
        assert!(
            ev.iter()
                .all(|e| !matches!(e, EventKind::TestStarted { .. }))
        );
        ev.extend(r.partial("tests/t.py::test_hangs "));
        assert_eq!(
            finished(&ev),
            vec![("tests.t::test_slow".to_owned(), Outcome::Passed)]
        );
        let started: Vec<String> = ev
            .iter()
            .filter_map(|e| match e {
                EventKind::TestStarted { test } => Some(test.display()),
                _ => None,
            })
            .collect();
        assert_eq!(started, vec!["tests.t::test_hangs".to_owned()]);
        // The same partial again does not start it twice.
        assert!(r.partial("tests/t.py::test_hangs ").is_empty());
    }

    /// Round 15: pytest-xdist's verbose lines put the outcome first.
    #[test]
    fn pytest_xdist_verbose_lines_are_recognized() {
        let (r, ev) = feed_all(
            "[gw0] [ 33%] FAILED tests/test_a.py::test_q\n[gw1] [ 66%] PASSED tests/test_a.py::TestC::test_ok[a b]\n[gw0] [100%] SKIPPED tests/test_a.py::test_s\n=========================== short test summary info ============================\nFAILED tests/test_a.py::test_q - assert 2 == 1\n",
        );
        assert_eq!(
            finished(&ev),
            vec![
                ("tests.test_a::test_q".to_owned(), Outcome::Failed),
                (
                    "tests.test_a.TestC::test_ok[a b]".to_owned(),
                    Outcome::Passed
                ),
                ("tests.test_a::test_s".to_owned(), Outcome::Skipped),
            ]
        );
        assert_eq!(r.level, Some(Level::Recognized));
    }

    /// Round 15: -q names the failing test only in the short summary; the
    /// FAILURES block above it becomes that test's output.
    #[test]
    fn pytest_q_failure_block_is_the_failing_tests_output() {
        let (r, ev) = feed_all(concat!(
            ".F.   [100%]\n",
            "=================================== FAILURES ===================================\n",
            "__________________________________ TestC.test_q ________________________________\n",
            "\n",
            ">       assert 2 == 1\n",
            "E       assert 2 == 1\n",
            "=========================== short test summary info ============================\n",
            "FAILED tests/test_a.py::TestC::test_q - assert 2 == 1\n",
            "1 failed, 2 passed in 0.03s\n",
        ));
        assert_eq!(
            finished(&ev),
            vec![("tests.test_a.TestC::test_q".to_owned(), Outcome::Failed)]
        );
        assert_eq!(r.failure_output.len(), 1);
        assert!(r.failure_output[0].1.contains("E       assert 2 == 1"));
    }

    /// Round 15: a pass and a teardown error is one errored test in the
    /// verbose lines and in the `-rA` summary alike.
    #[test]
    fn pytest_teardown_error_is_one_errored_test_in_every_format() {
        let (_, verbose) = feed_all(
            "tests/t.py::test_x PASSED   [ 50%]\ntests/t.py::test_x ERROR   [ 50%]\ntests/t.py::test_y PASSED  [100%]\n",
        );
        let (_, summary) = feed_all(
            "tests/t.py::test_x PASSED   [ 50%]\ntests/t.py::test_y PASSED  [100%]\n=========================== short test summary info ============================\nPASSED tests/t.py::test_x\nERROR tests/t.py::test_x - RuntimeError: teardown\n",
        );
        let last = |ev: &[EventKind]| {
            let mut m: BTreeMap<String, Outcome> = BTreeMap::new();
            for (n, o) in finished(ev) {
                m.insert(n, o);
            }
            m
        };
        assert_eq!(last(&verbose), last(&summary));
        assert_eq!(last(&summary)["tests.t::test_x"], Outcome::Errored);
        assert!(
            !finished(&summary)
                .iter()
                .any(|(n, _)| n.contains("second result"))
        );
    }

    #[test]
    fn pytest_parameter_ids_with_spaces_start_only_when_whole() {
        let mut r = Recognizers::default();
        assert!(r.partial("tests/t.py::test_r[negative 2/").is_empty());
        assert!(r.partial("tests/t.py::test_r[negative ").is_empty());
        let ev = r.partial("tests/t.py::test_r[negative 2/-4] ");
        assert_eq!(ev.len(), 1);
    }

    #[test]
    fn a_held_pass_is_released_when_output_goes_quiet() {
        let mut r = Recognizers::default();
        r.line("tests/t.py::test_a PASSED   [100%]");
        let ev = r.release_held_pass();
        assert_eq!(
            finished(&ev),
            vec![("tests.t::test_a".to_owned(), Outcome::Passed)]
        );
    }

    #[test]
    fn pytest_summary_message_is_kept_for_a_verbose_failure() {
        let (r, ev) = feed(
            "tests/t.py::test_a FAILED   [100%]\nFAILED tests/t.py::test_a - AssertionError: boom",
        );
        assert_eq!(finished(&ev).len(), 1);
        assert_eq!(r.failure_fallback.len(), 1);
        assert_eq!(r.failure_fallback[0].1, "AssertionError: boom");
    }

    #[test]
    fn go_parallel_output_follows_name_and_cont_lines() {
        let (_, ev) = feed(
            "=== RUN   TestA\n=== RUN   TestA/one\n=== PAUSE TestA/one\n=== RUN   TestA/two\n=== PAUSE TestA/two\n=== CONT  TestA/one\n=== CONT  TestA/two\n    a_test.go:20: two failed here\n=== NAME  TestA/one\n    a_test.go:15: one says: bad\n=== NAME  TestA\n    a_test.go:30: parent cleanup failed\n--- FAIL: TestA (0.01s)\n    --- FAIL: TestA/one (0.00s)\n    --- FAIL: TestA/two (0.00s)",
        );
        assert_eq!(
            msg_of(&ev, "TestA/two"),
            Some("a_test.go:20: two failed here")
        );
        assert_eq!(
            msg_of(&ev, "TestA/one"),
            Some("a_test.go:15: one says: bad")
        );
        assert_eq!(
            msg_of(&ev, "TestA"),
            Some("a_test.go:30: parent cleanup failed")
        );
    }

    /// Round 14: Jest's verbose reporter was not recognized at all.
    #[test]
    fn jest_verbose_names_tests_and_failures() {
        let (r, ev) = feed_all(concat!(
            "PASS src/a.test.js\n",
            "  calc\n",
            "    \u{2713} adds (2 ms)\n",
            "    nested\n",
            "      \u{2713} deep\n",
            "    \u{25cb} skipped test\n",
            "\n",
            " FAIL  src/x.test.ts (5.2 s)\n",
            "  calc\n",
            "    \u{2713} subtracts (1 ms)\n",
            "    \u{2715} flaky clock (3 ms)\n",
            "\n",
            "  \u{25cf} calc \u{203a} flaky clock\n",
            "\n",
            "    expect(received).toBe(expected)\n",
            "\n",
            "    Expected: 1\n",
            "Test Suites: 1 failed, 1 passed, 2 total\n",
            "Tests:       1 failed, 4 passed, 5 total\n",
        ));
        assert_eq!(
            finished(&ev),
            vec![
                ("src/a.test.js::calc > adds".into(), Outcome::Passed),
                (
                    "src/a.test.js::calc > nested > deep".into(),
                    Outcome::Passed
                ),
                (
                    "src/a.test.js::calc > skipped test".into(),
                    Outcome::Skipped
                ),
                ("src/x.test.ts::calc > subtracts".into(), Outcome::Passed),
                ("src/x.test.ts::calc > flaky clock".into(), Outcome::Failed),
            ]
        );
        assert!(
            r.failure_output
                .iter()
                .any(|(k, t)| k.name == "calc > flaky clock" && t.contains("Expected: 1")),
            "{:?}",
            r.failure_output
        );
        assert_eq!((r.total(), r.failed_reported(), r.suites_failed), (5, 1, 1));
    }

    /// Non-verbose Jest names a failing test only in its failure block, and
    /// a file that failed to load has no test at all: it is the entry.
    #[test]
    fn jest_default_output_names_failures_and_files_that_failed_to_load() {
        let (r, ev) = feed_all(concat!(
            "PASS src/a.test.js\n",
            "FAIL src/b.test.js\n",
            "  \u{25cf} Test suite failed to run\n",
            "\n",
            "    Cannot find module './missing' from 'src/b.test.js'\n",
            "\n",
            "FAIL src/c.test.js\n",
            "  \u{25cf} calc \u{203a} divides\n",
            "\n",
            "    expect(received).toBe(expected)\n",
            "\n",
            "Test Suites: 2 failed, 1 passed, 3 total\n",
            "Tests:       1 failed, 3 passed, 4 total\n",
        ));
        assert_eq!(
            attempts(&ev),
            vec![
                ("src/b.test.js::(file)".to_owned(), 1, Outcome::Errored),
                (
                    "src/c.test.js::calc > divides".to_owned(),
                    1,
                    Outcome::Failed
                ),
            ]
        );
        let m = messages(&ev);
        assert!(m[0].1.contains("Cannot find module"), "{m:?}");
        assert_eq!(r.suites_failed, 2);
    }

    /// Round 14: Vitest prints a file that failed to load as `FAIL  file [ file ]`.
    #[test]
    fn vitest_file_that_failed_to_load_is_an_errored_entry() {
        let (r, ev) = feed_all(concat!(
            " \u{2713} src/a.test.ts (3 tests) 5ms\n",
            " FAIL  src/b.test.ts [ src/b.test.ts ]\n",
            "Error: Cannot find module ./missing\n",
            " Test Files  1 failed | 1 passed (2)\n",
            "      Tests  3 passed (3)\n",
        ));
        assert_eq!(
            attempts(&ev),
            vec![("src/b.test.ts::(file)".to_owned(), 1, Outcome::Errored)]
        );
        assert_eq!(r.suites_failed, 1);
    }

    #[test]
    fn surefire_console_summary_names_failing_tests() {
        let (_, ev) = feed_all(concat!(
            "[ERROR] Failures: \n",
            "[ERROR]   OtherTest.fooWorks:12 expected:<2> but was:<3>\n",
            "[ERROR] Errors: \n",
            "[ERROR]   com.acme.CalcTest.io \u{bb} IO disk full\n",
            "[INFO] \n",
            "[ERROR] Tests run: 5, Failures: 1, Errors: 1, Skipped: 0\n",
        ));
        assert_eq!(
            finished(&ev),
            vec![
                ("OtherTest::fooWorks".into(), Outcome::Failed),
                ("com.acme.CalcTest::io".into(), Outcome::Errored),
            ]
        );
        assert!(
            messages(&ev)
                .iter()
                .any(|(t, m)| t == "OtherTest::fooWorks" && m.starts_with("expected:<2>")),
            "{:?}",
            messages(&ev)
        );
    }

    /// Round 14: the Vitest reporter's entry for a test file that failed to load.
    #[test]
    fn vitest_stream_file_that_failed_to_load_is_an_errored_entry() {
        let (_, ev) = feed_all(
            r#"{"type":"test","name":"(file)","file":"src/broken.test.ts","event":"failed","message":"Cannot find module './nope'"}"#,
        );
        assert_eq!(
            attempts(&ev),
            vec![("src/broken.test.ts::(file)".to_owned(), 1, Outcome::Errored)]
        );
        assert!(messages(&ev)[0].1.contains("Cannot find module"));
    }

    fn attempts(events: &[EventKind]) -> Vec<(String, u32, Outcome)> {
        events
            .iter()
            .filter_map(|e| match e {
                EventKind::TestFinished {
                    test,
                    attempt,
                    outcome,
                    ..
                } => Some((test.display(), *attempt, *outcome)),
                _ => None,
            })
            .collect()
    }

    /// Round 14: a nextest retry that passes is one test, failed then
    /// passed (flaky), not a failure plus a "(second result)".
    #[test]
    fn nextest_text_retry_is_a_second_attempt() {
        let (r, ev) = feed_all(
            "        PASS [   0.004s] calc tests::adds\n  TRY 1 FAIL [   0.004s] calc tests::retry_me\n  RETRY 2/2 [         ] calc tests::retry_me\n  TRY 2 PASS [   0.003s] calc tests::retry_me\n     Summary [   0.020s] 2 tests run: 2 passed (1 flaky), 0 skipped\n   FLAKY 2/2 [   0.003s] calc tests::retry_me\n",
        );
        assert_eq!(
            attempts(&ev),
            vec![
                ("tests::adds".to_owned(), 1, Outcome::Passed),
                ("tests::retry_me".to_owned(), 1, Outcome::Failed),
                ("tests::retry_me".to_owned(), 2, Outcome::Passed),
            ]
        );
        assert_eq!((r.total(), r.failed_reported()), (2, 0));
    }

    /// A FLAKY line alone (the TRY lines hidden by the status level) still
    /// says the test failed before it passed.
    #[test]
    fn nextest_flaky_line_alone_is_a_retry() {
        let (_, ev) = feed_all("   FLAKY 3/3 [   0.003s] calc tests::retry_me\n");
        assert_eq!(
            attempts(&ev),
            vec![
                ("tests::retry_me".to_owned(), 1, Outcome::Failed),
                ("tests::retry_me".to_owned(), 3, Outcome::Passed),
            ]
        );
    }

    /// pytest-rerunfailures: `RERUN` then `PASSED` is flaky.
    #[test]
    fn pytest_text_rerun_then_pass_is_a_retry() {
        let (_, ev) = feed_all(
            "tests/test_a.py::test_ok PASSED [ 50%]\ntests/test_a.py::test_flaky RERUN [100%]\ntests/test_a.py::test_flaky PASSED [100%]\n==== 2 passed, 1 rerun in 0.10s ====\n",
        );
        assert_eq!(
            attempts(&ev),
            vec![
                ("tests.test_a::test_ok".to_owned(), 1, Outcome::Passed),
                ("tests.test_a::test_flaky".to_owned(), 1, Outcome::Failed),
                ("tests.test_a::test_flaky".to_owned(), 2, Outcome::Passed),
            ]
        );
    }

    fn report(nodeid: &str, when: &str, outcome: &str) -> String {
        format!(
            r#"{{"$report_type":"TestReport","nodeid":"{nodeid}","when":"{when}","outcome":"{outcome}","duration":0.001,"longrepr":null}}"#
        )
    }

    #[test]
    fn pytest_reportlog_rerun_then_pass_is_a_retry() {
        let id = "tests/test_a.py::test_flaky";
        let lines = [
            report(id, "setup", "passed"),
            report(id, "call", "rerun"),
            report(id, "teardown", "passed"),
            report(id, "setup", "passed"),
            report(id, "call", "passed"),
            report(id, "teardown", "passed"),
        ];
        let (_, ev) = feed_all(&lines.join("\n"));
        assert_eq!(
            attempts(&ev),
            vec![
                ("tests.test_a::test_flaky".to_owned(), 1, Outcome::Failed),
                ("tests.test_a::test_flaky".to_owned(), 2, Outcome::Passed),
            ]
        );
    }

    /// Round 14: a teardown error on a test whose call passed is that test
    /// erroring, not a second test.
    #[test]
    fn pytest_reportlog_teardown_error_errors_the_test() {
        let id = "tests/test_b.py::test_ok";
        let lines = [
            report(id, "setup", "passed"),
            report(id, "call", "passed"),
            report(id, "teardown", "failed"),
        ];
        let (_, ev) = feed_all(&lines.join("\n"));
        assert_eq!(
            attempts(&ev),
            vec![("tests.test_b::test_ok".to_owned(), 1, Outcome::Errored)]
        );
    }

    #[test]
    fn pytest_text_teardown_error_errors_the_test() {
        let (_, ev) = feed_all(
            "tests/test_b.py::test_ok PASSED [ 50%]\ntests/test_b.py::test_ok ERROR [ 50%]\ntests/test_b.py::test_next PASSED [100%]\n",
        );
        assert_eq!(
            attempts(&ev),
            vec![
                ("tests.test_b::test_ok".to_owned(), 1, Outcome::Errored),
                ("tests.test_b::test_next".to_owned(), 1, Outcome::Passed),
            ]
        );
    }

    /// A collection error names the file, in the summary and in the stream.
    #[test]
    fn pytest_collection_errors_are_named() {
        let (_, ev) = feed_all(
            "==== short test summary info ====\nERROR tests/test_e.py - ImportError: no module x\n!!!! Interrupted: 1 error during collection !!!!\n",
        );
        assert_eq!(
            attempts(&ev),
            vec![("tests/test_e.py::(file)".to_owned(), 1, Outcome::Errored)]
        );
        let (_, ev) = feed_all(
            r#"{"$report_type":"CollectReport","nodeid":"tests/test_e.py","outcome":"failed","longrepr":"ImportError: no module x","result":[]}"#,
        );
        assert_eq!(
            attempts(&ev),
            vec![("tests/test_e.py::(file)".to_owned(), 1, Outcome::Errored)]
        );
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
    // A result re-keyed once its package was known is that one result.
    let superseded: BTreeSet<String> = events
        .iter()
        .filter_map(|e| match e {
            EventKind::TestSuperseded { test, .. } => Some(test.unique()),
            _ => None,
        })
        .collect();
    let mut passed: Vec<String> = events
        .into_iter()
        .filter_map(|e| match e {
            EventKind::TestFinished {
                test,
                outcome: Outcome::Passed,
                ..
            } if !superseded.contains(&test.unique()) => Some(test.display()),
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
        assert_eq!(
            super::passed_tests(out),
            vec!["example.com/calc::TestConformance".to_owned()]
        );
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

// ------------------------------------------------------------------ Go text

/// A gotestsum rerun marker, ` (re-run 1)`, removed from `t`.
fn strip_rerun(t: &str) -> (String, bool) {
    if let Some(i) = t.find("(re-run ")
        && let Some(len) = t[i..].find(')')
    {
        let mut s = t[..i].trim_end().to_owned();
        let after = t[i + len + 1..].trim_start();
        if !after.is_empty() {
            s.push(' ');
            s.push_str(after);
        }
        return (s, true);
    }
    (t.to_owned(), false)
}

/// A line of a Go package's own output (not a test's) that says the test
/// binary crashed: a panic outside the tests, a fatal runtime error, a kill.
fn go_crash_line(line: &str) -> bool {
    !line.starts_with(char::is_whitespace)
        && (line.starts_with("panic: ")
            || line.starts_with("fatal error: ")
            || line.starts_with("signal: killed"))
}

impl Recognizers {
    /// Every `go test` package that ran said `[no tests to run]` (a `-run`
    /// typo): `ok` packages with tests, or failures, say it was not so.
    pub fn go_no_tests_only(&self) -> bool {
        self.go_no_tests_pkgs > 0 && self.go_ok_pkgs == 0 && self.go_fail_pkgs == 0
    }

    /// The framework prints every test it runs (`go test -v`, gotestsum
    /// `testname`): a count above what was seen means results went missing.
    pub fn prints_every_test(&self) -> bool {
        self.go_run_lines > 0 || self.gts_testname_lines > 0
    }

    /// The full import path of a package printed short (gotestsum trims the
    /// module path), when a `ok`/`FAIL` line of the run gave it.
    fn go_pkg(&self, printed: &str) -> String {
        // gotestsum prints the module's root package as `.`: the one the
        // other packages are under.
        if printed == "."
            && let Some(m) = &self.go_module
        {
            return m.clone();
        }
        if printed == "."
            && let Some(root) = self.go_full_pkgs.iter().min_by_key(|p| p.len())
            && self
                .go_full_pkgs
                .iter()
                .all(|p| p == root || p.starts_with(&format!("{root}/")))
        {
            return root.clone();
        }
        if let Some(full) = self
            .go_full_pkgs
            .iter()
            .find(|f| f.as_str() == printed || f.ends_with(&format!("/{printed}")))
        {
            return full.clone();
        }
        // gotestsum prints a package relative to the module (`calc` for
        // `example.com/m/calc`): go.mod completes it, as `go test` prints
        // it (round 16: two identities for one test).
        if let Some(m) = &self.go_module
            && !printed.starts_with(m.as_str())
            && !printed.split('/').next().unwrap_or("").contains('.')
            && !printed.contains(' ')
            && !printed.is_empty()
        {
            return format!("{m}/{printed}");
        }
        printed.to_owned()
    }

    /// The name a Go test has until its package is known. The segment keeps
    /// the same name in two packages apart; `display` is the bare name.
    fn go_key(&self, name: &str) -> TestKey {
        TestKey {
            file: Some(format!("(package pending) #{}", self.go_segment)),
            path: Vec::new(),
            name: name.to_owned(),
        }
    }

    /// One execution of a Go test whose key is final. The first is its
    /// result. A later one inside the same package run is an iteration
    /// (`-count=N`): a failure sticks, a pass after it changes nothing. A
    /// later one after the package's own result is a retry (gotestsum's
    /// `--rerun-fails`, `retry`): the last decides, and a failure is kept in
    /// the history whatever came after it. Equal passes change nothing.
    #[allow(clippy::too_many_arguments)]
    fn go_result(
        &mut self,
        key: TestKey,
        outcome: Outcome,
        duration_ms: Option<u64>,
        message: Option<String>,
        level: Level,
        subtest: bool,
        retry: bool,
        out: &mut Vec<EventKind>,
    ) {
        let d = key.unique();
        if outcome.is_failure()
            && let Some(pkg) = key.path.first()
        {
            self.go_failed_pkgs.insert(pkg.clone());
        }
        let Some((_, before)) = self.seen.get(&d).cloned().filter(|_| !self.holding) else {
            if !self.holding {
                self.go_attempts.insert(d, 1);
            }
            return self.result(key, outcome, duration_ms, message, level, subtest, out);
        };
        self.repeats += 1;
        if !before.is_failure() && !outcome.is_failure() {
            return;
        }
        if before.is_failure() && !outcome.is_failure() {
            if !retry {
                // A later iteration passing does not undo the failure.
                return;
            }
            self.go_recovered += 1;
        }
        if outcome.is_failure()
            && let Some(m) = &message
        {
            self.failure_fallback.push((key.clone(), m.clone()));
        }
        let n = self.go_attempts.entry(d.clone()).or_insert(1);
        *n += 1;
        let attempt = *n;
        self.seen_keys.insert(d.clone(), key.clone());
        self.seen.insert(d, (key.display(), outcome));
        out.push(EventKind::TestFinished {
            test: key,
            attempt,
            outcome,
            duration_ms,
            message,
            source: level,
            subtest,
        });
    }

    /// A Go result printed without its package (`--- PASS: TestX`).
    fn go_bare(
        &mut self,
        name: &str,
        outcome: Outcome,
        duration_ms: Option<u64>,
        message: Option<String>,
        out: &mut Vec<EventKind>,
    ) {
        let key = self.go_key(name);
        let exec = GoExec {
            outcome,
            duration_ms,
            message: message.clone(),
            subtest: name.contains('/'),
        };
        match self.go_pending.iter_mut().find(|r| r.key == key) {
            Some(run) => run.execs.push(exec),
            None => self.go_pending.push(GoRun {
                key: key.clone(),
                execs: vec![exec],
            }),
        }
        let sub = name.contains('/');
        self.go_result(
            key,
            outcome,
            duration_ms,
            message,
            Level::Recognized,
            sub,
            false,
            out,
        );
    }

    /// The package of the pending results is known: they are the same
    /// results under their full name (the provisional ones are superseded).
    /// Returns whether a test failed in the package.
    fn go_requalify(&mut self, run: GoRun, pkg: &str, out: &mut Vec<EventKind>) {
        let q = TestKey::new(vec![pkg.to_owned()], run.key.name.clone());
        let (bd, qd) = (run.key.unique(), q.unique());
        self.seen.remove(&bd);
        self.go_attempts.remove(&bd);
        self.started.remove(&bd);
        for (k, _) in self
            .failure_fallback
            .iter_mut()
            .chain(self.failure_output.iter_mut())
        {
            if *k == run.key {
                *k = q.clone();
            }
        }
        // The executions were counted once already.
        self.repeats = self.repeats.saturating_sub(run.execs.len() as u64 - 1);
        if run.execs.iter().any(|e| e.outcome.is_failure()) {
            self.go_failed_pkgs.insert(pkg.to_owned());
        }
        // A stream (gotestsum's JSON) may have reported them already. The
        // executions of a package that already had its result (gotestsum
        // reruns the failed tests: a second `go test` of the package) are
        // retries.
        let retry = self.go_pkg_done.contains(pkg);
        if !self.seen.contains_key(&qd) || retry {
            for (i, e) in run.execs.into_iter().enumerate() {
                let (q, level) = (q.clone(), Level::Recognized);
                // Within one run: the first execution of a retry is one.
                self.go_result(
                    q,
                    e.outcome,
                    e.duration_ms,
                    e.message,
                    level,
                    e.subtest,
                    retry && i == 0,
                    out,
                );
            }
        }
        // After the results it names: the store links the two.
        out.push(EventKind::TestSuperseded {
            test: run.key.clone(),
            by: q,
        });
    }

    /// A result line with the package, `PASS pkg.TestX`: the pending result
    /// of that name (from `--- PASS: TestX`) is it. False when none is.
    fn go_qualify_one(&mut self, test: &str, pkg: &str, out: &mut Vec<EventKind>) -> bool {
        let key = self.go_key(test);
        let Some(i) = self.go_pending.iter().position(|r| r.key == key) else {
            return false;
        };
        let run = self.go_pending.remove(i);
        self.go_requalify(run, pkg, out);
        true
    }

    /// `ok  \tpkg\t0.01s`, `FAIL\tpkg\t0.01s`, `FAIL\tpkg [build failed]`:
    /// the package is known, and says whether it failed. A package that
    /// failed without a failing test (a build error, `TestMain`, a crash) is
    /// an errored `(package)` test, never covered by another test's
    /// quarantine, whatever the exit code (round 14).
    fn go_package(&mut self, line: &str, out: &mut Vec<EventKind>) {
        let Some((status, rest)) = line.split_once('\t') else {
            return;
        };
        let rest = rest.trim();
        let (printed, tail) = rest
            .split_once(char::is_whitespace)
            .map(|(p, t)| (p, t.trim()))
            .unwrap_or((rest, ""));
        if printed.is_empty() {
            return;
        }
        let pkg = printed.to_owned();
        if !self.go_full_pkgs.contains(&pkg) {
            self.go_full_pkgs.push(pkg.clone());
        }
        let text = std::mem::take(&mut self.go_segment_text);
        let hung = std::mem::take(&mut self.go_hung);
        self.go_reading_hung = false;
        let timed_out = |name: &str| {
            format!("{name} was still running when the test binary timed out (go test -timeout)")
        };
        // A test that started and never finished: the timeout stopped it.
        if status.trim() != "ok" {
            for name in &hung {
                let started = self.go_key(name).unique();
                if self.started.contains(&started) && !self.seen.contains_key(&started) {
                    self.go_bare(name, Outcome::Errored, None, Some(timed_out(name)), out);
                }
            }
        }
        self.go_flush_tails();
        self.go_open.clear();
        self.go_after_fail = None;
        self.go_panic_block = false;
        let crashed = text.iter().any(|l| go_crash_line(l));
        let pending = std::mem::take(&mut self.go_pending);
        let failed_here = pending
            .iter()
            .any(|r| r.execs.iter().any(|e| e.outcome.is_failure()))
            || self.go_failed_pkgs.contains(&pkg);
        for run in pending {
            self.go_requalify(run, &pkg, out);
        }
        self.go_pkg_done.insert(pkg.clone());
        self.go_segment += 1;
        if status.trim() == "ok" {
            if tail.contains("[no tests to run]") {
                self.go_no_tests_pkgs += 1;
            } else {
                self.go_ok_pkgs += 1;
            }
            return;
        }
        self.go_fail_pkgs += 1;
        // A panic or a kill outside any test is the package's own failure,
        // whatever a test did (round 15: a quarantined failure hid it).
        let mut explained = failed_here && !crashed;
        for name in hung {
            explained = true;
            let message = timed_out(&name);
            self.result(
                TestKey::new(vec![pkg.clone()], name.clone()),
                Outcome::Errored,
                None,
                Some(message),
                Level::Recognized,
                name.contains('/'),
                out,
            );
        }
        if !explained {
            let mut body = text;
            body.push(line.trim().to_owned());
            self.result(
                TestKey::new(vec![pkg], "(package)"),
                Outcome::Errored,
                None,
                message(&body),
                Level::Recognized,
                false,
                out,
            );
        }
    }

    /// The package line held while a test ran: more of that test's output
    /// (its own result line, `=== NAME`) says it was a forgery of the test's
    /// stdout; the next test, package or build line says it was real.
    fn go_settle_tentative(&mut self, line: &str, out: &mut Vec<EventKind>) {
        let Some((_, open)) = &self.go_tentative else {
            return;
        };
        let t = line.trim_start();
        let continues = [
            "--- PASS: ",
            "--- FAIL: ",
            "--- SKIP: ",
            "=== NAME",
            "=== CONT",
        ]
        .iter()
        .any(|p| {
            t.strip_prefix(p).is_some_and(|rest| {
                let name = rest.trim();
                let name = name.rsplit_once(" (").map_or(name, |(n, _)| n).trim();
                open.contains(name)
            })
        });
        if continues {
            self.go_tentative = None;
        } else if t.starts_with("=== RUN")
            || line.starts_with("ok  \t")
            || line.starts_with("FAIL\t")
            || line.starts_with("?   \t")
            || line.starts_with("# ")
        {
            self.go_confirm_tentative(out);
        }
    }

    fn go_confirm_tentative(&mut self, out: &mut Vec<EventKind>) {
        if let Some((line, _)) = self.go_tentative.take() {
            self.go_package_lines += 1;
            self.go_package(&line, out);
        }
    }

    /// What a failed test printed after its `--- FAIL` line (`go test`
    /// without `-v`, a panic) is its output.
    fn go_flush_tails(&mut self) {
        let keys: Vec<String> = self
            .go_output
            .keys()
            .filter(|k| k.starts_with("v\0"))
            .cloned()
            .collect();
        for k in keys {
            let name = &k[2..];
            let key = self.go_key(name);
            let failed = self
                .go_pending
                .iter()
                .any(|r| r.key == key && r.execs.iter().any(|e| e.outcome.is_failure()));
            let lines = self.go_output.remove(&k).unwrap_or_default();
            if failed && !lines.is_empty() {
                self.failure_output.push((key, lines.join("\n")));
            }
        }
    }

    /// Tests that passed because every subtest skipped: nothing ran.
    fn go_note_all_skipped(&mut self, out: &mut Vec<EventKind>) {
        let names = std::mem::take(&mut self.go_all_skipped);
        if names.is_empty() {
            return;
        }
        let shown: Vec<&str> = names.iter().take(5).map(String::as_str).collect();
        out.push(EventKind::RunNote {
            message: format!(
                "{} test(s) passed with every subtest skipped, so nothing ran in them: {}{}",
                names.len(),
                shown.join(", "),
                if names.len() > shown.len() {
                    ", ..."
                } else {
                    ""
                }
            ),
        });
    }

    /// Lines between packages: what a package failure explains itself with,
    /// and the tests a timeout panic names under `running tests:`.
    fn go_segment_line(&mut self, line: &str) {
        let t = line.trim();
        // A panic right after a test's `--- FAIL` line, and what follows it
        // until the package line, is that test's output, not the package's.
        if self.go_panic_block || (line.starts_with("panic: ") && self.go_after_fail.is_some()) {
            self.go_panic_block = true;
            if let Some(owner) = self.go_after_fail.clone() {
                let lines = self.go_output.entry(format!("v\0{owner}")).or_default();
                if lines.len() < 200 {
                    lines.push(line.trim_end().to_owned());
                }
            }
            return;
        }
        if !t.is_empty() && !line.starts_with(char::is_whitespace) {
            self.go_after_fail = None;
        }
        if t == "running tests:" {
            self.go_reading_hung = true;
            return;
        }
        if self.go_reading_hung {
            // "\t\tTestHang (1m0s)"
            if let Some((name, dur)) = t.split_once(" (")
                && dur.ends_with(')')
                && !name.contains(' ')
                && line.starts_with('\t')
            {
                self.go_hung.push(name.to_owned());
                return;
            }
            self.go_reading_hung = false;
        }
        if t.is_empty() || t == "PASS" || t == "FAIL" || line.starts_with("=== ") {
            return;
        }
        self.go_segment_text.push(line.trim_end().to_owned());
        if self.go_segment_text.len() > 60 {
            self.go_segment_text.remove(0);
        }
    }
}

/// Round 14, Go and gotestsum: identities with the package, repeated
/// executions, reruns, package failures.
#[cfg(test)]
mod go_tests {
    use super::*;

    fn feed_all(input: &str) -> (Recognizers, Vec<EventKind>) {
        let mut r = Recognizers::default();
        let mut all = Vec::new();
        for l in input.lines() {
            all.extend(r.line(l));
        }
        all.extend(r.finish());
        (r, all)
    }

    /// What the run ends with per test: the outcomes of all attempts of each
    /// test that was not superseded, in attempt order.
    fn settled(events: &[EventKind]) -> BTreeMap<String, Vec<Outcome>> {
        let superseded: BTreeSet<String> = events
            .iter()
            .filter_map(|e| match e {
                EventKind::TestSuperseded { test, .. } => Some(test.unique()),
                _ => None,
            })
            .collect();
        let mut out: BTreeMap<String, Vec<(u32, Outcome)>> = BTreeMap::new();
        for e in events {
            if let EventKind::TestFinished {
                test,
                attempt,
                outcome,
                ..
            } = e
                && !superseded.contains(&test.unique())
            {
                out.entry(test.display())
                    .or_default()
                    .push((*attempt, *outcome));
            }
        }
        out.into_iter()
            .map(|(k, mut v)| {
                v.sort_by_key(|(a, _)| *a);
                (k, v.into_iter().map(|(_, o)| o).collect())
            })
            .collect()
    }

    fn one(s: &BTreeMap<String, Vec<Outcome>>, name: &str) -> Vec<Outcome> {
        s.get(name)
            .unwrap_or_else(|| panic!("no {name} in {s:?}"))
            .clone()
    }

    #[test]
    fn go_verbose_same_test_in_two_packages_keeps_both() {
        let (_, ev) = feed_all(concat!(
            "=== RUN   TestDup\n--- PASS: TestDup (0.00s)\nPASS\nok  \texample.com/m/a\t0.01s\n",
            "=== RUN   TestDup\n    b_test.go:7: bug\n--- FAIL: TestDup (0.00s)\nFAIL\nFAIL\texample.com/m/b\t0.01s\n",
        ));
        let s = settled(&ev);
        assert_eq!(one(&s, "example.com/m/a::TestDup"), vec![Outcome::Passed]);
        assert_eq!(one(&s, "example.com/m/b::TestDup"), vec![Outcome::Failed]);
        assert_eq!(s.len(), 2, "{s:?}");
    }

    #[test]
    fn go_verbose_failure_is_live_before_its_package_is_known() {
        let mut r = Recognizers::default();
        let mut ev = Vec::new();
        for l in [
            "=== RUN   TestBad",
            "    x_test.go:3: no",
            "--- FAIL: TestBad (0.00s)",
        ] {
            ev.extend(r.line(l));
        }
        assert!(
            ev.iter().any(|e| matches!(
                e,
                EventKind::TestFinished {
                    outcome: Outcome::Failed,
                    ..
                }
            )),
            "{ev:?}"
        );
    }

    /// `go test -count=3`: iterations are not retries. A failure in any of
    /// them is the test's failure, whatever the later ones did.
    #[test]
    fn go_count_iterations_keep_the_failure() {
        let (_, ev) = feed_all(concat!(
            "=== RUN   TestRace\n--- PASS: TestRace (0.00s)\n",
            "=== RUN   TestRace\n    race_test.go:9: lost update\n--- FAIL: TestRace (0.00s)\n",
            "=== RUN   TestRace\n--- PASS: TestRace (0.00s)\n",
            "FAIL\nFAIL\texample.com/m/r\t0.01s\n",
        ));
        let s = settled(&ev);
        assert_eq!(
            one(&s, "example.com/m/r::TestRace"),
            vec![Outcome::Passed, Outcome::Failed]
        );
        assert_eq!(s.len(), 1, "no phantom test, no package error: {s:?}");
    }

    #[test]
    fn go_count_fail_then_pass_is_a_failure_text() {
        let (r, ev) = feed_all(concat!(
            "=== RUN   TestF\n    f_test.go:5: boom\n--- FAIL: TestF (0.00s)\n",
            "=== RUN   TestF\n--- PASS: TestF (0.00s)\n",
            "FAIL\nFAIL\texample.com/f\t0.01s\n",
        ));
        let s = settled(&ev);
        assert_eq!(one(&s, "example.com/f::TestF"), vec![Outcome::Failed]);
        assert_eq!(s.len(), 1, "{s:?}");
        assert_eq!(r.failed_reported(), 0, "no stale totals");
    }

    /// gotestsum `--rerun-fails` with `-v` text: the rerun is another
    /// `go test` of the package, after its own result line: a retry.
    #[test]
    fn go_text_rerun_after_the_package_line_is_a_retry() {
        let (r, ev) = feed_all(concat!(
            "=== RUN   TestFlaky\n    f_test.go:4: first\n--- FAIL: TestFlaky (0.00s)\nFAIL\nFAIL\texample.com/q\t0.01s\n",
            "=== RUN   TestFlaky\n--- PASS: TestFlaky (0.00s)\nPASS\nok  \texample.com/q\t0.01s\n",
        ));
        let s = settled(&ev);
        assert_eq!(
            one(&s, "example.com/q::TestFlaky"),
            vec![Outcome::Failed, Outcome::Passed]
        );
        assert_eq!(r.failed_reported(), 0);
    }

    #[test]
    fn go_count_fail_then_pass_is_a_failure_json() {
        let (_, ev) = feed_all(concat!(
            r#"{"Action":"run","Package":"p","Test":"TestF"}"#,
            "\n",
            r#"{"Action":"fail","Package":"p","Test":"TestF","Elapsed":0}"#,
            "\n",
            r#"{"Action":"run","Package":"p","Test":"TestF"}"#,
            "\n",
            r#"{"Action":"pass","Package":"p","Test":"TestF","Elapsed":0}"#,
            "\n",
            r#"{"Action":"fail","Package":"p","Elapsed":0}"#,
            "\n",
        ));
        let s = settled(&ev);
        assert_eq!(one(&s, "p::TestF"), vec![Outcome::Failed]);
        assert_eq!(s.len(), 1, "{s:?}");
    }

    /// A panic outside any test fails the package even when a test of it
    /// failed too (round 15: a quarantined failure hid it).
    #[test]
    fn go_json_panic_in_package_output_is_a_package_error() {
        let (_, ev) = feed_all(concat!(
            r#"{"Action":"run","Package":"q","Test":"TestQ"}"#,
            "\n",
            r#"{"Action":"fail","Package":"q","Test":"TestQ","Elapsed":0}"#,
            "\n",
            r#"{"Action":"run","Package":"q","Test":"TestQueue"}"#,
            "\n",
            r#"{"Action":"pass","Package":"q","Test":"TestQueue","Elapsed":0}"#,
            "\n",
            r#"{"Action":"output","Package":"q","Output":"panic: send on closed channel\n"}"#,
            "\n",
            r#"{"Action":"output","Package":"q","Output":"goroutine 12 [running]:\n"}"#,
            "\n",
            r#"{"Action":"fail","Package":"q"}"#,
            "\n",
        ));
        let s = settled(&ev);
        assert_eq!(one(&s, "q::(package)"), vec![Outcome::Errored]);
        assert_eq!(one(&s, "q::TestQ"), vec![Outcome::Failed]);
    }

    #[test]
    fn go_text_panic_outside_a_test_is_a_package_error() {
        let (_, ev) = feed_all(concat!(
            "=== RUN   TestQ\n    q_test.go:5: no\n--- FAIL: TestQ (0.00s)\n",
            "=== RUN   TestQueue\n--- PASS: TestQueue (0.00s)\n",
            "panic: send on closed channel\n\ngoroutine 12 [running]:\n",
            "FAIL\texample.com/q\t0.01s\n",
        ));
        let s = settled(&ev);
        assert_eq!(one(&s, "example.com/q::(package)"), vec![Outcome::Errored]);
        assert_eq!(one(&s, "example.com/q::TestQ"), vec![Outcome::Failed]);
    }

    /// A test's own panic follows its `--- FAIL` line: it explains the
    /// test, it is not another failure.
    #[test]
    fn go_text_panic_in_a_test_is_the_tests_output() {
        let (r, ev) = feed_all(concat!(
            "=== RUN   TestP\n--- FAIL: TestP (0.00s)\npanic: boom [recovered]\n\ngoroutine 7 [running]:\n",
            "\tq_test.go:4 +0x25\nFAIL\texample.com/q\t0.01s\n",
        ));
        let s = settled(&ev);
        assert_eq!(s.len(), 1, "{s:?}");
        let (_, text) = r
            .failure_output
            .iter()
            .find(|(k, _)| k.name == "TestP")
            .expect("the panic is kept as the test's output");
        assert!(text.starts_with("panic: boom"), "{text}");
        assert_eq!(r.failure_output[0].0.path, vec!["example.com/q".to_owned()]);
    }

    /// Without `-v` the message follows the `--- FAIL` line.
    #[test]
    fn go_text_message_after_the_fail_line_is_kept() {
        let (r, _) = feed_all(
            "--- FAIL: TestX (0.00s)\n    x_test.go:9: boom\nFAIL\nFAIL\texample.com/x\t0.01s\n",
        );
        let (k, text) = &r.failure_output[0];
        assert_eq!(
            (k.display().as_str(), text.as_str()),
            ("example.com/x::TestX", "    x_test.go:9: boom")
        );
    }

    /// Round 15: a test printing a package line is not a package line.
    #[test]
    fn a_package_line_printed_by_a_running_test_is_its_output() {
        let (_, ev) = feed_all(concat!(
            "=== RUN   TestA\n    a_test.go:3: no\n--- FAIL: TestA (0.00s)\n",
            "=== RUN   TestSub\nok  \texample.com/fake\t0.010s\n--- PASS: TestSub (0.00s)\n",
            "FAIL\nFAIL\texample.com/real\t0.02s\n",
        ));
        let s = settled(&ev);
        assert_eq!(one(&s, "example.com/real::TestA"), vec![Outcome::Failed]);
        assert_eq!(one(&s, "example.com/real::TestSub"), vec![Outcome::Passed]);
        assert_eq!(s.len(), 2, "{s:?}");
    }

    #[test]
    fn go_json_parent_with_every_subtest_skipped_is_noted() {
        let (_, ev) = feed_all(concat!(
            r#"{"Action":"run","Package":"p","Test":"TestS"}"#,
            "\n",
            r#"{"Action":"run","Package":"p","Test":"TestS/a"}"#,
            "\n",
            r#"{"Action":"skip","Package":"p","Test":"TestS/a","Elapsed":0}"#,
            "\n",
            r#"{"Action":"pass","Package":"p","Test":"TestS","Elapsed":0}"#,
            "\n",
            r#"{"Action":"pass","Package":"p","Elapsed":0}"#,
            "\n",
        ));
        assert!(
            ev.iter().any(|e| matches!(e, EventKind::RunNote { message } if message.contains("every subtest skipped") && message.contains("p::TestS"))),
            "{ev:?}"
        );
    }

    /// gotestsum `--rerun-fails` ends with `DONE 2 runs, 4 tests`: the first
    /// DONE is the suite's count.
    #[test]
    fn gotestsum_rerun_totals_are_the_first_done() {
        let (r, _) = feed_all(concat!(
            "DONE 3 tests, 1 failure in 0.002s\n\n",
            "PASS example.com/m/a.TestFlaky (re-run 1)\n",
            "DONE 2 runs, 4 tests, 1 failure in 0.106s\n",
        ));
        assert_eq!(r.total(), 3);
    }

    #[test]
    fn gotestsum_testname_qualifies_with_the_package() {
        let (_, ev) = feed_all(concat!(
            "FAIL example.com/m/a.TestDup (0.00s)\n    a_test.go:7: REAL BUG\nFAIL example.com/m/a\n",
            "PASS example.com/m/b.TestDup (0.00s)\nok   example.com/m/b\n\n",
            "=== Failed\n=== FAIL: example.com/m/a TestDup (0.00s)\n    a_test.go:7: REAL BUG\n\n",
            "DONE 2 tests, 1 failure in 0.2s\n",
        ));
        let s = settled(&ev);
        assert_eq!(one(&s, "example.com/m/a::TestDup"), vec![Outcome::Failed]);
        assert_eq!(one(&s, "example.com/m/b::TestDup"), vec![Outcome::Passed]);
        assert_eq!(s.len(), 2, "{s:?}");
    }

    #[test]
    fn gotestsum_rerun_that_passes_is_a_second_attempt() {
        let (_, ev) = feed_all(concat!(
            "FAIL example.com/m/a.TestFlaky (0.00s)\n",
            "PASS example.com/m/a.TestFlaky (re-run 1)\n",
            "DONE 2 tests, 1 failure in 0.2s\n",
        ));
        let s = settled(&ev);
        assert_eq!(
            one(&s, "example.com/m/a::TestFlaky"),
            vec![Outcome::Failed, Outcome::Passed]
        );
        assert_eq!(s.len(), 1, "{s:?}");
    }

    #[test]
    fn gotestsum_json_rerun_that_passes_is_a_second_attempt() {
        let (_, ev) = feed_all(concat!(
            r#"{"Action":"run","Package":"p","Test":"TestF"}"#,
            "\n",
            r#"{"Action":"fail","Package":"p","Test":"TestF","Elapsed":0}"#,
            "\n",
            r#"{"Action":"fail","Package":"p","Elapsed":0}"#,
            "\n",
            r#"{"Action":"run","Package":"p","Test":"TestF"}"#,
            "\n",
            r#"{"Action":"pass","Package":"p","Test":"TestF","Elapsed":0}"#,
            "\n",
            r#"{"Action":"fail","Package":"p","Elapsed":0}"#,
            "\n",
        ));
        let s = settled(&ev);
        assert_eq!(one(&s, "p::TestF"), vec![Outcome::Failed, Outcome::Passed]);
        assert_eq!(s.len(), 1, "{s:?}");
    }

    #[test]
    fn a_go_package_that_failed_to_build_is_an_errored_package() {
        let (_, ev) = feed_all(concat!(
            "=== RUN   TestA1\n--- PASS: TestA1 (0.00s)\nPASS\nok  \texample.com/m/a\t0.002s\n",
            "# example.com/m/b [example.com/m/b.test]\n./b_test.go:5:2: undefined: foo\n",
            "FAIL\texample.com/m/b [build failed]\nFAIL\n",
        ));
        let s = settled(&ev);
        assert_eq!(
            one(&s, "example.com/m/b::(package)"),
            vec![Outcome::Errored]
        );
        let msg = ev.iter().find_map(|e| match e {
            EventKind::TestFinished { test, message, .. } if test.name == "(package)" => {
                message.clone()
            }
            _ => None,
        });
        assert!(msg.unwrap().contains("undefined: foo"));
    }

    #[test]
    fn a_go_package_that_failed_without_a_failing_test_is_errored() {
        // TestMain / goleak: only PASS lines, then the package fails.
        let (_, ev) = feed_all(concat!(
            "=== RUN   TestA\n--- PASS: TestA (0.00s)\nPASS\ngoleak: found unexpected goroutines\n",
            "FAIL\texample.com/m/a\t0.01s\n",
        ));
        let s = settled(&ev);
        assert_eq!(
            one(&s, "example.com/m/a::(package)"),
            vec![Outcome::Errored]
        );
        assert_eq!(one(&s, "example.com/m/a::TestA"), vec![Outcome::Passed]);
    }

    #[test]
    fn gotestsum_package_build_failure_and_errors_count() {
        let (r, ev) = feed_all(concat!(
            "PASS example.com/m/a.TestA1 (0.00s)\nok   example.com/m/a\n",
            "FAIL example.com/m/b [build failed]\n\n",
            "=== Errors\nb/b_test.go:5:2: undefined: foo\n\n",
            "DONE 1 tests, 1 error in 0.4s\n",
        ));
        let s = settled(&ev);
        assert_eq!(
            one(&s, "example.com/m/b::(package)"),
            vec![Outcome::Errored]
        );
        assert_eq!(r.failed_reported(), 1);
    }

    #[test]
    fn gotestsum_errors_are_not_counted_twice() {
        // gotestsum lists a build error among its failures too.
        let (r, _) = feed_all(
            &std::fs::read_to_string(format!(
                "{}/fixtures/gotestsum-pkgname.txt",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap(),
        );
        assert_eq!(r.failed_reported(), 3);
    }

    #[test]
    fn gotestsum_testname_failure_keeps_what_the_test_printed() {
        let (_, ev) = feed_all(concat!(
            "FAIL example.com/m/a.TestBad (0.00s)\n    a_test.go:7: want 2 got 3\n",
            "PASS example.com/m/a.TestOk (0.00s)\nDONE 2 tests, 1 failure in 0.1s\n",
        ));
        let msg = ev.iter().find_map(|e| match e {
            EventKind::TestFinished {
                test,
                message: Some(m),
                ..
            } if test.name == "TestBad" => Some(m.clone()),
            _ => None,
        });
        assert_eq!(msg.as_deref(), Some("a_test.go:7: want 2 got 3"));
        assert_eq!(settled(&ev).len(), 2);
    }

    #[test]
    fn a_go_timeout_names_the_running_test() {
        let (_, ev) = feed_all(concat!(
            "=== RUN   TestHang\n",
            "panic: test timed out after 1m0s\n\trunning tests:\n\t\tTestHang (1m0s)\n\ngoroutine 33 [running]:\n",
            "FAIL\texample.com/m/h\t60.01s\n",
        ));
        let s = settled(&ev);
        assert_eq!(one(&s, "example.com/m/h::TestHang"), vec![Outcome::Errored]);
        assert!(!s.contains_key("example.com/m/h::(package)"), "{s:?}");
    }

    #[test]
    fn go_no_tests_to_run_in_every_package_is_noted() {
        let (r, _) = feed_all(
            "testing: warning: no tests to run\nPASS\nok  \texample.com/m/p\t0.001s [no tests to run]\n",
        );
        assert!(r.go_no_tests_only());
        let (r, _) = feed_all(concat!(
            "ok  \texample.com/m/p\t0.001s [no tests to run]\n",
            "ok  \texample.com/m/q\t0.001s\n",
        ));
        assert!(!r.go_no_tests_only());
        let (r, _) = feed_all(
            "ok  \texample.com/m/p\t0.001s [no tests to run]\n?   \texample.com/m/c\t[no test files]\n",
        );
        assert!(r.go_no_tests_only());
    }
}

/// Round 15 (framework recognizers): probes with a hostile console, a suite
/// error next to a failing test, and the Surefire rerun sections.
#[cfg(test)]
mod round15_fw_tests {
    use super::*;

    fn feed_all(input: &str) -> (Recognizers, Vec<EventKind>) {
        let mut r = Recognizers::default();
        let mut all = Vec::new();
        for l in input.lines() {
            all.extend(r.line(l));
        }
        all.extend(r.finish());
        (r, all)
    }

    fn results(ev: &[EventKind]) -> Vec<(String, u32, Outcome)> {
        ev.iter()
            .filter_map(|e| match e {
                EventKind::TestFinished {
                    test,
                    attempt,
                    outcome,
                    ..
                } => Some((test.display(), *attempt, *outcome)),
                _ => None,
            })
            .collect()
    }

    fn msgs(ev: &[EventKind]) -> Vec<(String, String)> {
        ev.iter()
            .filter_map(|e| match e {
                EventKind::TestFinished {
                    test,
                    message: Some(m),
                    ..
                } => Some((test.display(), m.clone())),
                _ => None,
            })
            .collect()
    }

    /// Console output looked like tests: `✕ db migration (pending)` printed
    /// by a `console.log` was recorded as a failed test (exit 0 became 1).
    #[test]
    fn jest_console_output_is_not_a_test() {
        let (_, ev) = feed_all(concat!(
            " PASS  src/deploy.test.js\n",
            "  console.log\n",
            "    Deploy plan:\n",
            "      \u{2715} db migration (pending)\n",
            "      \u{2713} web\n",
            "      at log (src/deploy.test.js:3:11)\n",
            "  deploy\n",
            "    \u{2713} plans (4 ms)\n",
            "Tests: 1 passed, 1 total\n",
        ));
        assert_eq!(
            results(&ev),
            vec![(
                "src/deploy.test.js::deploy > plans".into(),
                1,
                Outcome::Passed
            )]
        );
    }

    #[test]
    fn jest_console_after_the_tree_and_in_console_blocks_is_not_a_test() {
        let (_, ev) = feed_all(concat!(
            " PASS  src/a.test.js\n",
            "  deploy\n",
            "    \u{2713} plans (4 ms)\n",
            "\n",
            "  console.warn\n",
            "    \u{2715} fake one\n",
            "\n",
            "      at warn (src/a.test.js:3:11)\n",
            "\n",
            " PASS  src/b.test.js\n",
            "  \u{25cf} Console\n",
            "\n",
            "    console.error\n",
            "      \u{2715} fake two\n",
            "      \u{2713} fake three\n",
            "\n",
            "      at error (src/b.test.js:3:11)\n",
            "\n",
            "  other\n",
            "    \u{2713} real\n",
            "Tests: 2 passed, 2 total\n",
        ));
        assert_eq!(
            results(&ev),
            vec![
                ("src/a.test.js::deploy > plans".into(), 1, Outcome::Passed),
                ("src/b.test.js::other > real".into(), 1, Outcome::Passed),
            ]
        );
    }

    /// A failing test and a suite that failed to run in one file: both.
    #[test]
    fn jest_suite_error_next_to_a_failing_test_is_recorded() {
        let (_, ev) = feed_all(concat!(
            " FAIL  src/db.test.js\n",
            "  db\n",
            "    \u{2715} writes (3 ms)\n",
            "\n",
            "  \u{25cf} db \u{203a} writes\n",
            "\n",
            "    expect(received).toBe(expected) // Object.is equality\n",
            "\n",
            "    Expected: 1\n",
            "    Received: 2\n",
            "\n",
            "      4 |   expect(a).toBe(1);\n",
            "\n",
            "  \u{25cf} Test suite failed to run\n",
            "\n",
            "    afterAll: connection pool did not drain\n",
            "\n",
            "Tests: 1 failed, 1 total\n",
        ));
        let got = results(&ev);
        assert!(
            got.contains(&("src/db.test.js::db > writes".into(), 1, Outcome::Failed)),
            "{got:?}"
        );
        assert!(
            got.contains(&("src/db.test.js::(file)".into(), 1, Outcome::Errored)),
            "{got:?}"
        );
        // The block's first assertion lines are the message.
        assert!(
            msgs(&ev).contains(&(
                "src/db.test.js::db > writes".into(),
                "expect(received).toBe(expected) // Object.is equality\nExpected: 1\nReceived: 2"
                    .into()
            )),
            "{:?}",
            msgs(&ev)
        );
        let file_msg = msgs(&ev)
            .into_iter()
            .find(|(t, _)| t == "src/db.test.js::(file)")
            .unwrap();
        assert!(file_msg.1.contains("connection pool"), "{file_msg:?}");
    }

    /// A passing verbose file with a bare suite error is the same entry.
    #[test]
    fn jest_suite_error_alone_is_the_file_entry() {
        let (_, ev) = feed_all(concat!(
            " FAIL  src/db.test.js\n",
            "  \u{25cf} Test suite failed to run\n",
            "\n",
            "    boom\n",
        ));
        assert_eq!(
            results(&ev),
            vec![("src/db.test.js::(file)".into(), 1, Outcome::Errored)]
        );
    }

    /// nextest's separator is not the failure's message.
    #[test]
    fn nextest_message_skips_separator_lines() {
        let (_, ev) = feed_all(concat!(
            "        FAIL [   0.004s] crate_a tests::it_works\n",
            "\n",
            "--- STDOUT:              crate_a tests::it_works ---\n",
            "------------\n",
            "thread 'tests::it_works' panicked at src/lib.rs:5:9:\n",
            "assertion failed: false\n",
            "------------\n",
            "     Summary [   0.004s] 1 test run: 0 passed, 1 failed, 0 skipped\n",
        ));
        let m = msgs(&ev);
        assert_eq!(m.len(), 1, "{m:?}");
        assert!(!m[0].1.starts_with("---"), "{m:?}");
        assert!(m[0].1.contains("assertion failed"), "{m:?}");
    }

    #[test]
    fn nextest_text_keeps_the_binary_apart() {
        let (_, ev) = feed_all(concat!(
            "        FAIL [   0.004s] crate_a tests::it_works\n",
            "        PASS [   0.003s] crate_b tests::it_works\n",
        ));
        let files: Vec<Option<String>> = ev
            .iter()
            .filter_map(|e| match e {
                EventKind::TestFinished { test, .. } => Some(test.file.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(files, vec![Some("crate_a".into()), Some("crate_b".into())]);
    }

    #[test]
    fn surefire_flakes_section_is_a_failed_then_a_passed_attempt() {
        let (_, ev) = feed_all(concat!(
            "[WARNING] Tests run: 3, Failures: 0, Errors: 0, Skipped: 0, Flakes: 1\n",
            "[WARNING] Flakes: \n",
            "[WARNING] com.acme.FlakyTest.sometimes\n",
            "[ERROR]   Run 1: FlakyTest.sometimes:12 expected:<1> but was:<2>\n",
            "[INFO]   Run 2: PASS\n",
            "[INFO] \n",
            "[INFO] BUILD SUCCESS\n",
        ));
        assert_eq!(
            results(&ev),
            vec![
                ("com.acme.FlakyTest::sometimes".into(), 1, Outcome::Failed),
                ("com.acme.FlakyTest::sometimes".into(), 2, Outcome::Passed),
            ]
        );
        assert!(
            msgs(&ev)
                .iter()
                .any(|(_, m)| m.contains("expected:<1> but was:<2>"))
        );
    }

    #[test]
    fn surefire_rerun_that_fails_every_time_names_the_test() {
        let (_, ev) = feed_all(concat!(
            "[ERROR] Failures: \n",
            "[ERROR] com.acme.FailTest.always\n",
            "[ERROR]   Run 1: FailTest.always:5 expected:<1> but was:<2>\n",
            "[ERROR]   Run 2: FailTest.always:5 expected:<1> but was:<2>\n",
            "[INFO] \n",
            "[ERROR] Tests run: 1, Failures: 1, Errors: 0, Skipped: 0\n",
        ));
        let got = results(&ev);
        assert_eq!(
            got.last().unwrap().0,
            "com.acme.FailTest::always",
            "{got:?}"
        );
        assert!(got.iter().all(|(_, _, o)| *o == Outcome::Failed), "{got:?}");
    }

    #[test]
    fn surefire_errors_section_entries_are_errored() {
        let (_, ev) = feed_all(concat!(
            "[ERROR] Errors: \n",
            "[ERROR]   com.acme.CalcTest.io:12 IllegalState disk full\n",
            "[ERROR] Failures: \n",
            "[ERROR]   com.acme.CalcTest.adds:9 expected:<2> but was:<3>\n",
        ));
        assert_eq!(
            results(&ev),
            vec![
                ("com.acme.CalcTest::io".into(), 1, Outcome::Errored),
                ("com.acme.CalcTest::adds".into(), 1, Outcome::Failed),
            ]
        );
    }

    #[test]
    fn vitest_unhandled_errors_block_is_an_errored_entry() {
        let (_, ev) = feed_all(concat!(
            " \u{2713} src/a.test.ts > adds 1ms\n",
            "\n",
            " \u{23af}\u{23af}\u{23af}\u{23af} Unhandled Errors \u{23af}\u{23af}\u{23af}\u{23af}\n",
            "\n",
            "Vitest caught 1 unhandled error during the test run.\n",
            "\n",
            "     Errors  1 error\n",
        ));
        assert!(
            results(&ev).iter().any(|(_, _, o)| *o == Outcome::Errored),
            "{:?}",
            results(&ev)
        );
    }

    #[test]
    fn vitest_errors_count_is_remembered() {
        let (r, _) = feed_all("     Errors  2 errors\n");
        assert_eq!(r.unhandled_errors, 2);
    }
}
