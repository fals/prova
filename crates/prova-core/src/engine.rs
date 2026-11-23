//! Run engine: create, supervise, wait (ADR 0002, 0004, 0013).
//!
//! `supervise` runs in its own detached process, one per run. Everything
//! else talks to it through the store: events out, cancel requests in.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::{CommandSpec, RunSpec};
use crate::identity::{code_identity, machine_fingerprint, output_fingerprint, recorded_env};
use crate::ids::{new_run_id, now_ms};
use crate::lifecycle::Lifecycle;
use crate::model::{EVENT_VERSION, Event, EventKind, Level, Outcome, StallBasis, Status, TestKey};
use crate::process;
use crate::store::{NewRun, RunRow, Store, StoreError};

pub type Result<T> = std::result::Result<T, StoreError>;

const TICK: Duration = Duration::from_millis(100);
const SAMPLE_EVERY: Duration = Duration::from_millis(500);
const GRACE: Duration = Duration::from_secs(3);
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(1);
const TAIL_LINES: usize = 40;

/// Hooks for adapters (prova-adapters depends on prova-core, not the reverse).
pub trait Observer {
    /// Highest level this observer can reach while the run is live.
    fn live_level(&self) -> Level {
        Level::Opaque
    }
    /// Called for every output line, stdout and stderr interleaved.
    fn on_line(&mut self, _line: &str, _now_ms: u64) -> Vec<EventKind> {
        Vec::new()
    }
    /// Extra environment for the process under test (opt-in injection).
    /// `run_dir` is private to this run; files written there are the
    /// observer's to read in `on_exit`.
    fn env(&mut self, _run_dir: &Path) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Called every tick while the process runs: events from side streams
    /// the framework writes to files (opt-in injection), read live.
    fn poll(&mut self, _now_ms: u64) -> Vec<EventKind> {
        Vec::new()
    }
    /// Called after the process exited; may ingest result files.
    fn on_exit(&mut self, _root: &Path, _started_ms: u64) -> ExitReport {
        ExitReport::default()
    }
}

/// What an observer found after the process exited.
#[derive(Debug, Default)]
pub struct ExitReport {
    pub events: Vec<EventKind>,
    pub level: Option<Level>,
    /// Failing tests' own output (stack traces, captured output), kept
    /// compressed with the run (ADR 0016).
    pub test_outputs: Vec<(TestKey, String)>,
    /// Evidence against a pass (ADR 0017): the run fails, with these reasons.
    pub contradictions: Vec<String>,
    /// A pass that verified little (ADR 0017): shown with the pass.
    pub warnings: Vec<String>,
}

/// Observer that sees nothing: a level-0 run.
pub struct Opaque;
impl Observer for Opaque {}

pub struct NewRunRequest {
    pub suite: String,
    pub spec: RunSpec,
    pub cwd: PathBuf,
    pub extra_env: Vec<String>,
}

pub fn create_run(store: &Store, req: &NewRunRequest) -> Result<String> {
    let id = new_run_id();
    let spec = serde_json::to_value(&req.spec)?;
    store.create_run(&NewRun {
        id: &id,
        suite: &req.suite,
        spec: &spec,
        cwd: &req.cwd.to_string_lossy(),
        code: &code_identity(&req.cwd),
        env: &recorded_env(&req.extra_env),
        machine: &machine_fingerprint(),
        origin: &crate::identity::run_origin(),
        created_ms: now_ms(),
    })?;
    Ok(id)
}

/// Starts `prova __supervise` for the run as a detached process.
pub fn spawn_supervisor(store: &Store, exe: &Path, run: &str) -> std::io::Result<Child> {
    let dir = store.run_dir(run);
    crate::store::private_dir(&dir)?;
    let log = crate::store::private_file(&dir.join("supervisor.log"))?;
    Command::new(exe)
        .arg("__supervise")
        .arg("--root")
        .arg(store.root())
        .arg("--dir")
        .arg(store.dir())
        .arg(run)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        // own group: a Ctrl-C aimed at the caller does not reach the run
        .process_group(0)
        .spawn()
}

use std::os::unix::process::CommandExt;

enum Chunk {
    Line(String),
    Eof,
}

fn reader(stream: impl Read + Send + 'static, tx: mpsc::Sender<Chunk>) {
    std::thread::spawn(move || {
        let mut r = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).into_owned();
                    if tx.send(Chunk::Line(line)).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = tx.send(Chunk::Eof);
    });
}

struct Supervisor<'a> {
    store: &'a Store,
    run: String,
    seq: u64,
    pending: Vec<Event>,
    lifecycle: Lifecycle,
    attempts: BTreeMap<String, u32>,
    output: File,
    distiller: crate::output::Distiller,
    tail: VecDeque<String>,
    any_test_failed: bool,
    level: Level,
    suite: String,
    default_slow: Duration,
    /// Running tests: (key, started, slow budget, slow reported).
    running: BTreeMap<String, (TestKey, Instant, Duration, bool)>,
}

impl Supervisor<'_> {
    fn emit(&mut self, mut kind: EventKind) {
        // Frameworks that report a start but no duration: measure it.
        if let EventKind::TestFinished {
            test,
            duration_ms: d @ None,
            ..
        } = &mut kind
            && let Some((_, started, _, _)) = self.running.get(&test.unique())
        {
            *d = Some(started.elapsed().as_millis() as u64);
        }
        // Guard every test event with the lifecycle (spec/run.qnt).
        let ok = match &kind {
            EventKind::TestStarted { test } => {
                let budget = self.slow_budget(test);
                self.running
                    .insert(test.unique(), (test.clone(), Instant::now(), budget, false));
                let n = self.attempts.entry(test.unique()).or_insert(0);
                *n += 1;
                let key = format!("{}#{}", test.unique(), n);
                self.lifecycle.test_started(&key).is_ok()
            }
            EventKind::TestFinished {
                test,
                attempt,
                outcome,
                ..
            } => {
                self.running.remove(&test.unique());
                let key = format!("{}#{attempt}", test.unique());
                let ok = self.lifecycle.test_finished(&key, *outcome).is_ok();
                if ok && outcome.is_failure() {
                    self.any_test_failed = true;
                }
                ok
            }
            _ => true,
        };
        if !ok {
            return;
        }
        self.seq += 1;
        self.pending.push(Event {
            v: EVENT_VERSION,
            run: self.run.clone(),
            seq: self.seq,
            ts_ms: now_ms(),
            kind,
        });
    }

    /// History p95 times 3 (at least 1s), else the size default (ADR 0004).
    fn slow_budget(&self, test: &TestKey) -> Duration {
        let id = crate::store::test_id(&self.suite, test);
        match crate::analysis::stats(self.store, &id, 20) {
            Ok(Some(s)) if s.p95_ms.is_some() => {
                Duration::from_millis((s.p95_ms.unwrap_or(0) * 3).max(1000))
            }
            _ => self.default_slow,
        }
    }

    /// Emits `test_slow` once per running test over its budget.
    fn check_slow(&mut self) {
        let now = Instant::now();
        let mut slow = Vec::new();
        for (key, started, budget, reported) in self.running.values_mut() {
            let elapsed = now.duration_since(*started);
            if !*reported && elapsed > *budget {
                *reported = true;
                slow.push(EventKind::TestSlow {
                    test: key.clone(),
                    elapsed_ms: elapsed.as_millis() as u64,
                    budget_ms: budget.as_millis() as u64,
                });
            }
        }
        for e in slow {
            self.emit(e);
        }
    }

    fn flush(&mut self) -> Result<()> {
        let events = std::mem::take(&mut self.pending);
        self.store.append_events(&events)
    }

    fn line(&mut self, line: &str, observer: &mut dyn Observer) {
        let _ = self.output.write_all(line.as_bytes());
        self.distiller.push(line);
        let trimmed = line.trim_end();
        if !trimmed.is_empty() {
            if self.tail.len() == TAIL_LINES {
                self.tail.pop_front();
            }
            self.tail.push_back(trimmed.to_owned());
        }
        let events = observer.on_line(trimmed, now_ms());
        self.observed(events, observer);
    }

    fn observed(&mut self, events: Vec<EventKind>, observer: &dyn Observer) {
        for e in events {
            self.emit(e);
        }
        let live = observer.live_level();
        if live > self.level {
            self.level = live;
            let _ = self.store.set_level(&self.run, live);
        }
    }
}

/// Body of `prova __supervise`: runs the process and records everything.
pub fn supervise(store: &Store, run: &str, observer: &mut dyn Observer) -> Result<()> {
    let row = store.run(run)?;
    let spec: RunSpec = serde_json::from_value(row.spec.clone())?;
    if !store.mark_started(run, std::process::id(), now_ms())? {
        return Ok(()); // cancelled before start
    }
    let started_ms = now_ms();
    let dir = store.run_dir(run);
    crate::store::private_dir(&dir)?;
    let mut sup = Supervisor {
        store,
        run: run.to_owned(),
        seq: 0,
        pending: Vec::new(),
        lifecycle: Lifecycle::at(Status::Running),
        attempts: BTreeMap::new(),
        output: crate::store::private_file(&dir.join("output.log"))?,
        distiller: crate::output::Distiller::default(),
        tail: VecDeque::new(),
        any_test_failed: false,
        level: observer.live_level(),
        suite: row.suite.clone(),
        default_slow: spec
            .size
            .map(crate::config::Size::slow_budget)
            .unwrap_or(Duration::from_secs(60)),
        running: BTreeMap::new(),
    };
    store.set_level(run, sup.level)?;

    let mut cmd = match &spec.command {
        CommandSpec::Argv(argv) => {
            let mut c = Command::new(&argv[0]);
            c.args(&argv[1..]);
            c
        }
        CommandSpec::Shell(s) => {
            let mut c = Command::new("sh");
            c.arg("-c").arg(s);
            c
        }
    };
    let injected = observer.env(&dir);
    for (k, v) in &injected {
        cmd.env(k, v);
    }
    cmd.current_dir(&row.cwd)
        .env("PROVA_RUN_ID", run)
        // The selection reaches the shell only as a variable, never as text.
        .env("PROVA_FILTER", spec.selection.as_deref().unwrap_or(""))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let reason = format!("could not start {}: {e}", spec.command.display());
            sup.line(&format!("prova: {reason}\n"), observer);
            return store.finish_run(
                run,
                1,
                now_ms(),
                Status::Aborted,
                Some(Outcome::Errored),
                None,
                Some(&reason),
                vec![],
                None,
            );
        }
    };
    let pgid = child.id();
    sup.emit(EventKind::RunStarted { pid: pgid });
    if !injected.is_empty() {
        let vars: Vec<String> = injected.iter().map(|(k, v)| format!("{k}={v}")).collect();
        sup.emit(EventKind::RunNote {
            message: format!("injected by prova.toml (opt-in): {}", vars.join("; ")),
        });
    }
    let result = observe_child(
        store, run, &spec, started_ms, &mut sup, &mut child, pgid, observer,
    );
    if let Err(e) = &result {
        // Never leave the process under test running unsupervised.
        process::kill_group(pgid);
        let _ = child.wait();
        let reason = format!("supervisor error: {e}");
        let _ = store.finish_run(
            run,
            sup.seq + 1,
            now_ms(),
            Status::Aborted,
            Some(Outcome::Errored),
            None,
            Some(&reason),
            vec![],
            None,
        );
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn observe_child(
    store: &Store,
    run: &str,
    spec: &RunSpec,
    started_ms: u64,
    sup: &mut Supervisor<'_>,
    child: &mut Child,
    pgid: u32,
    observer: &mut dyn Observer,
) -> Result<()> {
    sup.flush()?;

    let (tx, rx) = mpsc::channel();
    if let Some(out) = child.stdout.take() {
        reader(out, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        reader(err, tx.clone());
    }
    drop(tx);

    let stall_after = Duration::from_millis(spec.stall_after_ms);
    let kill_after = Duration::from_millis(spec.kill_after_ms);
    let start = Instant::now();
    let mut last_activity = Instant::now();
    let mut last_sample = Instant::now();
    let mut sample = process::sample_group(pgid);
    let mut stalled = false;
    let mut terminating: Option<(Instant, Status, String)> = None;
    let mut open_streams = 2;

    let exit = loop {
        // Drain output for up to one tick.
        let deadline = Instant::now() + TICK;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(Chunk::Line(l)) => {
                    last_activity = Instant::now();
                    sup.line(&l, observer);
                }
                Ok(Chunk::Eof) => open_streams -= 1,
                Err(_) => break,
            }
        }
        let streamed = observer.poll(now_ms());
        if !streamed.is_empty() {
            last_activity = Instant::now();
            sup.observed(streamed, observer);
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let now = Instant::now();
        if now.duration_since(last_sample) >= SAMPLE_EVERY {
            last_sample = now;
            let s = process::sample_group(pgid);
            if s.cpu_ticks != sample.cpu_ticks || !s.pids.is_subset(&sample.pids) {
                last_activity = now;
            }
            sample = s;
            if terminating.is_none() && store.cancel_requested(run)? {
                process::terminate_group(pgid);
                terminating = Some((now, Status::Cancelled, "cancelled".into()));
            }
        }
        let idle = now.duration_since(last_activity);
        if idle >= stall_after && !stalled {
            stalled = true;
            sup.emit(EventKind::RunStalled {
                idle_ms: idle.as_millis() as u64,
                basis: StallBasis::ProcessTree,
            });
        } else if idle < stall_after {
            stalled = false;
        }
        sup.check_slow();
        if terminating.is_none() && now.duration_since(start) >= kill_after {
            process::terminate_group(pgid);
            let reason = format!(
                "killed: exceeded kill_after {}",
                crate::ids::format_duration(kill_after)
            );
            terminating = Some((now, Status::Aborted, reason));
        }
        if let Some((since, _, _)) = &terminating
            && now.duration_since(*since) >= GRACE
        {
            process::kill_group(pgid);
        }
        sup.flush()?;
    };

    // Collect remaining output; leaked processes may hold the pipes open,
    // so never wait on EOF without a bound.
    let drain = |sup: &mut Supervisor<'_>, open: &mut i32, observer: &mut dyn Observer| {
        let until = Instant::now() + DRAIN_AFTER_EXIT;
        while *open > 0 {
            let Some(left) = until.checked_duration_since(Instant::now()) else {
                break;
            };
            match rx.recv_timeout(left) {
                Ok(Chunk::Line(l)) => sup.line(&l, observer),
                Ok(Chunk::Eof) => *open -= 1,
                Err(_) => break,
            }
        }
    };
    drain(sup, &mut open_streams, observer);
    let leaked: Vec<u32> = process::sample_group(pgid).pids.into_iter().collect();
    if !leaked.is_empty() {
        process::kill_group(pgid);
        drain(sup, &mut open_streams, observer);
    }

    let report = observer.on_exit(Path::new(store.root()), started_ms);
    for e in report.events {
        sup.emit(e);
    }
    if let Some(l) = report.level
        && l > sup.level
    {
        sup.level = l;
    }
    store.set_level(run, sup.level)?;

    let exit_code = exit.code();
    let (status, mut reason) = match terminating {
        Some((_, s, r)) => (s, Some(r)),
        None => (
            Status::Finished,
            exit_code
                .is_none()
                .then(|| "terminated by signal".to_owned()),
        ),
    };
    // ADR 0017: the verdict is decided from all the evidence.
    let mut contradictions = report.contradictions;
    let mut warnings = report.warnings;
    let observed = sup.lifecycle.outcomes().len() + sup.lifecycle.open().len();
    if spec.expects_tests() && status == Status::Finished {
        if let Some(sel) = &spec.selection
            && observed == 0
            && sup.level > Level::Opaque
        {
            contradictions.push(format!(
                "selection {sel:?} matched no test: nothing was verified"
            ));
        }
        if spec.selection.is_none() {
            sup.flush()?; // this run's results must be in the store to compare
            let current: Vec<String> = {
                let mut stmt = store
                    .conn()
                    .prepare("SELECT DISTINCT test_id FROM attempts WHERE run_id = ?1")?;
                stmt.query_map(rusqlite::params![run], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            if let Some((prev, missing, total)) =
                missing_since_last_pass(store, run, &sup.suite, &current)?
            {
                if current.is_empty() && observed == 0 {
                    contradictions.push(format!(
                        "the suite's last passing run {prev} ran {total} test(s); this run observed none"
                    ));
                } else if !missing.is_empty() {
                    let names: Vec<&str> = missing.iter().take(5).map(String::as_str).collect();
                    warnings.push(format!(
                        "{} test(s) from the last passing run {prev} did not run: {}{}",
                        missing.len(),
                        names.join(", "),
                        if missing.len() > names.len() {
                            ", ..."
                        } else {
                            ""
                        }
                    ));
                }
            }
        }
    }
    if let Some(code @ (126 | 127)) = exit_code {
        reason = Some(format!(
            "the command could not start (exit {code}: {}); no test ran",
            if code == 127 {
                "command not found"
            } else {
                "not executable"
            }
        ));
    }
    if !contradictions.is_empty() {
        let _ = sup.lifecycle.contradict();
        reason.get_or_insert_with(|| contradictions.join("; "));
        for c in &contradictions {
            sup.emit(EventKind::RunNote { message: c.clone() });
        }
    }
    let _ = sup.lifecycle.exited(exit_code);
    // Tests still open at the end become errored (spec: end closes open tests).
    let closed = sup.lifecycle.end(status).unwrap_or_default();
    let outcome = sup.lifecycle.verdict().unwrap_or(Outcome::Errored);
    if outcome == Outcome::Passed {
        if let Some((line, text)) = sup.distiller.first_crash() {
            warnings.push(format!(
                "the output shows an error outside any failing test (line {line}): {}",
                text.trim().chars().take(160).collect::<String>()
            ));
        }
        for w in &warnings {
            sup.emit(EventKind::RunWarning { message: w.clone() });
        }
    } else if !closed.is_empty() {
        reason.get_or_insert_with(|| {
            format!(
                "{} test(s) did not finish before the run ended",
                closed.len()
            )
        });
    }
    let no_test_failed = !sup.lifecycle.outcomes().values().any(|o| o.is_failure());
    if outcome == Outcome::Failed && no_test_failed && reason.is_none() {
        // A failure no test explains: say so, and show the first error.
        reason = Some(match sup.distiller.first_error() {
            Some((line, text)) => format!(
                "no test failed; exit {}; first error at line {line}: {}",
                exit_code.unwrap_or(-1),
                text.trim().chars().take(200).collect::<String>()
            ),
            None => format!(
                "no test failed; the command exited {}",
                exit_code.unwrap_or(-1)
            ),
        });
    }
    for key in closed {
        let (unique, attempt) = key.rsplit_once('#').unwrap_or((&key, "1"));
        // The test keeps its real identity (ADR 0018 round 5: rebuilding it
        // from the lifecycle key made a garbled duplicate test).
        let (test, duration_ms) = match sup.running.get(unique) {
            Some((t, started, _, _)) => (t.clone(), Some(started.elapsed().as_millis() as u64)),
            None => (
                TestKey::new(Vec::new(), unique.rsplit('\u{1f}').next().unwrap_or(unique)),
                None,
            ),
        };
        sup.seq += 1;
        let kind = EventKind::TestFinished {
            test,
            attempt: attempt.parse().unwrap_or(1),
            outcome: Outcome::Errored,
            duration_ms,
            message: Some("test did not finish before the run ended".into()),
            source: sup.level,
        };
        sup.pending.push(Event {
            v: EVENT_VERSION,
            run: run.to_owned(),
            seq: sup.seq,
            ts_ms: now_ms(),
            kind,
        });
    }
    sup.flush()?;
    // Distill before the run becomes terminal: readers of a finished run
    // find its excerpt (ADR 0016).
    let dir = store.run_dir(run);
    let excerpt = std::mem::take(&mut sup.distiller).finish(outcome == Outcome::Passed);
    let spool = dir.join("output.log");
    crate::output::keep_run(
        store,
        run,
        &excerpt,
        spec.keep_output.then_some(spool.as_path()),
    )?;
    for (test, text) in &report.test_outputs {
        crate::output::keep_test(store, run, &crate::store::test_id(&sup.suite, test), text)?;
    }
    let tail: Vec<String> = sup.tail.iter().cloned().collect();
    let fingerprint = (outcome != Outcome::Passed)
        .then(|| output_fingerprint(&tail[tail.len().saturating_sub(20)..]));
    store.finish_run(
        run,
        sup.seq + 1,
        now_ms(),
        status,
        Some(outcome),
        exit_code,
        reason.as_deref(),
        leaked,
        fingerprint.as_deref(),
    )?;
    // Nothing of the run stays on disk; kept output stays within budget.
    let _ = std::fs::remove_dir_all(&dir);
    crate::output::enforce_budget(store, crate::output::BUDGET_BYTES)?;
    crate::output::sweep(store)
}

/// Tests of the suite's last passing full run that this run did not record:
/// (that run, their names, its test count). `None` without such a run.
fn missing_since_last_pass(
    store: &Store,
    run: &str,
    suite: &str,
    current: &[String],
) -> Result<Option<(String, Vec<String>, usize)>> {
    use rusqlite::OptionalExtension;
    let prev: Option<String> = store
        .conn()
        .query_row(
            "SELECT id FROM runs WHERE suite = ?1 AND id <> ?2 AND status = 'finished' AND outcome = 'passed'
               AND json_extract(spec, '$.selection') IS NULL
             ORDER BY created_ms DESC LIMIT 1",
            rusqlite::params![suite, run],
            |r| r.get(0),
        )
        .optional()?;
    let Some(prev) = prev else { return Ok(None) };
    let mut stmt = store.conn().prepare(
        "SELECT a.test_id, t.display FROM attempts a JOIN tests t ON t.id = a.test_id
         WHERE a.run_id = ?1 GROUP BY a.test_id ORDER BY t.display",
    )?;
    let earlier: Vec<(String, String)> = stmt
        .query_map(rusqlite::params![prev], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if earlier.is_empty() {
        return Ok(None);
    }
    let missing = earlier
        .iter()
        .filter(|(id, _)| !current.contains(id))
        .map(|(_, d)| d.clone())
        .collect();
    Ok(Some((prev, missing, earlier.len())))
}

// ------------------------------------------------------------------ wait

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Until {
    /// First failure, stall, or end: anything an agent should react to.
    FirstFailure,
    /// Stall or end.
    Stall,
    /// End only.
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    Failure,
    /// A test exceeded its slow budget and is still running.
    Slow,
    Stall,
    Done,
    /// `max_wait` elapsed; the run is still going.
    Running,
}

#[derive(Debug, Clone, Serialize)]
pub struct Failure {
    pub test: String,
    /// `failed` or `errored` (errored includes a test that never finished).
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// History of this test: seen before, flaky, last passing code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<crate::analysis::FailureContext>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SlowTest {
    pub test: String,
    pub elapsed_ms: u64,
    pub budget_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    pub errored: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub run: String,
    pub suite: String,
    pub command: String,
    pub reason: WaitReason,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    pub level: Level,
    pub last_seq: u64,
    pub elapsed_ms: u64,
    pub counts: Counts,
    /// Tests running now (started, not finished), oldest first; at most 10.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub running: Vec<String>,
    /// The framework's own count (tests, failed), when it printed one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framework: Option<(u64, u64)>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<Failure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stalled_ms: Option<u64>,
    /// Tests over their slow budget that have not finished.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub slow: Vec<SlowTest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub leaked: Vec<u32>,
    /// What prova could not see, and how to fix it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// Why a pass verified little (ADR 0017); empty when it is trustworthy.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Set once the run ended: `true` only for a pass with no warning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub output_tail: Vec<String>,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// Data directory this run was recorded in (ADR 0014).
    pub store: String,
    /// History of the same suite, filled in by the caller when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<serde_json::Value>,
}

fn matches(until: Until, kind: &EventKind) -> Option<WaitReason> {
    match kind {
        EventKind::RunFinished { .. } => Some(WaitReason::Done),
        EventKind::RunStalled { .. } if until != Until::Done => Some(WaitReason::Stall),
        EventKind::TestSlow { .. } if until == Until::FirstFailure => Some(WaitReason::Slow),
        EventKind::TestFinished { outcome, .. }
            if until == Until::FirstFailure && outcome.is_failure() =>
        {
            Some(WaitReason::Failure)
        }
        _ => None,
    }
}

/// Supervisor gone while the run is not terminal: record it as aborted.
/// The update is conditional, so a supervisor that just moved the run on
/// is never overwritten.
/// Reaps every run whose supervisor is gone, so lists never show a dead run
/// as running (round 5: only `prova status <run>` did).
pub fn reap_orphans(store: &Store, rows: &[RunRow]) -> Result<bool> {
    let mut any = false;
    for row in rows.iter().filter(|r| !r.status.is_terminal()) {
        any |= reap_orphan(store, row)?;
    }
    Ok(any)
}

fn reap_orphan(store: &Store, row: &RunRow) -> Result<bool> {
    let orphan = match row.status {
        Status::Running => row
            .supervisor_pid
            .is_some_and(|p| !process::supervises(p, &row.id)),
        Status::Created => now_ms().saturating_sub(row.created_ms) > 15_000,
        _ => false,
    };
    if !orphan {
        return Ok(false);
    }
    store.abort_orphan(
        &row.id,
        row.status,
        row.supervisor_pid,
        now_ms(),
        "supervisor exited without ending the run",
    )
}

/// Blocks at most `max_wait` for the next significant event after `after`.
pub fn wait(
    store: &Store,
    run: &str,
    until: Until,
    after: u64,
    max_wait: Duration,
) -> Result<Summary> {
    let deadline = Instant::now() + max_wait;
    let mut cursor = after;
    let mut version = -1;
    loop {
        let v = store.data_version()?;
        if v != version {
            version = v;
            let events = store.events_after(run, cursor)?;
            for e in &events {
                cursor = e.seq;
                if let Some(reason) = matches(until, &e.kind) {
                    return summarize(store, run, reason, e.seq);
                }
            }
        }
        let row = store.run(run)?;
        if reap_orphan(store, &row)? {
            continue;
        }
        if Instant::now() >= deadline {
            return summarize(store, run, WaitReason::Running, cursor);
        }
        std::thread::sleep(TICK);
    }
}

/// Last `n` lines of a run's output: from the live spool, or from the
/// excerpt kept once the run ended.
pub fn output_tail(store: &Store, run: &str, n: usize) -> Vec<String> {
    if let Ok(crate::output::Kept::Excerpt(e)) = crate::output::run_excerpt(store, run) {
        return e.tail(n);
    }
    let path = store.run_dir(run).join("output.log");
    let Ok(mut f) = File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(64 * 1024);
    let mut buf = String::new();
    if f.seek(SeekFrom::Start(start)).is_err() || f.read_to_string(&mut buf).is_err() {
        // non-UTF-8 output: fall back to lossy
        let mut bytes = Vec::new();
        let _ = f.seek(SeekFrom::Start(start));
        let _ = f.read_to_end(&mut bytes);
        buf = String::from_utf8_lossy(&bytes).into_owned();
    }
    let lines: Vec<String> = buf
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

pub fn summarize(store: &Store, run: &str, reason: WaitReason, last_seq: u64) -> Result<Summary> {
    // A run whose supervisor is gone is recorded as aborted before it is
    // described: no summary may show a dead run as running.
    if reap_orphan(store, &store.run(run)?)? {
        return summarize(store, run, WaitReason::Done, last_seq);
    }
    let row = store.run(run)?;
    // An event matched, but the run is over by now: report the end.
    let reason = if row.status.is_terminal() && reason != WaitReason::Running {
        WaitReason::Done
    } else {
        reason
    };
    let events = store.events_after(run, 0)?;
    // Final outcome per test: the last attempt counts, retries do not.
    let mut finals: BTreeMap<String, (TestKey, Outcome, Option<String>, u64)> = BTreeMap::new();
    let mut stalled_ms = None;
    let mut leaked = Vec::new();
    let mut slow: BTreeMap<String, SlowTest> = BTreeMap::new();
    let mut notes: Vec<String> = Vec::new();
    let mut framework: Option<(u64, u64)> = None;
    let mut warnings: Vec<String> = Vec::new();
    // Tests started and not finished yet, in start order (ADR 0018: a stall
    // or slow report names what is running).
    let mut running: Vec<(String, String)> = Vec::new();
    let mut started_at: BTreeMap<String, u64> = BTreeMap::new();
    for e in &events {
        match &e.kind {
            EventKind::TestFinished {
                test,
                outcome,
                message,
                ..
            } => {
                running.retain(|(u, _)| *u != test.unique());
                slow.remove(&test.unique());
                finals.insert(
                    test.unique(),
                    (test.clone(), *outcome, message.clone(), e.seq),
                );
            }
            EventKind::TestSlow {
                test,
                elapsed_ms,
                budget_ms,
            } => {
                slow.insert(
                    test.unique(),
                    SlowTest {
                        test: test.display(),
                        // How long it has been running now, not when the
                        // slow event fired (round 5: "running 1.0s" at 2.8s).
                        elapsed_ms: started_at
                            .get(&test.unique())
                            .map(|t| now_ms().saturating_sub(*t))
                            .unwrap_or(0)
                            .max(*elapsed_ms),
                        budget_ms: *budget_ms,
                    },
                );
            }
            EventKind::RunStalled { idle_ms, .. } => stalled_ms = Some(*idle_ms),
            EventKind::TestStarted { test } => {
                stalled_ms = None;
                started_at.insert(test.unique(), e.ts_ms);
                if !running.iter().any(|(u, _)| *u == test.unique()) {
                    running.push((test.unique(), test.display()));
                }
            }
            EventKind::RunFinished { leaked: l, .. } => leaked = l.clone(),
            EventKind::RunNote { message } => notes.push(message.clone()),
            EventKind::RunWarning { message } => warnings.push(message.clone()),
            EventKind::RunTotals { tests, failed } => framework = Some((*tests, *failed)),
            _ => {}
        }
    }
    let mut counts = Counts::default();
    let mut failing: Vec<&(TestKey, Outcome, Option<String>, u64)> = Vec::new();
    for f in finals.values() {
        match f.1 {
            Outcome::Passed => counts.passed += 1,
            Outcome::Failed => counts.failed += 1,
            Outcome::Skipped => counts.skipped += 1,
            Outcome::Errored => counts.errored += 1,
        }
        if f.1.is_failure() {
            failing.push(f);
        }
    }
    failing.sort_by_key(|f| f.3); // in the order they happened
    // Did the same suite (same selection) pass on exactly this code before?
    let selection = serde_json::from_value::<RunSpec>(row.spec.clone())
        .ok()
        .and_then(|s| s.selection);
    // Only a run that finished can suggest flakiness: a cancelled or aborted
    // run's unfinished tests say nothing about the code (ADR 0018).
    let suite_passed_on_same_code = row.status == Status::Finished
        && store
            .suite_history(&row.suite, selection.as_deref(), row.created_ms, 200)?
            .iter()
            .any(|r| r.code == row.code && r.outcome == Some(Outcome::Passed));
    let mut failures = Vec::new();
    for (key, outcome, message, _) in failing.iter().take(20).copied() {
        let id = crate::store::test_id(&row.suite, key);
        failures.push(Failure {
            test: key.display(),
            outcome: *outcome,
            message: message.clone(),
            context: crate::analysis::failure_context(store, &id, run)?.map(|mut c| {
                c.suite_passed_on_same_code = suite_passed_on_same_code;
                c
            }),
        });
    }
    let n_failures = failing.len();
    let end = row.ended_ms.unwrap_or_else(now_ms);
    let failed_run = matches!(row.outcome, Some(o) if o != Outcome::Passed);
    // Raw output only when it adds something: a stall, or a failure that
    // per-test messages do not explain.
    let explained = !failures.is_empty() && failures.iter().all(|f| f.message.is_some());
    let want_tail = reason == WaitReason::Stall
        || ((failed_run || reason == WaitReason::Failure) && !explained);
    let command = serde_json::from_value::<RunSpec>(row.spec.clone())
        .map(|s| s.command.display())
        .unwrap_or_default();
    // A selection that ran nothing must not look like a pass.
    let ran = counts.passed + counts.failed + counts.skipped + counts.errored;
    // (A selection that matched nothing at level 1+ fails the run, with
    // its own note; at level 0 prova cannot tell.)
    if let Some(sel) = &selection
        && row.level == Level::Opaque
        && row.status == Status::Finished
        && ran == 0
    {
        notes.push(format!(
            "selection {sel:?}: no per-test results seen, so prova cannot tell whether any test matched"
        ));
    }
    Ok(Summary {
        run: row.id.clone(),
        suite: row.suite.clone(),
        command,
        reason,
        status: row.status,
        outcome: row.outcome,
        level: row.level,
        last_seq,
        elapsed_ms: end.saturating_sub(row.started_ms.unwrap_or(row.created_ms)),
        counts,
        running: if row.status.is_terminal() {
            Vec::new()
        } else {
            running.into_iter().map(|(_, d)| d).take(10).collect()
        },
        framework,
        failures,
        stalled_ms,
        slow: if row.status.is_terminal() {
            Vec::new()
        } else {
            slow.into_values().collect()
        },
        exit_code: row.exit_code,
        detail: match (row.reason.clone(), n_failures > 20) {
            (Some(r), _) => Some(r),
            (None, true) => Some(format!("{n_failures} failures; showing 20")),
            _ => None,
        },
        leaked,
        notes,
        verified: row
            .status
            .is_terminal()
            .then(|| row.outcome == Some(Outcome::Passed) && warnings.is_empty()),
        warnings,
        output_tail: if want_tail {
            output_tail(store, run, 30)
        } else {
            Vec::new()
        },
        code: row.code.clone(),
        fingerprint: row.fingerprint.clone(),
        store: store.dir().display().to_string(),
        history: suite_history(store, &row)?,
    })
}

/// What earlier runs of the same suite say about this one (ADR 0006).
fn suite_history(store: &Store, row: &RunRow) -> Result<Option<serde_json::Value>> {
    let selection = serde_json::from_value::<RunSpec>(row.spec.clone())
        .ok()
        .and_then(|s| s.selection);
    let prev = store.suite_history(&row.suite, selection.as_deref(), row.created_ms, 200)?;
    let Some(last) = prev.first() else {
        return Ok(None);
    };
    let last_passed = prev.iter().find(|r| r.outcome == Some(Outcome::Passed));
    let same = row
        .fingerprint
        .as_ref()
        .map(|f| {
            prev.iter()
                .filter(|r| r.fingerprint.as_ref() == Some(f))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // A cancelled or aborted run is named as such, not by an outcome.
    let same_code_outcomes: Vec<String> = prev
        .iter()
        .filter(|r| r.code == row.code)
        .map(|r| match r.status {
            Status::Finished => r.outcome.map(|o| o.as_str().to_owned()).unwrap_or_default(),
            s => s.as_str().to_owned(),
        })
        .collect();
    Ok(Some(serde_json::json!({
        "previous_runs": prev.len(),
        "selection": selection,
        "last": { "run": last.id, "outcome": last.outcome, "code": last.code },
        "last_passed": last_passed.map(|r| serde_json::json!({ "run": r.id, "code": r.code })),
        "same_failure_seen": same.len(),
        "same_failure_first_run": same.last().map(|r| r.id.clone()),
        "same_code_outcomes": same_code_outcomes,
    })))
}
