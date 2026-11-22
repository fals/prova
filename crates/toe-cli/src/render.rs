//! Compact output for agents: failures first, counts for the rest, and the
//! next useful command.

use toe_core::engine::{Summary, WaitReason};
use toe_core::ids::format_duration;
use toe_core::model::{Level, Outcome, Status};
use toe_core::store::RunRow;

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
        WaitReason::Failure => "FAILING (run still going)".into(),
        WaitReason::Done => match (s.status, s.outcome) {
            (Status::Finished, Some(Outcome::Passed)) => "PASSED".into(),
            (Status::Finished, _) => "FAILED".into(),
            (st, _) => st.as_str().to_uppercase(),
        },
    }
}

/// Strips terminal control sequences from text that came from the process
/// under test before it reaches a terminal.
pub fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .collect()
}

pub fn summary(s: &Summary, json: bool) {
    if json {
        println!("{}", serde_json::to_string(s).unwrap_or_default());
        return;
    }
    let dur = format_duration(std::time::Duration::from_millis(s.elapsed_ms));
    println!("run {}  suite {}  `{}`", s.run, s.suite, s.command);
    let mut line = format!("{}  {dur}", headline(s));
    if let Some(c) = s.exit_code {
        line.push_str(&format!("  exit {c}"));
    }
    line.push_str(&format!("  level {}  code {}", level_name(s.level), s.code));
    println!("{line}");
    let c = &s.counts;
    if c.passed + c.failed + c.skipped + c.errored > 0 {
        println!(
            "tests: {} passed, {} failed, {} errored, {} skipped",
            c.passed, c.failed, c.errored, c.skipped
        );
    }
    for f in &s.failures {
        match &f.message {
            Some(m) => println!(
                "  FAIL {}{}: {}",
                clean(&f.test),
                context(f),
                clean(&first_lines(m, 3))
            ),
            None => println!("  FAIL {}{}", clean(&f.test), context(f)),
        }
    }
    if let Some(ms) = s.stalled_ms {
        println!(
            "stalled: no activity for {}",
            format_duration(std::time::Duration::from_millis(ms))
        );
    }
    if let Some(d) = &s.detail {
        println!("detail: {d}");
    }
    if !s.leaked.is_empty() {
        println!("leaked processes (killed): {:?}", s.leaked);
    }
    if !s.output_tail.is_empty() {
        println!("output (last {} lines):", s.output_tail.len());
        for l in &s.output_tail {
            println!("  | {}", clean(l));
        }
    }
    if let Some(h) = &s.history {
        println!("history: {}", history_line(h));
    }
    println!("next: {}", next(s));
}

fn context(f: &toe_core::engine::Failure) -> String {
    let Some(c) = &f.context else {
        return String::new();
    };
    let mut parts = Vec::new();
    if c.seen_before > 0 {
        parts.push(format!("seen {}x before", c.seen_before));
    } else {
        parts.push("new failure".to_owned());
    }
    if c.flaky {
        parts.push("FLAKY".to_owned());
    }
    if let Some(code) = &c.last_passed_code {
        parts.push(format!("last passed at {code}"));
    }
    format!(" [{}]", parts.join(", "))
}

pub fn test_stats(s: &toe_core::analysis::TestStats, json: bool) {
    if json {
        println!("{}", serde_json::to_string(s).unwrap_or_default());
        return;
    }
    println!(
        "{}  (suite {}, id {})",
        clean(&s.display),
        s.suite,
        s.test_id
    );
    println!(
        "{} runs, {} failed ({:.0}%), {} transitions, {} passed on retry, {} flips on same code{}",
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
    for r in s.recent.iter().take(15) {
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
}

fn first_lines(m: &str, n: usize) -> String {
    let lines: Vec<&str> = m.lines().filter(|l| !l.trim().is_empty()).take(n).collect();
    lines.join(" / ")
}

fn history_line(h: &serde_json::Value) -> String {
    let mut parts = vec![format!("{} previous runs", h["previous_runs"])];
    match h["last_passed"].as_object() {
        Some(p) => parts.push(format!(
            "last passed {} at {}",
            p["run"].as_str().unwrap_or(""),
            p["code"].as_str().unwrap_or("")
        )),
        None => parts.push("never passed".into()),
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

fn next(s: &Summary) -> String {
    match s.reason {
        WaitReason::Running => format!("toe wait {} --after {}", s.run, s.last_seq),
        WaitReason::Failure => format!(
            "fix and rerun, or `toe cancel {}`; `toe wait {} --after {}` to keep watching",
            s.run, s.run, s.last_seq
        ),
        WaitReason::Stall => format!(
            "toe cancel {}  (or `toe wait {} --after {}` if it is expected)",
            s.run, s.run, s.last_seq
        ),
        WaitReason::Done if s.outcome == Some(Outcome::Passed) => "done".into(),
        WaitReason::Done => format!("toe output {} --tail 200", s.run),
    }
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
        WaitReason::Stall => {
            eprintln!(
                "stalled: no activity for {}",
                format_duration(std::time::Duration::from_millis(s.stalled_ms.unwrap_or(0)))
            );
        }
        _ => {}
    }
}

pub fn runs(rows: &[RunRow], json: bool) {
    if json {
        println!("{}", serde_json::to_string(rows).unwrap_or_default());
        return;
    }
    if rows.is_empty() {
        println!("no runs yet; start one with `toe run -- <your test command>`");
        return;
    }
    for r in rows {
        let outcome = r.outcome.map(Outcome::as_str).unwrap_or("-");
        let dur = match (r.started_ms, r.ended_ms) {
            (Some(a), Some(b)) => {
                format_duration(std::time::Duration::from_millis(b.saturating_sub(a)))
            }
            _ => "-".into(),
        };
        println!(
            "{}  {:9}  {:7}  {:>7}  {}",
            r.id,
            r.status.as_str(),
            outcome,
            dur,
            r.suite
        );
    }
}
