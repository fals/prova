//! Run engine: create, supervise, wait (ADR 0002, 0004, 0013).
//!
//! `supervise` runs in its own detached process, one per run. Everything
//! else talks to it through the store: events out, cancel requests in.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::{CommandSpec, RunSpec};
use crate::identity::{code_identity, machine_fingerprint, output_fingerprint, recorded_env};
use crate::ids::{new_run_id, now_ms};
use crate::lifecycle::Lifecycle;
use crate::model::{
    BudgetFrom, EVENT_VERSION, Event, EventKind, Level, Outcome, StallBasis, Status, TestKey,
};
use crate::process;
use crate::store::{NewRun, RunRow, Store, StoreError};

pub type Result<T> = std::result::Result<T, StoreError>;

const TICK: Duration = Duration::from_millis(100);
/// Ends the warnings whose run is no evidence about any test (its results
/// may not come from running them): history leaves such runs out.
pub const NO_EVIDENCE: &str = "; no evidence about the tests";

const SAMPLE_EVERY: Duration = Duration::from_millis(500);
/// CPU use that counts as activity: at least this many clock ticks (1/100 s
/// each) within `BUSY_WINDOW`, i.e. 2% of one core. An idle JVM or .NET
/// runtime still ticks now and then (GC, JIT, timers), and any tick used to
/// count, so a hung xUnit or JUnit test was never a stall (round 8).
const BUSY_TICKS: u64 = 10;
const BUSY_WINDOW: Duration = Duration::from_secs(5);
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
    /// Tests reported passed whose pass is known to be false (their harness
    /// never reported): recorded as errored, so history does not count them.
    pub not_passed: Vec<String>,
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
    /// A complete line from stream 0 (stdout) or 1 (stderr), with when its
    /// bytes arrived (ADR 0026: test windows are timed by arrival).
    Line(usize, String, Instant),
    /// The start of a line not finished yet (pytest's progress dots before
    /// a hang): shown in the live output at once (round 8).
    Partial(usize, String, Instant),
    Eof,
}

fn reader(id: usize, mut stream: impl Read + Send + 'static, tx: mpsc::Sender<Chunk>) {
    std::thread::spawn(move || {
        let mut pending: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    while let Some(i) = pending.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=i).collect();
                        let line = String::from_utf8_lossy(&line).into_owned();
                        if tx.send(Chunk::Line(id, line, Instant::now())).is_err() {
                            return;
                        }
                    }
                    if !pending.is_empty() {
                        let part = String::from_utf8_lossy(&pending).into_owned();
                        if tx.send(Chunk::Partial(id, part, Instant::now())).is_err() {
                            return;
                        }
                    }
                }
            }
        }
        if !pending.is_empty() {
            let _ = tx.send(Chunk::Line(
                id,
                String::from_utf8_lossy(&pending).into_owned(),
                Instant::now(),
            ));
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
    /// CPU ticks of waited-for children when the command started.
    /// Children's CPU ticks at the start, when the OS reports them.
    cpu_start: Option<u64>,
    /// The framework's own test count, when it printed one.
    framework_total: Option<u64>,
    /// Text of an unfinished line already written to the spool, per stream.
    partial: [String; 2],
    /// The run is being cancelled or killed: a test that "fails" now was
    /// killed by it, and says nothing about the code (round 7).
    terminating: bool,
    /// Running tests: (key, started, slow budget, slow reported).
    running: BTreeMap<String, (TestKey, Instant, (Duration, BudgetFrom), bool)>,
    /// The command's process group, once started (0 before).
    pgid: u32,
    /// What each running test was seen to use (ADR 0025).
    sizes: BTreeMap<String, SizeTrack>,
    /// The last snapshot and when it was taken: reused for 20 ms, so a
    /// burst of short tests does not rescan the process table each time.
    last_usage: Option<(Instant, crate::resources::Usage)>,
    /// Every pid and socket seen so far: a test is held only to what was
    /// first seen after its start (anything older is another test's or the
    /// runner's).
    known_pids: std::collections::BTreeSet<u32>,
    known_sockets: std::collections::BTreeSet<u64>,
    /// Processes that run a single test (`--exact`), with what their
    /// subtree was seen to use: matched to the test when it finishes, so a
    /// runner that reports starts late (nextest) still gets sizes.
    one_test_procs: BTreeMap<u32, (String, crate::resources::Seen)>,
    /// The suite's declared size, checked against what tests use.
    declared: Option<crate::config::Size>,
    /// When the line being handled arrived (None for stream files).
    line_at: Option<Instant>,
    /// When the run's first output line arrived, in ms since boot.
    first_line_ms: Option<u64>,
    /// This supervisor's start, as an Instant and in ms since boot (Linux):
    /// converts arrival times to compare with process start times.
    boot: Option<(Instant, u64)>,
    /// The run's test phase, from its first test event (ADR 0026 §2).
    phase: Option<Phase>,
}

/// What the run's tree used while its tests ran, beyond the runner.
struct Phase {
    start_ms: Option<u64>,
    base_pids: std::collections::BTreeSet<u32>,
    base_sockets: std::collections::BTreeSet<u64>,
    /// When the latest test event arrived: samples after it may be the
    /// runner's teardown, so only earlier ones count.
    last_event: Instant,
    samples: Vec<(Instant, crate::resources::Seen)>,
}

/// One running test's size observation (ADR 0025).
struct SizeTrack {
    /// The test's name as a runner would pass it to its own process.
    name: String,
    /// Pids and sockets seen up to its start.
    base_pids: std::collections::BTreeSet<u32>,
    base_sockets: std::collections::BTreeSet<u64>,
    /// It has been the only running test since it started.
    solo: bool,
    /// Its own process, when the runner started one for it.
    own: Option<u32>,
    seen: crate::resources::Seen,
    /// When its start line was read, and that in ms since boot (Linux).
    started: Instant,
    start_ms: Option<u64>,
    /// The latest sample's observation, kept until it is known to fall
    /// inside the test: the program may already be in the next test when
    /// prova reads a line, so a sample counts only when taken
    /// `SIZE_GUARD` after the start line and before the finish line.
    pending: Option<(Instant, crate::resources::Seen)>,
}

/// How far from a test's start and finish lines a sample must be to count
/// for that test (ADR 0025 §5: a missed resource is a lower bound, a
/// misattributed one is wrong).
const SIZE_GUARD: Duration = Duration::from_millis(50);

impl Supervisor<'_> {
    fn emit(&mut self, mut kind: EventKind) {
        if let EventKind::RunTotals { tests, .. } = &kind {
            self.framework_total = Some(*tests);
        }
        // The run's test phase starts at its first test event (ADR 0026).
        if matches!(
            kind,
            EventKind::TestStarted { .. } | EventKind::TestFinished { .. }
        ) && self.pgid != 0
        {
            let at = self.line_at.unwrap_or_else(Instant::now);
            if self.phase.is_none() {
                let _ = self.usage_now();
                self.phase = Some(Phase {
                    start_ms: self.boot_ms(at),
                    base_pids: self.known_pids.clone(),
                    base_sockets: self.known_sockets.clone(),
                    last_event: at,
                    samples: Vec::new(),
                });
            } else if let Some(p) = &mut self.phase {
                p.last_event = at;
            }
        }
        // Sizes (ADR 0025): a snapshot at each test's start and finish.
        let mut sized: Option<(TestKey, u32)> = None;
        match &kind {
            EventKind::TestStarted { test } if self.pgid != 0 => {
                // Everything seen up to now is not this test's. (A process it
                // started at once may be in this snapshot and be missed: a
                // lower bound, never another test's.)
                let _ = self.usage_now();
                let alone = self.running.is_empty();
                if !alone {
                    for t in self.sizes.values_mut() {
                        t.solo = false;
                    }
                }
                self.sizes.insert(
                    test.unique(),
                    SizeTrack {
                        name: test.display(),
                        base_pids: self.known_pids.clone(),
                        base_sockets: self.known_sockets.clone(),
                        solo: alone,
                        own: None,
                        seen: Default::default(),
                        started: self.line_at.unwrap_or_else(Instant::now),
                        start_ms: self.boot_ms(self.line_at.unwrap_or_else(Instant::now)),
                        pending: None,
                    },
                );
            }
            EventKind::TestFinished {
                outcome: Outcome::Skipped,
                test,
                ..
            } => {
                // A skipped test used nothing: no evidence about its size.
                self.sizes.remove(&test.unique());
            }
            EventKind::TestFinished { test, attempt, .. }
                if !self.sizes.contains_key(&test.unique()) =>
            {
                // No start seen (nextest reports it with the result): its own
                // process, if a sample saw one, still tells.
                let name = test.display();
                let mine: Vec<u32> = self
                    .one_test_procs
                    .iter()
                    .filter(|(_, (cmd, _))| crate::resources::names_test(cmd, &name))
                    .map(|(p, _)| *p)
                    .collect();
                if !mine.is_empty() {
                    let mut seen = crate::resources::Seen::default();
                    for p in mine {
                        if let Some((_, s)) = self.one_test_procs.remove(&p) {
                            seen.absorb(&s);
                        }
                    }
                    self.sizes.insert(
                        test.unique(),
                        SizeTrack {
                            name,
                            base_pids: Default::default(),
                            base_sockets: Default::default(),
                            solo: false,
                            own: Some(0),
                            seen,
                            started: Instant::now(),
                            start_ms: None,
                            pending: None,
                        },
                    );
                    sized = Some((test.clone(), *attempt));
                }
            }
            EventKind::TestFinished { test, attempt, .. }
                if self.sizes.contains_key(&test.unique()) =>
            {
                // Not observed now: the program may already be in the next
                // test. Only a sample taken well before this line counts.
                let read = self.line_at.unwrap_or_else(Instant::now);
                if let Some(t) = self.sizes.get_mut(&test.unique())
                    && let Some((at, seen)) = t.pending.take()
                    && read.saturating_duration_since(at) >= SIZE_GUARD
                {
                    t.seen.absorb(&seen);
                }
                let _ = self.usage_now();
                sized = Some((test.clone(), *attempt));
            }
            _ => {}
        }
        if self.terminating
            && let EventKind::TestFinished {
                outcome, message, ..
            } = &mut kind
            && outcome.is_failure()
        {
            *outcome = Outcome::Errored;
            *message = Some("test did not finish before the run ended (killed by it)".into());
        }
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
                // One lifecycle entry per test: a retry restarts it after a
                // failure, and its last attempt decides (ADR 0024).
                self.lifecycle.test_started(&test.unique()).is_ok()
            }
            EventKind::TestFinished {
                test,
                attempt,
                outcome,
                ..
            } => {
                self.running.remove(&test.unique());
                let _ = attempt;
                let ok = self
                    .lifecycle
                    .test_finished(&test.unique(), *outcome)
                    .is_ok();
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
        if let Some((test, attempt)) = sized
            && let Some(t) = self.sizes.remove(&test.unique())
            && (t.own.is_some() || t.solo)
            // Nothing sampled inside it (its lines came at once, or it was
            // too short): no size can be claimed (ADR 0026).
            && t.seen.samples > 0
        {
            let size = t.seen.size();
            let evidence = t.seen.evidence();
            if let Some(declared) = self.declared
                && size > declared
            {
                self.emit(EventKind::RunNote {
                    message: format!(
                        "{} is declared {} but {evidence} (ADR 0025: size is what a test uses)",
                        test.display(),
                        declared.as_str()
                    ),
                });
            }
            self.emit(EventKind::TestSize {
                test,
                attempt,
                size,
                evidence,
            });
        }
    }

    /// History p95 times 3 (at least 1s), else the size default (ADR 0004).
    fn slow_budget(&self, test: &TestKey) -> (Duration, BudgetFrom) {
        let id = crate::store::test_id(&self.suite, test);
        // The history is shared across suites (ADR 0018): a test this suite
        // has not recorded yet may be known from another (round 9: 60 s
        // default in `make` while `default` knew it takes 3 ms).
        let known = match crate::analysis::stats(self.store, &id, 20) {
            Ok(Some(s)) => Ok(Some(s)),
            _ => {
                let other: Option<String> = self
                    .store
                    .conn()
                    .query_row(
                        "SELECT id FROM tests WHERE display = ?1 LIMIT 1",
                        rusqlite::params![test.display()],
                        |r| r.get(0),
                    )
                    .ok();
                match other {
                    Some(o) => crate::analysis::stats(self.store, &o, 20),
                    None => Ok(None),
                }
            }
        };
        match known {
            Ok(Some(s)) if s.p95_ms.is_some() => (
                Duration::from_millis((s.p95_ms.unwrap_or(0) * 3).max(1000)),
                BudgetFrom::History,
            ),
            _ => (self.default_slow, BudgetFrom::Default),
        }
    }

    /// Emits `test_slow` once per running test over its budget.
    fn check_slow(&mut self) {
        let now = Instant::now();
        let mut slow = Vec::new();
        for (key, started, (budget, from), reported) in self.running.values_mut() {
            let elapsed = now.duration_since(*started);
            if !*reported && elapsed > *budget {
                *reported = true;
                slow.push(EventKind::TestSlow {
                    test: key.clone(),
                    elapsed_ms: elapsed.as_millis() as u64,
                    budget_ms: budget.as_millis() as u64,
                    budget_from: *from,
                });
            }
        }
        for e in slow {
            self.emit(e);
        }
    }

    /// An instant in ms since boot, where the OS says (Linux).
    fn boot_ms(&self, at: Instant) -> Option<u64> {
        let (i0, ms0) = self.boot?;
        Some(if at >= i0 {
            ms0 + at.duration_since(i0).as_millis() as u64
        } else {
            ms0.saturating_sub(i0.duration_since(at).as_millis() as u64)
        })
    }

    /// The run's test phase as one observation: samples taken before its
    /// last test event, at least two of them, or nothing (ADR 0026 §2).
    fn phase_seen(&self) -> Option<crate::resources::Seen> {
        let p = self.phase.as_ref()?;
        let mut seen = crate::resources::Seen::default();
        for (at, s) in &p.samples {
            if p.last_event.saturating_duration_since(*at) >= SIZE_GUARD {
                seen.absorb(s);
            }
        }
        (seen.samples >= 2).then_some(seen)
    }

    /// A snapshot of the command's group, reused when under 50 ms old.
    fn usage_now(&mut self) -> crate::resources::Usage {
        if let Some((at, u)) = &self.last_usage
            && at.elapsed() < Duration::from_millis(20)
        {
            return u.clone();
        }
        let u = crate::resources::sample(self.pgid);
        self.known_pids.extend(u.procs.keys().copied());
        self.known_sockets.extend(u.sockets.keys().copied());
        for (pid, (_, cmd)) in &u.procs {
            if crate::resources::runs_one_test(cmd) {
                let e = self
                    .one_test_procs
                    .entry(*pid)
                    .or_insert_with(|| (cmd.clone(), Default::default()));
                e.1.absorb(&crate::resources::seen_in_subtree(&u, *pid));
            }
        }
        self.last_usage = Some((Instant::now(), u.clone()));
        u
    }

    /// Adds what each running test can be held to (ADR 0025 §4): the
    /// subtree of a process that names it, or, while it runs alone,
    /// whatever appeared in the group since it started.
    fn observe_sizes(&mut self, now: &crate::resources::Usage) {
        let now_boot = self.boot_ms(Instant::now());
        // The run's test phase: what started inside it, beyond the runner.
        if let Some(p) = &mut self.phase {
            let mut s = crate::resources::seen_new(&p.base_pids, &p.base_sockets, now);
            if let (Some(from), Some(to), Some(since)) = (p.start_ms, now_boot, self.first_line_ms)
            {
                s.processes = crate::resources::started_within(now, from, to, since);
            } else {
                s.processes = now
                    .procs
                    .iter()
                    .filter(|(pid, (_, cmd))| {
                        !p.base_pids.contains(pid) && !crate::resources::runner_process(cmd)
                    })
                    .map(|(_, (_, cmd))| {
                        cmd.split(' ')
                            .next()
                            .unwrap_or("")
                            .rsplit('/')
                            .next()
                            .unwrap_or("")
                            .to_owned()
                    })
                    .filter(|n| !n.is_empty())
                    .collect();
            }
            s.samples = 1;
            p.samples.push((Instant::now(), s));
        }
        let alone = self.running.len() <= 1;
        let first_line = self.first_line_ms;
        for t in self.sizes.values_mut() {
            if t.own.is_none() {
                t.own = now
                    .procs
                    .iter()
                    .find(|(_, (_, cmd))| crate::resources::names_test(cmd, &t.name))
                    .map(|(pid, _)| *pid);
            }
            if let Some(own) = t.own {
                // A process tree cannot belong to the next test.
                let mut s = crate::resources::seen_in_subtree(now, own);
                s.samples = 1;
                t.seen.absorb(&s);
            } else if !alone {
                t.solo = false;
                t.pending = None;
            } else if t.solo && t.started.elapsed() >= SIZE_GUARD {
                // The previous sample was followed by this one inside the
                // test: it counts. This one waits for the next.
                if let Some((_, seen)) = t.pending.take() {
                    t.seen.absorb(&seen);
                }
                let mut s = crate::resources::seen_new(&t.base_pids, &t.base_sockets, now);
                // Where start times exist, a process is the test's if it
                // started inside it, though it was there when its start
                // line was read (ADR 0026 §1).
                if let (Some(from), Some(to), Some(since)) = (t.start_ms, now_boot, first_line) {
                    s.processes = crate::resources::started_within(now, from, to, since);
                }
                s.samples = 1;
                t.pending = Some((Instant::now(), s));
            }
        }
    }

    fn flush(&mut self) -> Result<()> {
        let events = std::mem::take(&mut self.pending);
        self.store.append_events(&events)
    }

    /// The start of a line not finished yet: into the spool at once, so
    /// `prova output` and a stall summary show it.
    fn partial(&mut self, stream: usize, text: &str) {
        let done = &mut self.partial[stream];
        let new = text.strip_prefix(done.as_str()).unwrap_or(text);
        let _ = self.output.write_all(new.as_bytes());
        let _ = self.output.flush();
        *done = text.to_owned();
    }

    fn line(&mut self, stream: usize, line: &str, observer: &mut dyn Observer) {
        let shown = std::mem::take(&mut self.partial[stream]);
        let _ = self
            .output
            .write_all(line.strip_prefix(shown.as_str()).unwrap_or(line).as_bytes());
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
        terminating: false,
        partial: Default::default(),
        framework_total: None,
        cpu_start: None,
        pgid: 0,
        sizes: BTreeMap::new(),
        last_usage: None,
        known_pids: Default::default(),
        known_sockets: Default::default(),
        line_at: None,
        first_line_ms: None,
        boot: crate::process::uptime_ms().map(|ms| (Instant::now(), ms)),
        phase: None,
        one_test_procs: BTreeMap::new(),
        declared: spec.size,
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
        .env(
            "PROVA_FILTER",
            spec.selection
                .as_deref()
                .map(|f| crate::config::framework_filter(&spec.command.display(), f))
                .unwrap_or_default(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let reason = format!("could not start {}: {e}", spec.command.display());
            sup.line(1, &format!("prova: {reason}\n"), observer);
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
    sup.pgid = pgid;
    sup.cpu_start = process::children_ticks();
    sup.emit(EventKind::RunStarted { pid: pgid });
    if !injected.is_empty() {
        // Short: the temporary paths are noise in every summary (round 9).
        let vars: Vec<String> = injected
            .iter()
            .map(|(k, v)| match v.find('/') {
                Some(i) => format!("{k}={}…", &v[..i]),
                None => format!("{k}={v}"),
            })
            .collect();
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
        reader(0, out, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        reader(1, err, tx.clone());
    }
    drop(tx);

    let stall_after = Duration::from_millis(spec.stall_after_ms);
    let kill_after = Duration::from_millis(spec.kill_after_ms);
    let start = Instant::now();
    let mut last_activity = Instant::now();
    let mut last_sample = Instant::now();
    let mut sample = process::sample_tree(pgid);
    let mut recent_ticks: VecDeque<(Instant, u64)> = VecDeque::new();
    let mut stalled = false;
    let mut terminating: Option<(Instant, Status, String)> = None;
    let mut open_streams = 2;

    let exit = loop {
        // Drain output for up to one tick.
        let deadline = Instant::now() + TICK;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(Chunk::Line(i, l, at)) => {
                    last_activity = Instant::now();
                    sup.line_at = Some(at);
                    if sup.first_line_ms.is_none() {
                        sup.first_line_ms = sup.boot_ms(at);
                    }
                    sup.line(i, &l, observer);
                    sup.line_at = None;
                }
                Ok(Chunk::Partial(i, p, _)) => {
                    last_activity = Instant::now();
                    sup.partial(i, &p);
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
            let usage = sup.usage_now();
            sup.observe_sizes(&usage);
            // The whole tree: a runner may start tests in groups of their
            // own (nextest), and their CPU is the run's activity.
            let s = process::sample_tree(pgid);
            recent_ticks.push_back((now, s.cpu_ticks.saturating_sub(sample.cpu_ticks)));
            while recent_ticks
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t) > BUSY_WINDOW)
            {
                recent_ticks.pop_front();
            }
            let busy = recent_ticks.iter().map(|(_, d)| d).sum::<u64>() >= BUSY_TICKS;
            if busy || !s.pids.is_subset(&sample.pids) {
                last_activity = now;
            }
            sample = s;
            if terminating.is_none() && store.cancel_requested(run)? {
                process::terminate_group(pgid);
                sup.terminating = true;
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
            sup.terminating = true;
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
    // so never wait on EOF without a bound. The bound is on waiting for
    // output, not on recording it: every line that arrived before the
    // deadline is recorded however long that takes (round 12: with history,
    // recording 723 Go results took over 1 s and two thirds were dropped
    // from a run that still passed).
    let drain = |sup: &mut Supervisor<'_>, open: &mut i32, observer: &mut dyn Observer| {
        let until = Instant::now() + DRAIN_AFTER_EXIT;
        while *open > 0 {
            let chunk = match rx.try_recv() {
                Ok(c) => c,
                Err(mpsc::TryRecvError::Disconnected) => break,
                Err(mpsc::TryRecvError::Empty) => {
                    let Some(left) = until.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    match rx.recv_timeout(left) {
                        Ok(c) => c,
                        Err(_) => break,
                    }
                }
            };
            match chunk {
                // A leaked process still writing: what came after the
                // deadline is not waited for.
                Chunk::Line(_, _, at) if at > until => break,
                // A read boundary mid-line sends a partial too: judged by
                // when it arrived, like a line (round 12 CI: 388 of 400).
                Chunk::Partial(_, _, at) if at > until => break,
                Chunk::Line(i, l, _) => sup.line(i, &l, observer),
                Chunk::Partial(i, p, _) => sup.partial(i, &p),
                Chunk::Eof => *open -= 1,
            }
        }
    };
    drain(sup, &mut open_streams, observer);
    let leaked: Vec<u32> = process::sample_group(pgid).pids.into_iter().collect();
    if !leaked.is_empty() {
        process::kill_group(pgid);
        drain(sup, &mut open_streams, observer);
    }
    // The test phase's size (ADR 0026 §2): what tests prova could not
    // isolate are held to. Only when the phase was sampled at least twice
    // (lines that all came at the end tell nothing about when tests ran).
    if let Some(seen) = sup.phase_seen() {
        let size = seen.size();
        let evidence = seen.evidence();
        // A declared size is checked against the run as well (§5).
        if let Some(declared) = sup.declared
            && size > declared
        {
            sup.emit(EventKind::RunNote {
                message: format!(
                    "this suite is declared {} but its run {evidence} (ADR 0025: size is what tests use)",
                    declared.as_str()
                ),
            });
        }
        sup.emit(EventKind::RunSize { size, evidence });
    }

    let report = observer.on_exit(Path::new(store.root()), started_ms);
    let from_files = report.events.iter().any(|e| {
        matches!(
            e,
            EventKind::TestFinished {
                source: Level::ResultFiles,
                ..
            }
        )
    });
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
    let not_passed = report.not_passed;
    let mut contradictions = report.contradictions;
    // A test whose harness never reported, in a run that ended on its own:
    // the run fails (a contradiction), and none of its results is evidence
    // (attempts are immutable, so the run is marked, not rewritten). A run
    // that was killed or cancelled cut the output itself (round 11).
    if status == Status::Finished {
        if !not_passed.is_empty() {
            sup.emit(EventKind::RunWarning {
                message: format!(
                    "{} test(s) were reported passed but their harness never finished, e.g. {}{NO_EVIDENCE}",
                    not_passed.len(),
                    not_passed[0]
                ),
            });
        }
    } else {
        // prova stopped it: a harness that never finished, or a test
        // binary cut off before its summary, is the cancel, not a test
        // that exits early (round 12).
        contradictions.retain(|c| {
            !c.contains("never reported a result") && !c.contains("the tests after it never ran")
        });
    }
    let mut warnings = report.warnings;
    let observed = sup.lifecycle.outcomes().len() + sup.lifecycle.open().len();
    if spec.expects_tests() && status == Status::Finished {
        // A selection that only skipped tests ran none (round 11: vitest -t
        // reports the rest as skipped; 0 passed, 5 skipped exited 0).
        let ran = sup
            .lifecycle
            .outcomes()
            .values()
            .filter(|o| **o != Outcome::Skipped)
            .count()
            + sup.lifecycle.open().len();
        if let Some(sel) = &spec.selection
            && (observed == 0 || ran == 0)
            && sup.level > Level::Opaque
        {
            contradictions.push(format!(
                "selection {sel:?} matched no test: nothing was verified"
            ));
        }
        if spec.selection.is_none() {
            sup.flush()?; // this run's results must be in the store to compare
            // A skipped test did not run (round 9: `#[ignore]` on a test
            // that passed before gave a clean pass).
            let current: Vec<String> = {
                let mut stmt = store.conn().prepare(
                    "SELECT DISTINCT test_id FROM attempts WHERE run_id = ?1 AND outcome <> 'skipped'",
                )?;
                stmt.query_map(rusqlite::params![run], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            if let Some((prev, missing, total)) =
                missing_since_last_pass(store, run, &sup.suite, &current)?
            {
                // Quiet output can hide every test while the framework counted
                // them: that is not "none ran" (round 9).
                if current.is_empty() && observed == 0 && sup.framework_total.unwrap_or(0) == 0 {
                    // Say why when the output does (a compile error, round 8).
                    let cause = (exit_code != Some(0))
                        .then(|| sup.distiller.first_crash())
                        .flatten()
                        .map(|(_, t)| {
                            format!(
                                "; the command failed first: {}",
                                t.trim().chars().take(160).collect::<String>()
                            )
                        })
                        .unwrap_or_default();
                    contradictions.push(format!(
                        "the suite's last passing run {prev} ran {total} test(s); this run observed none{cause}"
                    ));
                } else if !missing.is_empty() {
                    let names: Vec<&str> = missing.iter().take(5).map(String::as_str).collect();
                    warnings.push(format!(
                        "{} test(s) from the last passing run {prev} did not run (missing or skipped): {}{}",
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
    // Results that cannot come from this run: a test reported (or known
    // from history) to take longer than the whole run (round 9: a Makefile
    // that echoes `test x ... ok`, a stale JUnit file refreshed by touch).
    if status == Status::Finished && spec.expects_tests() {
        sup.flush()?;
        let wall = now_ms().saturating_sub(started_ms);
        let mut stmt = store.conn().prepare(
            "SELECT t.display, a.duration_ms,
                    -- the same code only: changed code may be faster
                    (SELECT MIN(h.duration_ms) FROM attempts h JOIN runs r ON r.id = h.run_id
                      WHERE h.test_id = a.test_id AND h.run_id <> a.run_id AND r.status = 'finished'
                        AND r.code = (SELECT code FROM runs WHERE id = a.run_id)
                        AND h.outcome = 'passed' AND h.duration_ms IS NOT NULL),
                    (SELECT COUNT(*) FROM attempts h JOIN runs r ON r.id = h.run_id
                      WHERE h.test_id = a.test_id AND h.run_id <> a.run_id AND r.status = 'finished'
                        AND r.code = (SELECT code FROM runs WHERE id = a.run_id)
                        AND h.outcome = 'passed' AND h.duration_ms IS NOT NULL)
               FROM attempts a JOIN tests t ON t.id = a.test_id
              WHERE a.run_id = ?1 AND a.outcome = 'passed'",
        )?;
        let rows: Vec<(String, Option<i64>, Option<i64>, i64)> = stmt
            .query_map(rusqlite::params![run], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let limit = wall as i64 + 200;
        let impossible = rows.iter().find_map(|(name, own, fastest, n)| {
            if own.is_some_and(|d| d > limit) {
                Some(format!(
                    "{name} reports {} but the whole run took {}",
                    crate::ids::format_duration(Duration::from_millis(own.unwrap_or(0) as u64)),
                    crate::ids::format_duration(Duration::from_millis(wall))
                ))
            } else if *n >= 2 && fastest.is_some_and(|d| d > limit) {
                Some(format!(
                    "{name} never took less than {} before, but the whole run took {}",
                    crate::ids::format_duration(Duration::from_millis(fastest.unwrap_or(0) as u64)),
                    crate::ids::format_duration(Duration::from_millis(wall))
                ))
            } else {
                None
            }
        });
        if let Some(why) = impossible {
            warnings.push(format!(
                "{why}: these results did not come from running the tests now (printed or stale output?){NO_EVIDENCE}"
            ));
        }
        // A test skipped now that last passed, in any suite, did not run: the
        // suite's own last pass may not exist (round 10: `@Disabled` on a failing
        // test passed in an ad-hoc `mvn test`).
        if status == Status::Finished && spec.selection.is_none() {
            sup.flush()?;
            let skipped: Vec<(String, String)> = {
                let mut stmt = store.conn().prepare(
                    "SELECT a.test_id, t.display FROM attempts a JOIN tests t ON t.id = a.test_id
                  WHERE a.run_id = ?1 AND a.outcome = 'skipped'",
                )?;
                stmt.query_map(rusqlite::params![run], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?
            };
            let mut was_passing = Vec::new();
            for (id, display) in skipped {
                if warnings.iter().any(|w| w.contains(&display)) {
                    continue;
                }
                // Skipped here last time too: this suite does not run it (a
                // medium suite ignoring large tests), not a test switched off.
                let skipped_here_before: bool = store
                    .conn()
                    .query_row(
                        "SELECT a.outcome = 'skipped' FROM attempts a JOIN runs r ON r.id = a.run_id
                          WHERE a.test_id = ?1 AND a.run_id <> ?2 AND r.status = 'finished'
                          ORDER BY r.created_ms DESC LIMIT 1",
                        rusqlite::params![id, run],
                        |r| r.get(0),
                    )
                    .unwrap_or(false);
                if skipped_here_before {
                    continue;
                }
                let last = crate::analysis::stats(store, &id, 20)?.and_then(|st| {
                    st.recent
                        .iter()
                        .find(|r| r.run != run && r.outcome != Outcome::Skipped)
                        .map(|r| (r.outcome, r.run.clone()))
                });
                if let Some((Outcome::Passed, r)) = last {
                    was_passing.push(format!("{display} (passed in run {r})"));
                }
            }
            if !was_passing.is_empty() {
                warnings.push(format!(
                    "{} test(s) that passed before were skipped now, so they verified nothing: {}",
                    was_passing.len(),
                    was_passing
                        .iter()
                        .take(5)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        // Passing tests from a command that used no CPU at all: printed, not run
        // (round 10: `echo "--- PASS: TestDiv"` passed in a Java project). Any
        // real test framework uses at least a clock tick.
        if status == Status::Finished
            && sup.cpu_start.is_some()
            && process::children_ticks() == sup.cpu_start
        {
            let passed: i64 = store.conn().query_row(
                "SELECT COUNT(*) FROM attempts WHERE run_id = ?1 AND outcome = 'passed'",
                rusqlite::params![run],
                |r| r.get(0),
            )?;
            if passed > 0 {
                warnings.push(format!(
                "{passed} test(s) passed, but the command used no CPU time at all: the results were printed, not produced by running tests{NO_EVIDENCE}"
            ));
            }
        }
        // Result files are shared by every process in the checkout: another run
        // going at the same time may have written them (round 10: a run that
        // ran no test reported another run's 5 passes).
        if status == Status::Finished && from_files {
            let overlapping: Option<String> = store
            .conn()
            .query_row(
                "SELECT id FROM runs WHERE id <> ?1 AND started_ms IS NOT NULL AND started_ms <= ?2
                   AND (ended_ms IS NULL OR ended_ms >= ?3)
                   AND cwd = (SELECT cwd FROM runs WHERE id = ?1) LIMIT 1",
                rusqlite::params![run, now_ms() as i64, started_ms as i64],
                |r| r.get(0),
            )
            .ok();
            if let Some(other) = overlapping {
                warnings.push(format!(
                "run {other} ran in this checkout at the same time, and this run's results come from result files that either may have written{NO_EVIDENCE}"
            ));
            }
        }
        // Every duration equal, to the millisecond, to an earlier run's: the
        // same result files read again (round 10: stale reports refreshed
        // with `touch`, in a run longer than any test).
        let mine: Vec<(String, i64)> = {
            let mut stmt = store.conn().prepare(
                "SELECT test_id, duration_ms FROM attempts WHERE run_id = ?1 AND duration_ms > 0 ORDER BY test_id, attempt",
            )?;
            stmt.query_map(rusqlite::params![run], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        // Fast tests repeat the same coarse durations by nature: only a
        // run with real time in it is identifiable this way.
        // Three or more tests of 10 ms or more: the same results by
        // coincidence would need every one of them to match (round 11:
        // a 1.5 s sleep and four 3 ms tests matched once).
        if mine.iter().filter(|(_, d)| *d >= 10).count() >= 3 {
            let mut stmt = store.conn().prepare(
                "SELECT DISTINCT run_id FROM attempts WHERE test_id = ?1 AND duration_ms = ?2 AND run_id <> ?3",
            )?;
            let others: Vec<String> = stmt
                .query_map(rusqlite::params![mine[0].0, mine[0].1, run], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut theirs_stmt = store.conn().prepare(
                "SELECT test_id, duration_ms FROM attempts WHERE run_id = ?1 AND duration_ms > 0 ORDER BY test_id, attempt",
            )?;
            for other in others {
                let theirs: Vec<(String, i64)> = theirs_stmt
                    .query_map(rusqlite::params![other], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                if theirs == mine {
                    warnings.push(format!(
                        "every test's duration equals run {other}'s to the millisecond: these are the same results read again (stale result files?), not a new run{NO_EVIDENCE}"
                    ));
                    break;
                }
            }
        }
    }
    // A run cut short where prova saw no test running (result files come
    // only at the end, or quiet output): name the tests of the last passing
    // run that never reported; the hung one is among them (round 8).
    if status != Status::Finished && spec.selection.is_none() && sup.running.is_empty() {
        sup.flush()?;
        let current: Vec<String> = {
            let mut stmt = store
                .conn()
                .prepare("SELECT DISTINCT test_id FROM attempts WHERE run_id = ?1")?;
            stmt.query_map(rusqlite::params![run], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        if let Some((prev, missing, _)) = missing_since_last_run(store, run, &sup.suite, &current)?
            && !missing.is_empty()
        {
            let names: Vec<&str> = missing.iter().take(5).map(String::as_str).collect();
            sup.emit(EventKind::RunNote {
                message: format!(
                    "{} test(s) of the last finished run {prev} never reported before the run was cut short (a hung test is among them): {}{}",
                    missing.len(),
                    names.join(", "),
                    if missing.len() > names.len() { ", ..." } else { "" }
                ),
            });
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
        // A test that passed only on a retry is flaky: the run passes (its
        // last attempt decides, ADR 0024) but the pass is not a green one.
        sup.flush()?;
        let rerun: Vec<String> = {
            let mut stmt = store.conn().prepare(
                "SELECT t.display FROM attempts a JOIN tests t ON t.id = a.test_id
                  WHERE a.run_id = ?1 GROUP BY a.test_id
                 HAVING SUM(a.outcome IN ('failed','errored')) > 0",
            )?;
            stmt.query_map(rusqlite::params![run], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        if !rerun.is_empty() {
            warnings.push(format!(
                "{} test(s) failed, then passed on a retry: flaky, not green: {}",
                rerun.len(),
                rerun.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
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
        let unique = key.as_str();
        let attempt = sup.attempts.get(unique).copied().unwrap_or(1).to_string();
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
            subtest: false,
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
    missing_since(store, run, suite, current, true)
}

/// Like `missing_since_last_pass`, from the last finished full run whatever
/// its outcome: the suspects of a hang (round 8: a suite that never passed
/// yet still names them).
fn missing_since_last_run(
    store: &Store,
    run: &str,
    suite: &str,
    current: &[String],
) -> Result<Option<(String, Vec<String>, usize)>> {
    missing_since(store, run, suite, current, false)
}

fn missing_since(
    store: &Store,
    run: &str,
    suite: &str,
    current: &[String],
    passed_only: bool,
) -> Result<Option<(String, Vec<String>, usize)>> {
    use rusqlite::OptionalExtension;
    let prev: Option<String> = store
        .conn()
        .query_row(
            "SELECT id FROM runs WHERE suite = ?1 AND id <> ?2 AND status = 'finished'
               AND (?3 = 0 OR outcome = 'passed')
               AND json_extract(spec, '$.selection') IS NULL
               -- a pass whose results are discredited is no baseline (round 9:
               -- a forged pass became the reference and failed honest runs
               -- after it); other warnings do not stop a run from becoming
               -- the reference, or one removed test would be reported on
               -- every run after it
               AND (?3 = 0 OR NOT EXISTS (SELECT 1 FROM events e WHERE e.run_id = runs.id AND e.kind = 'run_warning'
                                            AND e.payload LIKE '%; no evidence about the tests%'))
               -- a reference that saw passing tests: quiet output shows only
               -- failures, and named the wrong suspect (round 9)
               AND EXISTS (SELECT 1 FROM attempts a WHERE a.run_id = runs.id AND a.outcome = 'passed')
             ORDER BY created_ms DESC LIMIT 1",
            rusqlite::params![suite, run, passed_only as i64],
            |r| r.get(0),
        )
        .optional()?;
    let Some(prev) = prev else { return Ok(None) };
    let mut stmt = store.conn().prepare(
        "SELECT a.test_id, t.display FROM attempts a JOIN tests t ON t.id = a.test_id
         WHERE a.run_id = ?1 AND a.outcome <> 'skipped' GROUP BY a.test_id ORDER BY t.display",
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
    pub budget_from: BudgetFrom,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Counts {
    /// Tests, not counting subtests: a Go `t.Run` inside a test that reports
    /// its own result is counted once, under `subtests`.
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    pub errored: u64,
    /// Results nested under another test of this run (`TestX/case`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub subtests: u64,
    /// Of them, failed or errored.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub subtests_failed: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Counts {
    /// Every result seen, subtests included.
    pub fn results(&self) -> u64 {
        self.passed + self.failed + self.skipped + self.errored + self.subtests
    }
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
    /// Known flaky tests (ADR 0018) that passed in this run: luck, not proof.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub flaky_passed: Vec<String>,
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
    // The supervisor died, but the command may not have: kill what is left
    // of its process group (round 9: nextest and a hung test ran on).
    kill_leftovers(store, &row.id)?;
    store.abort_orphan(
        &row.id,
        row.status,
        row.supervisor_pid,
        now_ms(),
        "supervisor exited without ending the run",
    )
}

/// Kills what is left of a run's command: the members of its process group
/// (whose id is the command's pid) that carry the run's id. Returns how
/// many were killed.
pub fn kill_leftovers(store: &Store, run: &str) -> Result<usize> {
    use rusqlite::OptionalExtension;
    let payload: Option<String> = store
        .conn()
        .query_row(
            "SELECT payload FROM events WHERE run_id = ?1 AND kind = 'run_started' LIMIT 1",
            rusqlite::params![run],
            |r| r.get(0),
        )
        .optional()?;
    let Some(pgid) = payload
        .and_then(|p| serde_json::from_str::<serde_json::Value>(&p).ok())
        .and_then(|v| v["pid"].as_u64().or(v["kind"]["pid"].as_u64()))
    else {
        return Ok(0);
    };
    // Long after the run, the group id may belong to someone else: kill
    // only what carries this run's id.
    Ok(process::kill_run_processes(pgid as u32, run))
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
    // (test, outcome, message, seq, subtest)
    let mut finals: BTreeMap<String, (TestKey, Outcome, Option<String>, u64, bool)> =
        BTreeMap::new();
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
                subtest,
                ..
            } => {
                running.retain(|(u, _)| *u != test.unique());
                slow.remove(&test.unique());
                finals.insert(
                    test.unique(),
                    (test.clone(), *outcome, message.clone(), e.seq, *subtest),
                );
            }
            EventKind::TestSlow {
                test,
                elapsed_ms,
                budget_ms,
                budget_from,
            } => {
                // How long it has been running now: what the slow event said
                // plus the time since (a runner's own SLOW line may be the
                // first sign of the test, so its start time can be late),
                // keeping the largest budget seen (round 6).
                let now = now_ms();
                let elapsed = (elapsed_ms + now.saturating_sub(e.ts_ms)).max(
                    started_at
                        .get(&test.unique())
                        .map(|t| now.saturating_sub(*t))
                        .unwrap_or(0),
                );
                let entry = slow.entry(test.unique()).or_insert(SlowTest {
                    test: test.display(),
                    elapsed_ms: 0,
                    budget_ms: 0,
                    budget_from: *budget_from,
                });
                entry.elapsed_ms = entry.elapsed_ms.max(elapsed);
                if *budget_ms >= entry.budget_ms {
                    entry.budget_ms = *budget_ms;
                    entry.budget_from = *budget_from;
                }
            }
            EventKind::RunStalled { idle_ms, .. } => stalled_ms = Some(*idle_ms),
            EventKind::TestStarted { test } => {
                stalled_ms = None;
                started_at.entry(test.unique()).or_insert(e.ts_ms);
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
    let mut failing: Vec<&(TestKey, Outcome, Option<String>, u64, bool)> = Vec::new();
    for f in finals.values() {
        if f.4 {
            counts.subtests += 1;
            counts.subtests_failed += u64::from(f.1.is_failure());
            if f.1.is_failure() {
                failing.push(f);
            }
            continue;
        }
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
    // Only a verified pass is evidence the suite can pass on this code
    // (round 11: an echoed pass made a real failure "likely flaky").
    let suite_passed_on_same_code = row.status == Status::Finished && {
        let prev = store.suite_history(&row.suite, selection.as_deref(), row.created_ms, 200)?;
        let ids: Vec<String> = prev.iter().map(|r| r.id.clone()).collect();
        let thin = store.unverified(&ids)?;
        prev.iter().any(|r| {
            r.code == row.code && r.outcome == Some(Outcome::Passed) && !thin.contains(&r.id)
        })
    };
    let mut failures = Vec::new();
    for (key, outcome, message, _, _) in failing.iter().take(20).copied() {
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
    // A pass of a known flaky test proves little: say so, or a green run
    // reads as a reliably green suite (round 7).
    let mut flaky_passed = Vec::new();
    if row.status == Status::Finished {
        let passed: std::collections::BTreeSet<String> = finals
            .values()
            .filter(|(_, o, _, _, _)| *o == Outcome::Passed)
            .map(|(k, _, _, _, _)| k.display())
            .collect();
        if !passed.is_empty() {
            for f in crate::analysis::flaky(store, None, 50)? {
                if passed.contains(&f.display) && !flaky_passed.contains(&f.display) {
                    flaky_passed.push(f.display);
                }
            }
        }
    }
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
    // A stalled selection: the selected test is the suspect (round 10).
    if !row.status.is_terminal()
        && stalled_ms.is_some()
        && running.is_empty()
        && let Some(sel) = &selection
    {
        notes.push(format!(
            "the run selected {sel:?}: the hung test is the one it matches"
        ));
    }
    // A stalled run where no test is seen running: name the suspects, the
    // tests of the last passing run that have not reported yet (round 8).
    if !row.status.is_terminal()
        && stalled_ms.is_some()
        && running.is_empty()
        && selection.is_none()
    {
        let current: Vec<String> = {
            let mut stmt = store
                .conn()
                .prepare("SELECT DISTINCT test_id FROM attempts WHERE run_id = ?1")?;
            stmt.query_map(rusqlite::params![run], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        if let Some((prev, missing, _)) = missing_since_last_run(store, run, &row.suite, &current)?
            && !missing.is_empty()
        {
            let names: Vec<&str> = missing.iter().take(5).map(String::as_str).collect();
            notes.push(format!(
                "not reported yet ({} of the last finished run {prev}; the hung test is likely among them): {}{}",
                missing.len(),
                names.join(", "),
                if missing.len() > names.len() { ", ..." } else { "" }
            ));
        }
    }
    // A selection that ran nothing must not look like a pass.
    let ran = counts.results();
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
        // A pass that rests on a known flaky test passing proves little
        // (round 12: `verified: true` next to `flaky_passed`).
        verified: row.status.is_terminal().then(|| {
            row.outcome == Some(Outcome::Passed) && warnings.is_empty() && flaky_passed.is_empty()
        }),
        warnings,
        flaky_passed,
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
    // "Last green" is a verified pass only (round 10: echoed and stale
    // passes were named).
    let ids: Vec<String> = prev.iter().map(|r| r.id.clone()).collect();
    let thin = store.unverified(&ids)?;
    let last_passed = prev
        .iter()
        .find(|r| r.outcome == Some(Outcome::Passed) && !thin.contains(&r.id));
    let unverified_passes = prev
        .iter()
        .filter(|r| {
            r.status == Status::Finished
                && r.outcome == Some(Outcome::Passed)
                && thin.contains(&r.id)
        })
        .count();
    // Only finished runs are evidence of "the same failure" (round 8: a
    // cancelled hang was counted as seen before).
    let same = row
        .fingerprint
        .as_ref()
        .map(|f| {
            prev.iter()
                .filter(|r| r.status == Status::Finished && r.fingerprint.as_ref() == Some(f))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let finished = prev.iter().filter(|r| r.status == Status::Finished).count();
    // A cancelled or aborted run is named as such, not by an outcome.
    let same_code_outcomes: Vec<String> = prev
        .iter()
        .filter(|r| r.code == row.code)
        .map(|r| match r.status {
            // An unverified pass is not a plain "passed" (round 11: "none of
            // them passed; on this exact code: passed").
            Status::Finished if r.outcome == Some(Outcome::Passed) && thin.contains(&r.id) => {
                "passed?".to_owned()
            }
            Status::Finished => r.outcome.map(|o| o.as_str().to_owned()).unwrap_or_default(),
            s => s.as_str().to_owned(),
        })
        .collect();
    Ok(Some(serde_json::json!({
        "previous_runs": finished,
        "cut_short_runs": prev.len() - finished,
        "selection": selection,
        // A cut-short run is named by its status (round 12: JSON said
        // "errored" where the text said "cancelled").
        "last": {
            "run": last.id,
            "outcome": if last.status == Status::Finished {
                last.outcome.map(|o| o.as_str().to_owned())
            } else {
                Some(last.status.as_str().to_owned())
            },
            "code": last.code,
        },
        "unverified_passes": unverified_passes,
        "last_passed": last_passed.map(|r| serde_json::json!({ "run": r.id, "code": r.code })),
        "same_failure_seen": same.len(),
        "same_failure_first_run": same.last().map(|r| r.id.clone()),
        "same_code_outcomes": same_code_outcomes,
        // The same code may have passed in another suite (round 10: "none
        // of them passed" read as a regression).
        "passed_elsewhere": if last_passed.is_none() {
            store
                .conn()
                .query_row(
                    "SELECT id, suite FROM runs r WHERE code = ?1 AND code <> 'nogit' AND suite <> ?2
                       AND status = 'finished' AND outcome = 'passed'
                       AND json_extract(spec, '$.selection') IS NULL
                       AND NOT EXISTS (SELECT 1 FROM events w WHERE w.run_id = r.id AND w.kind = 'run_warning')
                     ORDER BY created_ms DESC LIMIT 1",
                    rusqlite::params![row.code, row.suite],
                    |r| Ok(serde_json::json!({ "run": r.get::<_, String>(0)?, "suite": r.get::<_, String>(1)? })),
                )
                .ok()
        } else {
            None
        },
    })))
}
