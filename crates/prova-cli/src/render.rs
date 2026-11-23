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
            (Status::Finished, Some(Outcome::Passed)) if !s.warnings.is_empty() => {
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
    let seen = c.passed + c.failed + c.skipped + c.errored;
    // When the framework counted more than prova saw, say both (ADR 0018).
    let counted = match s.framework {
        Some((n, f)) if n > seen => {
            format!(" seen individually; the framework counted {n} ({f} failed)")
        }
        _ => String::new(),
    };
    if seen > 0 || !counted.is_empty() {
        println!(
            "tests: {} passed, {} failed, {} errored, {} skipped{counted}",
            c.passed, c.failed, c.errored, c.skipped
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
            "  SLOW {}: running {} (budget {})",
            clean(&t.test),
            format_duration(std::time::Duration::from_millis(t.elapsed_ms)),
            format_duration(std::time::Duration::from_millis(t.budget_ms))
        );
    }
    for w in &s.warnings {
        println!("warning: {}", clean(w));
    }
    for n in &s.notes {
        println!("note: {}", clean(n));
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

fn context(f: &prova_core::engine::Failure) -> String {
    let Some(c) = &f.context else {
        return String::new();
    };
    let mut parts = Vec::new();
    // A test known to be flaky is not a "new failure", whatever its message;
    // a test with no earlier result anywhere is neither new nor old yet.
    // "New" needs an earlier pass; an earlier failure with another message
    // is still an earlier failure (round 5).
    if c.first_seen {
        parts.push("first time seen: no history yet".to_owned());
    } else if c.seen_before > 0 {
        parts.push(format!("seen {}x before", c.seen_before));
    } else if c.failed_before > 0 {
        parts.push(format!(
            "failed {}x before, with another message",
            c.failed_before
        ));
    } else if c.passed_before && !c.flaky {
        parts.push("new failure".to_owned());
    } else if !c.flaky {
        parts.push("has not passed yet".to_owned());
    }
    if c.flaky {
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
        "{} finished runs, {} failed ({:.0}%), {} pass/fail switches, {} passed on retry, both passed and failed on {} code version(s){}",
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
    let n = h["previous_runs"].as_u64().unwrap_or(0);
    // Same suite and same selection: a `--select` run is compared only
    // with runs of the same selection.
    let mut parts = vec![format!(
        "{n} earlier run{} of this suite{}",
        if n == 1 { "" } else { "s" },
        if h["selection"].is_string() {
            " with this selection"
        } else {
            ""
        }
    )];
    match h["last_passed"].as_object() {
        Some(p) => parts.push(format!(
            "last green: run {} at {}",
            p["run"].as_str().unwrap_or(""),
            p["code"].as_str().unwrap_or("")
        )),
        None => parts.push("none of them passed".into()),
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
        WaitReason::Running => format!("prova wait {} --after {}", s.run, s.last_seq),
        WaitReason::Failure => format!(
            "fix and rerun, or `prova cancel {}`; `prova wait {} --after {}` to keep watching",
            s.run, s.run, s.last_seq
        ),
        WaitReason::Stall | WaitReason::Slow => format!(
            "prova cancel {}  (or `prova wait {} --after {}` if it is expected)",
            s.run, s.run, s.last_seq
        ),
        WaitReason::Done if s.outcome == Some(Outcome::Passed) => "done".into(),
        WaitReason::Done => format!("prova output {} --tail 200", s.run),
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
        WaitReason::Slow => {
            for t in &s.slow {
                if reported.insert(format!("slow:{}", t.test)) {
                    eprintln!("SLOW {} (over {}ms budget)", clean(&t.test), t.budget_ms);
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
        println!(
            "no runs yet in this repository; start one with `prova run -- <your test command>` (`prova status --all` lists every repository on this machine)"
        );
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
            let p = &res["primary"];
            if p.is_object() {
                println!(
                    "{}: {} median {:.3} (MAD {:.3}), {} median {:.3} (MAD {:.3}), p={:.4} ({} test; adjusted {:.4} for {} comparison(s) in this experiment, Holm)",
                    p["metric"].as_str().unwrap_or(""),
                    c.baseline,
                    p["baseline_median"].as_f64().unwrap_or(f64::NAN),
                    p["baseline_mad"].as_f64().unwrap_or(f64::NAN),
                    c.candidate,
                    p["candidate_median"].as_f64().unwrap_or(f64::NAN),
                    p["candidate_mad"].as_f64().unwrap_or(f64::NAN),
                    p["p"].as_f64().unwrap_or(f64::NAN),
                    p["method"].as_str().unwrap_or(""),
                    p["p_adjusted"].as_f64().unwrap_or(f64::NAN),
                    res["comparisons_in_family"].as_u64().unwrap_or(1),
                );
                let outl = p["outliers_baseline"].as_array().map_or(0, Vec::len)
                    + p["outliers_candidate"].as_array().map_or(0, Vec::len);
                if outl > 0 {
                    println!(
                        "outliers flagged (kept): baseline {}, candidate {}",
                        p["outliers_baseline"].as_array().map_or(0, Vec::len),
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
    if !r.decisions.is_empty() {
        println!("decisions:");
        for d in &r.decisions {
            println!(
                "  {:7} {}  {}",
                d["decision"].as_str().unwrap_or(""),
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
        super::context(&Failure {
            test: "t".into(),
            outcome: prova_core::model::Outcome::Failed,
            message: None,
            context: Some(FailureContext {
                first_seen: false,
                seen_before,
                failed_before: seen_before,
                passed_before: true,
                flaky,
                last_passed_code: None,
                suite_passed_on_same_code: false,
            }),
        })
    }

    /// Usability round 3: "[new failure, FLAKY]" contradicted itself.
    #[test]
    fn a_flaky_test_is_not_a_new_failure() {
        assert_eq!(label(0, true), " [FLAKY]");
        assert_eq!(label(0, false), " [new failure]");
        assert_eq!(label(2, true), " [seen 2x before, FLAKY]");
        let mut first = super::context(&Failure {
            test: "t".into(),
            outcome: prova_core::model::Outcome::Failed,
            message: None,
            context: Some(FailureContext {
                failed_before: 0,
                passed_before: false,
                first_seen: true,
                seen_before: 0,
                flaky: false,
                last_passed_code: None,
                suite_passed_on_same_code: false,
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
                flaky: false,
                last_passed_code: None,
                suite_passed_on_same_code: false,
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
    match &p.oracle {
        Some(o) => s.push_str(&format!("; oracle `{o}`")),
        None if !p.specs.is_empty() => s.push_str("; no oracle"),
        None => {}
    }
    s
}
