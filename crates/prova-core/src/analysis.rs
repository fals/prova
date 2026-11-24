//! History and flake statistics over stored attempts (ADR 0006).

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::params;
use serde::Serialize;

use crate::model::Outcome;
use crate::store::{Result, Store};

/// Final outcome of one test in one run, plus whether it needed retries.
#[derive(Debug, Clone, Serialize)]
pub struct TestRun {
    pub run: String,
    pub code: String,
    pub created_ms: u64,
    pub outcome: Outcome,
    pub attempts: u32,
    /// Failed at least once, then passed, in the same run.
    pub passed_on_retry: bool,
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// The run's environment fingerprint (name -> hash of value).
    #[serde(skip)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestStats {
    pub test_id: String,
    pub display: String,
    pub suite: String,
    /// Every suite that ran this test (its history is shared, ADR 0018).
    pub suites: Vec<String>,
    pub runs: usize,
    pub failures: usize,
    pub failure_rate: f64,
    /// Outcome changes between consecutive runs, oldest to newest.
    pub transitions: usize,
    pub passed_on_retry: usize,
    /// Code versions on which the test both passed and failed.
    pub flips_on_same_code: usize,
    pub flaky: bool,
    /// Failed its last 3 runs in a row: failing now, whatever its past.
    #[serde(default)]
    pub failing_now: bool,
    /// Passed and failed on the same code only with different environment
    /// variables: these names differ.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env_dependent: Vec<String>,
    /// Its size from what it used (ADR 0025), never from its duration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<SizeView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p95_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_passed: Option<(String, String)>,
    /// Newest first.
    pub recent: Vec<TestRun>,
}

/// Nearest-rank percentile of a sorted slice.
pub fn percentile(sorted: &[u64], p: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    Some(sorted[rank.min(sorted.len()) - 1])
}

/// Per-run results of a test, newest first, at most `limit` runs.
/// One test's results, newest first, over every suite that ran it (ADR 0018).
/// Results are the same test when their full names match and their files
/// match or one is unknown (quiet `cargo test` names no binary), or when
/// one side knows only the bare name (`go test` text output names no
/// package). From a cancelled or aborted run, every completed result counts
/// but the tests it cut off (errored, "did not finish"): a test that fails
/// while the run is being cancelled is recorded as unfinished (ADR 0020), so
/// a failure there finished on its own (round 8); `current` is included.
pub fn test_runs(store: &Store, test_id: &str, limit: usize) -> Result<Vec<TestRun>> {
    test_runs_with(store, test_id, limit, None)
}

pub fn test_runs_with(
    store: &Store,
    test_id: &str,
    limit: usize,
    current: Option<&str>,
) -> Result<Vec<TestRun>> {
    let mut stmt = store.conn().prepare(
        "WITH same AS (
             SELECT t2.id FROM tests t1 JOIN tests t2
               ON (t2.display = t1.display
                   AND (COALESCE(t2.file, '') = COALESCE(t1.file, '') OR t1.file IS NULL OR t2.file IS NULL))
                OR (t2.name = t1.name
                   AND ((t1.file IS NULL AND t1.path = '[]') OR (t2.file IS NULL AND t2.path = '[]')))
             WHERE t1.id = ?1),
         counted AS (
             SELECT r.id, r.created_ms FROM runs r
             WHERE r.id IN (
                 SELECT a3.run_id FROM attempts a3 JOIN runs r3 ON r3.id = a3.run_id
                 WHERE a3.test_id IN (SELECT id FROM same)
                   AND (r3.status = 'finished' OR r3.id = ?3 OR a3.outcome <> 'errored')
                   -- an unverified run (stale, echoed or impossible results)
                   -- is no evidence about any test (round 10: it made a real
                   -- bug look flaky)
                   AND (r3.id = ?3 OR NOT EXISTS (SELECT 1 FROM events w WHERE w.run_id = r3.id AND w.kind = 'run_warning'
                                                  AND w.payload LIKE '%; no evidence about the tests%'))
                   AND r3.id NOT IN (SELECT run_id FROM forgotten)
                   -- labels of a run are as of that run: later runs do not
                   -- rewrite them (round 13: 'failed 8x before' on a run
                   -- that followed 3 failures)
                   AND (?3 IS NULL OR r3.created_ms <= (SELECT created_ms FROM runs WHERE id = ?3))))
         SELECT a.run_id, r.code, r.created_ms, a.attempt, a.outcome, a.duration_ms, a.message, a.fingerprint, r.env,
                json_extract(r.spec, '$.selection')
         FROM attempts a JOIN runs r ON r.id = a.run_id
         WHERE a.test_id IN (SELECT id FROM same)
           AND NOT EXISTS (SELECT 1 FROM superseded s WHERE s.run_id = a.run_id AND s.test_id = a.test_id)
           AND a.run_id IN (SELECT id FROM counted ORDER BY created_ms DESC LIMIT ?2)
           AND (r.status = 'finished' OR r.id = ?3 OR a.outcome <> 'errored')
         ORDER BY r.created_ms DESC, a.run_id, a.attempt ASC",
    )?;
    let rows = stmt.query_map(params![test_id, limit as i64, current], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, Option<String>>(9)?,
        ))
    })?;
    // A parent run only because its subtest was selected (Go runs the
    // parent of `-run TestX/case`) says nothing about the parent: its pass
    // there made a parent that fails in full runs FLAKY (round 13, Go).
    let leaf = |n: &str| n.rsplit("::").next().unwrap_or(n).to_owned();
    let this: Option<String> = store
        .conn()
        .query_row(
            "SELECT display FROM tests WHERE id = ?1",
            params![test_id],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .map(|d| leaf(&d));
    let mut out: Vec<TestRun> = Vec::new();
    for row in rows {
        let (run, code, created, attempt, outcome, dur, msg, fp, env, selection) = row?;
        if let (Some(sel), Some(this)) = (&selection, &this)
            && leaf(sel).starts_with(&format!("{this}/"))
        {
            continue;
        }
        let env: BTreeMap<String, String> = serde_json::from_str::<serde_json::Value>(&env)
            .ok()
            .and_then(|v| {
                v["__fingerprint"].as_object().map(|m| {
                    m.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_owned()))
                        .collect()
                })
            })
            .unwrap_or_default();
        let outcome = Outcome::parse(&outcome).unwrap_or(Outcome::Errored);
        match out.last_mut() {
            Some(last) if last.run == run => {
                let earlier_failed = last.outcome.is_failure();
                last.attempts = attempt as u32;
                last.passed_on_retry |= earlier_failed && outcome == Outcome::Passed;
                last.outcome = outcome;
                last.duration_ms = dur.map(|d| d as u64);
                if outcome.is_failure() {
                    last.message = msg;
                    last.fingerprint = fp;
                }
            }
            _ => out.push(TestRun {
                run,
                code,
                created_ms: created as u64,
                outcome,
                attempts: attempt as u32,
                passed_on_retry: false,
                duration_ms: dur.map(|d| d as u64),
                message: if outcome.is_failure() { msg } else { None },
                fingerprint: fp,
                env,
            }),
        }
    }
    Ok(out)
}

pub fn stats(store: &Store, test_id: &str, window: usize) -> Result<Option<TestStats>> {
    stats_with(store, test_id, window, None)
}

fn stats_with(
    store: &Store,
    test_id: &str,
    window: usize,
    current: Option<&str>,
) -> Result<Option<TestStats>> {
    let meta: Option<(String, String)> = store
        .conn()
        .query_row(
            "SELECT display, suite FROM tests WHERE id = ?1",
            params![test_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((display, suite)) = meta else {
        return Ok(None);
    };
    let recent = test_runs_with(store, test_id, window, current)?;
    let suites: Vec<String> = {
        let mut stmt = store.conn().prepare(
            "SELECT DISTINCT t2.suite FROM tests t1 JOIN tests t2
               ON (t2.display = t1.display
                   AND (COALESCE(t2.file, '') = COALESCE(t1.file, '') OR t1.file IS NULL OR t2.file IS NULL))
                OR (t2.name = t1.name
                   AND ((t1.file IS NULL AND t1.path = '[]') OR (t2.file IS NULL AND t2.path = '[]')))
             WHERE t1.id = ?1 ORDER BY t2.suite",
        )?;
        stmt.query_map(params![test_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let counted: Vec<&TestRun> = recent
        .iter()
        .filter(|r| r.outcome != Outcome::Skipped)
        .collect();
    let failures = counted.iter().filter(|r| r.outcome.is_failure()).count();
    let transitions = counted
        .windows(2)
        .filter(|w| w[0].outcome.is_failure() != w[1].outcome.is_failure())
        .count();
    // A flip is flaky only on the same code in the same environment: a test
    // that fails only when a variable is set is environment-dependent, not
    // flaky (round 11). Runs recorded before the fingerprint share "".
    let env_key = |r: &TestRun| {
        r.env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let mut by_code: BTreeMap<(&str, String), BTreeSet<bool>> = BTreeMap::new();
    // Outside git the code version is unknown: two runs there are never
    // "the same code" (round 12: a fix in a project without git was
    // called a flaky flip).
    for r in counted.iter().filter(|r| r.code != "nogit") {
        by_code
            .entry((r.code.as_str(), env_key(r)))
            .or_default()
            .insert(r.outcome.is_failure());
    }
    let flips = by_code.values().filter(|s| s.len() == 2).count();
    // Same code, opposite results, different environments: name the
    // variables that differ between a pass and a failure.
    let mut env_dependent: BTreeSet<String> = BTreeSet::new();
    if flips == 0 {
        for p in counted.iter().filter(|r| r.outcome == Outcome::Passed) {
            for f in counted
                .iter()
                .filter(|r| r.outcome.is_failure() && r.code == p.code && r.code != "nogit")
            {
                for k in p.env.keys().chain(f.env.keys()) {
                    if p.env.get(k) != f.env.get(k) {
                        env_dependent.insert(k.clone());
                    }
                }
            }
        }
    }
    let retry = recent.iter().filter(|r| r.passed_on_retry).count();
    // A test that failed its last 3 runs in a row is failing now, whatever
    // its past: "known flaky" must not hide a real break (round 9: an
    // environment-dependent test, once flipped, was dismissed for good).
    //
    // How long a streak must be depends on how often the test fails: a coin
    // flip fails 3 times in a row 1 run in 8 (round 10). The streak must be
    // one the test's own failure rate makes unlikely by chance (under 1%).
    let streak = counted
        .iter()
        .take_while(|r| r.outcome.is_failure() && !r.passed_on_retry)
        .count();
    // Its failure rate before the streak: the streak must not explain itself.
    let before = &counted[streak.min(counted.len())..];
    let rate = if before.is_empty() {
        0.0
    } else {
        before.iter().filter(|r| r.outcome.is_failure()).count() as f64 / before.len() as f64
    };
    let needed = if rate <= 0.0 || rate >= 1.0 {
        3
    } else {
        ((0.01f64).ln() / rate.ln()).ceil().max(3.0) as usize
    };
    // Only a test history calls flaky can be "failing now" instead; one that
    // never passed has simply not passed yet (round 11).
    let failing_now = streak >= needed && (retry > 0 || flips > 0);
    let mut durations: Vec<u64> = recent
        .iter()
        .filter(|r| r.outcome == Outcome::Passed)
        .filter_map(|r| r.duration_ms)
        .collect();
    durations.sort_unstable();
    let last_passed = recent
        .iter()
        .find(|r| r.outcome == Outcome::Passed)
        .map(|r| (r.run.clone(), r.code.clone()));
    Ok(Some(TestStats {
        test_id: test_id.to_owned(),
        display,
        suite,
        suites,
        runs: counted.len(),
        failures,
        failure_rate: if counted.is_empty() {
            0.0
        } else {
            failures as f64 / counted.len() as f64
        },
        transitions,
        passed_on_retry: retry,
        flips_on_same_code: flips,
        flaky: (retry > 0 || flips > 0) && !failing_now,
        env_dependent: env_dependent.into_iter().take(5).collect(),
        failing_now,
        size: observed_size(store, test_id)?,
        p50_ms: percentile(&durations, 50.0),
        p95_ms: percentile(&durations, 95.0),
        last_passed,
        recent,
    }))
}

/// Tests whose display name contains `query`, optionally in one suite.
pub fn find_tests(
    store: &Store,
    query: &str,
    suite: Option<&str>,
) -> Result<Vec<(String, String, String)>> {
    let mut stmt = store.conn().prepare(
        "SELECT id, display, suite FROM tests
         WHERE instr(display, ?1) > 0 AND (?2 IS NULL OR suite = ?2)
           AND id NOT IN (SELECT test_id FROM superseded)
         ORDER BY (display = ?1) DESC, length(display) ASC LIMIT 50",
    )?;
    let rows = stmt.query_map(params![query, suite], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Ids of every test that shares one history with `test_id` (ADR 0018).
pub fn shared_ids(store: &Store, test_id: &str) -> Result<Vec<String>> {
    let mut stmt = store.conn().prepare(
        "SELECT t2.id FROM tests t1 JOIN tests t2
           ON (t2.display = t1.display
               AND (COALESCE(t2.file, '') = COALESCE(t1.file, '') OR t1.file IS NULL OR t2.file IS NULL))
            OR (t2.name = t1.name
               AND ((t1.file IS NULL AND t1.path = '[]') OR (t2.file IS NULL AND t2.path = '[]')))
         WHERE t1.id = ?1",
    )?;
    let ids = stmt
        .query_map(params![test_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Of several test ids, the one with the most recent attempt.
pub fn most_recent(store: &Store, candidates: &[(String, String, String)]) -> Result<String> {
    let mut best = (
        0i64,
        candidates.first().map(|c| c.0.clone()).unwrap_or_default(),
    );
    for (id, _, _) in candidates {
        let ts: Option<i64> = store.conn().query_row(
            "SELECT MAX(ts_ms) FROM attempts WHERE test_id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        if ts.unwrap_or(0) > best.0 {
            best = (ts.unwrap_or(0), id.clone());
        }
    }
    Ok(best.1)
}

/// Flaky tests among those seen in the last `runs` runs of any suite.
pub fn flaky(store: &Store, suite: Option<&str>, window: usize) -> Result<Vec<TestStats>> {
    flaky_as_of(store, suite, window, None)
}

/// Tests flaky as of run `as_of` (its history up to it, its own result
/// included): a run's labels do not change when later runs come in.
pub fn flaky_as_of(
    store: &Store,
    suite: Option<&str>,
    window: usize,
    as_of: Option<&str>,
) -> Result<Vec<TestStats>> {
    let mut stmt = store.conn().prepare(
        // Any test that failed somewhere: its history is shared across
        // suites (ADR 0018), so a flip between suites counts (round 6).
        "SELECT DISTINCT t.id FROM tests t JOIN attempts a ON a.test_id = t.id
         WHERE (?1 IS NULL OR t.suite = ?1) AND a.outcome IN ('failed','errored')",
    )?;
    let ids: Vec<String> = stmt
        .query_map(params![suite], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out: Vec<TestStats> = Vec::new();
    for id in ids {
        if let Some(s) = stats_with(store, &id, window, as_of)?
            && s.flaky
            && !out.iter().any(|o| {
                o.display == s.display
                    && o.recent.first().map(|r| &r.run) == s.recent.first().map(|r| &r.run)
            })
        {
            out.push(s);
        }
    }
    out.sort_by(|a, b| b.failure_rate.total_cmp(&a.failure_rate));
    Ok(out)
}

/// A test's size from what it used (ADR 0025).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SizeView {
    /// Decided: medium or large as soon as seen, small after
    /// `SMALL_AFTER` attributed runs that saw nothing more.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<crate::config::Size>,
    /// When nothing could be attributed: the largest size of the runs it
    /// was in, an upper bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at_most: Option<crate::config::Size>,
    /// What the agent reads: the size and why.
    pub label: String,
}

/// Attributed runs that must all show one process and no socket before a
/// test is called small (ADR 0025 §5): sampling can miss what is brief.
pub const SMALL_AFTER: usize = 3;

pub fn observed_size(store: &Store, test_id: &str) -> Result<Option<SizeView>> {
    use crate::config::Size;
    // Only finished runs are evidence (ADR 0026 §6): not cancelled or
    // aborted ones, not forgotten ones.
    let mut stmt = store.conn().prepare(
        "SELECT s.size, s.evidence FROM test_sizes s JOIN runs r ON r.id = s.run_id
          WHERE s.test_id = ?1 AND r.status = 'finished'
            AND s.run_id NOT IN (SELECT run_id FROM forgotten)",
    )?;
    let attributed: Vec<(Size, String)> = stmt
        .query_map(params![test_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get(1)?))
        })?
        .filter_map(|r| r.ok())
        .filter_map(|(s, e)| Some((Size::parse(&s)?, e)))
        .collect();
    // Runs that ran the test but could not isolate it: their test phase
    // decides (ADR 0026 §2).
    let mut stmt = store.conn().prepare(
        "SELECT json_extract(e.payload, '$.size'), json_extract(e.payload, '$.evidence')
           FROM events e JOIN runs r ON r.id = e.run_id
          WHERE e.kind = 'run_size' AND r.status = 'finished'
            AND e.run_id IN (SELECT run_id FROM attempts WHERE test_id = ?1 AND outcome <> 'skipped')
            AND e.run_id NOT IN (SELECT run_id FROM test_sizes WHERE test_id = ?1)
            AND e.run_id NOT IN (SELECT run_id FROM forgotten)",
    )?;
    let by_run: Vec<(Size, String)> = stmt
        .query_map(params![test_id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .filter_map(|(s, e)| Some((Size::parse(&s?)?, e.unwrap_or_default())))
        .collect();
    if let Some((size, evidence)) = attributed
        .iter()
        .filter(|(s, _)| *s > Size::Small)
        .max_by_key(|(s, _)| *s)
    {
        return Ok(Some(SizeView {
            size: Some(*size),
            at_most: None,
            label: format!("{} ({evidence})", size.as_str()),
        }));
    }
    let small = attributed.len() + by_run.iter().filter(|(s, _)| *s == Size::Small).count();
    let bound = by_run
        .iter()
        .filter(|(s, _)| *s > Size::Small)
        .max_by_key(|(s, _)| *s);
    if small >= SMALL_AFTER {
        return Ok(Some(SizeView {
            size: Some(Size::Small),
            at_most: None,
            label: format!("small (no other process or socket seen, in {small} runs)"),
        }));
    }
    if let Some((b, evidence)) = bound {
        return Ok(Some(SizeView {
            size: None,
            at_most: Some(*b),
            label: format!(
                "{} or less (never seen on its own, and its run {evidence})",
                b.as_str()
            ),
        }));
    }
    if small > 0 {
        return Ok(Some(SizeView {
            size: None,
            at_most: None,
            label: format!("small? (nothing more seen in {small} of {SMALL_AFTER} runs)"),
        }));
    }
    Ok(None)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SizeReport {
    pub total: usize,
    pub small: usize,
    pub medium: usize,
    pub large: usize,
    /// Not yet decided: not attributed, or small in fewer runs than needed.
    pub unknown: usize,
    /// Of the unknown, those seen small in fewer runs than needed.
    pub small_so_far: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// Medium and large tests with what made them so: (test, label).
    pub larger: Vec<(String, String)>,
    /// Every test with its size label, or "unknown" (round 12: counts
    /// without names could not be checked).
    pub tests: Vec<(String, String)>,
}

pub fn sizes(store: &Store, suite: Option<&str>) -> Result<SizeReport> {
    use crate::config::Size;
    let mut stmt = store.conn().prepare(
        "SELECT id, display FROM tests WHERE (?1 IS NULL OR suite = ?1) ORDER BY display",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map(params![suite], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // One test, one count: the same test in several suites shares its
    // history (round 12: a 5-test project showed 12 tests). The most
    // decided view wins.
    let mut views: BTreeMap<String, Option<SizeView>> = BTreeMap::new();
    for (id, display) in rows {
        let view = observed_size(store, &id)?;
        let e = views.entry(display).or_insert(None);
        let better = match (&*e, &view) {
            (None, Some(_)) => true,
            (Some(old), Some(new)) => new.size > old.size,
            _ => false,
        };
        if better {
            *e = view;
        }
    }
    let mut r = SizeReport::default();
    for (display, view) in views {
        match view.as_ref().and_then(|v| v.size) {
            Some(Size::Small) => r.small += 1,
            Some(Size::Medium) => r.medium += 1,
            Some(Size::Large) => r.large += 1,
            None => {
                r.unknown += 1;
                if view.as_ref().is_some_and(|v| v.label.starts_with("small?")) {
                    r.small_so_far += 1;
                }
            }
        }
        if let Some(v) = &view
            && matches!(v.size, Some(Size::Medium | Size::Large))
        {
            r.larger.push((display.clone(), v.label.clone()));
        }
        r.tests.push((
            display,
            view.map_or_else(|| "unknown".to_owned(), |v| v.label),
        ));
    }
    r.total = r.small + r.medium + r.large + r.unknown;
    r.larger
        .sort_by_key(|(_, l)| std::cmp::Reverse(l.starts_with("large")));
    // Small takes several runs to establish, medium one: count tests seen
    // small so far, or the share is too low early on.
    let small = r.small + r.small_so_far;
    let seen = small + r.medium + r.large;
    if seen >= 10 && (small as f64) < 0.6 * seen as f64 {
        r.warning = Some(format!(
            "only {:.0}% of the tests with an observed size are small; aim for most tests to be small (rough guide 80/15/5)",
            100.0 * small as f64 / seen as f64
        ));
    }
    Ok(r)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RunDiff {
    pub base: String,
    pub head: String,
    pub newly_failing: Vec<String>,
    pub fixed: Vec<String>,
    pub still_failing: Vec<String>,
    /// Known flaky tests whose result changed: neither fixed nor broken by
    /// the change (round 7: a flaky pass was listed as "fixed").
    pub flaky: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// (test, base ms, head ms) where head is >50% and >100ms slower.
    pub slower: Vec<(String, u64, u64)>,
}

fn final_outcomes(store: &Store, run: &str) -> Result<BTreeMap<String, (Outcome, Option<u64>)>> {
    let mut stmt = store.conn().prepare(
        "SELECT t.display, a.outcome, a.duration_ms FROM attempts a JOIN tests t ON t.id = a.test_id
         WHERE a.run_id = ?1 ORDER BY a.attempt",
    )?;
    let rows = stmt.query_map(params![run], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
        ))
    })?;
    let mut m = BTreeMap::new();
    for row in rows {
        let (name, o, d) = row?;
        m.insert(
            name,
            (
                Outcome::parse(&o).unwrap_or(Outcome::Errored),
                d.map(|v| v as u64),
            ),
        );
    }
    Ok(m)
}

pub fn diff(store: &Store, base: &str, head: &str) -> Result<RunDiff> {
    let a = final_outcomes(store, base)?;
    let b = final_outcomes(store, head)?;
    let mut d = RunDiff {
        base: base.into(),
        head: head.into(),
        ..RunDiff::default()
    };
    for (name, (ob, db)) in &b {
        match a.get(name) {
            None => d.added.push(name.clone()),
            Some((oa, da)) => {
                match (oa.is_failure(), ob.is_failure()) {
                    (false, true) => d.newly_failing.push(name.clone()),
                    (true, false) if *ob == Outcome::Passed => d.fixed.push(name.clone()),
                    (true, true) => d.still_failing.push(name.clone()),
                    _ => {}
                }
                if let (Some(x), Some(y)) = (da, db)
                    && *y > x + 100
                    && *y * 2 > x * 3
                {
                    d.slower.push((name.clone(), *x, *y));
                }
            }
        }
    }
    d.removed = a.keys().filter(|k| !b.contains_key(*k)).cloned().collect();
    if !d.fixed.is_empty() || !d.newly_failing.is_empty() {
        let known: Vec<String> = flaky(store, None, 50)?
            .into_iter()
            .map(|s| s.display)
            .collect();
        for list in [&mut d.fixed, &mut d.newly_failing] {
            list.retain(|t| {
                let f = known.contains(t);
                if f {
                    d.flaky.push(t.clone());
                }
                !f
            });
        }
    }
    Ok(d)
}

/// What history says about one failing test in a run.
#[derive(Debug, Clone, Serialize)]
pub struct FailureContext {
    /// No earlier result of this test anywhere (ADR 0018): it cannot be
    /// called new or old yet.
    pub first_seen: bool,
    /// Earlier runs where this test failed with the same fingerprint.
    pub seen_before: usize,
    /// Earlier runs where it failed, whatever the message.
    pub failed_before: usize,
    /// It passed in an earlier finished run.
    pub passed_before: bool,
    /// How many earlier results of this test passed: a "new failure"
    /// after one pass is thin evidence (round 12).
    #[serde(default)]
    pub passes_before: usize,
    /// Failed its last 3 runs in a row (this one included): no flaky hint.
    #[serde(default)]
    pub failing_now: bool,
    /// Passed with a different environment on this code: these variables.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env_dependent: Vec<String>,
    /// This run was cancelled or aborted: its failures are no evidence
    /// about the code (a killed test reports FAIL), so no label applies.
    pub run_cut_short: bool,
    pub flaky: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_passed_code: Option<String>,
    /// The whole suite passed before on exactly this code: the failure is
    /// likely flaky even when per-test history is incomplete.
    pub suite_passed_on_same_code: bool,
}

pub fn failure_context(store: &Store, test_id: &str, run: &str) -> Result<Option<FailureContext>> {
    let Some(s) = stats_with(store, test_id, 50, Some(run))? else {
        return Ok(None);
    };
    // Flakiness counts this run's own result too, so the label shown live
    // and the one in the final summary agree (round 7: "new failure" live,
    // FLAKY at the end). A run cut short is answered before this is used.
    let flaky = s.flaky;
    let this = s.recent.iter().find(|r| r.run == run);
    let first_seen = !s.recent.iter().any(|r| r.run != run);
    // Only a test the cut cut off is no evidence; one that failed before
    // the cancel failed on its own (round 8).
    let run_cut_short = matches!(
        store.run(run)?.status,
        crate::model::Status::Cancelled | crate::model::Status::Aborted
    ) && this.is_some_and(|r| r.outcome == crate::model::Outcome::Errored);
    let fp = this.and_then(|r| r.fingerprint.clone());
    let seen_before = s
        .recent
        .iter()
        .filter(|r| r.run != run && r.outcome.is_failure() && fp.is_some() && r.fingerprint == fp)
        .count();
    let last_passed_code = s
        .recent
        .iter()
        .find(|r| r.run != run && r.outcome == Outcome::Passed)
        .map(|r| r.code.clone());
    let failed_before = s
        .recent
        .iter()
        .filter(|r| r.run != run && r.outcome.is_failure())
        .count();
    let passed_before = last_passed_code.is_some();
    let passes_before = s
        .recent
        .iter()
        .filter(|r| r.run != run && r.outcome == Outcome::Passed)
        .count();
    Ok(Some(FailureContext {
        first_seen,
        seen_before,
        failed_before,
        passed_before,
        passes_before,
        run_cut_short,
        failing_now: s.failing_now,
        env_dependent: s.env_dependent.clone(),
        flaky,
        last_passed_code,
        suite_passed_on_same_code: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let v = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        assert_eq!(percentile(&v, 50.0), Some(50));
        assert_eq!(percentile(&v, 95.0), Some(100));
        assert_eq!(percentile(&[7], 95.0), Some(7));
        assert_eq!(percentile(&[], 50.0), None);
    }
}

/// One test's typical duration over its recent results (ADR 0027 §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlowRow {
    pub test: String,
    pub suites: Vec<String>,
    /// Results with a duration the numbers come from.
    pub runs: usize,
    pub p50_ms: u64,
    pub p95_ms: u64,
}

/// The `n` slowest tests, worst first by p50 then p95, over each test's
/// last `window` finished results with a duration. Like history, one test
/// seen in several suites is one row. Forgotten runs and runs that are no
/// evidence about the tests do not count (as in `test_runs`).
pub fn slowest(
    store: &Store,
    suite: Option<&str>,
    n: usize,
    window: usize,
) -> Result<Vec<SlowRow>> {
    let mut stmt = store.conn().prepare(
        "SELECT t.display, t.suite, a.duration_ms FROM (
             SELECT a.test_id, a.duration_ms,
                    row_number() OVER (PARTITION BY a.test_id ORDER BY a.ts_ms DESC) AS rn
               FROM attempts a JOIN runs r ON r.id = a.run_id
              WHERE a.duration_ms IS NOT NULL AND a.outcome IN ('passed', 'failed')
                AND r.status = 'finished'
                AND r.id NOT IN (SELECT run_id FROM forgotten)
                AND NOT EXISTS (SELECT 1 FROM events w WHERE w.run_id = r.id AND w.kind = 'run_warning'
                                  AND w.payload LIKE '%; no evidence about the tests%')
         ) a JOIN tests t ON t.id = a.test_id
         WHERE a.rn <= ?1 AND (?2 IS NULL OR t.suite = ?2)",
    )?;
    let rows = stmt.query_map(params![window as i64, suite], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)? as u64,
        ))
    })?;
    let mut by_test: BTreeMap<String, (BTreeSet<String>, Vec<u64>)> = BTreeMap::new();
    for row in rows {
        let (display, suite, ms) = row?;
        let e = by_test.entry(display).or_default();
        e.0.insert(suite);
        e.1.push(ms);
    }
    let mut out: Vec<SlowRow> = by_test
        .into_iter()
        .filter_map(|(test, (suites, mut ms))| {
            ms.sort_unstable();
            Some(SlowRow {
                test,
                suites: suites.into_iter().collect(),
                runs: ms.len(),
                p50_ms: percentile(&ms, 50.0)?,
                p95_ms: percentile(&ms, 95.0)?,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.p50_ms
            .cmp(&a.p50_ms)
            .then(b.p95_ms.cmp(&a.p95_ms))
            .then(a.test.cmp(&b.test))
    });
    out.truncate(n);
    Ok(out)
}
