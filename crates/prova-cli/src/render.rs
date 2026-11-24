//! Compact output for agents: failures first, counts for the rest, and the
//! next useful command.

use prova_core::engine::{Summary, WaitReason};
use prova_core::ids::format_duration;
use prova_core::model::{Level, Outcome, Status};
use prova_core::store::RunRow;

fn level_name(l: Level) -> &'static str {
    match l {
        Level::Opaque => "opaque (run outcome only)",
        Level::ResultFiles => "result files",
        Level::Recognized => "recognized output",
        Level::Native => "native stream",
    }
}

fn headline(s: &Summary) -> String {
    match s.reason {
        WaitReason::Running => "RUNNING".into(),
        WaitReason::Stall => "STALLED".into(),
        WaitReason::Slow => "SLOW TEST (run still going)".into(),
        WaitReason::Failure => "FAILING (run still going)".into(),
        WaitReason::Done => match (s.status, s.outcome) {
            (Status::Finished, Some(Outcome::Passed))
                if !s.warnings.is_empty() || !s.flaky_passed.is_empty() =>
            {
                "PASSED (unverified: see warnings)".into()
            }
            (Status::Finished, Some(Outcome::Passed)) => "PASSED".into(),
            (Status::Finished, Some(Outcome::Errored)) => "ERRORED".into(),
            (Status::Finished, _) => "FAILED".into(),
            (st, _) => st.as_str().to_uppercase(),
        },
    }
}

/// Strips terminal control sequences from text that came from the process
/// under test before it reaches a terminal.
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // ESC [ params final-byte (CSI), or ESC + one char (e.g. "(B").
            match chars.next() {
                Some('[') => {
                    for d in chars.by_ref() {
                        if ('@'..='~').contains(&d) {
                            break;
                        }
                    }
                }
                Some('(' | ')') => {
                    chars.next();
                }
                _ => {}
            }
        } else if !c.is_control() || c == '\t' {
            out.push(c);
        }
    }
    out
}

pub fn summary(s: &Summary, json: bool) {
    summary_until(s, json, None);
}

/// A summary whose `next:` continues the same wait (`--until done`).
pub fn summary_until(s: &Summary, json: bool, until: Option<prova_core::engine::Until>) {
    print_summary(s, json, until);
}

fn print_summary(s: &Summary, json: bool, until: Option<prova_core::engine::Until>) {
    if json {
        println!("{}", serde_json::to_string(s).unwrap_or_default());
        return;
    }
    let dur = format_duration(std::time::Duration::from_millis(s.elapsed_ms));
    // The command's own output first, prova's verdict last: what is read
    // last is what prova concluded (passed, failed, history, next).
    if !s.output_tail.is_empty() {
        println!("output (last {} lines):", s.output_tail.len());
        for l in &s.output_tail {
            println!("  | {}", clean(l));
        }
    }
    println!("run {}  suite {}  `{}`", s.run, s.suite, s.command);
    let mut line = format!("{}  {dur}", headline(s));
    if let Some(c) = s.exit_code {
        line.push_str(&format!("  exit {c}"));
    }
    // While running, the level is what prova has seen so far: result files
    // and late streams can raise it at the end (round 7).
    let so_far = if s.status.is_terminal() {
        ""
    } else {
        " so far"
    };
    line.push_str(&format!(
        "  level{so_far} {}  code {}",
        level_name(s.level),
        s.code
    ));
    println!("{line}");
    let c = &s.counts;
    let seen = c.results();
    // When the framework counted more than prova saw, say both (ADR 0018).
    let counted = match s.framework {
        Some((n, f)) if n > seen => {
            format!(" seen individually; the framework counted {n} ({f} failed)")
        }
        _ => String::new(),
    };
    // Errored includes tests cut off by the end of the run: say how many.
    let unfinished = s
        .failures
        .iter()
        .filter(|f| {
            f.message
                .as_deref()
                .is_some_and(|m| m.starts_with("test did not finish"))
        })
        .count();
    let errored = if unfinished > 0 {
        format!(
            "{} errored, {unfinished} unfinished",
            c.errored.saturating_sub(unfinished as u64)
        )
    } else {
        format!("{} errored", c.errored)
    };
    if seen > 0 || !counted.is_empty() {
        println!(
            "tests: {} passed, {} failed, {errored}, {} skipped{}{counted}",
            c.passed,
            c.failed,
            c.skipped,
            if c.subtests > 0 {
                format!(
                    " (and {} subtests, {} failed)",
                    c.subtests, c.subtests_failed
                )
            } else {
                String::new()
            }
        );
    }
    for f in &s.failures {
        // An errored test is not a failed assertion; a test cut off by the
        // end of the run is neither (round 5).
        let tag = match (f.outcome, f.message.as_deref()) {
            (Outcome::Errored, Some(m)) if m.starts_with("test did not finish") => "UNFINISHED",
            (Outcome::Errored, _) => "ERROR",
            _ => "FAIL",
        };
        match &f.message {
            Some(m) => println!(
                "  {tag} {}{}: {}",
                clean(&f.test),
                context(f),
                clean(&first_lines(m, 3))
            ),
            None => println!("  {tag} {}{}", clean(&f.test), context(f)),
        }
    }
    if let Some(ms) = s.stalled_ms {
        println!(
            "stalled: no activity for {}",
            format_duration(std::time::Duration::from_millis(ms))
        );
    }
    if !s.running.is_empty() && s.reason != WaitReason::Failure {
        let names: Vec<String> = s.running.iter().map(|t| clean(t)).collect();
        println!("running now: {}", names.join(", "));
    }
    for t in &s.slow {
        println!(
            "  SLOW {}: running {} (budget {}: {})",
            clean(&t.test),
            format_duration(std::time::Duration::from_millis(t.elapsed_ms)),
            format_duration(std::time::Duration::from_millis(t.budget_ms)),
            t.budget_from.describe()
        );
    }
    for w in &s.warnings {
        println!("warning: {}", clean(w));
    }
    if !s.flaky_passed.is_empty() {
        println!(
            "warning: {} known flaky test(s) passed this time, which proves little (not verified): {}",
            s.flaky_passed.len(),
            s.flaky_passed
                .iter()
                .take(5)
                .map(|t| clean(t))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    for q in s.quarantined.iter().take(5) {
        println!(
            "quarantined: {} {} ({}; {})",
            clean(&q.test),
            q.result,
            clean(&q.reason),
            expires_in(q.expires_ms)
        );
    }
    if s.quarantined.len() > 5 {
        println!(
            "quarantined: ... and {} more (prova quarantine list)",
            s.quarantined.len() - 5
        );
    }
    for n in &s.notes {
        // The detail line says it already (round 13: printed twice).
        if s.detail.as_deref() == Some(n.as_str()) {
            continue;
        }
        println!("note: {}", clean(n));
    }
    if !s.slowest.is_empty() {
        println!(
            "slow tests: {} (prova slowest)",
            s.slowest
                .iter()
                .map(|t| format!(
                    "{} {}",
                    clean(&t.test),
                    format_duration(std::time::Duration::from_millis(t.duration_ms))
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(d) = &s.detail {
        println!("detail: {d}");
    }
    if !s.leaked.is_empty() {
        // Say what it means (round 13: a bare list of pids, unexplained).
        println!(
            "leaked processes (killed): {:?}: still alive when the command ended (a server or build daemon it started, such as the MSBuild server); prova stopped them",
            s.leaked
        );
    }
    if let Some(h) = &s.history {
        println!("history: {}", history_line(h));
    }
    println!("next: {}", next(s, until));
}

/// "expires in 13d", or "expired", from the current time.
pub fn expires_in(expires_ms: u64) -> String {
    let now = prova_core::ids::now_ms();
    if expires_ms <= now {
        "expired".to_owned()
    } else {
        format!("expires in {}d", (expires_ms - now).div_ceil(86_400_000))
    }
}

pub fn slowest(rows: &[prova_core::analysis::SlowRow], json: bool) {
    if json {
        println!("{}", serde_json::to_string(rows).unwrap_or_default());
        return;
    }
    if rows.is_empty() {
        println!("no test durations recorded yet");
        return;
    }
    for r in rows {
        println!(
            "p50 {:>7}  p95 {:>7}  {:3} runs  {}",
            format_duration(std::time::Duration::from_millis(r.p50_ms)),
            format_duration(std::time::Duration::from_millis(r.p95_ms)),
            r.runs,
            clean(&r.test)
        );
    }
}

fn context(f: &prova_core::engine::Failure) -> String {
    let Some(c) = &f.context else {
        return String::new();
    };
    let mut parts = Vec::new();
    // A test known to be flaky is not a "new failure", whatever its message;
    // a test with no earlier result anywhere is neither new nor old yet.
    // A cancelled or aborted run proves nothing about the code (round 6).
    if c.run_cut_short {
        return " [run cancelled or aborted: no evidence about the code]".to_owned();
    }
    // "New" needs an earlier pass. Earlier failures are counted whatever
    // their message: the same failure is often captured with more or less
    // detail by different runners (round 6).
    if c.first_seen {
        parts.push("first time seen: no history yet".to_owned());
    } else if c.failed_before > 0 {
        parts.push(format!("failed {}x before", c.failed_before));
    } else if c.passed_before && !c.flaky {
        // Thin evidence is said (round 12: "new failure" after one pass
        // sent the agent after its own fix).
        parts.push(match c.passes_before {
            0 | 1 => "new failure (it passed only once before)".to_owned(),
            _ => "new failure".to_owned(),
        });
    } else if !c.flaky {
        parts.push("has not passed yet".to_owned());
    }
    if !c.env_dependent.is_empty() {
        parts.push(format!(
            "passed on this code with a different environment ({}): environment-dependent, not flaky",
            c.env_dependent.join(", ")
        ));
    } else if c.failing_now {
        // Whatever its past: it fails every time now (round 9).
        parts.push("failing now: more failures in a row than its flakiness explains".to_owned());
    } else if c.flaky {
        parts.push("FLAKY".to_owned());
    } else if c.suite_passed_on_same_code {
        parts.push("likely FLAKY: suite passed on this exact code before".to_owned());
    }
    if let Some(code) = &c.last_passed_code {
        parts.push(format!("last passed at {code}"));
    }
    format!(" [{}]", parts.join(", "))
}

pub fn test_stats(s: &prova_core::analysis::TestStats, json: bool) {
    if json {
        println!("{}", serde_json::to_string(s).unwrap_or_default());
        return;
    }
    println!(
        "{}  (suite{} {}{})",
        clean(&s.display),
        if s.suites.len() > 1 { "s" } else { "" },
        s.suites.join(", "),
        if s.suites.len() > 1 {
            "; history shared across them"
        } else {
            ""
        }
    );
    println!(
        "{} runs with a result, {} failed ({:.0}%), {} pass/fail switches, {} passed on retry, both passed and failed on {} code version(s){}",
        s.runs,
        s.failures,
        s.failure_rate * 100.0,
        s.transitions,
        s.passed_on_retry,
        s.flips_on_same_code,
        if s.flaky { "  => FLAKY" } else { "" }
    );
    if let (Some(p50), Some(p95)) = (s.p50_ms, s.p95_ms) {
        println!("duration p50 {p50}ms  p95 {p95}ms");
    }
    if let Some(size) = &s.size {
        println!("size: {}", clean(&size.label));
    }
    // Skips (another test's `--select` run) say nothing about this test:
    // count them instead of listing them (round 7).
    let skipped = s
        .recent
        .iter()
        .filter(|r| r.outcome == Outcome::Skipped)
        .count();
    let shown: Vec<_> = s
        .recent
        .iter()
        .filter(|r| r.outcome != Outcome::Skipped)
        .collect();
    if skipped > 0 {
        println!(
            "  ({skipped} skipped attempts not listed: the test was ignored, or the run selected other tests)"
        );
    }
    for r in shown.iter().take(15) {
        let retry = if r.attempts > 1 {
            format!(" ({} attempts)", r.attempts)
        } else {
            String::new()
        };
        let msg = r
            .message
            .as_deref()
            .map(|m| format!("  {}", clean(&first_lines(m, 1))))
            .unwrap_or_default();
        println!(
            "  {}  {:7}  {}{}{}",
            r.run,
            r.outcome.as_str(),
            r.code,
            retry,
            msg
        );
    }
    if shown.len() > 15 {
        println!("  ... {} more (`--json` lists all)", shown.len() - 15);
    }
}

fn first_lines(m: &str, n: usize) -> String {
    let lines: Vec<&str> = m.lines().filter(|l| !l.trim().is_empty()).take(n).collect();
    lines.join(" / ")
}

fn history_line(h: &serde_json::Value) -> String {
    let n = h["previous_runs"].as_u64().unwrap_or(0);
    // Same suite and same selection: a `--select` run is compared only
    // with runs of the same selection.
    let mut parts = vec![format!(
        "{} earlier finished run{} of this suite{}",
        if n == 0 {
            "no".to_owned()
        } else {
            n.to_string()
        },
        if n == 1 { "" } else { "s" },
        if h["selection"].is_string() {
            " with this selection"
        } else {
            // Runs with --select are compared only with each other.
            " (full runs)"
        }
    )];
    let cut = h["cut_short_runs"].as_u64().unwrap_or(0);
    if cut > 0 {
        parts[0].push_str(&format!(", plus {cut} cancelled or aborted"));
    }
    match h["last_passed"].as_object() {
        Some(p) => parts.push(format!(
            "last green: run {} at {}",
            p["run"].as_str().unwrap_or(""),
            p["code"].as_str().unwrap_or("")
        )),
        // "None of them passed" about zero runs reads as a verdict (round 9).
        None if n > 0 => parts.push(match h["passed_elsewhere"].as_object() {
            Some(o) => format!(
                "none of them passed, but this code passed in suite {} (run {})",
                o["suite"].as_str().unwrap_or("?"),
                o["run"].as_str().unwrap_or("?")
            ),
            None => match h["unverified_passes"].as_u64().unwrap_or(0) {
                0 => "none of them passed".into(),
                // (round 12: "none of them passed; on this exact code: passed?")
                u => format!("none of them passed verified ({u} passed with warnings: `prova status <run>` says why)"),
            },
        }),
        None => {}
    }
    let seen = h["same_failure_seen"].as_u64().unwrap_or(0);
    if seen > 0 {
        parts.push(format!(
            "same failure seen {seen}x (first {})",
            h["same_failure_first_run"].as_str().unwrap_or("?")
        ));
    }
    if let Some(o) = h["same_code_outcomes"].as_array()
        && !o.is_empty()
    {
        parts.push(format!(
            "on this exact code: {}",
            o.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    parts.join("; ")
}

fn next(s: &Summary, until: Option<prova_core::engine::Until>) -> String {
    match s.reason {
        WaitReason::Running if until == Some(prova_core::engine::Until::Done) => {
            format!("prova wait {} --until done", s.run)
        }
        WaitReason::Running => format!("prova wait {} --after {}", s.run, s.last_seq),
        // Every failure so far is a known flaky test: there is nothing to
        // fix yet; keep watching for a real one (round 8).
        WaitReason::Failure if all_known_flaky(s) => format!(
            "only known flaky test(s) failed so far: `prova wait {} --after {}` to keep watching for a real failure",
            s.run, s.last_seq
        ),
        WaitReason::Failure => format!(
            "fix and rerun, or `prova cancel {}`; `prova wait {} --after {}` to keep watching",
            s.run, s.run, s.last_seq
        ),
        WaitReason::Stall | WaitReason::Slow => format!(
            "prova cancel {}  (or `prova wait {} --after {}` if it is expected)",
            s.run, s.run, s.last_seq
        ),
        WaitReason::Done
            if s.outcome == Some(Outcome::Passed)
                && (!s.warnings.is_empty() || !s.flaky_passed.is_empty()) =>
        {
            // Not "done": the pass is unverified (round 13).
            "done, but this pass is unverified: act on the warnings above".into()
        }
        WaitReason::Done if s.outcome == Some(Outcome::Passed) => "done".into(),
        WaitReason::Done if all_known_flaky(s) => {
            // The test's own name, not a placeholder (round 10).
            let t = s
                .failures
                .first()
                .map(|f| clean(&f.test))
                .unwrap_or_default();
            let confirm = if s.suite.starts_with("cmd:") {
                format!("`prova history {t}`")
            } else if s.can_select {
                format!("`prova run {} --select '{t}' --repeat 5`", s.suite)
            } else {
                // `make` cannot select (round 13: the suggestion failed).
                format!("`prova run {} --repeat 5`", s.suite)
            };
            format!(
                "only known flaky test(s) failed: confirm with {confirm}, and report them; nothing else failed"
            )
        }
        WaitReason::Done => format!("prova output {} --tail 200", s.run),
    }
}

/// Every failure shown is a test history already knows as flaky.
fn all_known_flaky(s: &Summary) -> bool {
    // Not when the run failed for another reason too (a crash, round 10).
    s.detail.is_none()
        && !s.failures.is_empty()
        && s.failures.len()
            == (s.counts.failed + s.counts.errored + s.counts.subtests_failed) as usize
        && s.failures.iter().all(|f| {
            f.context
                .as_ref()
                .is_some_and(|c| c.flaky && !c.run_cut_short)
        })
}

/// One stderr line while a long wait goes on (ADR 0019): the run is alive.
/// One status line for a person watching a run in a terminal: elapsed
/// time and counts, the test running now, or, when prova sees no tests
/// (an opaque `make test`), the command's last line of output.
pub fn live_line(s: &Summary, last_output: Option<&str>, width: usize) -> String {
    let c = &s.counts;
    let elapsed = format_duration(std::time::Duration::from_millis(s.elapsed_ms));
    let seen = c.passed + c.failed + c.errored + c.skipped;
    let mut line = if seen > 0 || !s.running.is_empty() {
        let mut l = format!("{elapsed}  {} passed", c.passed);
        if c.failed + c.errored > 0 {
            l.push_str(&format!("  {} failed", c.failed + c.errored));
        }
        if c.skipped > 0 {
            l.push_str(&format!("  {} skipped", c.skipped));
        }
        if let Some(t) = s.running.first() {
            l.push_str(&format!("  running {}", clean(t)));
        }
        l
    } else {
        match last_output.map(str::trim).filter(|o| !o.is_empty()) {
            Some(o) => format!("{elapsed}  {}", clean(o)),
            None => format!("{elapsed}  running `{}`", clean(&s.command)),
        }
    };
    if line.chars().count() > width {
        line = line
            .chars()
            .take(width.saturating_sub(1))
            .collect::<String>()
            + "…";
    }
    line
}

/// The live status line: redrawn in place on a terminal, never written
/// anywhere else (agents and CI read a log, where it would be noise).
pub struct Live {
    on: bool,
    shown: bool,
    width: usize,
}

impl Live {
    pub fn new() -> Self {
        use std::io::IsTerminal;
        let on =
            std::io::stderr().is_terminal() && std::env::var("TERM").map_or(true, |t| t != "dumb");
        let width = std::env::var("COLUMNS")
            .ok()
            .and_then(|c| c.parse::<usize>().ok())
            .unwrap_or(100)
            .clamp(20, 240)
            - 1;
        Live {
            on,
            shown: false,
            width,
        }
    }

    pub fn on(&self) -> bool {
        self.on
    }

    pub fn show(&mut self, s: &Summary, last_output: Option<&str>) {
        if self.on {
            eprint!("\r\x1b[2K{}", live_line(s, last_output, self.width));
            self.shown = true;
        }
    }

    /// Erases the line before anything else is printed.
    pub fn clear(&mut self) {
        if self.shown {
            eprint!("\r\x1b[2K");
            self.shown = false;
        }
    }
}

impl Default for Live {
    fn default() -> Self {
        Self::new()
    }
}

pub fn heartbeat(s: &Summary) {
    let c = &s.counts;
    let mut line = format!(
        "... {}: {} passed, {} failed, {} skipped",
        format_duration(std::time::Duration::from_millis(s.elapsed_ms)),
        c.passed,
        c.failed + c.errored,
        c.skipped
    );
    if !s.running.is_empty() {
        // Logs are read too: a 3000-character test name made a 3000-character
        // line (round 12).
        let names: Vec<String> = s
            .running
            .iter()
            .take(3)
            .map(|t| {
                let c = clean(t);
                if c.chars().count() > 100 {
                    c.chars().take(99).collect::<String>() + "…"
                } else {
                    c
                }
            })
            .collect();
        line.push_str(&format!("; running now: {}", names.join(", ")));
    }
    eprintln!("{line}");
}

pub fn progress(s: &Summary, reported: &mut std::collections::BTreeSet<String>) {
    match s.reason {
        WaitReason::Failure => {
            for f in &s.failures {
                if reported.insert(f.test.clone()) {
                    eprintln!("FAIL {}{}", clean(&f.test), context(f));
                }
            }
        }
        WaitReason::Slow => {
            for t in &s.slow {
                if reported.insert(format!("slow:{}", t.test)) {
                    eprintln!(
                        "SLOW {} (over its {} budget: {})",
                        clean(&t.test),
                        format_duration(std::time::Duration::from_millis(t.budget_ms)),
                        t.budget_from.describe()
                    );
                }
            }
        }
        WaitReason::Stall => {
            eprintln!(
                "stalled: no activity for {}",
                format_duration(std::time::Duration::from_millis(s.stalled_ms.unwrap_or(0)))
            );
        }
        _ => {}
    }
}

pub fn runs(rows: &[RunRow], unverified: &std::collections::BTreeSet<String>, json: bool) {
    if json {
        println!("{}", serde_json::to_string(rows).unwrap_or_default());
        return;
    }
    if rows.is_empty() {
        println!(
            "no runs yet in this repository; start one with `prova run -- <your test command>` (`prova status --all` lists every repository on this machine)"
        );
        return;
    }
    for r in rows {
        // A thin pass is not a plain "passed" in the list either (round 9).
        let outcome = match r.outcome {
            Some(Outcome::Passed) if unverified.contains(&r.id) => "passed?",
            // A cancelled or killed run has no test outcome of its own:
            // "cancelled errored" read as a test error (round 13).
            _ if matches!(r.status, prova_core::model::Status::Cancelled) => "-",
            o => o.map(Outcome::as_str).unwrap_or("-"),
        };
        let dur = match (r.started_ms, r.ended_ms) {
            (Some(a), Some(b)) => {
                format_duration(std::time::Duration::from_millis(b.saturating_sub(a)))
            }
            // A running run: how long so far (round 8 showed "-").
            (Some(a), None) => format!(
                "{}+",
                format_duration(std::time::Duration::from_millis(
                    prova_core::ids::now_ms().saturating_sub(a)
                ))
            ),
            _ => "-".into(),
        };
        // A `--select` run is not a full run of the suite: say so (round 7).
        let sel = r.spec["selection"]
            .as_str()
            .map(|x| format!("  --select {}", clean(x)))
            .unwrap_or_default();
        println!(
            "{}  {:9}  {:7}  {:>7}  {}{sel}",
            r.id,
            r.status.as_str(),
            outcome,
            dur,
            r.suite
        );
    }
}

pub fn exp_progress(r: &prova_core::experiment::Report) {
    if let Some(c) = &r.comparison {
        eprintln!(
            "{} vs {}: {}/{} baseline, {}/{} candidate trials",
            c.candidate,
            c.baseline,
            c.baseline_trials,
            c.target_trials,
            c.candidate_trials,
            c.target_trials
        );
    }
}

pub fn experiment(r: &prova_core::experiment::Report, json: bool) {
    if json {
        println!("{}", serde_json::to_string(r).unwrap_or_default());
        return;
    }
    println!(
        "experiment {}  best={}  protocol {}",
        r.experiment, r.best, r.protocol_hash
    );
    println!("hypothesis: {}", clean(&r.hypothesis));
    println!("{}", protocol_line(&r.protocol));
    // The current best leads, with how it became best, before the latest
    // comparison (which may be a later REJECT).
    if r.best != "baseline" {
        let how = r
            .decisions
            .iter()
            .rev()
            .find(|d| d["candidate"] == r.best.as_str() && d["decision"] != "discard")
            .map(|d| {
                let why = clean(d["reason"].as_str().unwrap_or(""));
                if d["decision"] == "reset" {
                    format!("set by hand: {why}")
                } else {
                    format!(
                        "ACCEPT, comparison {}: {why}",
                        d["comparison"].as_str().unwrap_or("")
                    )
                }
            })
            .unwrap_or_else(|| "accepted".to_owned());
        println!("best: {} ({how})", r.best);
    }
    if let Some(c) = &r.comparison {
        let head = match (c.status.as_str(), c.verdict.as_deref()) {
            ("decided", Some(v)) => v.to_uppercase(),
            (s, _) => s.to_uppercase(),
        };
        println!(
            "{head}  {} vs {}  ({} + {} trials, target {} per arm, max {})",
            c.candidate,
            c.baseline,
            c.baseline_trials,
            c.candidate_trials,
            c.target_trials,
            c.max_trials
        );
        if let Some(res) = &c.result {
            if let Some(reason) = res["reason"].as_str() {
                println!("reason: {}", clean(reason));
            }
            if let Some(spec) = res["specification"].as_str().filter(|s| *s != "violated") {
                println!("specification: {}", clean(spec));
            }
            if let Some(sb) = res["sandbox"].as_str() {
                println!("sandbox: {}", clean(sb));
            }
            for c in res["caveats"].as_array().into_iter().flatten() {
                println!("caveat: {}", clean(c.as_str().unwrap_or("")));
            }
            let p = &res["primary"];
            let num = |k: &str| prova_core::experiment::short(p[k].as_f64().unwrap_or(f64::NAN));
            if p.is_object() {
                println!(
                    "{}: {} median {} (MAD {}), {} median {} (MAD {}), p={:.4} ({} test; adjusted {:.4} for {} comparison(s) on this baseline and metric (revalidations count), Holm)",
                    p["metric"].as_str().unwrap_or(""),
                    c.baseline,
                    num("baseline_median"),
                    num("baseline_mad"),
                    c.candidate,
                    num("candidate_median"),
                    num("candidate_mad"),
                    p["p"].as_f64().unwrap_or(f64::NAN),
                    p["method"].as_str().unwrap_or(""),
                    p["p_adjusted"].as_f64().unwrap_or(f64::NAN),
                    res["comparisons_in_family"].as_u64().unwrap_or(1),
                );
                let outl = p["outliers_baseline"].as_array().map_or(0, Vec::len)
                    + p["outliers_candidate"].as_array().map_or(0, Vec::len);
                if outl > 0 {
                    println!(
                        "outliers flagged (kept): {} {}, {} {}",
                        c.baseline,
                        p["outliers_baseline"].as_array().map_or(0, Vec::len),
                        c.candidate,
                        p["outliers_candidate"].as_array().map_or(0, Vec::len)
                    );
                }
            }
        }
        let next = match (c.status.as_str(), c.verdict.as_deref()) {
            ("running", _) => format!("prova exp wait {}", r.experiment),
            // A revalidation: the best, again, against the original baseline.
            ("decided", Some("accept"))
                if c.baseline == "baseline"
                    && r.best == c.candidate
                    && r.decisions
                        .iter()
                        .filter(|d| d["candidate"] == c.candidate.as_str())
                        .count()
                        > 1 =>
            {
                format!(
                    "revalidated against the original baseline: report experiment {} and this verdict",
                    r.experiment
                )
            }
            ("decided", Some("accept")) => format!(
                "keep it; try the next idea, or `prova exp revalidate {}`",
                r.experiment
            ),
            ("decided", _) => format!(
                "discard it; record another candidate: prova exp candidate {}",
                r.experiment
            ),
            _ => format!(
                "read what failed: {}",
                r.logs
                    .clone()
                    .unwrap_or_else(|| format!("prova exp log {}", r.experiment))
            ),
        };
        println!("next: {next}");
    } else {
        println!(
            "no comparison yet; next: prova exp candidate {}",
            r.experiment
        );
    }
    for w in &r.integrity {
        println!("warning: {}", clean(w));
    }
    if !r.decisions.is_empty() {
        // The latest few: the log grows with every candidate (round 7).
        let skip = r.decisions.len().saturating_sub(5);
        if skip > 0 {
            println!("decisions ({skip} earlier not shown; `--json` has all):");
        } else {
            println!("decisions:");
        }
        for d in r.decisions.iter().skip(skip) {
            println!(
                "  {:7} {}  {}",
                // Stored as `crash` (a contract); it means the comparison
                // could not run to a verdict, whatever the cause.
                match d["decision"].as_str().unwrap_or("") {
                    "crash" => "failed",
                    other => other,
                },
                d["candidate"].as_str().unwrap_or(""),
                clean(d["reason"].as_str().unwrap_or(""))
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn clean_strips_ansi_sequences() {
        assert_eq!(
            super::clean("\u{1b}[31m-   x\u{1b}(B\u{1b}[m done\u{7}"),
            "-   x done"
        );
    }
}

#[cfg(test)]
mod label_tests {
    use prova_core::analysis::FailureContext;
    use prova_core::engine::Failure;

    fn label(seen_before: usize, flaky: bool) -> String {
        label_after(seen_before, flaky, 5)
    }

    fn label_after(seen_before: usize, flaky: bool, passes_before: usize) -> String {
        super::context(&Failure {
            test: "t".into(),
            outcome: prova_core::model::Outcome::Failed,
            message: None,
            context: Some(FailureContext {
                first_seen: false,
                seen_before,
                failed_before: seen_before,
                passed_before: true,
                passes_before,
                run_cut_short: false,
                flaky,
                last_passed_code: None,
                suite_passed_on_same_code: false,
                failing_now: false,
                env_dependent: vec![],
            }),
        })
    }

    /// Usability round 3: "[new failure, FLAKY]" contradicted itself.
    #[test]
    fn a_flaky_test_is_not_a_new_failure() {
        assert_eq!(label(0, true), " [FLAKY]");
        assert_eq!(label(0, false), " [new failure]");
        // Round 12: one earlier pass is thin evidence, and is said.
        assert_eq!(
            label_after(0, false, 1),
            " [new failure (it passed only once before)]"
        );
        assert_eq!(label(2, true), " [failed 2x before, FLAKY]");
        let mut first = super::context(&Failure {
            test: "t".into(),
            outcome: prova_core::model::Outcome::Failed,
            message: None,
            context: Some(FailureContext {
                failed_before: 0,
                passed_before: false,
                passes_before: 0,
                run_cut_short: false,
                first_seen: true,
                seen_before: 0,
                flaky: false,
                last_passed_code: None,
                suite_passed_on_same_code: false,
                failing_now: false,
                env_dependent: vec![],
            }),
        });
        assert_eq!(first, " [first time seen: no history yet]");
        first.clear();
        let never = super::context(&Failure {
            test: "t".into(),
            outcome: prova_core::model::Outcome::Failed,
            message: None,
            context: Some(FailureContext {
                first_seen: false,
                seen_before: 0,
                failed_before: 0,
                passed_before: false,
                passes_before: 0,
                run_cut_short: false,
                flaky: false,
                last_passed_code: None,
                suite_passed_on_same_code: false,
                failing_now: false,
                env_dependent: vec![],
            }),
        });
        assert_eq!(never, " [has not passed yet]", "never passed: not new");
    }
}

/// The protocol in one line, so a change is never silent.
pub fn protocol_line(p: &prova_core::experiment::Protocol) -> String {
    let mut s = format!(
        "protocol: metric {} ({} is better), min effect {:.1}%, trials {}..{} (min {}), warmup {}",
        p.metric,
        if p.better == prova_core::experiment::Better::Lower {
            "lower"
        } else {
            "higher"
        },
        p.min_effect * 100.0,
        p.trials,
        p.max_trials,
        p.min_trials,
        p.warmup
    );
    if !p.specs.is_empty() {
        let names: Vec<&str> = p.specs.iter().map(|f| f.path.as_str()).collect();
        s.push_str(&format!("; spec {}", names.join(", ")));
    }
    if p.scope_only {
        s.push_str(&format!(
            "; candidates may change only {}",
            p.scope.join(", ")
        ));
    } else if p.scope.is_empty() {
        s.push_str("; candidates may not change tests, benchmarks or build files");
    } else {
        s.push_str(&format!(
            "; candidates may not change tests, benchmarks or build files, except {}",
            p.scope.join(", ")
        ));
    }
    match &p.oracle {
        Some(o) => s.push_str(&format!("; oracle `{o}`")),
        None if !p.specs.is_empty() => s.push_str("; no oracle"),
        None => {}
    }
    s
}

#[cfg(test)]
mod live_tests {
    use prova_core::engine::{Counts, Summary, WaitReason};
    use prova_core::model::{Level, Status};

    fn summary(passed: u64, failed: u64, running: &[&str]) -> Summary {
        Summary {
            can_select: false,
            run: "r".into(),
            suite: "s".into(),
            command: "make test".into(),
            reason: WaitReason::Running,
            status: Status::Running,
            outcome: None,
            level: Level::Opaque,
            last_seq: 0,
            elapsed_ms: 12_000,
            counts: Counts {
                passed,
                failed,
                ..Counts::default()
            },
            running: running.iter().map(|s| (*s).to_owned()).collect(),
            framework: None,
            failures: vec![],
            stalled_ms: None,
            slow: vec![],
            exit_code: None,
            detail: None,
            leaked: vec![],
            notes: vec![],
            warnings: vec![],
            verified: None,
            output_tail: vec![],
            code: String::new(),
            fingerprint: None,
            store: String::new(),
            flaky_passed: vec![],
            history: None,
            quarantined: vec![],
            slowest: vec![],
        }
    }

    #[test]
    fn the_live_line_shows_counts_and_the_running_test() {
        let l = super::live_line(&summary(37, 1, &["calc::divides"]), None, 100);
        assert_eq!(l, "12.0s  37 passed  1 failed  running calc::divides");
    }

    /// A 50 s `prova run -- make test` that passed showed nothing until the
    /// end: with no tests seen, the command's own last line moves instead.
    #[test]
    fn an_opaque_run_shows_its_last_output() {
        let s = summary(0, 0, &[]);
        assert_eq!(
            super::live_line(&s, Some("  CC  src/parser.o\u{1b}[0m"), 100),
            "12.0s  CC  src/parser.o"
        );
        assert_eq!(
            super::live_line(&s, None, 100),
            "12.0s  running `make test`"
        );
        let long = super::live_line(&s, Some(&"x".repeat(300)), 40);
        assert_eq!(long.chars().count(), 40);
        assert!(long.ends_with('…'));
    }
}
