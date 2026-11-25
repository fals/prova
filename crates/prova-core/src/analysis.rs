//! History and flake statistics over stored attempts (ADR 0006).

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{OptionalExtension, params};
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
    /// Passed in one execution and failed in a later one of the same run
    /// (`go test -count=N`: a failure in any fails the test, 0029): flaky
    /// evidence from a run that failed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub passed_and_failed: bool,
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
    /// The test binary, when tests of other binaries share the name
    /// (ADR 0031 section 2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    pub suite: String,
    /// Every suite that ran this test (its history is shared, ADR 0018).
    pub suites: Vec<String>,
    /// Other names of the same test (a Go test recorded bare and
    /// package-qualified): a quarantine of either covers it.
    #[serde(skip)]
    pub aliases: Vec<String>,
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
    /// Runs cut short (cancelled, killed, stalled out) while this test was
    /// running: it is no result, but it may be why (round 17: a hung test
    /// showed "0 runs"). Newest first, at most 10.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cut_short: Vec<CutShortRun>,
}

/// A run that ended before a test did.
#[derive(Debug, Clone, Serialize)]
pub struct CutShortRun {
    pub run: String,
    pub code: String,
    pub status: String,
}

/// The runs cut short in which the test (by name) was running.
fn cut_short_runs(store: &Store, test_id: &str) -> Result<Vec<CutShortRun>> {
    let mut stmt = store.conn().prepare(
        "SELECT DISTINCT r.id, r.code, r.status, r.created_ms
           FROM attempts a JOIN runs r ON r.id = a.run_id
          WHERE a.test_id IN (SELECT t2.id FROM tests t1 JOIN tests t2 ON t2.display = t1.display
                               WHERE t1.id = ?1)
            AND a.outcome = 'errored' AND r.status <> 'finished'
            AND a.message LIKE 'test did not finish before the run ended%'
            AND r.id NOT IN (SELECT run_id FROM forgotten)
          ORDER BY r.created_ms DESC LIMIT 10",
    )?;
    let rows = stmt
        .query_map(params![test_id], |r| {
            Ok(CutShortRun {
                run: r.get(0)?,
                code: r.get(1)?,
                status: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// What makes two stored tests one test for history (ADR 0018), as the
/// `ON` of a self join of `tests` (`t1` the test asked about, `t2` another).
/// A name recorded bare (Go text with no package, quiet `cargo test` with no
/// crate) is the qualified one only when the name belongs to one place: a
/// name shared by several places is never matched to one of them (ADR 0035
/// section 1).
const SAME_TEST_ON: &str = "(t2.display = t1.display
                   AND (COALESCE(t2.file, '') = COALESCE(t1.file, '')
                        OR ((t1.file IS NULL OR t2.file IS NULL)
                            AND (SELECT COUNT(DISTINCT x.file) FROM tests x
                                 WHERE x.display = t1.display AND x.file IS NOT NULL
                                   AND x.file NOT LIKE '(package pending)%'
                                   AND x.file <> '(unknown directory)') < 2)))
                OR (t2.name = t1.name
                   AND ((t1.file IS NULL AND t1.path = '[]') OR (t2.file IS NULL AND t2.path = '[]'))
                   AND (SELECT COUNT(DISTINCT COALESCE(x.file, '') || '|' || x.path) FROM tests x
                        WHERE x.name = t1.name AND NOT (x.file IS NULL AND x.path = '[]')
                          AND COALESCE(x.file, '') NOT LIKE '(package pending)%'
                          AND COALESCE(x.file, '') <> '(unknown directory)') < 2)";

fn same_sql(sql: &str) -> String {
    sql.replace("{SAME}", SAME_TEST_ON)
}

/// A name recorded bare (`TestX`, from text output that named no package)
/// and its qualified form (`pkg::TestX`, `example.com/pkg::TestX`) are one
/// test: the same name, or one is the other behind a `::` or `/`.
pub fn same_test_name(a: &str, b: &str) -> bool {
    let tail = |long: &str, short: &str| {
        long.strip_suffix(short)
            .is_some_and(|head| head.ends_with("::") || head.ends_with('/'))
    };
    a == b || tail(a, b) || tail(b, a)
}

/// Names tests with their place where several places hold one name
/// (ADR 0031 section 2): `tests::flaky` alone when only one test binary
/// has it, `tests::flaky (geo)` when two do. A result of unknown place (quiet
/// `cargo test` names no binary) keeps the bare name.
pub struct Names<'a> {
    store: &'a Store,
    shared: std::cell::RefCell<BTreeMap<String, bool>>,
}

impl<'a> Names<'a> {
    pub fn new(store: &'a Store) -> Names<'a> {
        Names {
            store,
            shared: Default::default(),
        }
    }

    /// The place to show for a test of this name and file: `None` when the
    /// name is not shared.
    pub fn place(&self, display: &str, file: Option<&str>) -> Option<String> {
        let file = file.filter(|f| !f.is_empty() && !f.starts_with("(package pending)"))?;
        let shared = *self
            .shared
            .borrow_mut()
            .entry(display.to_owned())
            .or_insert_with(|| {
                crate::quarantine::files_of(self.store, display).is_ok_and(|f| f.len() > 1)
            });
        shared.then(|| file.to_owned())
    }

    /// The name to show and to group by.
    pub fn name(&self, display: &str, file: Option<&str>) -> String {
        match self.place(display, file) {
            Some(p) => format!("{display} ({p})"),
            None => display.to_owned(),
        }
    }
}

/// A place in the form that compares: no `./`, no trailing slash, no test
/// file suffix, `.` for `/` (`tests/test_calc.py` and `tests.test_calc` are
/// one place, so are `./a` and `ex/a`'s last part).
fn place_form(s: &str) -> String {
    let mut s = s.trim().replace('\\', "/");
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_owned();
    }
    let mut s = s.trim_end_matches('/').to_owned();
    for suffix in [
        ".test.js",
        ".test.ts",
        ".test.tsx",
        ".test.jsx",
        ".test.mjs",
        ".spec.js",
        ".spec.ts",
        ".spec.tsx",
        ".spec.jsx",
        ".py",
        ".js",
        ".ts",
        ".tsx",
        ".jsx",
        ".go",
        ".rs",
        ".java",
        ".cs",
    ] {
        if let Some(rest) = s.strip_suffix(suffix) {
            s = rest.to_owned();
            break;
        }
    }
    s.replace('/', ".")
}

/// Whether the place `wanted` (what `--in` says) names the place of a test:
/// its test binary (Rust), else its package, module or file (the first part
/// of its name). Whole parts: `a` is `ex/a` but not `ex/data`.
pub fn place_matches(wanted: &str, file: Option<&str>, path: &[String]) -> bool {
    place_rank(wanted, file, path) > 0
}

/// How well `wanted` names a test's place: 2 the same place, 1 the end of
/// it (`a` for `ex/a`), 0 not it. `tests/test_calc.py` is the same place as
/// `tests.test_calc`, and only the end of `sub2.tests.test_calc`.
pub fn place_rank(wanted: &str, file: Option<&str>, path: &[String]) -> u8 {
    let Some(place) = place_label(file, path) else {
        return 0;
    };
    let (w, p) = (place_form(wanted), place_form(&place));
    if w.is_empty() {
        0
    } else if w == p {
        2
    } else {
        // A crate names its integration tests (`calc` for `calc::it`).
        u8::from(
            p.ends_with(&format!(".{w}"))
                || w.ends_with(&format!(".{p}"))
                || p.starts_with(&format!("{w}::")),
        )
    }
}

/// The place `--in` names for a stored test: its binary (Rust), else the
/// package, module or file at the head of its name (Go, pytest, Vitest). A
/// Rust module path (`tests`) is no place: a test with no binary and a bare
/// first part has none.
pub fn place_label(file: Option<&str>, path: &[String]) -> Option<String> {
    file.filter(|f| !f.is_empty() && !f.starts_with("(package pending)"))
        .map(str::to_owned)
        .or_else(|| {
            path.first()
                .filter(|p| p.contains(['/', '.']))
                .map(|p| p.to_owned())
        })
}

/// Keeps what `--in wanted` names: the tests of the exact place when there
/// are any, else those of a place it ends. `place` gives a test's file and
/// name path.
pub fn retain_in_place<T>(
    items: &mut Vec<T>,
    wanted: &str,
    place: impl Fn(&T) -> (Option<String>, Vec<String>),
) {
    let ranks: Vec<u8> = items
        .iter()
        .map(|t| {
            let (file, path) = place(t);
            place_rank(wanted, file.as_deref(), &path)
        })
        .collect();
    let best = ranks.iter().copied().max().unwrap_or(0);
    let mut i = 0;
    items.retain(|_| {
        i += 1;
        best > 0 && ranks[i - 1] == best
    });
}

/// The place of a stored test, as `--in` names it: its binary, else the
/// first part of its name; `None` for a test that has neither.
pub fn place_of_test(
    store: &Store,
    test_id: &str,
) -> Result<Option<(Option<String>, Vec<String>)>> {
    let row: Option<(Option<String>, String)> = store
        .conn()
        .query_row(
            "SELECT file, path FROM tests WHERE id = ?1",
            params![test_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.map(|(file, path)| (file, serde_json::from_str(&path).unwrap_or_default())))
}

/// Nearest-rank percentile of a sorted slice.
pub fn percentile(sorted: &[u64], p: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    Some(sorted[rank.min(sorted.len()) - 1])
}

/// The p95 of a test's recent passing durations, for the slow budget of a
/// test that just started. `stats` answers the same and much more; that is
/// too much to read once per test while a run records (issue 91). Only
/// finished, remembered, verified runs count, as in `stats`. A test not yet
/// recorded under this id is known by its name from other suites.
pub fn usual_p95_ms(store: &Store, test_id: &str, display: &str) -> Result<Option<u64>> {
    let read = |ids_sql: &str, key: &str| -> Result<Vec<u64>> {
        let mut stmt = store.conn().prepare(&format!(
            "SELECT a.duration_ms FROM attempts a JOIN runs r ON r.id = a.run_id
              WHERE a.test_id IN ({ids_sql}) AND a.outcome = 'passed' AND a.duration_ms IS NOT NULL
                AND r.status = 'finished'
                AND r.id NOT IN (SELECT run_id FROM forgotten)
                AND NOT EXISTS (SELECT 1 FROM events w WHERE w.run_id = r.id AND w.kind = 'run_warning'
                                AND w.payload LIKE '%; no evidence about the tests%')
                AND NOT EXISTS (SELECT 1 FROM superseded s WHERE s.run_id = a.run_id AND s.test_id = a.test_id)
              ORDER BY r.created_ms DESC LIMIT 20"
        ))?;
        let mut v: Vec<u64> = stmt
            .query_map(params![key], |r| r.get::<_, i64>(0))?
            .map(|d| d.map(|d| d.max(0) as u64))
            .collect::<rusqlite::Result<_>>()?;
        v.sort_unstable();
        // Nothing but zeros: a framework that reports no time (round 17).
        if v.iter().all(|d| *d == 0) {
            v.clear();
        }
        Ok(v)
    };
    let mut ms = read("?1", test_id)?;
    if ms.is_empty() {
        ms = read("SELECT id FROM tests WHERE display = ?1", display)?;
    }
    Ok(percentile(&ms, 95.0))
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
    test_runs_scoped(store, test_id, limit, current, None)
}

/// `test_runs_with`, only over the runs of one suite when `suite` is given.
fn test_runs_scoped(
    store: &Store,
    test_id: &str,
    limit: usize,
    current: Option<&str>,
    suite: Option<&str>,
) -> Result<Vec<TestRun>> {
    let mut stmt = store.conn().prepare(&same_sql(
        "WITH same AS (
             SELECT t2.id FROM tests t1 JOIN tests t2
               ON {SAME}
                -- a Go test seen before its package is known (`(package
                -- pending) #N`) is the qualified test of that name that ran
                -- most recently
                OR (t1.file LIKE '(package pending) #%' AND t1.path = '[]' AND t2.id IN (
                      SELECT q.id FROM tests q JOIN attempts qa ON qa.test_id = q.id JOIN runs qr ON qr.id = qa.run_id
                      WHERE q.suite = t1.suite AND q.name = t1.name AND q.path <> '[]'
                      ORDER BY qr.created_ms DESC LIMIT 1))
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
                   AND (?4 IS NULL OR r3.suite = ?4)
                   -- labels of a run are as of that run: later runs do not
                   -- rewrite them (round 13: 'failed 8x before' on a run
                   -- that followed 3 failures)
                   AND (?3 IS NULL OR r3.created_ms <= (SELECT created_ms FROM runs WHERE id = ?3))))
         SELECT a.run_id, r.code, r.created_ms, a.attempt, a.outcome, a.duration_ms, a.message, a.fingerprint, r.env,
                json_extract(r.spec, '$.selection'), a.test_id, a.passed_in_iteration
         FROM attempts a JOIN runs r ON r.id = a.run_id
         WHERE a.test_id IN (SELECT id FROM same)
           AND NOT EXISTS (SELECT 1 FROM superseded s WHERE s.run_id = a.run_id AND s.test_id = a.test_id)
           AND a.run_id IN (SELECT id FROM counted ORDER BY created_ms DESC LIMIT ?2)
           AND (r.status = 'finished' OR r.id = ?3 OR a.outcome <> 'errored')
         ORDER BY r.created_ms DESC, a.run_id, a.attempt ASC"),
    )?;
    let rows = stmt.query_map(params![test_id, limit as i64, current, suite], |r| {
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
            r.get::<_, String>(10)?,
            r.get::<_, i64>(11)? != 0,
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
    let mut ids: Vec<String> = Vec::new();
    let mut had_pass_run = false;
    for row in rows {
        let (
            run,
            code,
            created,
            attempt,
            outcome,
            dur,
            msg,
            fp,
            env,
            selection,
            row_test,
            passed_in_iteration,
        ) = row?;
        // Only its pass says nothing of the parent: a failure there is the
        // parent's too, as its subtest's (round 18: the two disagreed).
        if let (Some(sel), Some(this)) = (&selection, &this)
            && leaf(sel).starts_with(&format!("{this}/"))
            && !Outcome::parse(&outcome).is_some_and(Outcome::is_failure)
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
                had_pass_run |= last.outcome == Outcome::Passed || passed_in_iteration;
                last.passed_and_failed = had_pass_run && outcome == Outcome::Failed;
                last.outcome = outcome;
                last.duration_ms = dur.map(|d| d as u64);
                if outcome.is_failure() {
                    // A later attempt that names no cause keeps the earlier
                    // one (a teardown error after the failure).
                    last.message = msg.or(last.message.take());
                    last.fingerprint = fp;
                    ids.pop();
                    ids.push(row_test);
                }
            }
            _ => {
                had_pass_run = outcome == Outcome::Passed;
                ids.push(row_test);
                out.push(TestRun {
                    run,
                    code,
                    created_ms: created as u64,
                    outcome,
                    attempts: attempt as u32,
                    passed_on_retry: false,
                    passed_and_failed: false,
                    duration_ms: dur.map(|d| d as u64),
                    message: if outcome.is_failure() { msg } else { None },
                    fingerprint: fp,
                    env,
                })
            }
        }
    }
    // A failure the framework reported before its message (pytest -v prints
    // the line first, the summary names the cause): the message is in the
    // test's kept output, as the run summary shows it (round 17).
    for (r, id) in out.iter_mut().zip(&ids) {
        if r.outcome.is_failure() && r.message.is_none() {
            r.message = crate::output::failure_message(store, &r.run, id);
        }
    }
    Ok(out)
}

pub fn stats(store: &Store, test_id: &str, window: usize) -> Result<Option<TestStats>> {
    stats_with(store, test_id, window, None)
}

/// `stats`, over the runs of one suite only (`prova history --suite`).
pub fn stats_in_suite(
    store: &Store,
    test_id: &str,
    window: usize,
    suite: Option<&str>,
) -> Result<Option<TestStats>> {
    stats_scoped(store, test_id, window, None, suite)
}

fn stats_with(
    store: &Store,
    test_id: &str,
    window: usize,
    current: Option<&str>,
) -> Result<Option<TestStats>> {
    stats_scoped(store, test_id, window, current, None)
}

fn stats_scoped(
    store: &Store,
    test_id: &str,
    window: usize,
    current: Option<&str>,
    only_suite: Option<&str>,
) -> Result<Option<TestStats>> {
    let meta: Option<(String, String, Option<String>)> = store
        .conn()
        .query_row(
            "SELECT display, suite, file FROM tests WHERE id = ?1",
            params![test_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let Some((display, suite, file)) = meta else {
        return Ok(None);
    };
    let place = Names::new(store).place(&display, file.as_deref());
    let recent = test_runs_scoped(store, test_id, window, current, only_suite)?;
    let suites: Vec<String> = {
        let mut stmt = store.conn().prepare(&same_sql(
            "SELECT DISTINCT t2.suite FROM tests t1 JOIN tests t2
               ON {SAME}
                -- a Go test seen before its package is known (`(package
                -- pending) #N`) is the qualified test of that name that ran
                -- most recently
                OR (t1.file LIKE '(package pending) #%' AND t1.path = '[]' AND t2.id IN (
                      SELECT q.id FROM tests q JOIN attempts qa ON qa.test_id = q.id JOIN runs qr ON qr.id = qa.run_id
                      WHERE q.suite = t1.suite AND q.name = t1.name AND q.path <> '[]'
                      ORDER BY qr.created_ms DESC LIMIT 1))
             WHERE t1.id = ?1 ORDER BY t2.suite"),
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
        let e = by_code.entry((r.code.as_str(), env_key(r))).or_default();
        e.insert(r.outcome.is_failure());
        // Passed and failed within the one run: both results on this code.
        if r.passed_and_failed {
            e.insert(false);
        }
    }
    // Code versions, not (code, environment) pairs: one code that flipped
    // in two environments is one (round 14: "2 code version(s)" for one).
    let flips = by_code
        .iter()
        .filter(|(_, s)| s.len() == 2)
        .map(|((code, _), _)| *code)
        .collect::<BTreeSet<_>>()
        .len();
    // Same code, opposite results, different environments: name the
    // variables that differ between a pass and a failure.
    let mut env_dependent: BTreeSet<String> = BTreeSet::new();
    // One earlier pass is too little to call a failure environment-
    // dependent (ADR 0035 section 2): the pass may be anything else.
    let mut env_passes: BTreeSet<&str> = BTreeSet::new();
    if flips == 0 {
        for p in counted.iter().filter(|r| r.outcome == Outcome::Passed) {
            for f in counted
                .iter()
                .filter(|r| r.outcome.is_failure() && r.code == p.code && r.code != "nogit")
            {
                for k in p.env.keys().chain(f.env.keys()) {
                    if p.env.get(k) != f.env.get(k) {
                        env_dependent.insert(k.clone());
                        env_passes.insert(p.run.as_str());
                    }
                }
            }
        }
    }
    if env_passes.len() < 2 {
        env_dependent.clear();
    }
    let retry = recent.iter().filter(|r| r.passed_on_retry).count();
    let mixed = recent.iter().filter(|r| r.passed_and_failed).count();
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
    // And on enough history for the rate to mean something: one earlier
    // pass and three failures is a thin record (round 15: "failing now",
    // then it passed again).
    let failing_now = streak >= needed
        && counted.len() >= FAILING_NOW_MIN_RUNS
        && (retry > 0 || flips > 0 || mixed > 0);
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
        place,
        suite,
        suites,
        aliases: Vec::new(),
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
        flaky: (retry > 0 || flips > 0 || mixed > 0) && !failing_now,
        env_dependent: env_dependent.into_iter().take(5).collect(),
        failing_now,
        size: observed_size(store, test_id)?,
        p50_ms: percentile(&durations, 50.0),
        p95_ms: percentile(&durations, 95.0),
        last_passed,
        recent,
        cut_short: cut_short_runs(store, test_id)?,
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
           -- a Go test of a run still going, under its provisional name,
           -- is listed once, under the package it ran in before
           AND NOT (COALESCE(file, '') LIKE '(package pending) #%' AND EXISTS (
                SELECT 1 FROM tests q WHERE q.suite = tests.suite AND q.name = tests.name AND q.path <> '[]'))
         ORDER BY (display = ?1) DESC, length(display) ASC LIMIT 50",
    )?;
    let rows = stmt.query_map(params![query, suite], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Ids of every test that shares one history with `test_id` (ADR 0018).
pub fn shared_ids(store: &Store, test_id: &str) -> Result<Vec<String>> {
    let mut stmt = store.conn().prepare(&same_sql(
        "SELECT t2.id FROM tests t1 JOIN tests t2
           ON {SAME}
         WHERE t1.id = ?1",
    ))?;
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
         WHERE (?1 IS NULL OR t.suite = ?1) AND a.outcome IN ('failed','errored')
           -- the provisional name of a test whose package was learned later
           -- is the qualified test, which history shows (round 18: flaky
           -- listed the bare one with a thinner history)
           AND t.id NOT IN (SELECT test_id FROM superseded)",
    )?;
    let ids: Vec<String> = stmt
        .query_map(params![suite], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out: Vec<TestStats> = Vec::new();
    for id in ids {
        if let Some(s) = stats_with(store, &id, window, as_of)?
            && s.flaky
        {
            // One test seen as a bare and as a qualified name is one row
            // (its history is shared): the qualified name shows.
            if let Some(o) = out
                .iter_mut()
                .find(|o| o.place == s.place && same_test_name(&o.display, &s.display))
            {
                if o.display != s.display {
                    if s.display.len() > o.display.len() {
                        let old = std::mem::replace(&mut o.display, s.display.clone());
                        o.aliases.push(old);
                    } else {
                        o.aliases.push(s.display.clone());
                    }
                }
                continue;
            }
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

/// Runs of history a test needs before a streak of failures overrides its
/// flakiness.
pub const FAILING_NOW_MIN_RUNS: usize = 5;

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
            // "1 of 3 runs" next to a history of 12 runs read as a
            // miscount (round 14): it is the evidence so far, of what is
            // needed.
            label: format!(
                "small? (nothing more seen in {small} run(s) that could show it; {SMALL_AFTER} needed)"
            ),
        }));
    }
    // Its runs went through a container client: what the tests use is out
    // of sight, and no amount of sampling will change that (round 15).
    let client: Option<(String, Option<String>)> = store
        .conn()
        .query_row(
            "SELECT json_extract(e.payload, '$.message'), json_extract(r.spec, '$.size')
               FROM events e JOIN runs r ON r.id = e.run_id
              WHERE e.kind = 'run_note' AND r.status = 'finished'
                AND json_extract(e.payload, '$.message') LIKE 'the tests ran through `%'
                AND e.run_id IN (SELECT run_id FROM attempts WHERE test_id = ?1)
                AND e.run_id NOT IN (SELECT run_id FROM forgotten)
              -- a run whose suite declared a size is preferred
              ORDER BY json_extract(r.spec, '$.size') IS NULL, r.created_ms DESC
              LIMIT 1",
            params![test_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((note, declared)) = client {
        let name = note.split('`').nth(1).unwrap_or("a container client");
        // What cannot be observed can still be declared: report it as such
        // (round 16: `size = "small"` on the suite still read "unknown").
        let declared = declared.as_deref().and_then(|d| match d {
            "small" => Some(Size::Small),
            "medium" => Some(Size::Medium),
            "large" => Some(Size::Large),
            _ => None,
        });
        if let Some(size) = declared {
            return Ok(Some(SizeView {
                size: Some(size),
                at_most: None,
                label: format!(
                    "{} (declared; out of sight in {name}, so not observed)",
                    size.as_str()
                ),
            }));
        }
        return Ok(Some(SizeView {
            size: None,
            at_most: None,
            label: format!(
                "unknown (ran through {name}: out of sight, so its size cannot be observed; declare it with `size = \"small\"` (or medium, large) under `[suite.<name>]` in prova.toml)"
            ),
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
    /// Of the unknown, those whose runs went through a container client:
    /// their size cannot be observed, only declared.
    pub out_of_sight: usize,
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
        "SELECT id, display, file FROM tests WHERE (?1 IS NULL OR suite = ?1) ORDER BY display",
    )?;
    let rows: Vec<(String, String, Option<String>)> = stmt
        .query_map(params![suite], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let names = Names::new(store);
    // One test, one count: the same test in several suites shares its
    // history (round 12: a 5-test project showed 12 tests). The most
    // decided view wins.
    let mut views: BTreeMap<String, Option<SizeView>> = BTreeMap::new();
    for (id, display, file) in rows {
        let view = observed_size(store, &id)?;
        let e = views
            .entry(names.name(&display, file.as_deref()))
            .or_insert(None);
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
                if view
                    .as_ref()
                    .is_some_and(|v| v.label.starts_with("unknown (ran through"))
                {
                    r.out_of_sight += 1;
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
    /// The runs are of different suites or levels: a test one did not
    /// print is not one the other dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// A run's final outcome per test, by name and place (two binaries' tests
/// of one name are two entries).
fn final_outcomes(store: &Store, run: &str) -> Result<BTreeMap<String, (Outcome, Option<u64>)>> {
    let mut stmt = store.conn().prepare(
        "SELECT t.display, a.outcome, a.duration_ms, t.file FROM attempts a JOIN tests t ON t.id = a.test_id
         WHERE a.run_id = ?1 ORDER BY a.attempt",
    )?;
    let rows = stmt.query_map(params![run], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    let names = Names::new(store);
    let mut m = BTreeMap::new();
    for row in rows {
        let (display, o, d, file) = row?;
        let name = names.name(&display, file.as_deref());
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
    let (rb, rh) = (store.run(base)?, store.run(head)?);
    let mut why = Vec::new();
    if rb.suite != rh.suite {
        why.push(format!("different suites ({}, {})", rb.suite, rh.suite));
    }
    if rb.level != rh.level {
        why.push("different levels of detail".to_owned());
    }
    if !why.is_empty() {
        d.warning = Some(format!(
            "the runs are of {}: a test one did not print shows as added or not visible, not as removed",
            why.join(" and ")
        ));
    }
    if !d.fixed.is_empty() || !d.newly_failing.is_empty() {
        let names = Names::new(store);
        let known: Vec<String> = flaky(store, None, 50)?
            .into_iter()
            .map(|s| names.name(&s.display, s.place.as_deref()))
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
    use crate::model::TestKey;

    /// A store with one finished run per `(code, [(key, outcome)])`.
    fn store_with(runs: &[(&str, Vec<(TestKey, Outcome)>)]) -> Store {
        store_with_ms(runs, |_| 5)
    }

    /// `store_with`, the test of run `i` taking `ms(i)`.
    fn store_with_ms(runs: &[(&str, Vec<(TestKey, Outcome)>)], ms: impl Fn(usize) -> u64) -> Store {
        use crate::model::{EVENT_VERSION, Event, EventKind, Status};
        let d = std::env::temp_dir().join(format!(
            "prova-analysis-{}-{}",
            std::process::id(),
            crate::ids::new_run_id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        let s = Store::open_dir(&d, &d.join(".prova")).unwrap();
        let v = serde_json::json!({});
        for (i, (code, results)) in runs.iter().enumerate() {
            let id = format!("r{i}");
            let ts = 10 + i as u64 * 10;
            s.create_run(&crate::store::NewRun {
                id: &id,
                suite: "s",
                spec: &v,
                cwd: "/",
                code,
                env: &v,
                machine: &v,
                origin: &v,
                created_ms: ts,
            })
            .unwrap();
            s.mark_started(&id, 1, ts + 1).unwrap();
            let mut events = vec![Event {
                v: EVENT_VERSION,
                run: id.clone(),
                seq: 1,
                ts_ms: ts + 1,
                kind: EventKind::RunStarted { pid: 1 },
            }];
            for (n, (key, outcome)) in results.iter().enumerate() {
                events.push(Event {
                    v: EVENT_VERSION,
                    run: id.clone(),
                    seq: 2 + n as u64,
                    ts_ms: ts + 2,
                    kind: EventKind::TestFinished {
                        test: key.clone(),
                        attempt: 1,
                        outcome: *outcome,
                        duration_ms: Some(ms(i)),
                        message: None,
                        source: crate::model::Level::Recognized,
                        subtest: false,
                        passed_in_iteration: false,
                    },
                });
            }
            s.append_events(&events).unwrap();
            s.finish_run(
                &id,
                100,
                ts + 5,
                Status::Finished,
                Some(Outcome::Passed),
                Some(0),
                None,
                vec![],
                None,
            )
            .unwrap();
        }
        s
    }

    /// Issue 91: the slow budget read the whole `stats` of each test as it
    /// started; the lean query must give the same p95 (and none for zeros).
    #[test]
    fn the_slow_budget_p95_matches_the_stats_p95() {
        let k = TestKey::new(vec!["example.com/calc".into()], "TestTiming");
        let runs: Vec<_> = (0..8)
            .map(|_| ("tree:a", vec![(k.clone(), Outcome::Passed)]))
            .collect();
        let s = store_with_ms(&runs, |i| 100 * (i as u64 + 1));
        let id = crate::store::test_id("s", &k);
        let full = stats(&s, &id, 20).unwrap().unwrap().p95_ms;
        assert_eq!(full, Some(800));
        assert_eq!(usual_p95_ms(&s, &id, &k.display()).unwrap(), full);
        // A test never seen under this id is known by its name.
        let other = crate::store::test_id("other", &k);
        assert_eq!(usual_p95_ms(&s, &other, &k.display()).unwrap(), full);
        // Zero durations are a framework that reports no time.
        let z = store_with_ms(&runs, |_| 0);
        assert_eq!(usual_p95_ms(&z, &id, &k.display()).unwrap(), None);
    }

    /// Round 17: `prova flaky` listed a Go test twice, once under the bare
    /// name the text output gave and once under its package.
    #[test]
    fn a_test_recorded_bare_and_qualified_is_one_flaky_row() {
        let bare = TestKey::new(vec![], "TestFlakyClock");
        let full = TestKey::new(vec!["example.com/calc".into()], "TestFlakyClock");
        let s = store_with(&[
            ("tree:a", vec![(bare.clone(), Outcome::Failed)]),
            ("tree:a", vec![(bare, Outcome::Passed)]),
            ("tree:a", vec![(full.clone(), Outcome::Failed)]),
            ("tree:a", vec![(full, Outcome::Passed)]),
        ]);
        let f = flaky(&s, None, 50).unwrap();
        assert_eq!(
            f.len(),
            1,
            "{:?}",
            f.iter().map(|t| &t.display).collect::<Vec<_>>()
        );
        assert_eq!(f[0].display, "example.com/calc::TestFlakyClock");
        assert_eq!(f[0].aliases, ["TestFlakyClock"]);
    }

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
        "SELECT t.display, t.suite, a.duration_ms, t.file FROM (
             SELECT a.test_id, a.duration_ms,
                    row_number() OVER (PARTITION BY a.test_id ORDER BY a.ts_ms DESC) AS rn
               FROM attempts a JOIN runs r ON r.id = a.run_id
              WHERE a.duration_ms IS NOT NULL AND a.outcome IN ('passed', 'failed')
                AND r.status = 'finished'
                AND NOT EXISTS (SELECT 1 FROM superseded s WHERE s.run_id = a.run_id AND s.test_id = a.test_id)
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
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    let names = Names::new(store);
    let mut by_test: BTreeMap<String, (BTreeSet<String>, Vec<u64>)> = BTreeMap::new();
    for row in rows {
        let (display, suite, ms, file) = row?;
        let e = by_test
            .entry(names.name(&display, file.as_deref()))
            .or_default();
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
